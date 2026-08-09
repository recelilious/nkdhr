use std::{any::Any, cmp::Ordering, fmt, sync::Arc, time::Duration};

use nkdhr_render::{CornerRadii, Point, Rect};

use crate::theme::with_alpha;
use crate::{
    AnimationCtx, ArrangeCtx, Constraints, EventCtx, Invalidation, Key, MeasureCtx, Modifiers,
    MotionFamily, PaintCtx, PointerButton, Reactive, ScalarMotion, ScrollPhase, SemanticRole,
    Semantics, SemanticsCtx, Size, Theme, ThemeReadSet, UiError, UiEvent, UpdateCtx, Widget,
};

const MINIMUM_THUMB: f32 = 24.0;
const MINIMUM_POINTER_TARGET: f32 = 16.0;
const IDLE_DELAY: Duration = Duration::from_millis(700);
const IDLE_HINT: f32 = 0.10;
const INERTIA_DECAY_PER_SECOND: f32 = 7.5;
const MINIMUM_INERTIA_VELOCITY: f32 = 30.0;
const MAXIMUM_INERTIA_VELOCITY: f32 = 5_000.0;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScrollOffset {
    pub x: f32,
    pub y: f32,
}

impl ScrollOffset {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrollAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ScrollbarPolicy {
    #[default]
    Auto,
    Persistent,
}

/// One revisioned visual anchor in content and viewport coordinates. A
/// revision is applied at most once, so ordinary user scrolling remains free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollAnchor {
    pub revision: u64,
    pub content_position: Point,
    pub viewport_position: Point,
}

impl ScrollAnchor {
    pub fn new(
        revision: u64,
        content_position: Point,
        viewport_position: Point,
    ) -> Result<Self, ScrollError> {
        if !point_is_finite(content_position) || !point_is_finite(viewport_position) {
            return Err(ScrollError::InvalidAnchor);
        }
        Ok(Self {
            revision,
            content_position,
            viewport_position,
        })
    }
}

/// Revisioned content rectangle that should become visible with the minimum
/// possible movement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollReveal {
    pub revision: u64,
    pub content_rect: Rect,
}

impl ScrollReveal {
    pub fn new(revision: u64, content_rect: Rect) -> Result<Self, ScrollError> {
        if !content_rect.is_finite() || content_rect.width < 0.0 || content_rect.height < 0.0 {
            return Err(ScrollError::InvalidReveal);
        }
        Ok(Self {
            revision,
            content_rect,
        })
    }
}

pub struct Scroll {
    label: String,
    content_size: Size,
    offset: Reactive<ScrollOffset>,
    theme: Arc<Theme>,
    horizontal: bool,
    vertical: bool,
    enabled: bool,
    scrollbar_policy: ScrollbarPolicy,
    high_contrast_scrollbars: bool,
    elastic: bool,
    follow_tail: bool,
    anchor: Option<ScrollAnchor>,
    reveal: Option<ScrollReveal>,
    snap_x: Vec<f32>,
    snap_y: Vec<f32>,
}

impl Scroll {
    pub fn new(
        label: impl Into<String>,
        content_size: Size,
        offset: Reactive<ScrollOffset>,
        theme: Arc<Theme>,
    ) -> Result<Self, ScrollError> {
        if !content_size.is_valid() {
            return Err(ScrollError::InvalidContentSize);
        }
        Ok(Self {
            label: label.into(),
            content_size,
            offset,
            theme,
            horizontal: true,
            vertical: true,
            enabled: true,
            scrollbar_policy: ScrollbarPolicy::Auto,
            high_contrast_scrollbars: false,
            elastic: true,
            follow_tail: false,
            anchor: None,
            reveal: None,
            snap_x: Vec::new(),
            snap_y: Vec::new(),
        })
    }

    pub fn horizontal(mut self, horizontal: bool) -> Self {
        self.horizontal = horizontal;
        self
    }

