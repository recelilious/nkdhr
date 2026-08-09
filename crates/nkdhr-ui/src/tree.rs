//! Retained widget identity, reconciliation, lifecycle and frame passes.

use std::{
    any::{Any, TypeId, type_name},
    collections::{HashMap, HashSet},
    fmt,
    ops::{BitOr, BitOrAssign},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use nkdhr_render::{BuildError, Color, DisplayListBuilder, Point, Rect, TextureStore, Transform};

use crate::reactive::SubscriptionToken;
use crate::text::{TextDrawStats, TextError, TextLayout, TextResources, TextStyle};
use crate::{
    Clock, Constraints, Key, Modifiers, Reactive, RootReactivity, ScrollPhase, SemanticNode,
    Semantics, Size, SystemClock, UiEvent,
};

pub type UiResult<T> = Result<T, UiError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiError {
    InvalidConstraints,
    InvalidSize,
    SizeOutsideConstraints,
    InvalidRect,
    InvalidInsets,
    InvalidGap,
    InvalidFlex,
    InvalidEvent,
    InvalidAnimationDuration,
    LayoutRequired,
    DuplicateKey(WidgetKey),
    WidgetCapacityExceeded,
    MissingWidget(WidgetId),
    StateTypeMismatch(&'static str),
    UnexpectedChildCount {
        expected_maximum: usize,
        actual: usize,
    },
    TextResourcesRequired,
    Text(String),
    DisplayList(BuildError),
}

impl fmt::Display for UiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConstraints => {
                formatter.write_str("layout constraints must be finite, non-negative and ordered")
            }
            Self::InvalidSize => formatter.write_str("widget size must be finite and non-negative"),
            Self::SizeOutsideConstraints => {
                formatter.write_str("widget size fell outside its parent constraints")
            }
            Self::InvalidRect => {
                formatter.write_str("arranged rectangles must be finite and non-negative")
            }
            Self::InvalidInsets => {
                formatter.write_str("layout insets must be finite and non-negative")
            }
            Self::InvalidGap => formatter.write_str("layout gap must be finite and non-negative"),
            Self::InvalidFlex => {
                formatter.write_str("child flex factor must be finite and non-negative")
            }
            Self::InvalidEvent => formatter.write_str("input event contains invalid coordinates"),
            Self::InvalidAnimationDuration => {
                formatter.write_str("animation duration must be greater than zero")
            }
            Self::LayoutRequired => formatter.write_str("layout must complete before paint"),
            Self::DuplicateKey(key) => write!(formatter, "duplicate sibling widget key {key:?}"),
            Self::WidgetCapacityExceeded => formatter.write_str("widget arena capacity exceeded"),
            Self::MissingWidget(id) => write!(formatter, "widget {id:?} is no longer alive"),
            Self::StateTypeMismatch(expected) => {
                write!(formatter, "widget retained state is not {expected}")
            }
            Self::UnexpectedChildCount {
                expected_maximum,
                actual,
            } => write!(
                formatter,
                "widget accepts at most {expected_maximum} child(ren), received {actual}"
            ),
            Self::TextResourcesRequired => {
                formatter.write_str("this widget requires text resources on its UI root")
            }
            Self::Text(error) => write!(formatter, "text operation failed: {error}"),
            Self::DisplayList(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for UiError {}

impl From<BuildError> for UiError {
    fn from(value: BuildError) -> Self {
        Self::DisplayList(value)
    }
}

impl From<TextError> for UiError {
    fn from(value: TextError) -> Self {
        Self::Text(value.to_string())
    }
}

/// Stable arena identity. Reusing a slot changes its generation, so stale
/// focus, capture and reactive subscriptions cannot target a new widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetId {
    index: u32,
    generation: u32,
}

impl WidgetId {
    pub const fn index(self) -> u32 {
        self.index
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Explicit sibling reconciliation key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetKey(pub u64);

impl From<u64> for WidgetKey {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

/// Dirty-pass mask. Layout invalidation always also requires paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Invalidation(u8);

impl Invalidation {
    pub const NONE: Self = Self(0);
    pub const LAYOUT: Self = Self(1 << 0);
    pub const PAINT: Self = Self(1 << 1);
    pub const SEMANTICS: Self = Self(1 << 2);
    pub const ALL: Self = Self(Self::LAYOUT.0 | Self::PAINT.0 | Self::SEMANTICS.0);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    const fn expanded(self) -> Self {
        if self.contains(Self::LAYOUT) {
            Self(self.0 | Self::PAINT.0)
        } else {
            self
        }
    }

    fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl BitOr for Invalidation {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0).expanded()
    }
}

impl BitOrAssign for Invalidation {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = *self | rhs;
    }
}

pub trait AsAny {
    fn as_any(&self) -> &dyn Any;
}

impl<T: Any> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Retained widget behavior. Implementations are descriptors; mutable
/// lifecycle data belongs in the state returned by `create_state`.
pub trait Widget: AsAny + 'static {
    fn create_state(&self) -> Box<dyn Any> {
        Box::new(())
    }

    /// Reconcile new descriptor data against the previous descriptor. The
    /// conservative default requests layout; implementations may override it
    /// and request a narrower pass after comparing properties.
    fn update(&self, _previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> UiResult<Size> {
        let mut desired = constraints.min();
        for index in 0..ctx.child_count() {
            let child = ctx.measure_child(index, constraints)?;
            desired.width = desired.width.max(child.width);
            desired.height = desired.height.max(child.height);
        }
        Ok(constraints.constrain(desired))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> UiResult<()> {
        ctx.arrange_children(rect)
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> UiResult<()> {
        ctx.paint_children()
    }

    /// Advance state that must change before the next layout pass. Paint-only
    /// transitions can continue sampling time from `PaintCtx` instead.
    fn animation(&self, _ctx: &mut AnimationCtx<'_>) {}

    fn event(&self, _ctx: &mut EventCtx<'_>, _event: &UiEvent) -> UiResult<()> {
        Ok(())
    }

    /// Final unconsumed remainder after nested scroll bubbling reaches the
    /// outer boundary. Scroll containers use this for visual elasticity.
    fn scroll_boundary(
        &self,
        _ctx: &mut EventCtx<'_>,
        _delta_x: f32,
        _delta_y: f32,
    ) -> UiResult<()> {
        Ok(())
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx<'_>) -> Semantics {
        Semantics::default()
    }

    fn focusable(&self) -> bool {
        false
    }

    fn focus_scope(&self) -> bool {
        false
    }

    fn accepts_pointer(&self) -> bool {
        false
    }

    fn clips_children(&self) -> bool {
        false
    }
}

/// Declarative input to retained reconciliation.
pub struct Element {
    widget: Box<dyn Widget>,
    key: Option<WidgetKey>,
    flex: f32,
    children: Vec<Self>,
}

impl Element {
    pub fn new(widget: impl Widget) -> Self {
        Self {
            widget: Box::new(widget),
            key: None,
            flex: 0.0,
            children: Vec::new(),
        }
    }

    pub fn keyed(mut self, key: impl Into<WidgetKey>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn flex(mut self, factor: f32) -> Self {
        self.flex = factor;
        self
    }

    pub fn child(mut self, child: Self) -> Self {
        self.children.push(child);
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = Self>) -> Self {
        self.children.extend(children);
        self
    }

    fn validate(&self) -> UiResult<()> {
        if !self.flex.is_finite() || self.flex < 0.0 {
            return Err(UiError::InvalidFlex);
        }
        let mut keys = HashSet::new();
        for child in &self.children {
            if let Some(key) = child.key
                && !keys.insert(key)
            {
                return Err(UiError::DuplicateKey(key));
            }
            child.validate()?;
        }
        Ok(())
    }

    fn widget_type(&self) -> TypeId {
        self.widget.as_ref().as_any().type_id()
    }
}

struct Node {
    parent: Option<WidgetId>,
    key: Option<WidgetKey>,
    flex: f32,
    widget: Box<dyn Widget>,
    state: Box<dyn Any>,
    subscriptions: Vec<SubscriptionToken>,
    children: Vec<WidgetId>,
    measured: Size,
    rect: Rect,
    dirty: Invalidation,
}

struct Slot {
    generation: u32,
    node: Option<Node>,
}

#[derive(Default)]
struct Arena {
    slots: Vec<Slot>,
    free: Vec<u32>,
}

impl Arena {
    fn allocate(&mut self, node: Node) -> UiResult<WidgetId> {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.node.is_none());
            slot.node = Some(node);
            return Ok(WidgetId {
                index,
                generation: slot.generation,
            });
        }
        let index = u32::try_from(self.slots.len()).map_err(|_| UiError::WidgetCapacityExceeded)?;
        let generation = 1;
        self.slots.push(Slot {
            generation,
            node: Some(node),
        });
        Ok(WidgetId { index, generation })
    }

    fn get(&self, id: WidgetId) -> Option<&Node> {
        let slot = self.slots.get(id.index as usize)?;
        (slot.generation == id.generation)
            .then_some(slot.node.as_ref())
            .flatten()
    }

    fn get_mut(&mut self, id: WidgetId) -> Option<&mut Node> {
        let slot = self.slots.get_mut(id.index as usize)?;
        (slot.generation == id.generation)
            .then_some(slot.node.as_mut())
            .flatten()
    }

    fn take(&mut self, id: WidgetId) -> UiResult<Node> {
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
            .ok_or(UiError::MissingWidget(id))?;
        slot.node.take().ok_or(UiError::MissingWidget(id))
    }

    fn restore(&mut self, id: WidgetId, node: Node) {
        let slot = &mut self.slots[id.index as usize];
        debug_assert_eq!(slot.generation, id.generation);
        debug_assert!(slot.node.is_none());
        slot.node = Some(node);
    }

    fn release(&mut self, id: WidgetId) {
        let slot = &mut self.slots[id.index as usize];
        debug_assert_eq!(slot.generation, id.generation);
        debug_assert!(slot.node.is_none());
        slot.generation = slot.generation.wrapping_add(1).max(1);
        self.free.push(id.index);
    }

    fn aggregate_dirty(&self) -> Invalidation {
        self.slots
            .iter()
            .filter_map(|slot| slot.node.as_ref())
            .fold(Invalidation::NONE, |dirty, node| dirty | node.dirty)
    }
}

#[derive(Debug, Clone, Copy)]
enum EffectiveClip {
    Unbounded,
    Rect(Rect),
    Empty,
}

impl EffectiveClip {
    fn intersect(self, rect: Rect) -> Self {
        if rect.is_empty() {
            return Self::Empty;
        }
        match self {
            Self::Unbounded => Self::Rect(rect),
            Self::Rect(parent) => parent.intersect(rect).map_or(Self::Empty, Self::Rect),
            Self::Empty => Self::Empty,
        }
    }

    fn contains(self, point: Point) -> bool {
        match self {
            Self::Unbounded => true,
            Self::Rect(rect) => rect.contains(point),
            Self::Empty => false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PaintEntry {
    id: WidgetId,
    rect: Rect,
    clip: EffectiveClip,
    accepts_pointer: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchResult {
    pub target: Option<WidgetId>,
    pub handled: bool,
    pub focused: Option<WidgetId>,
    pub pointer_capture: Option<WidgetId>,
}

#[derive(Debug, Clone, Copy)]
enum CaptureRequest {
    Set(WidgetId),
    Release,
}

#[derive(Debug, Default)]
struct EventRequests {
    handled: bool,
    focus: Option<WidgetId>,
    capture: Option<CaptureRequest>,
    scroll_remainder: Option<(f32, f32)>,
}

/// One retained UI tree. Hosts call `reconcile`, `layout`, `paint`, `dispatch`
/// and `tick` at explicit boundaries.
pub struct UiRoot {
    arena: Arena,
    root: Option<WidgetId>,
    reactivity: Rc<RootReactivity>,
    clock: Box<dyn Clock>,
    text: Option<TextResources>,
    focus: Option<WidgetId>,
    pointer_capture: Option<WidgetId>,
    pointer_position: Option<Point>,
    hover_path: Vec<WidgetId>,
    animations: HashSet<WidgetId>,
    frame_requested: bool,
    paint_order: Vec<PaintEntry>,
    dirty: Invalidation,
}

impl UiRoot {
    pub fn new(element: Element) -> UiResult<Self> {
        Self::with_clock(element, SystemClock::new())
    }

    pub fn with_clock(element: Element, clock: impl Clock) -> UiResult<Self> {
        Self::with_optional_text(element, clock, None)
    }

    pub fn with_text(element: Element, text: TextResources) -> UiResult<Self> {
        Self::with_clock_and_text(element, SystemClock::new(), text)
    }

    pub fn with_clock_and_text(
        element: Element,
        clock: impl Clock,
        text: TextResources,
    ) -> UiResult<Self> {
        Self::with_optional_text(element, clock, Some(text))
    }

    fn with_optional_text(
        element: Element,
        clock: impl Clock,
        text: Option<TextResources>,
    ) -> UiResult<Self> {
        element.validate()?;
        let mut root = Self {
            arena: Arena::default(),
            root: None,
            reactivity: RootReactivity::new(),
            clock: Box::new(clock),
            text,
            focus: None,
            pointer_capture: None,
            pointer_position: None,
            hover_path: Vec::new(),
            animations: HashSet::new(),
            frame_requested: false,
            paint_order: Vec::new(),
            dirty: Invalidation::ALL,
        };
        let id = root.mount(element, None)?;
        root.root = Some(id);
        Ok(root)
    }

    pub fn text_resources(&self) -> Option<&TextResources> {
        self.text.as_ref()
    }

    pub fn texture_store(&self) -> Option<&TextureStore> {
        self.text.as_ref().map(TextResources::textures)
    }

    pub fn texture_store_mut(&mut self) -> Option<&mut TextureStore> {
        self.text.as_mut().map(TextResources::textures_mut)
    }

    pub fn set_text_output_scale(&mut self, scale: f32) -> UiResult<()> {
        let text = self.text.as_mut().ok_or(UiError::TextResourcesRequired)?;
        text.set_output_scale(scale)?;
        self.dirty |= Invalidation::LAYOUT | Invalidation::SEMANTICS;
        Ok(())
    }

    pub fn root_id(&self) -> WidgetId {
        self.root.expect("a UI root always owns a root widget")
    }

    pub fn is_alive(&self, id: WidgetId) -> bool {
        self.arena.get(id).is_some()
    }

    pub fn rect(&self, id: WidgetId) -> Option<Rect> {
        self.arena.get(id).map(|node| node.rect)
    }

    pub fn measured_size(&self, id: WidgetId) -> Option<Size> {
        self.arena.get(id).map(|node| node.measured)
    }

    pub fn children(&self, id: WidgetId) -> Option<&[WidgetId]> {
        self.arena.get(id).map(|node| node.children.as_slice())
    }

    pub fn state<T: 'static>(&self, id: WidgetId) -> Option<&T> {
        self.arena.get(id)?.state.downcast_ref()
    }

    pub fn focused(&self) -> Option<WidgetId> {
        self.focus.filter(|id| self.is_alive(*id))
    }

    pub fn pointer_capture(&self) -> Option<WidgetId> {
        self.pointer_capture.filter(|id| self.is_alive(*id))
    }

    pub fn hovered(&self) -> Option<WidgetId> {
        self.hover_path
            .first()
            .copied()
            .filter(|id| self.is_alive(*id))
    }

    pub fn pointer_position(&self) -> Option<Point> {
        self.pointer_position
    }

    pub fn focus_path(&self) -> Vec<WidgetId> {
        self.focused()
            .map(|id| self.path_to_root(id))
            .unwrap_or_default()
    }

    pub fn now(&self) -> Duration {
        self.clock.now()
    }

    pub fn reconcile(&mut self, element: Element) -> UiResult<WidgetId> {
        element.validate()?;
        self.flush_reactive();
        let old = self.root;
        let id = self.reconcile_node(old, element, None)?;
        self.root = Some(id);
        self.dirty |= Invalidation::ALL;
        Ok(id)
    }

    pub fn invalidation(&mut self) -> Invalidation {
        self.flush_reactive();
        self.dirty
    }

    pub fn layout(&mut self, viewport: Size) -> UiResult<Size> {
        self.flush_reactive();
        let constraints = Constraints::tight(viewport)?;
        let root = self.root_id();
        let measured = self.measure_node(root, constraints)?;
        self.arrange_node(root, Rect::new(0.0, 0.0, viewport.width, viewport.height))?;
        self.dirty = self.arena.aggregate_dirty();
        Ok(measured)
    }

    pub fn paint(&mut self, builder: &mut DisplayListBuilder) -> UiResult<()> {
        self.flush_reactive();
        if self.dirty.contains(Invalidation::LAYOUT) {
            return Err(UiError::LayoutRequired);
        }
        if let Some(text) = &mut self.text {
            text.begin_frame();
        }
        self.paint_order.clear();
        self.paint_node(self.root_id(), builder, EffectiveClip::Unbounded)?;
        if let Some(position) = self.pointer_position {
            let target = self.hit_test(position);
            self.update_hover(target)?;
        }
        self.dirty = self.arena.aggregate_dirty();
        Ok(())
    }

    pub fn hit_test(&self, position: Point) -> Option<WidgetId> {
        if !position.x.is_finite() || !position.y.is_finite() {
            return None;
        }
        self.paint_order.iter().rev().find_map(|entry| {
            (entry.accepts_pointer
                && entry.rect.contains(position)
                && entry.clip.contains(position)
                && self.is_alive(entry.id))
            .then_some(entry.id)
        })
    }

    pub fn dispatch(&mut self, event: &UiEvent) -> UiResult<DispatchResult> {
        validate_event(event)?;
        self.flush_reactive();
        if event.is_pointer() {
            self.pointer_position = event.pointer_position();
        }
        let pointer_hit = if event.is_pointer() {
            event
                .pointer_position()
                .and_then(|point| self.hit_test(point))
        } else {
            None
        };
        if event.is_pointer() {
            self.update_hover(pointer_hit)?;
        }
        let target = if event.is_pointer() {
            self.pointer_capture().or(pointer_hit)
        } else if event.is_keyboard() {
            self.focused()
        } else {
            None
        };
        let mut requests = EventRequests::default();
        if let Some(target) = target {
            let mut bubbling_event = event.clone();
            let mut boundary = None;
            for id in self.path_to_root(target) {
                let current = self.call_event(id, &bubbling_event)?;
                requests.handled |= current.handled;
                if current.focus.is_some() {
                    requests.focus = current.focus;
                }
                if current.capture.is_some() {
                    requests.capture = current.capture;
                }
                if let Some((delta_x, delta_y)) = current.scroll_remainder {
                    if delta_x.abs() <= f32::EPSILON && delta_y.abs() <= f32::EPSILON {
                        boundary = None;
                        if is_scroll_lifecycle(&bubbling_event) {
                            bubbling_event = with_scroll_delta(&bubbling_event, 0.0, 0.0);
                            continue;
                        }
                        break;
                    }
                    boundary = Some((id, delta_x, delta_y));
                    bubbling_event = with_scroll_delta(&bubbling_event, delta_x, delta_y);
                    continue;
                }
                if current.handled {
                    boundary = None;
                    break;
                }
            }
            if let Some((id, delta_x, delta_y)) = boundary {
                let current = self.call_scroll_boundary(id, delta_x, delta_y)?;
                requests.handled |= current.handled;
                if current.focus.is_some() {
                    requests.focus = current.focus;
                }
                if current.capture.is_some() {
                    requests.capture = current.capture;
                }
            }
        }

        if !requests.handled
            && let UiEvent::KeyDown {
                key: Key::Tab,
                modifiers: Modifiers { shift, .. },
                ..
            } = event
        {
            requests.handled = self.focus_next(*shift)?;
        }
        self.apply_event_requests(&requests)?;
        if matches!(event, UiEvent::PointerCancel) {
            self.pointer_capture = None;
        }
        self.dirty = self.arena.aggregate_dirty();
        Ok(DispatchResult {
            target,
            handled: requests.handled,
            focused: self.focused(),
            pointer_capture: self.pointer_capture(),
        })
    }

    pub fn set_focus(&mut self, focus: Option<WidgetId>) -> UiResult<()> {
        self.change_focus(focus)
    }

    pub fn move_focus(&mut self, backwards: bool) -> UiResult<bool> {
        self.focus_next(backwards)
    }

    pub fn frame_requested(&self) -> bool {
        self.frame_requested
    }

    /// Advance registered animations to the next explicit host frame.
    /// Widgets may update layout state here, then re-register from animation
    /// or paint while their timeline remains active.
    pub fn tick(&mut self) -> bool {
        self.flush_reactive();
        let active = std::mem::take(&mut self.animations);
        self.frame_requested = false;
        let had_active = !active.is_empty();
        for id in active {
            self.call_animation(id);
        }
        had_active
    }

    pub fn semantic_tree(&mut self) -> Vec<SemanticNode> {
        self.flush_reactive();
        let mut ids = Vec::new();
        self.collect_preorder(self.root_id(), &mut ids);
        let mut result = Vec::with_capacity(ids.len());
        for id in ids {
            let Ok(mut node) = self.arena.take(id) else {
                continue;
            };
            let mut ctx = SemanticsCtx {
                id,
                state: node.state.as_mut(),
                subscriptions: &mut node.subscriptions,
                reactivity: Rc::clone(&self.reactivity),
                now: self.now(),
                invalidation: Invalidation::NONE,
                animation_requested: false,
            };
            let mut semantics = node.widget.semantics(&mut ctx);
            semantics.focusable |= node.widget.focusable();
            let requested = ctx.invalidation;
            let animation_requested = ctx.animation_requested;
            node.dirty.remove(Invalidation::SEMANTICS);
            result.push(SemanticNode {
                id,
                parent: node.parent,
                bounds: node.rect,
                semantics,
            });
            self.arena.restore(id, node);
            self.mark_dirty(id, requested);
            if animation_requested {
                self.register_animation(id);
            }
        }
        self.dirty = self.arena.aggregate_dirty();
        result
    }

    fn mount(&mut self, element: Element, parent: Option<WidgetId>) -> UiResult<WidgetId> {
        let state = element.widget.create_state();
        let children = element.children;
        let id = self.arena.allocate(Node {
            parent,
            key: element.key,
            flex: element.flex,
            widget: element.widget,
            state,
            subscriptions: Vec::new(),
            children: Vec::new(),
            measured: Size::ZERO,
            rect: Rect::default(),
            dirty: Invalidation::ALL,
        })?;
        self.reactivity.insert(id);
        let mut mounted = Vec::with_capacity(children.len());
        for child in children {
            match self.mount(child, Some(id)) {
                Ok(child) => mounted.push(child),
                Err(error) => {
                    for child in mounted {
                        self.remove_subtree(child);
                    }
                    let mut node = self.arena.take(id).expect("newly allocated widget exists");
                    node.children.clear();
                    drop(node);
                    self.reactivity.remove(id);
                    self.arena.release(id);
                    return Err(error);
                }
            }
        }
        self.arena
            .get_mut(id)
            .expect("mounted widget exists")
            .children = mounted;
        Ok(id)
    }

    fn reconcile_node(
        &mut self,
        candidate: Option<WidgetId>,
        element: Element,
        parent: Option<WidgetId>,
    ) -> UiResult<WidgetId> {
        let reusable = candidate.is_some_and(|id| {
            self.arena.get(id).is_some_and(|node| {
                node.key == element.key
                    && node.widget.as_ref().as_any().type_id() == element.widget_type()
            })
        });
        if !reusable {
            let id = self.mount(element, parent)?;
            if let Some(candidate) = candidate {
                self.remove_subtree(candidate);
            }
            return Ok(id);
        }

        let id = candidate.expect("reusable candidate exists");
        let Element {
            widget,
            key,
            flex,
            children,
        } = element;
        let now = self.now();
        let reactivity = Rc::clone(&self.reactivity);
        let mut node = self.arena.take(id)?;
        let old_children = node.children.clone();
        let old_flex = node.flex;
        node.subscriptions.clear();
        let mut update = UpdateCtx {
            id,
            state: node.state.as_mut(),
            subscriptions: &mut node.subscriptions,
            reactivity,
            now,
            invalidation: Invalidation::NONE,
            animation_requested: false,
        };
        widget.update(node.widget.as_ref().as_any(), &mut update);
        let requested = update.invalidation;
        let animation_requested = update.animation_requested;
        node.parent = parent;
        node.key = key;
        node.flex = flex;
        node.widget = widget;
        self.arena.restore(id, node);
        self.mark_dirty(id, requested);
        if old_flex != flex {
            self.mark_dirty(id, Invalidation::LAYOUT);
        }
        if animation_requested {
            self.register_animation(id);
        }

        let keyed_old = old_children
            .iter()
            .enumerate()
            .filter_map(|(index, child)| {
                self.arena
                    .get(*child)
                    .and_then(|node| node.key.map(|key| (key, (index, *child))))
            })
            .collect::<HashMap<_, _>>();
        let mut used = vec![false; old_children.len()];
        let mut reconciled = Vec::with_capacity(children.len());
        for (index, child) in children.into_iter().enumerate() {
            let matched = match child.key {
                Some(key) => keyed_old
                    .get(&key)
                    .copied()
                    .filter(|(old_index, _)| !used[*old_index]),
                None => old_children.get(index).and_then(|old| {
                    (!used[index] && self.arena.get(*old).is_some_and(|node| node.key.is_none()))
                        .then_some((index, *old))
                }),
            };
            if let Some((old_index, _)) = matched {
                used[old_index] = true;
            }
            reconciled.push(self.reconcile_node(
                matched.map(|(_, widget)| widget),
                child,
                Some(id),
            )?);
        }
        for (old_index, old) in old_children.into_iter().enumerate() {
            if !used[old_index] {
                self.remove_subtree(old);
            }
        }
        let changed = self
            .arena
            .get(id)
            .is_none_or(|node| node.children != reconciled);
        self.arena
            .get_mut(id)
            .expect("reconciled widget exists")
            .children = reconciled;
        if changed {
            self.mark_dirty(id, Invalidation::LAYOUT | Invalidation::SEMANTICS);
        }
        Ok(id)
    }

    fn remove_subtree(&mut self, id: WidgetId) {
        let Ok(node) = self.arena.take(id) else {
            return;
        };
        for child in node.children.iter().copied() {
            self.remove_subtree(child);
        }
        if self.focus == Some(id) {
            self.focus = None;
        }
        if self.pointer_capture == Some(id) {
            self.pointer_capture = None;
        }
        self.animations.remove(&id);
        self.hover_path.retain(|hovered| *hovered != id);
        self.paint_order.retain(|entry| entry.id != id);
        self.reactivity.remove(id);
        drop(node);
        self.arena.release(id);
    }

    fn flush_reactive(&mut self) {
        for (id, invalidation) in self.reactivity.drain() {
            self.mark_dirty(id, invalidation);
        }
    }

    fn mark_dirty(&mut self, id: WidgetId, invalidation: Invalidation) {
        let invalidation = invalidation.expanded();
        if invalidation.is_empty() {
            return;
        }
        if let Some(node) = self.arena.get_mut(id) {
            node.dirty |= invalidation;
            self.dirty |= invalidation;
        }
    }

    fn clear_subtree_dirty(&mut self, id: WidgetId, invalidation: Invalidation) {
        let Some(node) = self.arena.get_mut(id) else {
            return;
        };
        node.dirty.remove(invalidation);
        let children = node.children.clone();
        for child in children {
            self.clear_subtree_dirty(child, invalidation);
        }
    }

    fn register_animation(&mut self, id: WidgetId) {
        if self.is_alive(id) {
            self.animations.insert(id);
            self.frame_requested = true;
        }
    }

    fn measure_node(&mut self, id: WidgetId, constraints: Constraints) -> UiResult<Size> {
        let now = self.now();
        let reactivity = Rc::clone(&self.reactivity);
        let mut node = self.arena.take(id)?;
        let children = node.children.clone();
        let mut ctx = MeasureCtx {
            root: self,
            id,
            state: node.state.as_mut(),
            subscriptions: &mut node.subscriptions,
            children,
            reactivity,
            now,
            invalidation: Invalidation::NONE,
            animation_requested: false,
        };
        let result = node.widget.measure(&mut ctx, constraints);
        let requested = ctx.invalidation;
        let animation_requested = ctx.animation_requested;
        let result = result.and_then(|size| {
            if !size.is_valid() {
                Err(UiError::InvalidSize)
            } else if !constraints.contains(size) {
                Err(UiError::SizeOutsideConstraints)
            } else {
                Ok(size)
            }
        });
        if let Ok(size) = result {
            node.measured = size;
            node.dirty.remove(Invalidation::LAYOUT);
        }
        self.arena.restore(id, node);
        self.mark_dirty(id, requested);
        if animation_requested {
            self.register_animation(id);
        }
        result
    }

    fn arrange_node(&mut self, id: WidgetId, rect: Rect) -> UiResult<()> {
        validate_rect(rect)?;
        let now = self.now();
        let reactivity = Rc::clone(&self.reactivity);
        let mut node = self.arena.take(id)?;
        node.rect = rect;
        let children = node.children.clone();
        let mut ctx = ArrangeCtx {
            root: self,
            id,
            state: node.state.as_mut(),
            subscriptions: &mut node.subscriptions,
            children,
            reactivity,
            now,
            invalidation: Invalidation::NONE,
            animation_requested: false,
        };
        let result = node.widget.arrange(&mut ctx, rect);
        let requested = ctx.invalidation;
        let animation_requested = ctx.animation_requested;
        if result.is_ok() {
            node.dirty.remove(Invalidation::LAYOUT);
        }
        self.arena.restore(id, node);
        self.mark_dirty(id, requested);
        if animation_requested {
            self.register_animation(id);
        }
        result
    }

    fn paint_node(
        &mut self,
        id: WidgetId,
        builder: &mut DisplayListBuilder,
        inherited_clip: EffectiveClip,
    ) -> UiResult<()> {
        if matches!(inherited_clip, EffectiveClip::Empty) {
            self.clear_subtree_dirty(id, Invalidation::PAINT);
            return Ok(());
        }
        let now = self.now();
        let reactivity = Rc::clone(&self.reactivity);
        let mut node = self.arena.take(id)?;
        let rect = node.rect;
        let clips = node.widget.clips_children();
        let child_clip = if clips {
            inherited_clip.intersect(rect)
        } else {
            inherited_clip
        };
        self.paint_order.push(PaintEntry {
            id,
            rect,
            clip: inherited_clip,
            accepts_pointer: node.widget.accepts_pointer(),
        });
        let children = node.children.clone();
        let invoke = |root: &mut Self, builder: &mut DisplayListBuilder, node: &mut Node| {
            let mut ctx = PaintCtx {
                root,
                id,
                state: node.state.as_mut(),
                subscriptions: &mut node.subscriptions,
                children: children.clone(),
                painted_children: vec![false; children.len()],
                reactivity: Rc::clone(&reactivity),
                now,
                invalidation: Invalidation::NONE,
                animation_requested: false,
                builder,
                rect,
                clips_children: clips,
                child_clip,
            };
            let result = node.widget.paint(&mut ctx);
            if result.is_ok() {
                let hidden = ctx
                    .children
                    .iter()
                    .zip(&ctx.painted_children)
                    .filter_map(|(child, painted)| (!painted).then_some(*child))
                    .collect::<Vec<_>>();
                for child in hidden {
                    ctx.root.clear_subtree_dirty(child, Invalidation::PAINT);
                }
            }
            (result, ctx.invalidation, ctx.animation_requested)
        };
        let (result, requested, animation_requested) = invoke(self, builder, &mut node);
        if result.is_ok() {
            node.dirty.remove(Invalidation::PAINT);
        }
        self.arena.restore(id, node);
        self.mark_dirty(id, requested);
        if animation_requested {
            self.register_animation(id);
        }
        result
    }

    fn call_animation(&mut self, id: WidgetId) {
        if !self.is_alive(id) {
            return;
        }
        let now = self.now();
        let reactivity = Rc::clone(&self.reactivity);
        let Ok(mut node) = self.arena.take(id) else {
            return;
        };
        let mut ctx = AnimationCtx {
            id,
            rect: node.rect,
            state: node.state.as_mut(),
            subscriptions: &mut node.subscriptions,
            reactivity,
            now,
            invalidation: Invalidation::PAINT,
            animation_requested: false,
        };
        node.widget.animation(&mut ctx);
        let requested = ctx.invalidation;
        let animation_requested = ctx.animation_requested;
        self.arena.restore(id, node);
        self.mark_dirty(id, requested);
        if animation_requested {
            self.register_animation(id);
        }
    }

    fn call_event(&mut self, id: WidgetId, event: &UiEvent) -> UiResult<EventRequests> {
        let now = self.now();
        let is_focused = self.focus == Some(id);
        let reactivity = Rc::clone(&self.reactivity);
        let mut node = self.arena.take(id)?;
        let mut ctx = EventCtx {
            id,
            rect: node.rect,
            state: node.state.as_mut(),
            subscriptions: &mut node.subscriptions,
            reactivity,
            now,
            invalidation: Invalidation::NONE,
            animation_requested: false,
            requests: EventRequests::default(),
            is_focused,
        };
        let result = node.widget.event(&mut ctx, event);
        let requested = ctx.invalidation;
        let animation_requested = ctx.animation_requested;
        let requests = ctx.requests;
        self.arena.restore(id, node);
        self.mark_dirty(id, requested);
        if animation_requested {
            self.register_animation(id);
        }
        result.map(|()| requests)
    }

    fn call_scroll_boundary(
        &mut self,
        id: WidgetId,
        delta_x: f32,
        delta_y: f32,
    ) -> UiResult<EventRequests> {
        let now = self.now();
        let is_focused = self.focus == Some(id);
        let reactivity = Rc::clone(&self.reactivity);
        let mut node = self.arena.take(id)?;
        let mut ctx = EventCtx {
            id,
            rect: node.rect,
            state: node.state.as_mut(),
            subscriptions: &mut node.subscriptions,
            reactivity,
            now,
            invalidation: Invalidation::NONE,
            animation_requested: false,
            requests: EventRequests::default(),
            is_focused,
        };
        let result = node.widget.scroll_boundary(&mut ctx, delta_x, delta_y);
        let requested = ctx.invalidation;
        let animation_requested = ctx.animation_requested;
        let requests = ctx.requests;
        self.arena.restore(id, node);
        self.mark_dirty(id, requested);
        if animation_requested {
            self.register_animation(id);
        }
        result.map(|()| requests)
    }

    fn apply_event_requests(&mut self, requests: &EventRequests) -> UiResult<()> {
        if let Some(focus) = requests.focus {
            self.change_focus(Some(focus))?;
        }
        match requests.capture {
            Some(CaptureRequest::Set(id)) if self.is_alive(id) => {
                self.pointer_capture = Some(id);
            }
            Some(CaptureRequest::Release) => self.pointer_capture = None,
            _ => {}
        }
        Ok(())
    }

    fn update_hover(&mut self, target: Option<WidgetId>) -> UiResult<()> {
        let next = target
            .map(|target| self.path_to_root(target))
            .unwrap_or_default();
        if next == self.hover_path {
            return Ok(());
        }
        let previous = std::mem::replace(&mut self.hover_path, next.clone());
        let previous_set = previous.iter().copied().collect::<HashSet<_>>();
        let next_set = next.iter().copied().collect::<HashSet<_>>();
        for id in previous.iter().copied().filter(|id| !next_set.contains(id)) {
            if self.is_alive(id) {
                let requests = self.call_event(id, &UiEvent::HoverChanged(false))?;
                self.apply_non_focus_requests(&requests);
            }
        }
        for id in next
            .iter()
            .rev()
            .copied()
            .filter(|id| !previous_set.contains(id))
        {
            if self.is_alive(id) {
                let requests = self.call_event(id, &UiEvent::HoverChanged(true))?;
                self.apply_non_focus_requests(&requests);
            }
        }
        Ok(())
    }

    fn change_focus(&mut self, focus: Option<WidgetId>) -> UiResult<()> {
        if let Some(id) = focus {
            let node = self.arena.get(id).ok_or(UiError::MissingWidget(id))?;
            if !node.widget.focusable() {
                return Ok(());
            }
        }
        let previous = self.focused();
        if previous == focus {
            return Ok(());
        }
        self.focus = focus;
        if let Some(previous) = previous {
            let requests = self.call_event(previous, &UiEvent::FocusChanged(false))?;
            self.apply_non_focus_requests(&requests);
        }
        if let Some(focus) = focus {
            let requests = self.call_event(focus, &UiEvent::FocusChanged(true))?;
            self.apply_non_focus_requests(&requests);
        }
        Ok(())
    }

    fn apply_non_focus_requests(&mut self, requests: &EventRequests) {
        match requests.capture {
            Some(CaptureRequest::Set(id)) if self.is_alive(id) => {
                self.pointer_capture = Some(id);
            }
            Some(CaptureRequest::Release) => self.pointer_capture = None,
            _ => {}
        }
    }

    fn focus_next(&mut self, backwards: bool) -> UiResult<bool> {
        let scope = self.focused().and_then(|focused| {
            self.path_to_root(focused).into_iter().find(|id| {
                self.arena
                    .get(*id)
                    .is_some_and(|node| node.widget.focus_scope())
            })
        });
        let mut order = Vec::new();
        self.collect_focusable(scope.unwrap_or_else(|| self.root_id()), &mut order);
        if order.is_empty() {
            return Ok(false);
        }
        let current = self
            .focused()
            .and_then(|id| order.iter().position(|item| *item == id));
        let index = match (current, backwards) {
            (Some(0), true) | (None, true) => order.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % order.len(),
            (None, false) => 0,
        };
        self.change_focus(Some(order[index]))?;
        Ok(true)
    }

    fn path_to_root(&self, mut id: WidgetId) -> Vec<WidgetId> {
        let mut path = Vec::new();
        while let Some(node) = self.arena.get(id) {
            path.push(id);
            let Some(parent) = node.parent else {
                break;
            };
            id = parent;
        }
        path
    }

    fn collect_preorder(&self, id: WidgetId, output: &mut Vec<WidgetId>) {
        let Some(node) = self.arena.get(id) else {
            return;
        };
        output.push(id);
        for child in node.children.iter().copied() {
            self.collect_preorder(child, output);
        }
    }

    fn collect_focusable(&self, id: WidgetId, output: &mut Vec<WidgetId>) {
        let Some(node) = self.arena.get(id) else {
            return;
        };
        if node.widget.focusable() {
            output.push(id);
        }
        for child in node.children.iter().copied() {
            self.collect_focusable(child, output);
        }
    }
}

fn validate_rect(rect: Rect) -> UiResult<()> {
    if rect.is_finite() && rect.width >= 0.0 && rect.height >= 0.0 {
        Ok(())
    } else {
        Err(UiError::InvalidRect)
    }
}

fn validate_event(event: &UiEvent) -> UiResult<()> {
    if matches!(event, UiEvent::FocusChanged(_) | UiEvent::HoverChanged(_)) {
        return Err(UiError::InvalidEvent);
    }
    if event
        .pointer_position()
        .is_some_and(|point| !point.x.is_finite() || !point.y.is_finite())
    {
        return Err(UiError::InvalidEvent);
    }
    match event {
        UiEvent::PointerScroll {
            delta_x, delta_y, ..
        }
        | UiEvent::ScrollGesture {
            delta_x, delta_y, ..
        } if !delta_x.is_finite() || !delta_y.is_finite() => {
            return Err(UiError::InvalidEvent);
        }
        _ => {}
    }
    if let UiEvent::ImePreedit {
        text,
        selection: Some((start, end)),
    } = event
        && (start > end
            || *end > text.len()
            || !text.is_char_boundary(*start)
            || !text.is_char_boundary(*end))
    {
        return Err(UiError::InvalidEvent);
    }
    Ok(())
}

fn with_scroll_delta(event: &UiEvent, delta_x: f32, delta_y: f32) -> UiEvent {
    match event {
        UiEvent::PointerScroll {
            position,
            modifiers,
            ..
        } => UiEvent::PointerScroll {
            position: *position,
            delta_x,
            delta_y,
            modifiers: *modifiers,
        },
        UiEvent::ScrollGesture {
            position,
            phase,
            modifiers,
            ..
        } => UiEvent::ScrollGesture {
            position: *position,
            delta_x,
            delta_y,
            phase: *phase,
            modifiers: *modifiers,
        },
        _ => event.clone(),
    }
}

fn is_scroll_lifecycle(event: &UiEvent) -> bool {
    matches!(
        event,
        UiEvent::ScrollGesture {
            phase: ScrollPhase::Begin | ScrollPhase::End | ScrollPhase::Cancel,
            ..
        }
    )
}

macro_rules! common_context {
    () => {
        pub fn widget_id(&self) -> WidgetId {
            self.id
        }

        pub fn now(&self) -> Duration {
            self.now
        }

        pub fn state_mut<T: 'static>(&mut self) -> UiResult<&mut T> {
            self.state
                .downcast_mut()
                .ok_or(UiError::StateTypeMismatch(type_name::<T>()))
        }

        pub fn watch<T: Clone + 'static>(
            &mut self,
            reactive: &Reactive<T>,
            invalidation: Invalidation,
        ) -> T {
            reactive.watch(&self.reactivity, self.id, invalidation, self.subscriptions)
        }

        pub fn invalidate(&mut self, invalidation: Invalidation) {
            self.invalidation |= invalidation;
        }

        pub fn request_animation_frame(&mut self) {
            self.animation_requested = true;
        }
    };
}

pub struct UpdateCtx<'a> {
    id: WidgetId,
    state: &'a mut dyn Any,
    subscriptions: &'a mut Vec<SubscriptionToken>,
    reactivity: Rc<RootReactivity>,
    now: Duration,
    invalidation: Invalidation,
    animation_requested: bool,
}

impl UpdateCtx<'_> {
    common_context!();
}

pub struct MeasureCtx<'a> {
    root: &'a mut UiRoot,
    id: WidgetId,
    state: &'a mut dyn Any,
    subscriptions: &'a mut Vec<SubscriptionToken>,
    children: Vec<WidgetId>,
    reactivity: Rc<RootReactivity>,
    now: Duration,
    invalidation: Invalidation,
    animation_requested: bool,
}

impl MeasureCtx<'_> {
    common_context!();

    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    pub fn layout_text(
        &mut self,
        text: &str,
        style: &TextStyle,
        width: Option<f32>,
    ) -> UiResult<Arc<TextLayout>> {
        self.root
            .text
            .as_mut()
            .ok_or(UiError::TextResourcesRequired)?
            .layout(text, style, width)
            .map_err(Into::into)
    }

    pub fn measure_child(&mut self, index: usize, constraints: Constraints) -> UiResult<Size> {
        let child = *self
            .children
            .get(index)
            .ok_or(UiError::UnexpectedChildCount {
                expected_maximum: self.children.len(),
                actual: index + 1,
            })?;
        self.root.measure_node(child, constraints)
    }

    pub fn child_flex(&self, index: usize) -> UiResult<f32> {
        let child = *self
            .children
            .get(index)
            .ok_or(UiError::UnexpectedChildCount {
                expected_maximum: self.children.len(),
                actual: index + 1,
            })?;
        self.root
            .arena
            .get(child)
            .map(|node| node.flex)
            .ok_or(UiError::MissingWidget(child))
    }
}

pub struct ArrangeCtx<'a> {
    root: &'a mut UiRoot,
    id: WidgetId,
    state: &'a mut dyn Any,
    subscriptions: &'a mut Vec<SubscriptionToken>,
    children: Vec<WidgetId>,
    reactivity: Rc<RootReactivity>,
    now: Duration,
    invalidation: Invalidation,
    animation_requested: bool,
}

impl ArrangeCtx<'_> {
    common_context!();

    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    pub fn child_size(&self, index: usize) -> UiResult<Size> {
        let child = *self
            .children
            .get(index)
            .ok_or(UiError::UnexpectedChildCount {
                expected_maximum: self.children.len(),
                actual: index + 1,
            })?;
        self.root
            .arena
            .get(child)
            .map(|node| node.measured)
            .ok_or(UiError::MissingWidget(child))
    }

    pub fn arrange_child(&mut self, index: usize, rect: Rect) -> UiResult<()> {
        let child = *self
            .children
            .get(index)
            .ok_or(UiError::UnexpectedChildCount {
                expected_maximum: self.children.len(),
                actual: index + 1,
            })?;
        self.root.arrange_node(child, rect)
    }

    pub fn arrange_children(&mut self, rect: Rect) -> UiResult<()> {
        for index in 0..self.children.len() {
            self.arrange_child(index, rect)?;
        }
        Ok(())
    }
}

