use std::{any::Any, fmt, rc::Rc, sync::Arc};

use nkdhr_render::{CornerRadii, Rect};

use crate::theme::with_alpha;
use crate::{
    Constraints, EventCtx, Invalidation, Key, MaterialCapabilities, MaterialTier, MeasureCtx,
    Modifiers, MotionFamily, PaintCtx, PointerButton, Reactive, ScalarMotion, SemanticRole,
    Semantics, SemanticsCtx, Size, Theme, UiError, UiEvent, Widget,
};

use super::surface::{SurfaceState, paint_surface};

pub struct Slider {
    label: String,
    value: Reactive<f32>,
    effective_value: Option<Reactive<f32>>,
    minimum: f32,
    maximum: f32,
    step: Option<f32>,
    ideal_width: f32,
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    enabled: bool,
    on_change: Option<Rc<dyn Fn(f32)>>,
}

impl Slider {
    pub fn new(
        label: impl Into<String>,
        value: Reactive<f32>,
        minimum: f32,
        maximum: f32,
        theme: Arc<Theme>,
    ) -> Result<Self, SliderError> {
        if !minimum.is_finite() || !maximum.is_finite() || minimum >= maximum {
            return Err(SliderError::InvalidRange);
        }
        Ok(Self {
            label: label.into(),
            value,
            effective_value: None,
            minimum,
            maximum,
            step: None,
            ideal_width: 180.0,
            theme,
            capabilities: MaterialCapabilities::default(),
            enabled: true,
            on_change: None,
        })
    }

    pub fn step(mut self, step: f32) -> Result<Self, SliderError> {
        if !step.is_finite() || step <= 0.0 {
            return Err(SliderError::InvalidStep);
        }
        self.step = Some(step);
        Ok(self)
    }

    pub fn effective_value(mut self, value: Reactive<f32>) -> Self {
        self.effective_value = Some(value);
        self
    }

