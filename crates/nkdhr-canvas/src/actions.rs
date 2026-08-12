//! Canvas implementations for the backend-neutral typed action language.

use std::sync::Arc;
use std::time::Duration;

use nkdhr_ui::{
    ActionDispatcher, ActionPhase, ActionRegistry, BindingContext, CompiledBinding, DispatchError,
    TerminalReason, ValidatedActionInvocation, built_in_compositor_catalog,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Size};

use crate::canvas::world::{Animation, Drag, ResizeEdge, Viewport, World};
use crate::state::App;

const KEYBOARD_PAN_TRANSITION: Duration = Duration::from_millis(140);
const KEYBOARD_WINDOW_TRANSITION: Duration = Duration::from_millis(120);
const PAN_STEP: f64 = 80.0;
const MIN_ZOOM: f64 = 0.1;
const MAX_ZOOM: f64 = 4.0;

pub type CanvasActionDispatcher = ActionDispatcher<App, CanvasActionPayload>;

#[derive(Debug, Clone)]
pub struct PointerTarget {
    pub surface: WlSurface,
    pub position: Point<f64, World>,
    pub size: Size<i32, Logical>,
}

#[derive(Debug, Clone)]
pub enum CanvasActionPayload {
    None,
    Group {
        size: Size<i32, Logical>,
        canvas_anchor: Point<f64, Logical>,
    },
    PointerStart {
        pointer: Point<f64, Logical>,
        target: Option<PointerTarget>,
        viewport: Viewport,
        resize_edge: Option<ResizeEdge>,
    },
    PointerPosition(Point<f64, Logical>),
    GestureStart {
        logical_center: Point<f64, Logical>,
        canvas_anchor: Point<f64, Logical>,
    },
    GesturePan {
        delta: Point<f64, Logical>,
    },
    GesturePinch {
        delta: Point<f64, Logical>,
        scale: f64,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct PinchState {
    world_anchor: Point<f64, World>,
    logical_center: Point<f64, Logical>,
    canvas_anchor: Point<f64, Logical>,
    start_zoom: f64,
}

pub fn dispatcher() -> CanvasActionDispatcher {
    let catalog = built_in_compositor_catalog();
    let mut registry = ActionRegistry::new(catalog.clone());
    for descriptor in catalog.descriptors() {
        registry
            .register_adapter(descriptor.id.as_str(), dispatch_to_canvas)
            .expect("every built-in compositor action has one canvas adapter");
    }
    ActionDispatcher::new(Arc::new(registry))
}

pub fn invoke_binding(
    app: &mut App,
    binding: &CompiledBinding,
    payload: CanvasActionPayload,
) -> bool {
    with_dispatcher(app, |dispatcher, app| {
        dispatcher.invoke(app, &binding.invocation, payload)
    })
    .map_or_else(log_dispatch_error, |_| true)
}

pub fn begin_pointer_binding(
    app: &mut App,
    binding: &CompiledBinding,
    payload: CanvasActionPayload,
) -> bool {
    let invocation = binding.invocation.clone();
    let result = with_dispatcher(app, |dispatcher, app| {
        dispatcher.begin(app, invocation, payload)
    });
    match result {
        Ok((id, _)) => {
            app.pointer_action = Some(id);
            true
        }
        Err(error) => log_dispatch_error(error),
    }
}

pub fn begin_gesture_binding(
    app: &mut App,
    binding: &CompiledBinding,
    payload: CanvasActionPayload,
) -> bool {
    let invocation = binding.invocation.clone();
    let result = with_dispatcher(app, |dispatcher, app| {
        dispatcher.begin(app, invocation, payload)
    });
    match result {
        Ok((id, _)) => {
            app.gesture_action = Some(id);
            true
        }
        Err(error) => log_dispatch_error(error),
    }
}

pub fn begin_named_pointer(app: &mut App, action: &str, payload: CanvasActionPayload) -> bool {
    let catalog = built_in_compositor_catalog();
    let invocation = match catalog.validate_invocation(&nkdhr_ui::ActionInvocation::new(action)) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("nkdhr-canvas: invalid built-in action invocation: {error}");
            return false;
        }
    };
    let result = with_dispatcher(app, |dispatcher, app| {
        dispatcher.begin(app, invocation, payload)
    });
    match result {
        Ok((id, _)) => {
            app.pointer_action = Some(id);
            true
        }
        Err(error) => log_dispatch_error(error),
    }
}