pub struct PaintCtx<'a> {
    root: &'a mut UiRoot,
    id: WidgetId,
    state: &'a mut dyn Any,
    subscriptions: &'a mut Vec<SubscriptionToken>,
    children: Vec<WidgetId>,
    painted_children: Vec<bool>,
    reactivity: Rc<RootReactivity>,
    now: Duration,
    invalidation: Invalidation,
    animation_requested: bool,
    builder: &'a mut DisplayListBuilder,
    rect: Rect,
    clips_children: bool,
    child_clip: EffectiveClip,
}

impl PaintCtx<'_> {
    common_context!();

    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    pub fn builder(&mut self) -> &mut DisplayListBuilder {
        self.builder
    }

    pub fn draw_text(
        &mut self,
        layout: &TextLayout,
        origin: Point,
        color: Color,
        clip: Option<Rect>,
    ) -> UiResult<TextDrawStats> {
        self.root
            .text
            .as_mut()
            .ok_or(UiError::TextResourcesRequired)?
            .draw(self.builder, layout, origin, color, clip)
            .map_err(Into::into)
    }

    /// Arranged child geometry for container-owned decoration such as a list
    /// selection mass or shared group separators.
    pub fn child_rect(&self, index: usize) -> UiResult<Rect> {
        let child = *self
            .children
            .get(index)
            .ok_or(UiError::UnexpectedChildCount {
                expected_maximum: self.children.len(),
                actual: index + 1,
            })?;
        self.root
            .arena
            .get(child)
            .map(|node| node.rect)
            .ok_or(UiError::MissingWidget(child))
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Register a pointer region painted above this widget's children. This is
    /// used by overlay controls such as scrollbar tracks without making the
    /// container intercept its entire content area.
    pub fn register_pointer_overlay(&mut self, rect: Rect) -> UiResult<()> {
        validate_rect(rect)?;
        if let Some(rect) = rect.intersect(self.rect) {
            self.root.paint_order.push(PaintEntry {
                id: self.id,
                rect,
                clip: self.child_clip,
                accepts_pointer: true,
            });
        }
        Ok(())
    }

    pub fn paint_child(&mut self, index: usize) -> UiResult<()> {
        self.paint_child_translated(index, 0.0, 0.0)
    }

    /// Paint one child with a visual-only translation. Layout and hit geometry
    /// stay rigid; Scroll uses this for bounded elastic feedback.
    pub fn paint_child_translated(
        &mut self,
        index: usize,
        delta_x: f32,
        delta_y: f32,
    ) -> UiResult<()> {
        let child = *self
            .children
            .get(index)
            .ok_or(UiError::UnexpectedChildCount {
                expected_maximum: self.children.len(),
                actual: index + 1,
            })?;
        let transform = Transform::translation(delta_x, delta_y);
        let result = if self.clips_children {
            self.builder.with_clip(self.rect, |builder| {
                builder.with_transform(transform, |builder| {
                    Ok(self.root.paint_node(child, builder, self.child_clip))
                })
            })?
        } else {
            self.builder.with_transform(transform, |builder| {
                Ok(self.root.paint_node(child, builder, self.child_clip))
            })?
        };
        if result.is_ok() {
            self.painted_children[index] = true;
        }
        result
    }

    pub fn paint_children(&mut self) -> UiResult<()> {
        for index in 0..self.children.len() {
            self.paint_child(index)?;
        }
        Ok(())
    }
}

