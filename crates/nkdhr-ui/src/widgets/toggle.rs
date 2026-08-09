use std::{any::Any, rc::Rc, sync::Arc};

use nkdhr_render::{CornerRadii, Rect};

use crate::theme::with_alpha;
use crate::{
    Constraints, EventCtx, Invalidation, Key, MaterialCapabilities, MaterialTier, MeasureCtx,
    MotionFamily, PaintCtx, PointerButton, Reactive, ScalarMotion, SemanticRole, Semantics,
    SemanticsCtx, Size, Theme, ThemeReadSet, UiError, UiEvent, Widget,
};

use super::surface::{SurfaceState, paint_surface, surface_theme_reads};

pub struct Toggle {
    label: String,
    value: Reactive<bool>,
    effective_value: Option<Reactive<bool>>,
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    enabled: bool,
    pending: bool,
    on_change: Option<Rc<dyn Fn(bool)>>,
}

impl Toggle {
    pub fn new(label: impl Into<String>, value: Reactive<bool>, theme: Arc<Theme>) -> Self {
        Self {
            label: label.into(),
            value,
            effective_value: None,
            theme,
            capabilities: MaterialCapabilities::default(),
            enabled: true,
            pending: false,
            on_change: None,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn pending(mut self, pending: bool) -> Self {
        self.pending = pending;
        self
    }

    /// Supply the last backend-confirmed value. A difference from the
    /// requested `value` is presented as pending without moving the requested
    /// node away from its exact destination.
    pub fn effective_value(mut self, value: Reactive<bool>) -> Self {
        self.effective_value = Some(value);
        self
    }

    pub fn capabilities(mut self, capabilities: MaterialCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn on_change(mut self, callback: impl Fn(bool) + 'static) -> Self {
        self.on_change = Some(Rc::new(callback));
        self
    }

    fn interactive(&self) -> bool {
        self.enabled && !self.is_pending()
    }

    fn is_pending(&self) -> bool {
        self.pending
            || self
                .effective_value
                .as_ref()
                .is_some_and(|effective| effective.get() != self.value.get())
    }

    fn toggle(&self) {
        if !self.interactive() {
            return;
        }
        let next = !self.value.get();
        self.value.set(next);
        if let Some(callback) = &self.on_change {
            callback(next);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ToggleState {
    position: ScalarMotion,
    last_value: Option<bool>,
    hovered: ScalarMotion,
    pressed: ScalarMotion,
    focused: bool,
    pointer_pressed: bool,
    armed: bool,
    keyboard_pressed: bool,
}

impl Default for ToggleState {
    fn default() -> Self {
        Self {
            position: ScalarMotion::settled(0.0),
            last_value: None,
            hovered: ScalarMotion::settled(0.0),
            pressed: ScalarMotion::settled(0.0),
            focused: false,
            pointer_pressed: false,
            armed: false,
            keyboard_pressed: false,
        }
    }
}

impl Widget for Toggle {
    fn theme_reads(&self) -> ThemeReadSet {
        let mut reads = surface_theme_reads(MaterialTier::CompactNode);
        reads.extend([
            "density",
            "palette.accent",
            "palette.accent_secondary",
            "palette.text_secondary",
            "palette.edge",
            "motion.mode",
            "motion.speed_multiplier",
            "motion.standard",
            "motion.settle",
            "motion.exit",
            "motion.durations.toggle",
            "motion.durations.hover_in",
            "motion.durations.hover_out",
            "motion.durations.press",
            "motion.durations.release",
            "motion.fluid.toggle_stretch",
        ]);
        reads
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn create_state(&self) -> Box<dyn Any> {
        Box::<ToggleState>::default()
    }

    fn measure(
        &self,
        _ctx: &mut MeasureCtx<'_>,
        constraints: Constraints,
    ) -> Result<Size, UiError> {
        let metrics = self.theme.density_metrics();
        Ok(constraints.constrain(Size::new(
            metrics.toggle_width.max(36.0),
            metrics.control_height.max(36.0),
        )))
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let value = ctx.watch(&self.value, Invalidation::PAINT | Invalidation::SEMANTICS);
        let effective = self.effective_value.as_ref().map_or(value, |effective| {
            ctx.watch(effective, Invalidation::PAINT | Invalidation::SEMANTICS)
        });
        let pending = self.pending || effective != value;
        let now = ctx.now();
        let spatial = self.theme.motion.spatial_motion_enabled();
        let (position, hovered, pressed, focused, active) = {
            let state = ctx.state_mut::<ToggleState>()?;
            if state.last_value.is_none() {
                state.position.settle(if value { 1.0 } else { 0.0 });
                state.last_value = Some(value);
            } else if state.last_value != Some(value) {
                if spatial {
                    state.position.retarget(
                        now,
                        if value { 1.0 } else { 0.0 },
                        self.theme.motion.spec(MotionFamily::Toggle),
                    );
                } else {
                    state.position.settle(if value { 1.0 } else { 0.0 });
                }
                state.last_value = Some(value);
            }
            (
                state.position.value(now).clamp(0.0, 1.0),
                state.hovered.value(now),
                state.pressed.value(now),
                state.focused,
                state.position.is_active(now)
                    || state.hovered.is_active(now)
                    || state.pressed.is_active(now),
            )
        };
        if active {
            ctx.request_animation_frame();
        }

        let metrics = self.theme.density_metrics();
        let track = Rect::new(
            ctx.rect().x + (ctx.rect().width - metrics.toggle_width) * 0.5,
            ctx.rect().y + (ctx.rect().height - metrics.toggle_height) * 0.5,
            metrics.toggle_width,
            metrics.toggle_height,
        );
        paint_surface(
            ctx.builder(),
            track,
            CornerRadii::all(track.height * 0.5),
            &self.theme,
            MaterialTier::CompactNode,
            self.capabilities,
            SurfaceState {
                hovered,
                pressed,
                focused,
                disabled: !self.enabled,
                ..SurfaceState::default()
            },
        )?;

        let inset = 2.0;
        let node_size = (track.height - inset * 2.0).max(0.0);
        let left = track.x + inset + node_size * 0.5;
        let right = track.right() - inset - node_size * 0.5;
        let node_x = left + (right - left) * position;
        let node_rect = Rect::new(
            node_x - node_size * 0.5,
            track.y + inset,
            node_size,
            node_size,
        );

        let transition_active = position > 0.001 && position < 0.999 && spatial;
        if transition_active {
            let source_x = if value { left } else { right };
            let bridge_left = source_x.min(node_x);
            let bridge_width = (source_x - node_x).abs();
            let middle = (position * (1.0 - position) * 4.0).clamp(0.0, 1.0);
            let thickness =
                node_size * 0.42 + middle * self.theme.motion.fluid.toggle_stretch * 0.18;
            ctx.builder().rounded_rect(
                Rect::new(
                    bridge_left,
                    track.y + track.height * 0.5 - thickness * 0.5,
                    bridge_width,
                    thickness,
                ),
                CornerRadii::all(thickness * 0.5),
                with_alpha(self.theme.palette.accent, 0.62),
            )?;
        }

        let node_color = if value || transition_active {
            self.theme.palette.accent
        } else {
            self.theme.palette.text_secondary
        };
        ctx.builder().rounded_rect(
            node_rect,
            CornerRadii::all(node_size * 0.5),
            with_alpha(node_color, if self.enabled { 0.96 } else { 0.52 }),
        )?;
        ctx.builder().border(
            node_rect,
            CornerRadii::all(node_size * 0.5),
            1.0,
            with_alpha(self.theme.palette.edge, 0.34),
        )?;
        if pending {
            let width = (track.width * 0.22).clamp(5.0, 9.0).min(track.width);
            let travel = (track.width - width).max(0.0);
            let progress = if spatial {
                let phase = (now.as_secs_f64() % 0.8) / 0.8;
                if phase <= 0.5 {
                    (phase * 2.0) as f32
                } else {
                    ((1.0 - phase) * 2.0) as f32
                }
            } else {
                0.5
            };
            ctx.builder().rounded_rect(
                Rect::new(track.x + travel * progress, track.y - 1.0, width, 2.0),
                CornerRadii::all(1.0),
                with_alpha(self.theme.palette.accent_secondary, 0.92),
            )?;
            if spatial {
                ctx.request_animation_frame();
            }
        }
        Ok(())
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        let now = ctx.now();
        match event {
            UiEvent::PointerDown {
                button: PointerButton::Primary,
                ..
            } if self.enabled && self.is_pending() => {
                ctx.set_handled();
            }
            UiEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } if self.enabled && self.is_pending() => {
                let state = ctx.state_mut::<ToggleState>()?;
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
            } if self.enabled && self.is_pending() => {
                ctx.set_handled();
            }
            UiEvent::HoverChanged(hovered) => {
                let state = ctx.state_mut::<ToggleState>()?;
                state.hovered.retarget(
                    now,
                    if *hovered { 1.0 } else { 0.0 },
                    self.theme.motion.spec(if *hovered {
                        MotionFamily::HoverIn
                    } else {
                        MotionFamily::HoverOut
                    }),
                );
                if state.pointer_pressed {
                    state.armed = *hovered;
                }
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::FocusChanged(focused) => {
                let state = ctx.state_mut::<ToggleState>()?;
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
                let state = ctx.state_mut::<ToggleState>()?;
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
                let state = ctx.state_mut::<ToggleState>()?;
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
                    let state = ctx.state_mut::<ToggleState>()?;
                    let activate = state.pointer_pressed && state.armed && rect.contains(*position);
                    state.pointer_pressed = false;
                    state.armed = false;
                    state
                        .pressed
                        .retarget(now, 0.0, self.theme.motion.spec(MotionFamily::Release));
                    activate
                };
                if activate {
                    self.toggle();
                }
                ctx.release_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::PointerCancel => {
                let state = ctx.state_mut::<ToggleState>()?;
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
                let state = ctx.state_mut::<ToggleState>()?;
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
                    let state = ctx.state_mut::<ToggleState>()?;
                    let activate = state.keyboard_pressed;
                    state.keyboard_pressed = false;
                    state
                        .pressed
                        .retarget(now, 0.0, self.theme.motion.spec(MotionFamily::Release));
                    activate
                };
                if activate {
                    self.toggle();
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
                self.toggle();
                ctx.set_handled();
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let value = ctx.watch(&self.value, Invalidation::SEMANTICS);
        let effective = self
            .effective_value
            .as_ref()
            .map(|effective| ctx.watch(effective, Invalidation::SEMANTICS));
        let pending = self.pending || effective.is_some_and(|effective| effective != value);
        let requested = if value { "on" } else { "off" };
        Semantics {
            role: SemanticRole::Toggle,
            label: Some(self.label.clone()),
            value: Some(if pending {
                format!(
                    "{requested}; pending; effective {}",
                    if effective.unwrap_or(value) {
                        "on"
                    } else {
                        "off"
                    }
                )
            } else {
                requested.to_owned()
            }),
            enabled: self.enabled && !pending,
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