pub fn update_pointer(app: &mut App, payload: CanvasActionPayload) -> bool {
    let Some(id) = app.pointer_action else {
        return false;
    };
    with_dispatcher(app, |dispatcher, app| dispatcher.update(app, id, payload))
        .map_or_else(log_dispatch_error, |_| true)
}

pub fn update_gesture(app: &mut App, payload: CanvasActionPayload) -> bool {
    let Some(id) = app.gesture_action else {
        return false;
    };
    with_dispatcher(app, |dispatcher, app| dispatcher.update(app, id, payload))
        .map_or_else(log_dispatch_error, |_| true)
}

pub fn end_pointer(app: &mut App) -> bool {
    let Some(id) = app.pointer_action.take() else {
        return false;
    };
    with_dispatcher(app, |dispatcher, app| dispatcher.end(app, id))
        .map_or_else(log_dispatch_error, |_| true)
}

pub fn end_gesture(app: &mut App) -> bool {
    let Some(id) = app.gesture_action.take() else {
        return false;
    };
    with_dispatcher(app, |dispatcher, app| dispatcher.end(app, id))
        .map_or_else(log_dispatch_error, |_| true)
}

pub fn cancel(app: &mut App, reason: TerminalReason) {
    app.suppress_pointer_release |= app.pointer_action.is_some();
    app.suppress_gesture_remainder |= app.gesture_action.is_some();
    app.pointer_action = None;
    app.gesture_action = None;
    if let Err(error) = with_dispatcher(app, |dispatcher, app| dispatcher.cancel(app, reason)) {
        let _ = log_dispatch_error(error);
    }
}

pub fn sync_binding_generation(app: &mut App) {
    let generation = app
        .interaction_settings
        .lock()
        .unwrap()
        .binding_snapshot()
        .generation();
    if generation != app.binding_generation {
        cancel(app, TerminalReason::ConfigurationChanged);
        app.binding_generation = generation;
    }
}

fn with_dispatcher<R>(
    app: &mut App,
    operation: impl FnOnce(&mut CanvasActionDispatcher, &mut App) -> R,
) -> R {
    let mut dispatcher = app
        .action_dispatcher
        .take()
        .expect("the canvas action dispatcher is not re-entrant");
    let result = operation(&mut dispatcher, app);
    app.action_dispatcher = Some(dispatcher);
    result
}

fn log_dispatch_error(error: DispatchError) -> bool {
    eprintln!("nkdhr-canvas: typed action dispatch failed: {error:?}");
    false
}