    pub fn ideal_width(mut self, width: f32) -> Result<Self, SliderError> {
        if !width.is_finite() || width <= 0.0 {
            return Err(SliderError::InvalidWidth);
        }
        self.ideal_width = width;
        Ok(self)
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn capabilities(mut self, capabilities: MaterialCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn on_change(mut self, callback: impl Fn(f32) + 'static) -> Self {
        self.on_change = Some(Rc::new(callback));
        self
    }

    fn normalize(&self, value: f32) -> f32 {
        ((value - self.minimum) / (self.maximum - self.minimum)).clamp(0.0, 1.0)
    }

    fn quantize(&self, value: f32) -> f32 {
        let value = value.clamp(self.minimum, self.maximum);
        self.step.map_or(value, |step| {
            let steps = ((value - self.minimum) / step).round();
            (self.minimum + steps * step).clamp(self.minimum, self.maximum)
        })
    }

    fn apply(&self, value: f32) {
        if !self.enabled {
            return;
        }
        let value = self.quantize(value);
        if self.value.set_if_changed(value)
            && let Some(callback) = &self.on_change
        {
            callback(value);
        }
    }

    fn apply_pointer(&self, rect: Rect, position_x: f32) {
        let metrics = self.theme.density_metrics();
        let inset = metrics.slider_node * 0.5;
        let width = (rect.width - inset * 2.0).max(1.0);
        let progress = ((position_x - rect.x - inset) / width).clamp(0.0, 1.0);
        self.apply(self.minimum + (self.maximum - self.minimum) * progress);
    }

    fn keyboard_delta(&self, modifiers: Modifiers) -> f32 {
        let base = self.step.unwrap_or((self.maximum - self.minimum) / 100.0);
        if modifiers.shift {
            base * 10.0
        } else if modifiers.control {
            base * 0.1
        } else {
            base
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SliderState {
    hovered: ScalarMotion,
    focused: bool,
    dragging: bool,
    trail: ScalarMotion,
    last_progress: Option<f32>,
}

impl Default for SliderState {
    fn default() -> Self {
        Self {
            hovered: ScalarMotion::settled(0.0),
            focused: false,
            dragging: false,
            trail: ScalarMotion::settled(0.0),
            last_progress: None,
        }
    }
}

impl Widget for Slider {
    fn create_state(&self) -> Box<dyn Any> {
        Box::<SliderState>::default()
    }

    fn measure(
        &self,
        _ctx: &mut MeasureCtx<'_>,
        constraints: Constraints,
    ) -> Result<Size, UiError> {
        Ok(constraints.constrain(Size::new(
            self.ideal_width,
            self.theme.density_metrics().control_height.max(40.0),
        )))
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let value =
            self.quantize(ctx.watch(&self.value, Invalidation::PAINT | Invalidation::SEMANTICS));
        let effective = self.effective_value.as_ref().map_or(value, |effective| {
            self.quantize(ctx.watch(effective, Invalidation::PAINT | Invalidation::SEMANTICS))
        });
        let progress = self.normalize(value);
        let effective_progress = self.normalize(effective);
        let now = ctx.now();
        let (trail, hovered, focused, dragging, active) = {
            let state = ctx.state_mut::<SliderState>()?;
            if state.last_progress.is_none() {
                state.trail.settle(progress);
                state.last_progress = Some(progress);
            } else if state.last_progress != Some(progress) {
                state.trail.retarget(
                    now,
                    progress,
                    self.theme.motion.spec(MotionFamily::SliderTrail),
                );
                state.last_progress = Some(progress);
            }
            (
                state.trail.value(now).clamp(0.0, 1.0),
                state.hovered.value(now),
                state.focused,
                state.dragging,
                state.trail.is_active(now) || state.hovered.is_active(now),
            )
        };
        if active {
            ctx.request_animation_frame();
        }

        let metrics = self.theme.density_metrics();
        let node_size = metrics.slider_node;
        let track_height = metrics.slider_track + hovered * 1.0;
        let inset = node_size * 0.5;
        let track = Rect::new(
            ctx.rect().x + inset,
            ctx.rect().y + (ctx.rect().height - track_height) * 0.5,
            (ctx.rect().width - node_size).max(0.0),
            track_height,
        );
        let material = self
            .theme
            .resolve_material(MaterialTier::CompactNode, self.capabilities);
        ctx.builder()
            .rounded_rect(track, CornerRadii::all(track.height * 0.5), material.fill)?;
        ctx.builder().border(
            track,
            CornerRadii::all(track.height * 0.5),
            1.0,
            material.edge,
        )?;

        let fill_width = track.width * progress;
        if fill_width > 0.0 {
            ctx.builder().rounded_rect(
                Rect::new(track.x, track.y, fill_width, track.height),
                CornerRadii::all(track.height * 0.5),
                with_alpha(
                    self.theme.palette.accent,
                    if self.enabled { 0.90 } else { 0.42 },
                ),
            )?;
        }

        if (trail - progress).abs() > 0.001 {
            let trail_x = track.x + track.width * trail;
            let exact_x = track.x + track.width * progress;
            let left = trail_x.min(exact_x);
            ctx.builder().rounded_rect(
                Rect::new(left, track.y, (trail_x - exact_x).abs(), track.height),
                CornerRadii::all(track.height * 0.5),
                with_alpha(self.theme.palette.accent_secondary, 0.38),
            )?;
        }

        if (effective_progress - progress).abs() > f32::EPSILON {
            let effective_x = track.x + track.width * effective_progress;
            ctx.builder().rounded_rect(
                Rect::new(effective_x - 2.0, track.y - 3.0, 4.0, track.height + 6.0),
                CornerRadii::all(2.0),
                with_alpha(self.theme.palette.warning, 0.88),
            )?;
        }

        let node_x = track.x + track.width * progress;
        let node = Rect::new(
            node_x - node_size * 0.5,
            ctx.rect().y + (ctx.rect().height - node_size) * 0.5,
            node_size,
            node_size,
        );
        paint_surface(
            ctx.builder(),
            node,
            CornerRadii::all(node_size * 0.5),
            &self.theme,
            MaterialTier::CompactNode,
            self.capabilities,
            SurfaceState {
                hovered,
                pressed: if dragging { 1.0 } else { 0.0 },
                focused,
                accented: true,
                disabled: !self.enabled,
                ..SurfaceState::default()
            },
        )?;
        Ok(())
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        let now = ctx.now();
        match event {
            UiEvent::HoverChanged(hovered) => {
                ctx.state_mut::<SliderState>()?.hovered.retarget(
                    now,
                    if *hovered { 1.0 } else { 0.0 },
                    self.theme.motion.spec(if *hovered {
                        MotionFamily::HoverIn
                    } else {
                        MotionFamily::HoverOut
                    }),
                );
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::FocusChanged(focused) => {
                ctx.state_mut::<SliderState>()?.focused = *focused;
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerDown {
                position,
                button: PointerButton::Primary,
                ..
            } if self.enabled => {
                ctx.state_mut::<SliderState>()?.dragging = true;
                self.apply_pointer(ctx.rect(), position.x);
                ctx.request_focus();
                ctx.capture_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerMoved { position } if self.enabled => {
                if ctx.state_mut::<SliderState>()?.dragging {
                    self.apply_pointer(ctx.rect(), position.x);
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::PointerUp {
                position,
                button: PointerButton::Primary,
                ..
            } if self.enabled => {
                let dragging = ctx.state_mut::<SliderState>()?.dragging;
                if dragging {
                    self.apply_pointer(ctx.rect(), position.x);
                    ctx.state_mut::<SliderState>()?.dragging = false;
                    ctx.release_pointer();
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::PointerCancel => {
                ctx.state_mut::<SliderState>()?.dragging = false;
                ctx.release_pointer();
                ctx.invalidate(Invalidation::PAINT);
            }
            UiEvent::KeyDown { key, modifiers, .. } if self.enabled => {
                let current = self.value.get();
                let delta = self.keyboard_delta(*modifiers);
                let next = match key {
                    Key::ArrowLeft | Key::ArrowDown => Some(current - delta),
                    Key::ArrowRight | Key::ArrowUp => Some(current + delta),
                    Key::Home => Some(self.minimum),
                    Key::End => Some(self.maximum),
                    Key::PageDown => Some(current - delta * 10.0),
                    Key::PageUp => Some(current + delta * 10.0),
                    _ => None,
                };
                if let Some(next) = next {
                    self.apply(next);
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let value = self.quantize(ctx.watch(&self.value, Invalidation::SEMANTICS));
        Semantics {
            role: SemanticRole::Slider,
            label: Some(self.label.clone()),
            value: Some(format!("{value}")),
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
pub enum SliderError {
    InvalidRange,
    InvalidStep,
    InvalidWidth,
}

impl fmt::Display for SliderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidRange => "slider bounds must be finite and minimum must be below maximum",
            Self::InvalidStep => "slider step must be finite and positive",
            Self::InvalidWidth => "slider width must be finite and positive",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SliderError {}
