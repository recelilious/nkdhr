use std::{any::Any, collections::HashSet, fmt, rc::Rc, sync::Arc};

use nkdhr_render::{CornerRadii, Point, Rect};

use crate::text::{TextLayout, TextWrap};
use crate::theme::with_alpha;
use crate::{
    ArrangeCtx, Constraints, EventCtx, Invalidation, Key, MaterialCapabilities, MaterialTier,
    MeasureCtx, MotionFamily, PaintCtx, PointerButton, Reactive, ScalarMotion, SemanticRole,
    Semantics, SemanticsCtx, Size, Theme, UiError, UiEvent, Widget,
};

use super::surface::{SurfaceState, paint_surface};

/// One shared-panel vertical list. Selection uses a stable item identity;
/// `item_ids` supplies the current declarative child order separately.
pub struct List {
    label: String,
    selection: Reactive<Option<u64>>,
    item_ids: Vec<u64>,
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
}

impl List {
    pub fn new(
        label: impl Into<String>,
        selection: Reactive<Option<u64>>,
        item_ids: impl IntoIterator<Item = u64>,
        theme: Arc<Theme>,
    ) -> Result<Self, ListError> {
        let item_ids = item_ids.into_iter().collect::<Vec<_>>();
        let unique = item_ids.iter().copied().collect::<HashSet<_>>();
        if unique.len() != item_ids.len() {
            return Err(ListError::DuplicateItemIdentity);
        }
        Ok(Self {
            label: label.into(),
            selection,
            item_ids,
            theme,
            capabilities: MaterialCapabilities::default(),
        })
    }