    pub fn vertical(mut self, vertical: bool) -> Self {
        self.vertical = vertical;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn scrollbar_policy(mut self, policy: ScrollbarPolicy) -> Self {
        self.scrollbar_policy = policy;
        self
    }

    pub fn high_contrast_scrollbars(mut self, high_contrast: bool) -> Self {
        self.high_contrast_scrollbars = high_contrast;
        self
    }

    pub fn elastic(mut self, elastic: bool) -> Self {
        self.elastic = elastic;
        self
    }

    pub fn follow_tail(mut self, follow_tail: bool) -> Self {
        self.follow_tail = follow_tail;
        self
    }

    pub fn anchor(mut self, anchor: Option<ScrollAnchor>) -> Self {
        self.anchor = anchor;
        self
    }

    pub fn reveal(mut self, reveal: Option<ScrollReveal>) -> Self {
        self.reveal = reveal;
        self
    }

    pub fn snap_points(
        mut self,
        axis: ScrollAxis,
        points: impl IntoIterator<Item = f32>,
    ) -> Result<Self, ScrollError> {
        let mut points = points.into_iter().collect::<Vec<_>>();
        if points
            .iter()
            .any(|point| !point.is_finite() || *point < 0.0)
        {
            return Err(ScrollError::InvalidSnapPoint);
        }
        points.sort_by(|first, second| first.partial_cmp(second).unwrap_or(Ordering::Equal));
        points.dedup_by(|first, second| (*first - *second).abs() <= f32::EPSILON);
        match axis {
            ScrollAxis::Horizontal => self.snap_x = points,
            ScrollAxis::Vertical => self.snap_y = points,
        }
        Ok(self)
    }

    fn clamp_offset(&self, offset: ScrollOffset, viewport: Size) -> ScrollOffset {
        let offset = finite_offset(offset);
        let maximum = self.maximum_offset(viewport);
        ScrollOffset {
            x: if self.horizontal {
                offset.x.clamp(0.0, maximum.x)
            } else {
                0.0
            },
            y: if self.vertical {
                offset.y.clamp(0.0, maximum.y)
            } else {
                0.0
            },
        }
    }

    fn maximum_offset(&self, viewport: Size) -> ScrollOffset {
        ScrollOffset::new(
            (self.content_size.width - viewport.width).max(0.0),
            (self.content_size.height - viewport.height).max(0.0),
        )
    }

    fn consume_delta(&self, viewport: Size, delta_x: f32, delta_y: f32) -> ScrollConsumption {
        if !self.enabled {
            return ScrollConsumption {
                remainder: ScrollOffset::new(delta_x, delta_y),
                ..ScrollConsumption::default()
            };
        }
        let current = self.clamp_offset(self.offset.get(), viewport);
        let next = self.clamp_offset(
            ScrollOffset::new(current.x + delta_x, current.y + delta_y),
            viewport,
        );
        let consumed = ScrollOffset::new(next.x - current.x, next.y - current.y);
        let changed = self.offset.set_if_changed(next);
        ScrollConsumption {
            next,
            consumed,
            remainder: ScrollOffset::new(delta_x - consumed.x, delta_y - consumed.y),
            changed,
        }
    }

    fn snap_target(&self, offset: ScrollOffset, viewport: Size) -> ScrollOffset {
        let maximum = self.maximum_offset(viewport);
        ScrollOffset::new(
            nearest_snap(offset.x, maximum.x, &self.snap_x),
            nearest_snap(offset.y, maximum.y, &self.snap_y),
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct ThumbDrag {
    axis: ScrollAxis,
    grab_offset: f32,
}

#[derive(Debug, Clone, Copy)]
struct ScrollState {
    visibility: ScalarMotion,
    elastic_x: ScalarMotion,
    elastic_y: ScalarMotion,
    stretch_x: ScalarMotion,
    stretch_y: ScalarMotion,
    snap_x: ScalarMotion,
    snap_y: ScalarMotion,
    focused: bool,
    edge_hover: bool,
    activity_seen: bool,
    last_activity: Duration,
    dragging: Option<ThumbDrag>,
    gesture_active: bool,
    last_gesture: Duration,
    velocity: ScrollOffset,
    inertia_active: bool,
    last_animation: Duration,
    snap_active: bool,
    observed_content: Size,
    observed_viewport: Size,
    initialized: bool,
    last_anchor_revision: Option<u64>,
    last_reveal_revision: Option<u64>,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            visibility: ScalarMotion::settled(IDLE_HINT),
            elastic_x: ScalarMotion::settled(0.0),
            elastic_y: ScalarMotion::settled(0.0),
            stretch_x: ScalarMotion::settled(0.0),
            stretch_y: ScalarMotion::settled(0.0),
            snap_x: ScalarMotion::settled(0.0),
            snap_y: ScalarMotion::settled(0.0),
            focused: false,
            edge_hover: false,
            activity_seen: false,
            last_activity: Duration::ZERO,
            dragging: None,
            gesture_active: false,
            last_gesture: Duration::ZERO,
            velocity: ScrollOffset::ZERO,
            inertia_active: false,
            last_animation: Duration::ZERO,
            snap_active: false,
            observed_content: Size::ZERO,
            observed_viewport: Size::ZERO,
            initialized: false,
            last_anchor_revision: None,
            last_reveal_revision: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ScrollConsumption {
    next: ScrollOffset,
    consumed: ScrollOffset,
    remainder: ScrollOffset,
    changed: bool,
}

#[derive(Debug, Clone, Copy)]
struct ScrollbarGeometry {
    thumb: Rect,
    hit: Rect,
    track_start: f32,
    travel: f32,
    maximum: f32,
}

impl Widget for Scroll {
    fn theme_reads(&self) -> ThemeReadSet {
        ThemeReadSet::from_paths([
            "density",
            "palette.text_secondary",
            "palette.edge",
            "motion.mode",
            "motion.speed_multiplier",
            "motion.standard",
            "motion.settle",
            "motion.exit",
            "motion.durations.scrollbar_show",
            "motion.durations.scrollbar_hide",
            "motion.durations.overscroll",
        ])
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn create_state(&self) -> Box<dyn Any> {
        Box::<ScrollState>::default()
    }

    fn update(&self, previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        if previous.content_size != self.content_size
            || previous.horizontal != self.horizontal
            || previous.vertical != self.vertical
            || previous.anchor != self.anchor
            || previous.reveal != self.reveal
        {
            ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
        } else {
            ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
        }
        let state = ctx
            .state_mut::<ScrollState>()
            .expect("Scroll owns ScrollState");
        if !self.enabled {
            cancel_kinetic(state);
            state.dragging = None;
            state.gesture_active = false;
        }
        if !self.theme.motion.spatial_motion_enabled() {
            cancel_kinetic(state);
            state.elastic_x.settle(0.0);
            state.elastic_y.settle(0.0);
            state.stretch_x.settle(0.0);
            state.stretch_y.settle(0.0);
        }
        if !self.elastic {
            state.elastic_x.settle(0.0);
            state.elastic_y.settle(0.0);
        }
        if !self.horizontal {
            state.elastic_x.settle(0.0);
            state.stretch_x.settle(0.0);
            state.velocity.x = 0.0;
        }
        if !self.vertical {
            state.elastic_y.settle(0.0);
            state.stretch_y.settle(0.0);
            state.velocity.y = 0.0;
        }
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        if ctx.child_count() == 1 {
            ctx.measure_child(0, Constraints::tight(self.content_size)?)?;
        }
        Ok(constraints.constrain(Size::new(
            self.content_size.width.min(constraints.max().width),
            self.content_size.height.min(constraints.max().height),
        )))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        let viewport = Size::new(rect.width, rect.height);
        let mut offset = self.clamp_offset(self.offset.get(), viewport);
        {
            let state = ctx.state_mut::<ScrollState>()?;
            if state.initialized && state.observed_content != self.content_size && self.follow_tail
            {
                let old_max = ScrollOffset::new(
                    (state.observed_content.width - state.observed_viewport.width).max(0.0),
                    (state.observed_content.height - state.observed_viewport.height).max(0.0),
                );
                let threshold = self.theme.density_metrics().row_height;
                let new_max = self.maximum_offset(viewport);
                if self.horizontal && old_max.x - offset.x <= threshold {
                    offset.x = new_max.x;
                }
                if self.vertical && old_max.y - offset.y <= threshold {
                    offset.y = new_max.y;
                }
            }
            if let Some(anchor) = self.anchor
                && state.last_anchor_revision != Some(anchor.revision)
            {
                offset.x = anchor.content_position.x - anchor.viewport_position.x;
                offset.y = anchor.content_position.y - anchor.viewport_position.y;
                state.last_anchor_revision = Some(anchor.revision);
            }
            if let Some(reveal) = self.reveal
                && state.last_reveal_revision != Some(reveal.revision)
            {
                offset = reveal_minimally(offset, viewport, reveal.content_rect);
                state.last_reveal_revision = Some(reveal.revision);
            }
            state.observed_content = self.content_size;
            state.observed_viewport = viewport;
            state.initialized = true;
        }
        offset = self.clamp_offset(offset, viewport);
        self.offset.set_if_changed(offset);
        if ctx.child_count() == 1 {
            ctx.arrange_child(
                0,
                Rect::new(
                    rect.x - offset.x,
                    rect.y - offset.y,
                    self.content_size.width,
                    self.content_size.height,
                ),
            )?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        let viewport = Size::new(rect.width, rect.height);
        let offset = self.clamp_offset(
            ctx.watch(&self.offset, Invalidation::LAYOUT | Invalidation::SEMANTICS),
            viewport,
        );
        let now = ctx.now();
        let (visibility, elastic_x, elastic_y, stretch_x, stretch_y, active, waiting_for_idle) = {
            let state = ctx.state_mut::<ScrollState>()?;
            let held_visible = self.scrollbar_policy == ScrollbarPolicy::Persistent
                || state.focused
                || state.edge_hover
                || state.dragging.is_some()
                || state.gesture_active;
            if held_visible {
                state.visibility.retarget(
                    now,
                    1.0,
                    self.theme.motion.spec(MotionFamily::ScrollbarShow),
                );
            } else if state.activity_seen && now.saturating_sub(state.last_activity) >= IDLE_DELAY {
                state.visibility.retarget(
                    now,
                    IDLE_HINT,
                    self.theme.motion.spec(MotionFamily::ScrollbarHide),
                );
            }
            let waiting_for_idle = self.scrollbar_policy == ScrollbarPolicy::Auto
                && state.activity_seen
                && !held_visible
                && now.saturating_sub(state.last_activity) < IDLE_DELAY;
            (
                state.visibility.value(now).clamp(0.0, 1.0),
                state.elastic_x.value(now),
                state.elastic_y.value(now),
                state.stretch_x.value(now).clamp(0.0, 1.0),
                state.stretch_y.value(now).clamp(0.0, 1.0),
                state.visibility.is_active(now)
                    || state.elastic_x.is_active(now)
                    || state.elastic_y.is_active(now)
                    || state.stretch_x.is_active(now)
                    || state.stretch_y.is_active(now),
                waiting_for_idle,
            )
        };
        if active || waiting_for_idle {
            ctx.request_animation_frame();
        }

        if ctx.child_count() == 1 {
            ctx.paint_child_translated(0, elastic_x, elastic_y)?;
        }

        let thickness = self.theme.density_metrics().scrollbar;
        let vertical = scrollbar_geometry(
            ScrollAxis::Vertical,
            rect,
            self.content_size,
            offset,
            thickness,
            stretch_y,
        );
        let horizontal = scrollbar_geometry(
            ScrollAxis::Horizontal,
            rect,
            self.content_size,
            offset,
            thickness,
            stretch_x,
        );
        if self.vertical
            && let Some(geometry) = vertical
        {
            ctx.register_pointer_overlay(geometry.hit)?;
            paint_thumb(ctx, geometry.thumb, visibility, self)?;
        }
        if self.horizontal
            && let Some(geometry) = horizontal
        {
            ctx.register_pointer_overlay(geometry.hit)?;
            paint_thumb(ctx, geometry.thumb, visibility, self)?;
        }
        Ok(())
    }

    fn animation(&self, ctx: &mut AnimationCtx<'_>) {
        let viewport = Size::new(ctx.rect().width, ctx.rect().height);
        let now = ctx.now();
        let current = self.clamp_offset(self.offset.get(), viewport);
        let mut next = current;
        let mut needs_layout = false;
        let mut keep_running = false;
        {
            let state = ctx
                .state_mut::<ScrollState>()
                .expect("Scroll owns ScrollState");
            if state.inertia_active {
                let elapsed = now.saturating_sub(state.last_animation);
                let seconds = elapsed.as_secs_f32().min(0.05);
                state.last_animation = now;
                if seconds > 0.0 {
                    let delta =
                        ScrollOffset::new(state.velocity.x * seconds, state.velocity.y * seconds);
                    let (candidate, remainder) = consume_from(
                        current,
                        delta,
                        self.maximum_offset(viewport),
                        self.horizontal,
                        self.vertical,
                    );
                    next = candidate;
                    needs_layout |= next != current;
                    if remainder.x.abs() > f32::EPSILON {
                        state.velocity.x = 0.0;
                    }
                    if remainder.y.abs() > f32::EPSILON {
                        state.velocity.y = 0.0;
                    }
                    trigger_elastic(state, now, remainder, viewport, self.elastic, &self.theme);
                    let decay = (-INERTIA_DECAY_PER_SECOND * seconds).exp();
                    state.velocity.x *= decay;
                    state.velocity.y *= decay;
                    pulse_stretch(state, now, state.velocity, viewport, &self.theme);
                }
                if magnitude(state.velocity) < MINIMUM_INERTIA_VELOCITY {
                    state.inertia_active = false;
                    next = begin_snap(state, now, next, viewport, self);
                    needs_layout |= next != current;
                } else {
                    keep_running = true;
                }
            }
            if state.snap_active {
                next = ScrollOffset::new(state.snap_x.value(now), state.snap_y.value(now));
                needs_layout |= next != current;
                if state.snap_x.is_active(now) || state.snap_y.is_active(now) {
                    keep_running = true;
                } else {
                    state.snap_active = false;
                }
            }
        }
        next = self.clamp_offset(next, viewport);
        if needs_layout && self.offset.set_if_changed(next) {
            ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
        }
        if keep_running {
            ctx.request_animation_frame();
        }
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        let rect = ctx.rect();
        let viewport = Size::new(rect.width, rect.height);
        match event {
            UiEvent::HoverChanged(false) => {
                ctx.state_mut::<ScrollState>()?.edge_hover = false;
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::HoverChanged(true) => {}
            UiEvent::FocusChanged(focused) => {
                let now = ctx.now();
                let state = ctx.state_mut::<ScrollState>()?;
                state.focused = *focused;
                if *focused {
                    show_activity(state, now, &self.theme);
                }
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                ctx.request_animation_frame();
            }
            UiEvent::PointerMoved { position } => {
                let dragging = ctx.state_mut::<ScrollState>()?.dragging;
                if let Some(drag) = dragging {
                    let offset = drag_thumb_to(*position, drag, rect, self, viewport);
                    self.offset.set_if_changed(offset);
                    let now = ctx.now();
                    let state = ctx.state_mut::<ScrollState>()?;
                    show_activity(state, now, &self.theme);
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
                    ctx.request_animation_frame();
                } else {
                    let edge = pointer_near_scrollbar(*position, rect, self);
                    let now = ctx.now();
                    let state = ctx.state_mut::<ScrollState>()?;
                    if state.edge_hover != edge {
                        state.edge_hover = edge;
                        if edge {
                            show_activity(state, now, &self.theme);
                        }
                        ctx.invalidate(Invalidation::PAINT);
                        ctx.request_animation_frame();
                    }
                }
            }
            UiEvent::PointerDown {
                position,
                button: PointerButton::Primary,
                ..
            } if self.enabled => {
                if handle_pointer_down(self, ctx, *position, rect, viewport)? {
                    return Ok(());
                }
            }
            UiEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } => {
                let was_dragging = ctx.state_mut::<ScrollState>()?.dragging.take().is_some();
                if was_dragging {
                    let current = self.clamp_offset(self.offset.get(), viewport);
                    let now = ctx.now();
                    let next = {
                        let state = ctx.state_mut::<ScrollState>()?;
                        begin_snap(state, now, current, viewport, self)
                    };
                    self.offset.set_if_changed(next);
                    ctx.release_pointer();
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
                    ctx.request_animation_frame();
                }
            }
            UiEvent::PointerCancel => {
                let was_dragging = {
                    let state = ctx.state_mut::<ScrollState>()?;
                    let was_dragging = state.dragging.take().is_some();
                    state.gesture_active = false;
                    cancel_kinetic(state);
                    was_dragging
                };
                if was_dragging {
                    ctx.release_pointer();
                    ctx.invalidate(Invalidation::PAINT);
                }
            }
            UiEvent::PointerScroll {
                delta_x,
                delta_y,
                modifiers,
                ..
            } if self.enabled => {
                if ctx.state_mut::<ScrollState>()?.dragging.is_some() {
                    ctx.set_handled();
                    return Ok(());
                }
                let (delta_x, delta_y) = remap_scroll(*delta_x, *delta_y, *modifiers);
                let result = self.consume_delta(viewport, delta_x, delta_y);
                let now = ctx.now();
                let state = ctx.state_mut::<ScrollState>()?;
                cancel_kinetic(state);
                show_activity(state, now, &self.theme);
                pulse_stretch(state, now, result.consumed, viewport, &self.theme);
                ctx.handoff_scroll(result.remainder.x, result.remainder.y, result.changed)?;
                if result.changed {
                    ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
                }
                ctx.request_animation_frame();
            }
            UiEvent::ScrollGesture {
                delta_x,
                delta_y,
                phase,
                modifiers,
                ..
            } if self.enabled => {
                if ctx.state_mut::<ScrollState>()?.dragging.is_some() {
                    ctx.set_handled();
                    return Ok(());
                }
                handle_scroll_gesture(self, ctx, *delta_x, *delta_y, *phase, *modifiers, viewport)?;
            }
            UiEvent::KeyDown { key, modifiers, .. } if self.enabled => {
                let line = self.theme.density_metrics().row_height;
                let (delta_x, delta_y, absolute) = keyboard_scroll(key, *modifiers, line, viewport);
                let changed = match absolute {
                    Some(target) => {
                        let next = self.clamp_offset(target, viewport);
                        self.offset.set_if_changed(next)
                    }
                    None => self.consume_delta(viewport, delta_x, delta_y).changed,
                };
                if changed {
                    let now = ctx.now();
                    let state = ctx.state_mut::<ScrollState>()?;
                    cancel_kinetic(state);
                    show_activity(state, now, &self.theme);
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
                    ctx.request_animation_frame();
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn scroll_boundary(
        &self,
        ctx: &mut EventCtx<'_>,
        delta_x: f32,
        delta_y: f32,
    ) -> Result<(), UiError> {
        let viewport = Size::new(ctx.rect().width, ctx.rect().height);
        let now = ctx.now();
        let state = ctx.state_mut::<ScrollState>()?;
        let delta_x = if self.horizontal { delta_x } else { 0.0 };
        let delta_y = if self.vertical { delta_y } else { 0.0 };
        if state.dragging.is_none()
            && trigger_elastic(
                state,
                now,
                ScrollOffset::new(delta_x, delta_y),
                viewport,
                self.elastic,
                &self.theme,
            )
        {
            show_activity(state, now, &self.theme);
            ctx.set_handled();
            ctx.invalidate(Invalidation::PAINT);
            ctx.request_animation_frame();
        }
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let offset = self.clamp_offset(
            ctx.watch(&self.offset, Invalidation::SEMANTICS),
            ctx.state_mut::<ScrollState>()
                .map(|state| state.observed_viewport)
                .unwrap_or(Size::ZERO),
        );
        let maximum = self.maximum_offset(
            ctx.state_mut::<ScrollState>()
                .map(|state| state.observed_viewport)
                .unwrap_or(Size::ZERO),
        );
        Semantics {
            role: SemanticRole::ScrollArea,
            label: Some(self.label.clone()),
            value: Some(format!(
                "horizontal {} of {}; vertical {} of {}",
                offset.x, maximum.x, offset.y, maximum.y
            )),
            enabled: self.enabled,
            focusable: self.enabled,
        }
    }

    fn focusable(&self) -> bool {
        self.enabled
    }

    fn accepts_pointer(&self) -> bool {
        self.enabled
    }

    fn clips_children(&self) -> bool {
        true
    }
}

fn handle_pointer_down(
    scroll: &Scroll,
    ctx: &mut EventCtx<'_>,
    position: Point,
    rect: Rect,
    viewport: Size,
) -> Result<bool, UiError> {
    let offset = scroll.clamp_offset(scroll.offset.get(), viewport);
    let thickness = scroll.theme.density_metrics().scrollbar;
    let vertical = scroll.vertical.then(|| {
        scrollbar_geometry(
            ScrollAxis::Vertical,
            rect,
            scroll.content_size,
            offset,
            thickness,
            0.0,
        )
    });
    let horizontal = scroll.horizontal.then(|| {
        scrollbar_geometry(
            ScrollAxis::Horizontal,
            rect,
            scroll.content_size,
            offset,
            thickness,
            0.0,
        )
    });
    for (axis, geometry) in [
        (ScrollAxis::Vertical, vertical.flatten()),
        (ScrollAxis::Horizontal, horizontal.flatten()),
    ] {
        let Some(geometry) = geometry else {
            continue;
        };
        if geometry.hit.contains(position)
            && geometry
                .thumb
                .expand(MINIMUM_POINTER_TARGET * 0.5)
                .contains(position)
        {
            let pointer = axis_value(axis, position);
            let origin = axis_rect_start(axis, geometry.thumb);
            let now = ctx.now();
            let state = ctx.state_mut::<ScrollState>()?;
            cancel_kinetic(state);
            state.dragging = Some(ThumbDrag {
                axis,
                grab_offset: pointer - origin,
            });
            show_activity(state, now, &scroll.theme);
            ctx.request_focus();
            ctx.capture_pointer();
            ctx.set_handled();
            ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            ctx.request_animation_frame();
            return Ok(true);
        }
        if geometry.hit.contains(position) {
            let pointer = axis_value(axis, position);
            let center = axis_rect_start(axis, geometry.thumb)
                + axis_rect_length(axis, geometry.thumb) * 0.5;
            let direction = if pointer < center { -1.0 } else { 1.0 };
            let result = match axis {
                ScrollAxis::Horizontal => {
                    scroll.consume_delta(viewport, viewport.width * direction, 0.0)
                }
                ScrollAxis::Vertical => {
                    scroll.consume_delta(viewport, 0.0, viewport.height * direction)
                }
            };
            let now = ctx.now();
            let state = ctx.state_mut::<ScrollState>()?;
            cancel_kinetic(state);
            show_activity(state, now, &scroll.theme);
            ctx.request_focus();
            ctx.set_handled();
            if result.changed {
                ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
            }
            ctx.request_animation_frame();
            return Ok(true);
        }
    }
    Ok(false)
}

fn handle_scroll_gesture(
    scroll: &Scroll,
    ctx: &mut EventCtx<'_>,
    delta_x: f32,
    delta_y: f32,
    phase: ScrollPhase,
    modifiers: Modifiers,
    viewport: Size,
) -> Result<(), UiError> {
    let now = ctx.now();
    if phase == ScrollPhase::Cancel {
        let state = ctx.state_mut::<ScrollState>()?;
        state.gesture_active = false;
        cancel_kinetic(state);
        ctx.handoff_scroll(0.0, 0.0, false)?;
        return Ok(());
    }
    if phase == ScrollPhase::Begin {
        let state = ctx.state_mut::<ScrollState>()?;
        cancel_kinetic(state);
        state.gesture_active = true;
        state.last_gesture = now;
        state.velocity = ScrollOffset::ZERO;
        show_activity(state, now, &scroll.theme);
    }
    let (delta_x, delta_y) = remap_scroll(delta_x, delta_y, modifiers);
    let result = scroll.consume_delta(viewport, delta_x, delta_y);
    {
        let state = ctx.state_mut::<ScrollState>()?;
        let elapsed = now.saturating_sub(state.last_gesture).as_secs_f32();
        if elapsed > 0.0 && phase != ScrollPhase::Begin {
            let instantaneous =
                ScrollOffset::new(result.consumed.x / elapsed, result.consumed.y / elapsed);
            state.velocity.x = (state.velocity.x * 0.35 + instantaneous.x * 0.65)
                .clamp(-MAXIMUM_INERTIA_VELOCITY, MAXIMUM_INERTIA_VELOCITY);
            state.velocity.y = (state.velocity.y * 0.35 + instantaneous.y * 0.65)
                .clamp(-MAXIMUM_INERTIA_VELOCITY, MAXIMUM_INERTIA_VELOCITY);
        }
        state.last_gesture = now;
        show_activity(state, now, &scroll.theme);
        pulse_stretch(state, now, result.consumed, viewport, &scroll.theme);
        if phase == ScrollPhase::End {
            state.gesture_active = false;
            if scroll.theme.motion.spatial_motion_enabled()
                && magnitude(state.velocity) >= MINIMUM_INERTIA_VELOCITY
            {
                state.inertia_active = true;
                state.last_animation = now;
            } else {
                let current = result.next;
                let next = begin_snap(state, now, current, viewport, scroll);
                scroll.offset.set_if_changed(next);
            }
        }
    }
    ctx.handoff_scroll(result.remainder.x, result.remainder.y, result.changed)?;
    if result.changed {
        ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
    }
    ctx.request_animation_frame();
    Ok(())
}

fn drag_thumb_to(
    position: Point,
    drag: ThumbDrag,
    rect: Rect,
    scroll: &Scroll,
    viewport: Size,
) -> ScrollOffset {
    let current = scroll.clamp_offset(scroll.offset.get(), viewport);
    let geometry = scrollbar_geometry(
        drag.axis,
        rect,
        scroll.content_size,
        current,
        scroll.theme.density_metrics().scrollbar,
        0.0,
    );
    let Some(geometry) = geometry else {
        return current;
    };
    let thumb_start = (axis_value(drag.axis, position) - drag.grab_offset - geometry.track_start)
        .clamp(0.0, geometry.travel);
    let value = if geometry.travel > 0.0 {
        geometry.maximum * thumb_start / geometry.travel
    } else {
        0.0
    };
    match drag.axis {
        ScrollAxis::Horizontal => ScrollOffset::new(value, current.y),
        ScrollAxis::Vertical => ScrollOffset::new(current.x, value),
    }
}

fn scrollbar_geometry(
    axis: ScrollAxis,
    rect: Rect,
    content: Size,
    offset: ScrollOffset,
    thickness: f32,
    stretch: f32,
) -> Option<ScrollbarGeometry> {
    let (viewport_length, content_length, value, track_start) = match axis {
        ScrollAxis::Horizontal => (rect.width, content.width, offset.x, rect.x),
        ScrollAxis::Vertical => (rect.height, content.height, offset.y, rect.y),
    };
    if content_length <= viewport_length || viewport_length <= 0.0 {
        return None;
    }
    let base_length = (viewport_length * viewport_length / content_length)
        .max(MINIMUM_THUMB)
        .min(viewport_length);
    let travel = (viewport_length - base_length).max(0.0);
    let maximum = (content_length - viewport_length).max(0.0);
    let base_start = track_start + travel * (value / maximum.max(1.0));
    let center = base_start + base_length * 0.5;
    let visible_length =
        (base_length * (1.0 + stretch.clamp(0.0, 1.0) * 0.12)).min(viewport_length);
    let visible_start = center - visible_length * 0.5;
    let (thumb, hit) = match axis {
        ScrollAxis::Horizontal => {
            let y = rect.bottom() - thickness - 3.0;
            (
                Rect::new(visible_start, y, visible_length, thickness),
                Rect::new(
                    rect.x,
                    rect.bottom() - MINIMUM_POINTER_TARGET,
                    rect.width,
                    MINIMUM_POINTER_TARGET,
                ),
            )
        }
        ScrollAxis::Vertical => {
            let x = rect.right() - thickness - 3.0;
            (
                Rect::new(x, visible_start, thickness, visible_length),
                Rect::new(
                    rect.right() - MINIMUM_POINTER_TARGET,
                    rect.y,
                    MINIMUM_POINTER_TARGET,
                    rect.height,
                ),
            )
        }
    };
    Some(ScrollbarGeometry {
        thumb,
        hit,
        track_start,
        travel,
        maximum,
    })
}

fn paint_thumb(
    ctx: &mut PaintCtx<'_>,
    thumb: Rect,
    visibility: f32,
    scroll: &Scroll,
) -> Result<(), UiError> {
    if visibility <= 0.001 {
        return Ok(());
    }
    let alpha = if scroll.high_contrast_scrollbars {
        0.92
    } else {
        0.62
    };
    let radius = thumb.width.min(thumb.height) * 0.5;
    ctx.builder().rounded_rect(
        thumb,
        CornerRadii::all(radius),
        with_alpha(scroll.theme.palette.text_secondary, alpha * visibility),
    )?;
    if scroll.high_contrast_scrollbars {
        ctx.builder().border(
            thumb,
            CornerRadii::all(radius),
            1.0,
            with_alpha(scroll.theme.palette.edge, 0.72 * visibility),
        )?;
    }
    Ok(())
}

fn show_activity(state: &mut ScrollState, now: Duration, theme: &Theme) {
    state.activity_seen = true;
    state.last_activity = now;
    state
        .visibility
        .retarget(now, 1.0, theme.motion.spec(MotionFamily::ScrollbarShow));
}

fn cancel_kinetic(state: &mut ScrollState) {
    state.inertia_active = false;
    state.snap_active = false;
    state.velocity = ScrollOffset::ZERO;
}

fn begin_snap(
    state: &mut ScrollState,
    now: Duration,
    current: ScrollOffset,
    viewport: Size,
    scroll: &Scroll,
) -> ScrollOffset {
    let target = scroll.snap_target(current, viewport);
    if target == current {
        state.snap_active = false;
        return current;
    }
    if !scroll.theme.motion.spatial_motion_enabled() {
        state.snap_active = false;
        return target;
    }
    let spec = scroll.theme.motion.spec(MotionFamily::Overscroll);
    state.snap_x.settle(current.x);
    state.snap_y.settle(current.y);
    state.snap_x.retarget(now, target.x, spec);
    state.snap_y.retarget(now, target.y, spec);
    state.snap_active = true;
    current
}

fn trigger_elastic(
    state: &mut ScrollState,
    now: Duration,
    remainder: ScrollOffset,
    viewport: Size,
    enabled: bool,
    theme: &Theme,
) -> bool {
    if !enabled || !theme.motion.spatial_motion_enabled() {
        return false;
    }
    let maximum_x = 24.0_f32.min(viewport.width * 0.05);
    let maximum_y = 24.0_f32.min(viewport.height * 0.05);
    let x = -rubber_band(remainder.x, maximum_x);
    let y = -rubber_band(remainder.y, maximum_y);
    let spec = theme.motion.spec(MotionFamily::Overscroll);
    let mut changed = false;
    if x.abs() > f32::EPSILON {
        state.elastic_x.settle(x);
        state.elastic_x.retarget(now, 0.0, spec);
        changed = true;
    }
    if y.abs() > f32::EPSILON {
        state.elastic_y.settle(y);
        state.elastic_y.retarget(now, 0.0, spec);
        changed = true;
    }
    changed
}

fn pulse_stretch(
    state: &mut ScrollState,
    now: Duration,
    movement: ScrollOffset,
    viewport: Size,
    theme: &Theme,
) {
    if !theme.motion.spatial_motion_enabled() {
        return;
    }
    let spec = theme.motion.spec(MotionFamily::SliderTrail);
    let x = (movement.x.abs() / (viewport.width * 0.25).max(1.0)).clamp(0.0, 1.0);
    let y = (movement.y.abs() / (viewport.height * 0.25).max(1.0)).clamp(0.0, 1.0);
    if x > 0.0 {
        state.stretch_x.settle(x);
        state.stretch_x.retarget(now, 0.0, spec);
    }
    if y > 0.0 {
        state.stretch_y.settle(y);
        state.stretch_y.retarget(now, 0.0, spec);
    }
}

fn consume_from(
    current: ScrollOffset,
    delta: ScrollOffset,
    maximum: ScrollOffset,
    horizontal: bool,
    vertical: bool,
) -> (ScrollOffset, ScrollOffset) {
    let next = ScrollOffset::new(
        if horizontal {
            (current.x + delta.x).clamp(0.0, maximum.x)
        } else {
            0.0
        },
        if vertical {
            (current.y + delta.y).clamp(0.0, maximum.y)
        } else {
            0.0
        },
    );
    (
        next,
        ScrollOffset::new(
            delta.x - (next.x - current.x),
            delta.y - (next.y - current.y),
        ),
    )
}

fn reveal_minimally(offset: ScrollOffset, viewport: Size, target: Rect) -> ScrollOffset {
    ScrollOffset::new(
        reveal_axis(offset.x, viewport.width, target.x, target.right()),
        reveal_axis(offset.y, viewport.height, target.y, target.bottom()),
    )
}

fn reveal_axis(offset: f32, viewport: f32, start: f32, end: f32) -> f32 {
    if start <= offset && end >= offset + viewport {
        offset
    } else if start < offset {
        start
    } else if end > offset + viewport {
        end - viewport
    } else {
        offset
    }
}

fn nearest_snap(value: f32, maximum: f32, points: &[f32]) -> f32 {
    points
        .iter()
        .map(|point| point.clamp(0.0, maximum))
        .min_by(|first, second| {
            (value - *first)
                .abs()
                .partial_cmp(&(value - *second).abs())
                .unwrap_or(Ordering::Equal)
        })
        .unwrap_or(value.clamp(0.0, maximum))
}

fn remap_scroll(delta_x: f32, delta_y: f32, modifiers: Modifiers) -> (f32, f32) {
    if modifiers.shift && delta_x.abs() <= f32::EPSILON {
        (delta_y, 0.0)
    } else {
        (delta_x, delta_y)
    }
}

fn keyboard_scroll(
    key: &Key,
    modifiers: Modifiers,
    line: f32,
    viewport: Size,
) -> (f32, f32, Option<ScrollOffset>) {
    let character = match key {
        Key::Character(character) if !modifiers.control && !modifiers.alt && !modifiers.logo => {
            Some(character.as_str())
        }
        _ => None,
    };
    match (key, character) {
        (Key::ArrowUp, _) | (_, Some("k" | "K")) => (0.0, -line, None),
        (Key::ArrowDown, _) | (_, Some("j" | "J")) => (0.0, line, None),
        (Key::ArrowLeft, _) | (_, Some("h" | "H")) => (-line, 0.0, None),
        (Key::ArrowRight, _) | (_, Some("l" | "L")) => (line, 0.0, None),
        (Key::PageUp, _) => (0.0, -viewport.height, None),
        (Key::PageDown, _) => (0.0, viewport.height, None),
        (Key::Space, _) if modifiers.shift => (0.0, -viewport.height, None),
        (Key::Space, _) => (0.0, viewport.height, None),
        (Key::Home, _) => (0.0, 0.0, Some(ScrollOffset::ZERO)),
        (Key::End, _) => (0.0, 0.0, Some(ScrollOffset::new(f32::MAX, f32::MAX))),
        _ => (0.0, 0.0, None),
    }
}

fn pointer_near_scrollbar(position: Point, rect: Rect, scroll: &Scroll) -> bool {
    (scroll.vertical && position.x >= rect.right() - MINIMUM_POINTER_TARGET)
        || (scroll.horizontal && position.y >= rect.bottom() - MINIMUM_POINTER_TARGET)
}

fn rubber_band(delta: f32, maximum: f32) -> f32 {
    if maximum <= 0.0 || delta == 0.0 {
        return 0.0;
    }
    delta.signum() * maximum * (1.0 - 1.0 / (1.0 + delta.abs() / maximum))
}

fn magnitude(offset: ScrollOffset) -> f32 {
    offset.x.hypot(offset.y)
}

fn finite_offset(offset: ScrollOffset) -> ScrollOffset {
    ScrollOffset::new(
        if offset.x.is_finite() { offset.x } else { 0.0 },
        if offset.y.is_finite() { offset.y } else { 0.0 },
    )
}

fn point_is_finite(point: Point) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

fn axis_value(axis: ScrollAxis, point: Point) -> f32 {
    match axis {
        ScrollAxis::Horizontal => point.x,
        ScrollAxis::Vertical => point.y,
    }
}

fn axis_rect_start(axis: ScrollAxis, rect: Rect) -> f32 {
    match axis {
        ScrollAxis::Horizontal => rect.x,
        ScrollAxis::Vertical => rect.y,
    }
}

fn axis_rect_length(axis: ScrollAxis, rect: Rect) -> f32 {
    match axis {
        ScrollAxis::Horizontal => rect.width,
        ScrollAxis::Vertical => rect.height,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollError {
    InvalidContentSize,
    InvalidAnchor,
    InvalidReveal,
    InvalidSnapPoint,
}

impl fmt::Display for ScrollError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidContentSize => "scroll content size must be finite and non-negative",
            Self::InvalidAnchor => "scroll anchor points must be finite",
            Self::InvalidReveal => "scroll reveal rectangle must be finite and non-negative",
            Self::InvalidSnapPoint => "scroll snap points must be finite and non-negative",
        })
    }
}

impl std::error::Error for ScrollError {}