fn dispatch_to_canvas(
    app: &mut App,
    invocation: &ValidatedActionInvocation,
    phase: ActionPhase<CanvasActionPayload>,
) -> Result<Option<String>, String> {
    match (invocation.action.as_str(), phase) {
        ("canvas.window.close", ActionPhase::Invoke(_)) => {
            crate::input::close_focused_window(app);
        }
        ("canvas.window.cycle-focus", ActionPhase::Invoke(_)) => {
            crate::input::cycle_focus(app);
        }
        (
            "canvas.overview.toggle",
            ActionPhase::Invoke(CanvasActionPayload::Group {
                size,
                canvas_anchor,
            }),
        ) => crate::input::toggle_overview(app, size, canvas_anchor),
        ("canvas.overview.exit", ActionPhase::Invoke(_)) => {
            crate::input::exit_overview(app, None);
        }
        ("canvas.viewport.pan-step", ActionPhase::Invoke(_)) => {
            pan_viewport_step(app, required_direction(invocation)?);
        }
        ("canvas.window.move-step", ActionPhase::Invoke(_)) => {
            move_focused_step(app, required_direction(invocation)?);
        }
        ("canvas.window.resize-step", ActionPhase::Invoke(_)) => {
            resize_focused_step(app, required_direction(invocation)?);
        }
        ("canvas.mark.jump", ActionPhase::Invoke(_)) => {
            crate::input::jump_to_mark(app, required_index(invocation)?);
        }
        ("canvas.mark.set", ActionPhase::Invoke(_)) => {
            crate::input::set_mark(app, required_index(invocation)?);
        }
        ("session.vt.switch", ActionPhase::Invoke(_)) => {
            let vt = invocation
                .integer("vt")
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| "validated VT argument is missing".to_owned())?;
            app.request_vt_switch(vt);
        }
        (
            "canvas.window.move",
            ActionPhase::Begin(CanvasActionPayload::PointerStart {
                pointer,
                target: Some(target),
                ..
            }),
        ) => {
            app.drag = Some(Drag::Move {
                surface: target.surface,
                window_start: target.position,
                pointer_start: pointer,
            });
        }
        (
            "canvas.window.resize",
            ActionPhase::Begin(CanvasActionPayload::PointerStart {
                pointer,
                target: Some(target),
                resize_edge,
                ..
            }),
        ) => {
            app.drag = Some(Drag::Resize {
                surface: target.surface,
                size_start: target.size,
                window_start: target.position,
                pointer_start: pointer,
                edge: resize_edge.unwrap_or(ResizeEdge::BottomRight),
            });
        }
        (
            "canvas.viewport.pan",
            ActionPhase::Begin(CanvasActionPayload::PointerStart {
                pointer,
                viewport,
                target: None,
                ..
            }),
        ) => {
            app.drag = Some(Drag::Pan {
                viewport_start: viewport.center,
                pointer_start: pointer,
                zoom: viewport.zoom,
            });
            app.active_view_mut().animation = None;
        }
        ("canvas.viewport.pan", ActionPhase::Begin(CanvasActionPayload::GestureStart { .. })) => {
            app.active_view_mut().animation = None
        }
        (
            "canvas.viewport.pinch",
            ActionPhase::Begin(CanvasActionPayload::GestureStart {
                logical_center,
                canvas_anchor,
            }),
        ) => {
            let viewport = app.active_view().viewport;
            app.active_view_mut().animation = None;
            app.pinch_state = Some(PinchState {
                world_anchor: viewport.group_logical_to_world(logical_center, canvas_anchor),
                logical_center,
                canvas_anchor,
                start_zoom: viewport.zoom,
            });
        }
        (
            "canvas.window.move" | "canvas.window.resize" | "canvas.viewport.pan",
            ActionPhase::Update(CanvasActionPayload::PointerPosition(pointer)),
        ) => {
            let drag = app
                .drag
                .clone()
                .ok_or_else(|| "pointer action has no operational drag state".to_owned())?;
            crate::input::apply_drag(app, &drag, pointer);
        }
        ("canvas.viewport.pan", ActionPhase::Update(CanvasActionPayload::GesturePan { delta })) => {
            let view = app.active_view_mut();
            view.viewport.center =
                crate::input::gesture_pan_center(view.viewport.center, delta, view.viewport.zoom);
        }
        (
            "canvas.viewport.pinch",
            ActionPhase::Update(CanvasActionPayload::GesturePinch { delta, scale }),
        ) => update_pinch(app, delta, scale)?,
        (
            "canvas.window.move" | "canvas.window.resize" | "canvas.viewport.pan",
            ActionPhase::End,
        ) => {
            if let Some(drag) = app.drag.take() {
                crate::input::finish_drag(app, &drag);
            } else {
                crate::input::animate_viewport_snap(app);
            }
        }
        ("canvas.viewport.pinch", ActionPhase::End) => {
            app.pinch_state = None;
            crate::input::animate_viewport_snap(app);
        }
        (
            "canvas.window.move"
            | "canvas.window.resize"
            | "canvas.viewport.pan"
            | "canvas.viewport.pinch",
            ActionPhase::Cancel(_),
        ) => {
            app.drag = None;
            app.pinch_state = None;
        }
        (action, _) => return Err(format!("action {action:?} received an incompatible phase")),
    }
    Ok(None)
}

fn required_direction(invocation: &ValidatedActionInvocation) -> Result<&str, String> {
    invocation
        .string("direction")
        .ok_or_else(|| "validated direction argument is missing".to_owned())
}