pub struct AnimationCtx<'a> {
    id: WidgetId,
    rect: Rect,
    state: &'a mut dyn Any,
    subscriptions: &'a mut Vec<SubscriptionToken>,
    reactivity: Rc<RootReactivity>,
    now: Duration,
    invalidation: Invalidation,
    animation_requested: bool,
}

impl AnimationCtx<'_> {
    common_context!();

    pub fn rect(&self) -> Rect {
        self.rect
    }
}

pub struct EventCtx<'a> {
    id: WidgetId,
    rect: Rect,
    state: &'a mut dyn Any,
    subscriptions: &'a mut Vec<SubscriptionToken>,
    reactivity: Rc<RootReactivity>,
    now: Duration,
    invalidation: Invalidation,
    animation_requested: bool,
    requests: EventRequests,
    is_focused: bool,
}

pub struct SemanticsCtx<'a> {
    id: WidgetId,
    state: &'a mut dyn Any,
    subscriptions: &'a mut Vec<SubscriptionToken>,
    reactivity: Rc<RootReactivity>,
    now: Duration,
    invalidation: Invalidation,
    animation_requested: bool,
}

impl SemanticsCtx<'_> {
    common_context!();
}

impl EventCtx<'_> {
    common_context!();

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn local_position(&self, event: &UiEvent) -> Option<Point> {
        event
            .pointer_position()
            .map(|point| Point::new(point.x - self.rect.x, point.y - self.rect.y))
    }

    pub fn set_handled(&mut self) {
        self.requests.handled = true;
        self.requests.scroll_remainder = None;
    }

    /// Continue a scroll transaction with only the unconsumed logical delta.
    /// `consumed` reports whether this widget moved before reaching its bound.
    pub fn handoff_scroll(&mut self, delta_x: f32, delta_y: f32, consumed: bool) -> UiResult<()> {
        if !delta_x.is_finite() || !delta_y.is_finite() {
            return Err(UiError::InvalidEvent);
        }
        self.requests.handled |= consumed;
        self.requests.scroll_remainder = Some((delta_x, delta_y));
        Ok(())
    }

    pub fn request_focus(&mut self) {
        self.requests.focus = Some(self.id);
    }

    pub fn capture_pointer(&mut self) {
        self.requests.capture = Some(CaptureRequest::Set(self.id));
    }

    pub fn release_pointer(&mut self) {
        self.requests.capture = Some(CaptureRequest::Release);
    }

    pub fn focused(&self) -> bool {
        self.is_focused
    }
}
