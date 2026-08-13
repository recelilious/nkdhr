use std::{any::Any, rc::Rc, sync::Arc};

use nkdhr_render::{CornerRadii, Point, Rect};

use crate::text::{TextLayout, TextWrap};
use crate::{
    ArrangeCtx, Constraints, EventCtx, Invalidation, Key, MaterialCapabilities, MaterialTier,
    MeasureCtx, MotionFamily, PaintCtx, PointerButton, ScalarMotion, SemanticRole, Semantics,
    SemanticsCtx, Size, Theme, ThemeReadSet, UiError, UiEvent, UpdateCtx, Widget,
};

use super::surface::{SurfaceState, paint_fluid_surface, paint_surface, surface_theme_reads};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ButtonVariant {
    Primary,
    #[default]
    Secondary,
    Quiet,
    Destructive,
    Selected,
    Fluid,
    FluidSelected,
}

pub struct Button {
    label: String,
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    variant: ButtonVariant,
    enabled: bool,
    pending: bool,
    on_activate: Option<Rc<dyn Fn()>>,
}

impl Button {
    pub fn new(label: impl Into<String>, theme: Arc<Theme>) -> Self {
        Self {
            label: label.into(),
            theme,
            capabilities: MaterialCapabilities::default(),
            variant: ButtonVariant::Secondary,
            enabled: true,
            pending: false,
            on_activate: None,
        }
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn pending(mut self, pending: bool) -> Self {
        self.pending = pending;
        self
    }

    pub fn capabilities(mut self, capabilities: MaterialCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn on_activate(mut self, callback: impl Fn() + 'static) -> Self {
        self.on_activate = Some(Rc::new(callback));
        self
    }

    fn interactive(&self) -> bool {
        self.enabled && !self.pending
    }

    fn activate(&self) {
        if self.interactive()
            && let Some(callback) = &self.on_activate
        {
            callback();
        }
    }
}

#[derive(Debug, Clone)]
struct ButtonState {
    hovered: ScalarMotion,
    pressed: ScalarMotion,
    focused: bool,
    pointer_pressed: bool,
    armed: bool,
    keyboard_pressed: bool,
    label_layout: Option<Arc<TextLayout>>,
}

impl Default for ButtonState {
    fn default() -> Self {
        Self {
            hovered: ScalarMotion::settled(0.0),
            pressed: ScalarMotion::settled(0.0),
            focused: false,
            pointer_pressed: false,
            armed: false,
            keyboard_pressed: false,
            label_layout: None,
        }
    }
}

impl Widget for Button {
    fn theme_reads(&self) -> ThemeReadSet {
        let tier = if self.variant == ButtonVariant::Quiet {
            MaterialTier::Ghost
        } else {
            MaterialTier::CompactNode
        };
        let mut reads = surface_theme_reads(tier);
        reads.extend([
            "density",
            "spacing.small",
            "radii.control",
            "typography.ui_families",
            "typography.scale",
            "typography.label.font_size",
            "typography.label.line_height",
            "typography.label.weight",
            "palette.text_muted",
            "palette.on_accent",
            "palette.text_primary",
            "motion.mode",
            "motion.speed_multiplier",
            "motion.standard",
            "motion.settle",
            "motion.exit",
            "motion.durations.hover_in",
            "motion.durations.hover_out",
            "motion.durations.press",
            "motion.durations.release",
        ]);
        reads
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn create_state(&self) -> Box<dyn Any> {
        Box::<ButtonState>::default()
    }

    fn update(&self, previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        if previous.theme.density != self.theme.density
            || previous.theme.typography != self.theme.typography
            || previous.label != self.label
        {
            ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
        } else {
            ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
        }
        if !self.interactive()
            && let Ok(state) = ctx.state_mut::<ButtonState>()
        {
            state.pointer_pressed = false;
            state.keyboard_pressed = false;
            state.armed = false;
            state.pressed.settle(0.0);
        }
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        let metrics = self.theme.density_metrics();
        let horizontal = self.theme.spacing.small + 5.0;
        let child = if ctx.child_count() == 1 {
            ctx.measure_child(
                0,
                constraints.deflate(crate::Insets::symmetric(horizontal, 0.0))?,
            )?
        } else {
            let mut style = self.theme.text_style(crate::TextRole::Label);
            style.wrap = TextWrap::None;
            let layout = ctx.layout_text(&self.label, &style, None)?;
            let size = Size::new(layout.width(), layout.height());
            ctx.state_mut::<ButtonState>()?.label_layout = Some(layout);
            size
        };
        Ok(constraints.constrain(Size::new(
            (child.width + horizontal * 2.0).max(metrics.control_height),
            child.height.max(metrics.control_height),
        )))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        if ctx.child_count() == 1 {
            let size = ctx.child_size(0)?;
            ctx.arrange_child(
                0,
                Rect::new(
                    rect.x + (rect.width - size.width).max(0.0) * 0.5,
                    rect.y + (rect.height - size.height).max(0.0) * 0.5,
                    size.width.min(rect.width),
                    size.height.min(rect.height),
                ),
            )?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let now = ctx.now();
        let draw_internal_label = ctx.child_count() == 0;
        let (hovered, pressed, focused, active, label_layout) = {
            let state = ctx.state_mut::<ButtonState>()?;
            (
                state.hovered.value(now),
                state.pressed.value(now),
                state.focused,
                state.hovered.is_active(now) || state.pressed.is_active(now),
                state.label_layout.clone(),
            )
        };
        if active {
            ctx.request_animation_frame();
        }
        let spatial = self.theme.motion.spatial_motion_enabled();
        let lift = if spatial { hovered - pressed } else { 0.0 };
        let compression = if spatial { pressed * 2.0 } else { 0.0 };
        let rect = Rect::new(
            ctx.rect().x,
            ctx.rect().y - lift + compression * 0.5,
            ctx.rect().width,
            (ctx.rect().height - compression).max(0.0),
        );
        let tier = match self.variant {
            ButtonVariant::Quiet => MaterialTier::Ghost,
            _ => MaterialTier::CompactNode,
        };
        let surface_state = SurfaceState {
            hovered,
            pressed,
            focused,
            accented: matches!(self.variant, ButtonVariant::Primary),
            selected: matches!(
                self.variant,
                ButtonVariant::Selected | ButtonVariant::FluidSelected
            ),
            disabled: !self.enabled,
            destructive: matches!(self.variant, ButtonVariant::Destructive),
        };
        if self.variant != ButtonVariant::Quiet {
            paint_fluid_surface(
                ctx.builder(),
                rect,
                CornerRadii::all(self.theme.radii.control),
                &self.theme,
                self.capabilities,
                surface_state,
            )?;
        } else {
            paint_surface(
                ctx.builder(),
                rect,
                CornerRadii::all(self.theme.radii.control),
                &self.theme,
                tier,
                self.capabilities,
                surface_state,
            )?;
        }
        if self.pending {
            let edge_width = (rect.width * 0.24).clamp(10.0, 32.0).min(rect.width);
            let travel = (rect.width - edge_width).max(0.0);
            let progress = if spatial {
                let phase = (now.as_secs_f64() % 0.9) / 0.9;
                if phase <= 0.5 {
                    (phase * 2.0) as f32
                } else {
                    ((1.0 - phase) * 2.0) as f32
                }
            } else {
                0.5
            };
            ctx.builder().rounded_rect(
                Rect::new(
                    rect.x + travel * progress,
                    rect.bottom() - 2.0,
                    edge_width,
                    2.0,
                ),
                CornerRadii::all(1.0),
                crate::theme::with_alpha(self.theme.palette.accent_secondary, 0.88),
            )?;
            if spatial {
                ctx.request_animation_frame();
            }
        }
        if draw_internal_label && let Some(layout) = label_layout {
            let color = if !self.enabled {
                self.theme.palette.text_muted
            } else if matches!(self.variant, ButtonVariant::Primary) {
                self.theme.palette.on_accent
            } else {
                self.theme.palette.text_primary
            };
            ctx.draw_text(
                &layout,
                Point::new(
                    rect.x + (rect.width - layout.width()).max(0.0) * 0.5,
                    rect.y + (rect.height - layout.height()).max(0.0) * 0.5,
                ),
                color,
                Some(rect),
            )?;
        }
        ctx.paint_children()
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        let now = ctx.now();
        match event {
            UiEvent::PointerDown {
                button: PointerButton::Primary,
                ..
            } if self.enabled && self.pending => {
                ctx.set_handled();
            }
            UiEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } if self.enabled && self.pending => {
                let state = ctx.state_mut::<ButtonState>()?;
                state.pointer_pressed = false;
                state.armed = false;
                state.pressed.settle(0.0);
                ctx.release_pointer();
                ctx.set_handled();
            }
            UiEvent::KeyDown {
                key: Key::Space | Key::Enter,
                ..
            }
            | UiEvent::KeyUp {
                key: Key::Space | Key::Enter,
                ..
            } if self.enabled && self.pending => {
                ctx.set_handled();
            }
            UiEvent::HoverChanged(hovered) => {
                let family = if *hovered {
                    MotionFamily::HoverIn
                } else {
                    MotionFamily::HoverOut
                };
                let state = ctx.state_mut::<ButtonState>()?;
                state
                    .hovered
                    .retarget(now, f32::from(*hovered), self.theme.motion.spec(family));
                if state.pointer_pressed {
                    state.armed = *hovered;
                }
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::FocusChanged(focused) => {
                let state = ctx.state_mut::<ButtonState>()?;
                state.focused = *focused;
                if !focused && state.keyboard_pressed {
                    state.keyboard_pressed = false;
                    state
                        .pressed
                        .retarget(now, 0.0, self.theme.motion.spec(MotionFamily::Release));
                }
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerDown {
                button: PointerButton::Primary,
                ..
            } if self.interactive() => {
                let state = ctx.state_mut::<ButtonState>()?;
                state.pointer_pressed = true;
                state.armed = true;
                state
                    .pressed
                    .retarget(now, 1.0, self.theme.motion.spec(MotionFamily::Press));
                ctx.request_focus();
                ctx.capture_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::PointerMoved { position } => {
                let rect = ctx.rect();
                let state = ctx.state_mut::<ButtonState>()?;
                if state.pointer_pressed {
                    state.armed = rect.contains(*position);
                    ctx.set_handled();
                }
            }
            UiEvent::PointerUp {
                position,
                button: PointerButton::Primary,
                ..
            } => {
                let rect = ctx.rect();
                let activate = {
                    let state = ctx.state_mut::<ButtonState>()?;
                    if !state.pointer_pressed {
                        false
                    } else {
                        let activate = state.armed && rect.contains(*position);
                        state.pointer_pressed = false;
                        state.armed = false;
                        state.pressed.retarget(
                            now,
                            0.0,
                            self.theme.motion.spec(MotionFamily::Release),
                        );
                        activate
                    }
                };
                if activate {
                    self.activate();
                }
                ctx.release_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::PointerCancel => {
                let state = ctx.state_mut::<ButtonState>()?;
                state.pointer_pressed = false;
                state.armed = false;
                state
                    .pressed
                    .retarget(now, 0.0, self.theme.motion.spec(MotionFamily::Release));
                ctx.release_pointer();
                ctx.invalidate(Invalidation::PAINT);
            }
            UiEvent::KeyDown {
                key: Key::Space,
                repeat: false,
                ..
            } if self.interactive() => {
                let state = ctx.state_mut::<ButtonState>()?;
                state.keyboard_pressed = true;
                state
                    .pressed
                    .retarget(now, 1.0, self.theme.motion.spec(MotionFamily::Press));
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::KeyUp {
                key: Key::Space, ..
            } if self.interactive() => {
                let activate = {
                    let state = ctx.state_mut::<ButtonState>()?;
                    let activate = state.keyboard_pressed;
                    state.keyboard_pressed = false;
                    state
                        .pressed
                        .retarget(now, 0.0, self.theme.motion.spec(MotionFamily::Release));
                    activate
                };
                if activate {
                    self.activate();
                }
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::KeyDown {
                key: Key::Enter,
                repeat: false,
                ..
            } if self.interactive() => {
                self.activate();
                ctx.set_handled();
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx<'_>) -> Semantics {
        Semantics {
            role: SemanticRole::Button,
            label: Some(self.label.clone()),
            value: self.pending.then(|| "pending".to_owned()),
            enabled: self.enabled && !self.pending,
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