fn required_index(invocation: &ValidatedActionInvocation) -> Result<u8, String> {
    invocation
        .integer("index")
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| "validated mark index is missing".to_owned())
}

fn direction_delta(direction: &str, step: f64) -> Result<(f64, f64), String> {
    match direction {
        "left" => Ok((-step, 0.0)),
        "right" => Ok((step, 0.0)),
        "up" => Ok((0.0, -step)),
        "down" => Ok((0.0, step)),
        _ => Err(format!("unexpected validated direction {direction:?}")),
    }
}

fn pan_viewport_step(app: &mut App, direction: &str) {
    let (dx, dy) = direction_delta(direction, PAN_STEP).expect("direction is schema-validated");
    let grid = app.interaction_settings.lock().unwrap().grid;
    let view = app.active_view_mut();
    let base = view
        .animation
        .as_ref()
        .map(Animation::target)
        .unwrap_or(view.viewport);
    let target = crate::input::snapped_viewport(
        grid,
        Viewport {
            center: (base.center.x + dx, base.center.y + dy).into(),
            zoom: base.zoom,
        },
    );
    view.animation = Some(Animation::new(
        view.viewport,
        target,
        KEYBOARD_PAN_TRANSITION,
    ));
}

fn move_focused_step(app: &mut App, direction: &str) {
    let Some(surface) = crate::input::focused_surface(app) else {
        return;
    };
    let grid = app.interaction_settings.lock().unwrap().grid;
    let step = grid.size;
    let (dx, dy) = direction_delta(direction, step).expect("direction is schema-validated");
    let Some(position) = app
        .active_canvas()
        .windows()
        .iter()
        .find(|window| window.matches_surface(&surface))
        .map(|window| window.position)
    else {
        return;
    };
    let target = (grid.snap(position.x + dx), grid.snap(position.y + dy)).into();
    app.active_canvas_mut()
        .animate_position(&surface, target, KEYBOARD_WINDOW_TRANSITION);
}

fn resize_focused_step(app: &mut App, direction: &str) {
    let Some(surface) = crate::input::focused_surface(app) else {
        return;
    };
    let grid = app.interaction_settings.lock().unwrap().grid;
    let (dx, dy) = direction_delta(direction, grid.size).expect("direction is schema-validated");
    let Some(window) = app
        .active_canvas()
        .windows()
        .iter()
        .find(|window| window.matches_surface(&surface))
    else {
        return;
    };
    let size = window.size();
    let width = (size.w + dx).round().max(1.0) as i32;
    let height = (size.h + dy).round().max(1.0) as i32;
    window.request_size((width, height).into());
}

fn update_pinch(app: &mut App, delta: Point<f64, Logical>, scale: f64) -> Result<(), String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err("pinch scale must be finite and positive".to_owned());
    }
    let state = app
        .pinch_state
        .as_mut()
        .ok_or_else(|| "pinch action has no begin state".to_owned())?;
    state.logical_center += delta;
    let zoom = (state.start_zoom * scale).clamp(MIN_ZOOM, MAX_ZOOM);
    let center = (
        state.world_anchor.x - (state.logical_center.x - state.canvas_anchor.x) / zoom,
        state.world_anchor.y - (state.logical_center.y - state.canvas_anchor.y) / zoom,
    )
        .into();
    app.active_view_mut().viewport = Viewport { center, zoom };
    Ok(())
}

pub fn key_context(app: &App) -> BindingContext {
    if app.active_view().in_overview {
        BindingContext::Overview
    } else if crate::input::focused_surface(app).is_some() {
        BindingContext::Window
    } else {
        BindingContext::Canvas
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_vim_directions_match_arrow_directions() {
        assert_eq!(direction_delta("left", 32.0).unwrap(), (-32.0, 0.0));
        assert_eq!(direction_delta("down", 32.0).unwrap(), (0.0, 32.0));
        assert_eq!(direction_delta("up", 32.0).unwrap(), (0.0, -32.0));
        assert_eq!(direction_delta("right", 32.0).unwrap(), (32.0, 0.0));
    }
}