    pub fn capabilities(mut self, capabilities: MaterialCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

#[derive(Debug, Clone, Copy)]
struct ListState {
    selection_y: ScalarMotion,
    selection_height: ScalarMotion,
    selection_opacity: ScalarMotion,
    last_selection: Option<u64>,
    last_position: Option<usize>,
    initialized: bool,
}

impl Default for ListState {
    fn default() -> Self {
        Self {
            selection_y: ScalarMotion::settled(0.0),
            selection_height: ScalarMotion::settled(0.0),
            selection_opacity: ScalarMotion::settled(0.0),
            last_selection: None,
            last_position: None,
            initialized: false,
        }
    }
}

impl Widget for List {
    fn create_state(&self) -> Box<dyn Any> {
        Box::<ListState>::default()
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        let mut width = constraints.min().width;
        let mut height = 0.0_f32;
        for index in 0..ctx.child_count() {
            let child = ctx.measure_child(
                index,
                Constraints::new(Size::new(constraints.min().width, 0.0), constraints.max())?,
            )?;
            width = width.max(child.width);
            height += child.height;
            if !height.is_finite() {
                return Err(UiError::InvalidSize);
            }
        }
        Ok(constraints.constrain(Size::new(width, height)))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        let mut y = rect.y;
        for index in 0..ctx.child_count() {
            let size = ctx.child_size(index)?;
            ctx.arrange_child(index, Rect::new(rect.x, y, rect.width, size.height))?;
            y += size.height;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let selection = ctx.watch(
            &self.selection,
            Invalidation::PAINT | Invalidation::SEMANTICS,
        );
        let rect = ctx.rect();
        paint_surface(
            ctx.builder(),
            rect,
            CornerRadii::all(self.theme.radii.group),
            &self.theme,
            MaterialTier::ContentSurface,
            self.capabilities,
            SurfaceState::default(),
        )?;

        let selected_position = selection
            .and_then(|identity| self.item_ids.iter().position(|item| *item == identity))
            .filter(|position| *position < ctx.child_count());
        let selected_rect = selected_position
            .map(|position| ctx.child_rect(position))
            .transpose()?;
        let now = ctx.now();
        let (y, height, opacity, active) = {
            let state = ctx.state_mut::<ListState>()?;
            if !state.initialized {
                if let Some(selected) = selected_rect {
                    state.selection_y.settle(selected.y);
                    state.selection_height.settle(selected.height);
                    state.selection_opacity.settle(1.0);
                }
                state.last_selection = selection;
                state.last_position = selected_position;
                state.initialized = true;
            } else if state.last_selection != selection || state.last_position != selected_position
            {
                if let Some(selected) = selected_rect {
                    let nearby = state.last_position.is_some_and(|previous| {
                        previous
                            .abs_diff(selected_position.expect("selected rectangle has a position"))
                            <= 3
                    });
                    if nearby && self.theme.motion.spatial_motion_enabled() {
                        let spec = self.theme.motion.spec(MotionFamily::ListTransfer);
                        state.selection_y.retarget(now, selected.y, spec);
                        state.selection_height.retarget(now, selected.height, spec);
                    } else {
                        state.selection_y.settle(selected.y);
                        state.selection_height.settle(selected.height);
                    }
                    state.selection_opacity.retarget(
                        now,
                        1.0,
                        self.theme.motion.spec(MotionFamily::ListTransfer),
                    );
                } else {
                    state.selection_opacity.retarget(
                        now,
                        0.0,
                        self.theme.motion.spec(MotionFamily::ListTransfer),
                    );
                }
                state.last_selection = selection;
                state.last_position = selected_position;
            }
            (
                state.selection_y.value(now),
                state.selection_height.value(now),
                state.selection_opacity.value(now).clamp(0.0, 1.0),
                state.selection_y.is_active(now)
                    || state.selection_height.is_active(now)
                    || state.selection_opacity.is_active(now),
            )
        };
        if active {
            ctx.request_animation_frame();
        }
        if opacity > 0.0 && height > 0.0 {
            let mass = Rect::new(
                rect.x + 4.0,
                y + 4.0,
                (rect.width - 8.0).max(0.0),
                (height - 8.0).max(0.0),
            );
            ctx.builder().rounded_rect(
                mass,
                CornerRadii::all(self.theme.radii.control),
                with_alpha(self.theme.palette.accent, 0.18 * opacity),
            )?;
            ctx.builder().border(
                mass,
                CornerRadii::all(self.theme.radii.control),
                1.0,
                with_alpha(self.theme.palette.accent_secondary, 0.60 * opacity),
            )?;
        }

        for index in 0..ctx.child_count().saturating_sub(1) {
            let child = ctx.child_rect(index)?;
            ctx.builder().rect(
                Rect::new(
                    rect.x + 12.0,
                    child.bottom(),
                    (rect.width - 24.0).max(0.0),
                    1.0,
                ),
                with_alpha(self.theme.palette.edge, 0.08),
            )?;
        }
        ctx.paint_children()
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let selection = ctx.watch(&self.selection, Invalidation::SEMANTICS);
        Semantics {
            role: SemanticRole::List,
            label: Some(self.label.clone()),
            value: selection.map(|index| format!("selected item {index}")),
            enabled: true,
            focusable: false,
        }
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        let UiEvent::KeyDown { key, .. } = event else {
            return Ok(());
        };
        if self.item_ids.is_empty() {
            return Ok(());
        }
        let current = self
            .selection
            .get()
            .and_then(|identity| self.item_ids.iter().position(|item| *item == identity));
        let next = match key {
            Key::ArrowUp => Some(current.unwrap_or(0).saturating_sub(1)),
            Key::ArrowDown => Some(
                current
                    .map_or(0, |position| position.saturating_add(1))
                    .min(self.item_ids.len() - 1),
            ),
            Key::Home => Some(0),
            Key::End => Some(self.item_ids.len() - 1),
            Key::PageUp => Some(current.unwrap_or(0).saturating_sub(5)),
            Key::PageDown => Some(
                current
                    .map_or(0, |position| position.saturating_add(5))
                    .min(self.item_ids.len() - 1),
            ),
            _ => None,
        };
        if let Some(next) = next {
            self.selection.set(Some(self.item_ids[next]));
            ctx.set_handled();
            ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
        }
        Ok(())
    }

    fn clips_children(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ListItemBehavior {
    #[default]
    Navigation,
    Object,
}

pub struct ListItem {
    identity: u64,
    label: String,
    selection: Reactive<Option<u64>>,
    theme: Arc<Theme>,
    behavior: ListItemBehavior,
    enabled: bool,
    on_activate: Option<Rc<dyn Fn()>>,
}

impl ListItem {
    pub fn new(
        identity: u64,
        label: impl Into<String>,
        selection: Reactive<Option<u64>>,
        theme: Arc<Theme>,
    ) -> Self {
        Self {
            identity,
            label: label.into(),
            selection,
            theme,
            behavior: ListItemBehavior::Navigation,
            enabled: true,
            on_activate: None,
        }
    }

    pub fn behavior(mut self, behavior: ListItemBehavior) -> Self {
        self.behavior = behavior;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn on_activate(mut self, callback: impl Fn() + 'static) -> Self {
        self.on_activate = Some(Rc::new(callback));
        self
    }

    fn select(&self, activate_navigation: bool) {
        if !self.enabled {
            return;
        }
        self.selection.set(Some(self.identity));
        if (activate_navigation || self.behavior == ListItemBehavior::Navigation)
            && let Some(callback) = &self.on_activate
        {
            callback();
        }
    }
}

#[derive(Debug, Clone)]
struct ListItemState {
    hovered: ScalarMotion,
    focused: bool,
    pressed: bool,
    armed: bool,
    label_layout: Option<Arc<TextLayout>>,
}

impl Default for ListItemState {
    fn default() -> Self {
        Self {
            hovered: ScalarMotion::settled(0.0),
            focused: false,
            pressed: false,
            armed: false,
            label_layout: None,
        }
    }
}

impl Widget for ListItem {
    fn create_state(&self) -> Box<dyn Any> {
        Box::<ListItemState>::default()
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        let height = self.theme.density_metrics().row_height;
        let child = if ctx.child_count() == 1 {
            ctx.measure_child(
                0,
                Constraints::new(Size::ZERO, Size::new(constraints.max().width, height))?,
            )?
        } else {
            let mut style = self.theme.text_style(crate::TextRole::Body);
            style.wrap = TextWrap::None;
            let layout = ctx.layout_text(&self.label, &style, None)?;
            let size = Size::new(layout.width() + 32.0, layout.height());
            ctx.state_mut::<ListItemState>()?.label_layout = Some(layout);
            size
        };
        Ok(constraints.constrain(Size::new(child.width, height.max(child.height))))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        if ctx.child_count() == 1 {
            let child = ctx.child_size(0)?;
            ctx.arrange_child(
                0,
                Rect::new(
                    rect.x + 16.0,
                    rect.y + (rect.height - child.height).max(0.0) * 0.5,
                    child.width.min((rect.width - 32.0).max(0.0)),
                    child.height.min(rect.height),
                ),
            )?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let selected = ctx.watch(&self.selection, Invalidation::PAINT) == Some(self.identity);
        let now = ctx.now();
        let draw_internal_label = ctx.child_count() == 0;
        let (hovered, focused, pressed, active, label_layout) = {
            let state = ctx.state_mut::<ListItemState>()?;
            (
                state.hovered.value(now),
                state.focused,
                state.pressed,
                state.hovered.is_active(now),
                state.label_layout.clone(),
            )
        };
        if active {
            ctx.request_animation_frame();
        }
        let rect = ctx.rect().inset(4.0);
        if hovered > 0.0 || focused || pressed {
            ctx.builder().rounded_rect(
                rect,
                CornerRadii::all(self.theme.radii.control),
                with_alpha(
                    if selected {
                        self.theme.palette.accent_secondary
                    } else {
                        self.theme.palette.surface_raised
                    },
                    0.06 + hovered * 0.04 + if pressed { 0.04 } else { 0.0 },
                ),
            )?;
        }
        if focused {
            ctx.builder().border(
                rect,
                CornerRadii::all(self.theme.radii.control),
                2.0,
                with_alpha(self.theme.palette.accent_secondary, 0.80),
            )?;
        }
        if draw_internal_label && let Some(layout) = label_layout {
            ctx.draw_text(
                &layout,
                Point::new(
                    ctx.rect().x + 16.0,
                    ctx.rect().y + (ctx.rect().height - layout.height()).max(0.0) * 0.5,
                ),
                if self.enabled {
                    self.theme.palette.text_primary
                } else {
                    self.theme.palette.text_muted
                },
                Some(ctx.rect()),
            )?;
        }
        ctx.paint_children()
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        match event {
            UiEvent::HoverChanged(hovered) => {
                let now = ctx.now();
                let state = ctx.state_mut::<ListItemState>()?;
                state.hovered.retarget(
                    now,
                    if *hovered { 1.0 } else { 0.0 },
                    self.theme.motion.spec(if *hovered {
                        MotionFamily::HoverIn
                    } else {
                        MotionFamily::HoverOut
                    }),
                );
                if state.pressed {
                    state.armed = *hovered;
                }
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::FocusChanged(focused) => {
                ctx.state_mut::<ListItemState>()?.focused = *focused;
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerDown {
                button: PointerButton::Primary,
                ..
            } if self.enabled => {
                let state = ctx.state_mut::<ListItemState>()?;
                state.pressed = true;
                state.armed = true;
                ctx.request_focus();
                ctx.capture_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT);
            }
            UiEvent::PointerMoved { position } => {
                let rect = ctx.rect();
                let state = ctx.state_mut::<ListItemState>()?;
                if state.pressed {
                    state.armed = rect.contains(*position);
                    ctx.set_handled();
                }
            }
            UiEvent::PointerUp {
                position,
                button: PointerButton::Primary,
            } => {
                let rect = ctx.rect();
                let activate = {
                    let state = ctx.state_mut::<ListItemState>()?;
                    let activate = state.pressed && state.armed && rect.contains(*position);
                    state.pressed = false;
                    state.armed = false;
                    activate
                };
                if activate {
                    self.select(false);
                }
                ctx.release_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerCancel => {
                let state = ctx.state_mut::<ListItemState>()?;
                state.pressed = false;
                state.armed = false;
                ctx.release_pointer();
                ctx.invalidate(Invalidation::PAINT);
            }
            UiEvent::KeyDown {
                key: Key::Space,
                repeat: false,
                ..
            } if self.enabled => {
                self.selection.set(Some(self.identity));
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::KeyDown {
                key: Key::Enter,
                repeat: false,
                ..
            } if self.enabled => {
                self.select(true);
                ctx.set_handled();
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let selected = ctx.watch(&self.selection, Invalidation::SEMANTICS) == Some(self.identity);
        Semantics {
            role: SemanticRole::ListItem,
            label: Some(self.label.clone()),
            value: selected.then(|| "selected".to_owned()),
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListError {
    DuplicateItemIdentity,
}

impl fmt::Display for ListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("list item identities must be unique")
    }
}

impl std::error::Error for ListError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_stable_identities_are_rejected() {
        let result = List::new(
            "duplicate",
            Reactive::new(None),
            [7, 7],
            Arc::new(Theme::default()),
        );
        assert!(matches!(result, Err(ListError::DuplicateItemIdentity)));
    }
}
