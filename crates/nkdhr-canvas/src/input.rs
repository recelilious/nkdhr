use std::time::{Duration, Instant};

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Device, DeviceCapability, Event,
    GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent, GestureSwipeUpdateEvent,
    InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    PointerMotionEvent, TouchEvent,
};
use smithay::input::keyboard::{FilterResult, KeyboardHandle, Keysym, ModifiersState, xkb};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GesturePinchBeginEvent as PointerPinchBeginEvent,
    GesturePinchEndEvent as PointerPinchEndEvent,
    GesturePinchUpdateEvent as PointerPinchUpdateEvent,
    GestureSwipeBeginEvent as PointerSwipeBeginEvent, GestureSwipeEndEvent as PointerSwipeEndEvent,
    GestureSwipeUpdateEvent as PointerSwipeUpdateEvent, MotionEvent, PointerHandle,
    RelativeMotionEvent,
};
use smithay::input::touch::{
    DownEvent as TouchDownEvent, MotionEvent as TouchMotionEvent, UpEvent as TouchUpEvent,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size};
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};
use smithay::wayland::seat::WaylandFocus;

use crate::canvas::marks;
use crate::canvas::output_group::OutputLayout;
use crate::canvas::placement::PlacementDirection;
use crate::canvas::world::{Animation, Drag, ManagedWindow, ResizeEdge, Viewport, World};
use crate::settings::GridSettings;
use crate::state::{App, KeyboardFocusTarget};
use crate::widget_host::{InputHandled, PinnedLayer, PinnedPointerEvent};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const TRANSITION: Duration = Duration::from_millis(250);
const WINDOW_SNAP_TRANSITION: Duration = Duration::from_millis(120);
const VIEWPORT_SNAP_TRANSITION: Duration = Duration::from_millis(120);
type RelativeMotion = (Point<f64, Logical>, Point<f64, Logical>, u64);

#[derive(Clone)]
struct InputGroup {
    origin: Point<i32, Logical>,
    size: Size<i32, Logical>,
    canvas_anchor: Point<f64, Logical>,
    display_rect: Rectangle<i32, Logical>,
    output_name: String,
    output_global_location: Point<i32, Logical>,
}

/// Backend-independent compositor input dispatch. Backend code supplies the
/// resolved output layout; this layer maps compositor-global pointer
/// coordinates into the active output group's independent canvas view.
pub fn handle<B: InputBackend>(app: &mut App, layout: &OutputLayout, event: InputEvent<B>) {
    if app.session_locked() {
        handle_session_lock_input(app, layout, event);
        return;
    }

    crate::actions::sync_binding_generation(app);

    let keyboard = app
        .seat
        .get_keyboard()
        .expect("App::new always creates a keyboard");
    let pointer = app
        .seat
        .get_pointer()
        .expect("App::new always creates a pointer");
    let touch = app
        .seat
        .get_touch()
        .expect("App::new always creates touch input");
    let extent = layout.logical_extent();

    match event {
        InputEvent::DeviceRemoved { .. } => {
            crate::actions::cancel(app, nkdhr_ui::TerminalReason::DeviceRemoved);
            app.cancel_placement();
        }
        InputEvent::Keyboard { event } => {
            if let Some(group) = active_group(app, layout) {
                handle_keyboard::<B>(app, &keyboard, group, event);
            }
        }
        InputEvent::PointerMotion { event } => {
            let current = pointer.current_location();
            let delta = event.delta();
            let pointer_pos = (
                (current.x + delta.x).clamp(0.0, f64::from(extent.w.max(1)) - 1.0),
                (current.y + delta.y).clamp(0.0, f64::from(extent.h.max(1)) - 1.0),
            )
                .into();
            if !has_active_pointer_constraint(&pointer) {
                activate_group_at(app, layout, pointer_pos);
            }
            if let Some(group) = active_group(app, layout) {
                handle_pointer_motion(
                    app,
                    &pointer,
                    &group,
                    pointer_pos,
                    event.time_msec(),
                    Some((event.delta(), event.delta_unaccel(), event.time())),
                );
            }
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let pointer_pos = (event.x_transformed(extent.w), event.y_transformed(extent.h)).into();
            if !has_active_pointer_constraint(&pointer) {
                activate_group_at(app, layout, pointer_pos);
            }
            if let Some(group) = active_group(app, layout) {
                handle_pointer_motion(app, &pointer, &group, pointer_pos, event.time_msec(), None);
            }
        }
        InputEvent::PointerButton { event } => {
            activate_group_at(app, layout, pointer.current_location());
            if let Some(group) = active_group(app, layout) {
                handle_pointer_button::<B>(app, &keyboard, &pointer, &group, &event);
            }
        }
        InputEvent::PointerAxis { event } => {
            if let Some(group) = active_group(app, layout) {
                handle_pointer_axis::<B>(app, &keyboard, &pointer, &group, &event);
            }
        }
        InputEvent::GestureSwipeBegin { event } => {
            app.suppress_gesture_remainder = false;
            activate_group_at(app, layout, pointer.current_location());
            let binding = active_group(app, layout).and_then(|group| {
                let origin = gesture_origin_at(app, &group, pointer.current_location());
                find_binding(
                    app,
                    binding_context_for_origin(app, origin),
                    &nkdhr_ui::RuntimeTrigger::Gesture {
                        gesture: nkdhr_ui::GestureKind::Swipe,
                        device: nkdhr_ui::DeviceClass::Touchpad,
                        fingers: u8::try_from(event.fingers()).unwrap_or(u8::MAX),
                        origin,
                        direction: None,
                        activation: nkdhr_ui::GestureActivation::Begin,
                    },
                )
                .map(|binding| (group, binding))
            });
            let handled = binding.is_some_and(|(group, binding)| {
                crate::actions::begin_gesture_binding(
                    app,
                    &binding,
                    crate::actions::CanvasActionPayload::GestureStart {
                        logical_center: local_point(&group, pointer.current_location()),
                        canvas_anchor: group.canvas_anchor,
                    },
                )
            });
            if !handled {
                pointer.gesture_swipe_begin(
                    app,
                    &PointerSwipeBeginEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
            }
        }
        InputEvent::GestureSwipeUpdate { event } => {
            if app.suppress_gesture_remainder {
                return;
            }
            let compositor_owned = app.gesture_action.is_some();
            if compositor_owned {
                crate::actions::update_gesture(
                    app,
                    crate::actions::CanvasActionPayload::GesturePan {
                        delta: event.delta(),
                    },
                );
            } else {
                pointer.gesture_swipe_update(
                    app,
                    &PointerSwipeUpdateEvent {
                        time: event.time_msec(),
                        delta: event.delta(),
                    },
                );
            }
        }
        InputEvent::GestureSwipeEnd { event } => {
            if std::mem::take(&mut app.suppress_gesture_remainder) {
                return;
            }
            if app.gesture_action.is_some() {
                if event.cancelled() {
                    crate::actions::cancel(app, nkdhr_ui::TerminalReason::CancelledByInput);
                } else {
                    crate::actions::end_gesture(app);
                }
            } else {
                pointer.gesture_swipe_end(
                    app,
                    &PointerSwipeEndEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
        }
        InputEvent::GesturePinchBegin { event } => {
            app.suppress_gesture_remainder = false;
            activate_group_at(app, layout, pointer.current_location());
            let binding = active_group(app, layout).and_then(|group| {
                let origin = gesture_origin_at(app, &group, pointer.current_location());
                find_binding(
                    app,
                    binding_context_for_origin(app, origin),
                    &nkdhr_ui::RuntimeTrigger::Gesture {
                        gesture: nkdhr_ui::GestureKind::Pinch,
                        device: nkdhr_ui::DeviceClass::Touchpad,
                        fingers: u8::try_from(event.fingers()).unwrap_or(u8::MAX),
                        origin,
                        direction: None,
                        activation: nkdhr_ui::GestureActivation::Begin,
                    },
                )
                .map(|binding| (group, binding))
            });
            let handled = binding.is_some_and(|(group, binding)| {
                crate::actions::begin_gesture_binding(
                    app,
                    &binding,
                    crate::actions::CanvasActionPayload::GestureStart {
                        logical_center: local_point(&group, pointer.current_location()),
                        canvas_anchor: group.canvas_anchor,
                    },
                )
            });
            if !handled {
                pointer.gesture_pinch_begin(
                    app,
                    &PointerPinchBeginEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
            }
        }
        InputEvent::GesturePinchUpdate { event } => {
            if app.suppress_gesture_remainder {
                return;
            }
            let compositor_owned = app.gesture_action.is_some();
            if compositor_owned {
                crate::actions::update_gesture(
                    app,
                    crate::actions::CanvasActionPayload::GesturePinch {
                        delta: event.delta(),
                        scale: event.scale(),
                    },
                );
            } else {
                pointer.gesture_pinch_update(
                    app,
                    &PointerPinchUpdateEvent {
                        time: event.time_msec(),
                        delta: event.delta(),
                        scale: event.scale(),
                        rotation: event.rotation(),
                    },
                );
            }
        }
        InputEvent::GesturePinchEnd { event } => {
            if std::mem::take(&mut app.suppress_gesture_remainder) {
                return;
            }
            if app.gesture_action.is_some() {
                if event.cancelled() {
                    crate::actions::cancel(app, nkdhr_ui::TerminalReason::CancelledByInput);
                } else {
                    crate::actions::end_gesture(app);
                }
            } else {
                pointer.gesture_pinch_end(
                    app,
                    &PointerPinchEndEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
        }
        InputEvent::TouchDown { event } => {
            let location = event.position_transformed(extent);
            activate_group_at(app, layout, location);
            let focus =
                active_group(app, layout).and_then(|group| pointer_focus_at(app, &group, location));
            touch.down(
                app,
                focus,
                &TouchDownEvent {
                    slot: event.slot(),
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchMotion { event } => {
            let location = event.position_transformed(extent);
            let focus =
                active_group(app, layout).and_then(|group| pointer_focus_at(app, &group, location));
            touch.motion(
                app,
                focus,
                &TouchMotionEvent {
                    slot: event.slot(),
                    location,
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchUp { event } => {
            touch.up(
                app,
                &TouchUpEvent {
                    slot: event.slot(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchFrame { .. } => touch.frame(app),
        InputEvent::TouchCancel { .. } => touch.cancel(app),
        _ => {}
    }
}

/// Fail-closed input path used while ext-session-lock protects the session.
/// No compositor binding or normal application surface is reachable here.
fn handle_session_lock_input<B: InputBackend>(
    app: &mut App,
    layout: &OutputLayout,
    event: InputEvent<B>,
) {
    let keyboard = app
        .seat
        .get_keyboard()
        .expect("App::new always creates a keyboard");
    let pointer = app
        .seat
        .get_pointer()
        .expect("App::new always creates a pointer");
    let touch = app
        .seat
        .get_touch()
        .expect("App::new always creates touch input");
    let extent = layout.logical_extent();

    match event {
        InputEvent::Keyboard { event } => {
            let key_state = event.state();
            keyboard.input::<(), _>(
                app,
                event.key_code(),
                key_state,
                SERIAL_COUNTER.next_serial(),
                event.time_msec(),
                move |app, modifiers, keysym| {
                    let raw_syms = keysym.raw_syms();
                    if handle_vt_switch(
                        app,
                        modifiers,
                        keysym.modified_sym(),
                        &raw_syms,
                        key_state == KeyState::Pressed,
                    ) {
                        FilterResult::Intercept(())
                    } else {
                        FilterResult::Forward
                    }
                },
            );
        }
        InputEvent::PointerMotion { event } => {
            let current = pointer.current_location();
            let delta = event.delta();
            let location = (
                (current.x + delta.x).clamp(0.0, f64::from(extent.w.max(1)) - 1.0),
                (current.y + delta.y).clamp(0.0, f64::from(extent.h.max(1)) - 1.0),
            )
                .into();
            pointer.motion(
                app,
                lock_pointer_focus(app, layout, location),
                &MotionEvent {
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
            pointer.frame(app);
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let location = (event.x_transformed(extent.w), event.y_transformed(extent.h)).into();
            pointer.motion(
                app,
                lock_pointer_focus(app, layout, location),
                &MotionEvent {
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
            pointer.frame(app);
        }
        InputEvent::PointerButton { event } => {
            if event.state() == ButtonState::Pressed
                && let Some(surface) = pointer.current_focus()
            {
                keyboard.set_focus(
                    app,
                    Some(KeyboardFocusTarget::Wayland(surface)),
                    SERIAL_COUNTER.next_serial(),
                );
            }
            pointer.button(
                app,
                &ButtonEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    button: event.button_code(),
                    state: event.state(),
                },
            );
            pointer.frame(app);
        }
        InputEvent::PointerAxis { event } => {
            pointer.axis(app, axis_frame::<B>(&event));
            pointer.frame(app);
        }
        InputEvent::GestureSwipeBegin { event } => {
            pointer.gesture_swipe_begin(
                app,
                &PointerSwipeBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                },
            );
        }
        InputEvent::GestureSwipeUpdate { event } => {
            pointer.gesture_swipe_update(
                app,
                &PointerSwipeUpdateEvent {
                    time: event.time_msec(),
                    delta: event.delta(),
                },
            );
        }
        InputEvent::GestureSwipeEnd { event } => {
            pointer.gesture_swipe_end(
                app,
                &PointerSwipeEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                },
            );
        }
        InputEvent::GesturePinchBegin { event } => {
            pointer.gesture_pinch_begin(
                app,
                &PointerPinchBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                },
            );
        }
        InputEvent::GesturePinchUpdate { event } => {
            pointer.gesture_pinch_update(
                app,
                &PointerPinchUpdateEvent {
                    time: event.time_msec(),
                    delta: event.delta(),
                    scale: event.scale(),
                    rotation: event.rotation(),
                },
            );
        }
        InputEvent::GesturePinchEnd { event } => {
            pointer.gesture_pinch_end(
                app,
                &PointerPinchEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                },
            );
        }
        InputEvent::TouchDown { event } => {
            let location = event.position_transformed(extent);
            touch.down(
                app,
                lock_pointer_focus(app, layout, location),
                &TouchDownEvent {
                    slot: event.slot(),
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchMotion { event } => {
            let location = event.position_transformed(extent);
            touch.motion(
                app,
                lock_pointer_focus(app, layout, location),
                &TouchMotionEvent {
                    slot: event.slot(),
                    location,
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchUp { event } => {
            touch.up(
                app,
                &TouchUpEvent {
                    slot: event.slot(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchFrame { .. } => touch.frame(app),
        InputEvent::TouchCancel { .. } => touch.cancel(app),
        _ => {}
    }
}

fn lock_pointer_focus(
    app: &App,
    layout: &OutputLayout,
    location: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    let output = layout
        .groups
        .iter()
        .flat_map(|group| &group.outputs)
        .find(|output| {
            let origin = output.global_location.to_f64();
            location.x >= origin.x
                && location.y >= origin.y
                && location.x < origin.x + f64::from(output.logical_size.w)
                && location.y < origin.y + f64::from(output.logical_size.h)
        })?;
    Some((
        app.lock_surface_for_output(&output.name)?,
        output.global_location.to_f64(),
    ))
}

fn active_group(app: &App, layout: &OutputLayout) -> Option<InputGroup> {
    layout
        .groups
        .iter()
        .find(|group| group.name == app.active_group)
        .or_else(|| layout.groups.first())
        .map(|group| {
            let pointer = app.seat.get_pointer().map_or(
                group.canvas_anchor + group.global_location.to_f64(),
                |pointer| pointer.current_location(),
            );
            let display = group
                .outputs
                .iter()
                .find(|output| {
                    let origin = output.global_location.to_f64();
                    pointer.x >= origin.x
                        && pointer.y >= origin.y
                        && pointer.x < origin.x + f64::from(output.logical_size.w)
                        && pointer.y < origin.y + f64::from(output.logical_size.h)
                })
                .or_else(|| group.outputs.first());
            let display_rect = display.map_or_else(
                || Rectangle::new((0, 0).into(), group.logical_size),
                |output| Rectangle::new(output.group_location, output.logical_size),
            );
            InputGroup {
                origin: group.global_location,
                size: group.logical_size,
                canvas_anchor: group.canvas_anchor,
                display_rect,
                output_name: display.map_or_else(String::new, |output| output.name.clone()),
                output_global_location: display
                    .map_or(group.global_location, |output| output.global_location),
            }
        })
}

fn activate_group_at(app: &mut App, layout: &OutputLayout, point: Point<f64, Logical>) {
    if app.drag.is_some() || app.placement.is_some() {
        return;
    }
    if let Some(group) = layout.group_at(point) {
        app.activate_group(&group.name);
    }
}

fn has_active_pointer_constraint(pointer: &PointerHandle<App>) -> bool {
    pointer.current_focus().is_some_and(|surface| {
        with_pointer_constraint(&surface, pointer, |constraint| {
            constraint.is_some_and(|constraint| constraint.is_active())
        })
    })
}

fn local_point(group: &InputGroup, global: Point<f64, Logical>) -> Point<f64, Logical> {
    global - group.origin.to_f64()
}

fn find_binding(
    app: &App,
    context: nkdhr_ui::BindingContext,
    trigger: &nkdhr_ui::RuntimeTrigger,
) -> Option<nkdhr_ui::CompiledBinding> {
    app.interaction_settings
        .lock()
        .unwrap()
        .binding_snapshot()
        .find(context, trigger)
        .cloned()
}

fn binding_context_for_origin(
    app: &App,
    origin: nkdhr_ui::GestureOrigin,
) -> nkdhr_ui::BindingContext {
    if app.active_view().in_overview {
        return nkdhr_ui::BindingContext::Overview;
    }
    match origin {
        nkdhr_ui::GestureOrigin::Window => nkdhr_ui::BindingContext::Window,
        nkdhr_ui::GestureOrigin::WindowFrame => nkdhr_ui::BindingContext::WindowFrame,
        nkdhr_ui::GestureOrigin::EmptyCanvas => nkdhr_ui::BindingContext::EmptyCanvas,
        nkdhr_ui::GestureOrigin::Edge | nkdhr_ui::GestureOrigin::Anywhere => {
            nkdhr_ui::BindingContext::Canvas
        }
    }
}

fn gesture_origin_at(
    app: &App,
    group: &InputGroup,
    pointer_pos: Point<f64, Logical>,
) -> nkdhr_ui::GestureOrigin {
    const EDGE_WIDTH: f64 = 16.0;
    let local = local_point(group, pointer_pos);
    if local.x < EDGE_WIDTH
        || local.y < EDGE_WIDTH
        || local.x >= f64::from(group.size.w) - EDGE_WIDTH
        || local.y >= f64::from(group.size.h) - EDGE_WIDTH
    {
        return nkdhr_ui::GestureOrigin::Edge;
    }
    let viewport = app.active_view().viewport;
    let world_pos = viewport.group_logical_to_world(local, group.canvas_anchor);
    app.active_canvas().window_at(world_pos).map_or(
        nkdhr_ui::GestureOrigin::EmptyCanvas,
        |window| {
            if window.content_rect().contains(world_pos) {
                nkdhr_ui::GestureOrigin::Window
            } else {
                nkdhr_ui::GestureOrigin::WindowFrame
            }
        },
    )
}

fn handle_pointer_motion(
    app: &mut App,
    pointer: &PointerHandle<App>,
    group: &InputGroup,
    pointer_pos: Point<f64, Logical>,
    time: u32,
    relative: Option<RelativeMotion>,
) {
    if app.placement.is_some() {
        app.placement_pointer(local_point(group, pointer_pos));
        pointer.motion(
            app,
            None,
            &MotionEvent {
                location: pointer_pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(app);
        return;
    }
    if app.pointer_action.is_some() {
        crate::actions::update_pointer(
            app,
            crate::actions::CanvasActionPayload::PointerPosition(pointer_pos),
        );
        pointer.motion(
            app,
            None,
            &MotionEvent {
                location: pointer_pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(app);
        return;
    }
    let shell_position = pointer_pos - group.output_global_location.to_f64();
    if app.shell.pointer_motion(&group.output_name, shell_position) {
        pointer.motion(
            app,
            None,
            &MotionEvent {
                location: pointer_pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(app);
        return;
    }
    if app.active_view().in_overview {
        return;
    }

    if !has_active_pointer_constraint(pointer)
        && dispatch_pinned_pointer(app, group, pointer_pos, |position| {
            PinnedPointerEvent::Motion { position, time }
        }) == InputHandled::Captured
    {
        pointer.motion(
            app,
            None,
            &MotionEvent {
                location: pointer_pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(app);
        return;
    }

    let current_surface = pointer.current_focus();
    let mut focus = pointer_focus_at(app, group, pointer_pos);
    let mut location = pointer_pos;

    if let Some(current_surface) = current_surface.as_ref() {
        let active_constraint = with_pointer_constraint(current_surface, pointer, |constraint| {
            constraint.and_then(|constraint| {
                if !constraint.is_active() {
                    return None;
                }
                Some(match &*constraint {
                    PointerConstraint::Locked(_) => ActiveConstraint::Locked,
                    PointerConstraint::Confined(confined) => {
                        ActiveConstraint::Confined(confined.region().cloned())
                    }
                })
            })
        });

        match active_constraint {
            Some(ActiveConstraint::Locked) => {
                let focus = pointer_focus_for_surface(app, group, current_surface);
                if focus.is_none() {
                    with_pointer_constraint(current_surface, pointer, |constraint| {
                        if let Some(constraint) = constraint
                            && constraint.is_active()
                        {
                            constraint.deactivate();
                        }
                    });
                } else {
                    if let (Some(focus), Some((delta, delta_unaccel, utime))) = (focus, relative) {
                        pointer.relative_motion(
                            app,
                            Some(focus),
                            &RelativeMotionEvent {
                                delta,
                                delta_unaccel,
                                utime,
                            },
                        );
                        pointer.frame(app);
                    }
                    return;
                }
            }
            Some(ActiveConstraint::Confined(region)) => {
                let remains_on_surface = focus
                    .as_ref()
                    .is_some_and(|(surface, _)| surface == current_surface);
                let inside_region = region.as_ref().is_none_or(|region| {
                    pointer_focus_for_surface(app, group, current_surface).is_some_and(
                        |(_, origin)| {
                            let local = location - origin;
                            region.contains((local.x.floor() as i32, local.y.floor() as i32))
                        },
                    )
                });
                if !remains_on_surface || !inside_region {
                    location = pointer.current_location();
                    focus = pointer_focus_for_surface(app, group, current_surface);
                }
            }
            None => {}
        }
    }

    if current_surface.as_ref() != focus.as_ref().map(|(surface, _)| surface)
        && let Some(current_surface) = current_surface.as_ref()
    {
        with_pointer_constraint(current_surface, pointer, |constraint| {
            if let Some(constraint) = constraint
                && constraint.is_active()
            {
                constraint.deactivate();
            }
        });
    }

    if let Some((surface, origin)) = focus.as_ref() {
        let local = location - *origin;
        with_pointer_constraint(surface, pointer, |constraint| {
            if let Some(constraint) = constraint
                && !constraint.is_active()
                && constraint.region().is_none_or(|region| {
                    region.contains((local.x.floor() as i32, local.y.floor() as i32))
                })
            {
                constraint.activate();
            }
        });
    }

    if let Some((delta, delta_unaccel, utime)) = relative {
        pointer.relative_motion(
            app,
            focus.clone(),
            &RelativeMotionEvent {
                delta,
                delta_unaccel,
                utime,
            },
        );
    }
    pointer.motion(
        app,
        focus,
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    pointer.frame(app);
}

enum ActiveConstraint {
    Locked,
    Confined(Option<smithay::wayland::compositor::RegionAttributes>),
}

fn pointer_focus_at(
    app: &App,
    group: &InputGroup,
    pointer_pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    let viewport = app.active_view().viewport;
    let world_pos =
        viewport.group_logical_to_world(local_point(group, pointer_pos), group.canvas_anchor);
    let (window, surface, surface_offset) = app.active_canvas().surface_at(world_pos)?;
    let root_offset = viewport.to_group_logical(window.position, group.canvas_anchor);
    let surface_offset = surface_offset.to_f64().upscale(viewport.zoom);
    let offset = group.origin.to_f64() + root_offset + surface_offset;
    Some((surface, offset))
}

/// Give the front pinned layer first refusal, then the back layer only when
/// no window frame occludes that world-space point.
fn dispatch_pinned_pointer(
    app: &mut App,
    group: &InputGroup,
    pointer_pos: Point<f64, Logical>,
    event: impl Fn(Point<f64, crate::widget_host::PinnedLocal>) -> PinnedPointerEvent,
) -> InputHandled {
    let viewport = app.active_view().viewport;
    let world_pos =
        viewport.group_logical_to_world(local_point(group, pointer_pos), group.canvas_anchor);

    if app
        .active_canvas_mut()
        .dispatch_pinned_pointer(world_pos, PinnedLayer::AboveWindows, &event)
        == InputHandled::Captured
    {
        return InputHandled::Captured;
    }
    if app.active_canvas().window_at(world_pos).is_some() {
        app.active_canvas_mut()
            .leave_pinned_pointer_focus(PinnedLayer::BehindWindows);
        return InputHandled::Ignored;
    }
    app.active_canvas_mut()
        .dispatch_pinned_pointer(world_pos, PinnedLayer::BehindWindows, event)
}

fn pointer_focus_for_surface(
    app: &App,
    group: &InputGroup,
    surface: &WlSurface,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    let pointer_pos = app.seat.get_pointer()?.current_location();
    let viewport = app.active_view().viewport;
    let world_pos =
        viewport.group_logical_to_world(local_point(group, pointer_pos), group.canvas_anchor);
    let (window, candidate, surface_offset) = app.active_canvas().surface_at(world_pos)?;
    if &candidate != surface {
        return None;
    }
    let root_offset = viewport.to_group_logical(window.position, group.canvas_anchor);
    let surface_offset = surface_offset.to_f64().upscale(viewport.zoom);
    Some((
        surface.clone(),
        group.origin.to_f64() + root_offset + surface_offset,
    ))
}

fn handle_keyboard<B: InputBackend>(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    group: InputGroup,
    event: B::KeyboardKeyEvent,
) {
    let serial = SERIAL_COUNTER.next_serial();
    let key_state = event.state();
    keyboard.input::<(), _>(
        app,
        event.key_code(),
        key_state,
        serial,
        event.time_msec(),
        move |app, modifiers, keysym| {
            let sym = keysym.modified_sym();
            let raw_syms = keysym.raw_syms();
            let pressed = key_state == KeyState::Pressed;

            if !pressed && app.suppress_placement_terminal_release {
                let terminal_release =
                    raw_syms
                        .iter()
                        .copied()
                        .chain(std::iter::once(sym))
                        .any(|key| {
                            matches!(
                                placement_key(key),
                                Some(PlacementKey::Commit | PlacementKey::Cancel)
                            )
                        });
                if terminal_release {
                    app.suppress_placement_terminal_release = false;
                    return FilterResult::Intercept(());
                }
            }

            if app.placement.is_some() {
                let placement_key = raw_syms
                    .iter()
                    .copied()
                    .chain(std::iter::once(sym))
                    .find_map(placement_key);
                match placement_key {
                    Some(PlacementKey::Direction(direction)) => {
                        app.placement_direction(direction, pressed, Instant::now());
                    }
                    Some(PlacementKey::Commit) if pressed => {
                        app.commit_placement();
                        app.suppress_placement_terminal_release = true;
                    }
                    Some(PlacementKey::Cancel) if pressed => {
                        app.cancel_placement();
                        app.suppress_placement_terminal_release = true;
                    }
                    _ => {}
                }
                return FilterResult::Intercept(());
            }

            if !pressed
                && raw_syms
                    .iter()
                    .copied()
                    .chain(std::iter::once(sym))
                    .any(is_alt_key)
                && app.shell.release_alt(&group.output_name).is_some()
            {
                return FilterResult::Intercept(());
            }

            let ui_modifiers = ui_modifiers(*modifiers);
            let ui_key = ui_key(sym);
            let key_event = if pressed {
                nkdhr_ui::UiEvent::KeyDown {
                    key: ui_key,
                    modifiers: ui_modifiers,
                    repeat: false,
                }
            } else {
                nkdhr_ui::UiEvent::KeyUp {
                    key: ui_key,
                    modifiers: ui_modifiers,
                }
            };
            if app.shell.keyboard(key_event.clone()) {
                return FilterResult::Intercept(());
            }
            let mut ui_handled = app.active_canvas_mut().dispatch_pinned_keyboard(&key_event)
                == InputHandled::Captured;
            if pressed
                && !ui_modifiers.control
                && !ui_modifiers.alt
                && !ui_modifiers.logo
                && let Some(character) = sym.key_char()
                && !character.is_control()
            {
                ui_handled |= app
                    .active_canvas_mut()
                    .dispatch_pinned_keyboard(&nkdhr_ui::UiEvent::TextInput(character.to_string()))
                    == InputHandled::Captured;
            }
            if ui_handled {
                return FilterResult::Intercept(());
            }

            let context = crate::actions::key_context(app);
            let modifier_set = action_modifiers(*modifiers);
            let phase = if pressed {
                nkdhr_ui::KeyPhase::Press
            } else {
                nkdhr_ui::KeyPhase::Release
            };
            let mut symbols = raw_syms;
            if !symbols.contains(&sym) {
                symbols.push(sym);
            }
            let binding = symbols.iter().find_map(|symbol| {
                find_binding(
                    app,
                    context,
                    &nkdhr_ui::RuntimeTrigger::key(
                        xkb::keysym_get_name(*symbol),
                        modifier_set,
                        phase,
                    ),
                )
            });
            if let Some(binding) = binding {
                crate::actions::invoke_binding(
                    app,
                    &binding,
                    crate::actions::CanvasActionPayload::Group {
                        output_name: group.output_name.clone(),
                        size: group.size,
                        canvas_anchor: group.canvas_anchor,
                        display_rect: group.display_rect,
                    },
                );
                return FilterResult::Intercept(());
            }
            if !pressed
                && symbols.iter().any(|symbol| {
                    find_binding(
                        app,
                        context,
                        &nkdhr_ui::RuntimeTrigger::key(
                            xkb::keysym_get_name(*symbol),
                            modifier_set,
                            nkdhr_ui::KeyPhase::Press,
                        ),
                    )
                    .is_some()
                })
            {
                return FilterResult::Intercept(());
            }
            FilterResult::Forward
        },
    );
}

fn is_alt_key(key: Keysym) -> bool {
    matches!(key, Keysym::Alt_L | Keysym::Alt_R)
}

#[derive(Debug, Clone, Copy)]
enum PlacementKey {
    Direction(PlacementDirection),
    Commit,
    Cancel,
}

fn placement_key(key: Keysym) -> Option<PlacementKey> {
    match key {
        Keysym::Left | Keysym::h | Keysym::H => {
            Some(PlacementKey::Direction(PlacementDirection::Left))
        }
        Keysym::Right | Keysym::l | Keysym::L => {
            Some(PlacementKey::Direction(PlacementDirection::Right))
        }
        Keysym::Up | Keysym::k | Keysym::K => Some(PlacementKey::Direction(PlacementDirection::Up)),
        Keysym::Down | Keysym::j | Keysym::J => {
            Some(PlacementKey::Direction(PlacementDirection::Down))
        }
        Keysym::Return | Keysym::KP_Enter => Some(PlacementKey::Commit),
        Keysym::Escape => Some(PlacementKey::Cancel),
        _ => None,
    }
}

fn handle_vt_switch(
    app: &mut App,
    modifiers: &ModifiersState,
    modified_sym: Keysym,
    raw_syms: &[Keysym],
    pressed: bool,
) -> bool {
    if !app.vt_switching_enabled() {
        return false;
    }
    let dedicated_vt = xf86_vt_number(modified_sym);
    let chord_vt = (modifiers.ctrl && modifiers.alt)
        .then(|| raw_syms.iter().find_map(|sym| function_key_vt_number(*sym)))
        .flatten();
    let Some(vt) = dedicated_vt.or(chord_vt) else {
        return false;
    };
    if pressed {
        app.request_vt_switch(vt);
    }
    true
}

fn function_key_vt_number(sym: Keysym) -> Option<i32> {
    match sym {
        Keysym::F1 => Some(1),
        Keysym::F2 => Some(2),
        Keysym::F3 => Some(3),
        Keysym::F4 => Some(4),
        Keysym::F5 => Some(5),
        Keysym::F6 => Some(6),
        Keysym::F7 => Some(7),
        Keysym::F8 => Some(8),
        Keysym::F9 => Some(9),
        Keysym::F10 => Some(10),
        Keysym::F11 => Some(11),
        Keysym::F12 => Some(12),
        _ => None,
    }
}

fn xf86_vt_number(sym: Keysym) -> Option<i32> {
    match sym {
        Keysym::XF86_Switch_VT_1 => Some(1),
        Keysym::XF86_Switch_VT_2 => Some(2),
        Keysym::XF86_Switch_VT_3 => Some(3),
        Keysym::XF86_Switch_VT_4 => Some(4),
        Keysym::XF86_Switch_VT_5 => Some(5),
        Keysym::XF86_Switch_VT_6 => Some(6),
        Keysym::XF86_Switch_VT_7 => Some(7),
        Keysym::XF86_Switch_VT_8 => Some(8),
        Keysym::XF86_Switch_VT_9 => Some(9),
        Keysym::XF86_Switch_VT_10 => Some(10),
        Keysym::XF86_Switch_VT_11 => Some(11),
        Keysym::XF86_Switch_VT_12 => Some(12),
        _ => None,
    }
}

pub(crate) fn close_focused_window(app: &mut App) {
    let Some(focused) = focused_surface(app) else {
        return;
    };
    if let Some(window) = app
        .active_canvas()
        .windows()
        .iter()
        .find(|window| window.matches_surface(&focused))
    {
        window.close();
    }
}

pub(crate) fn cycle_focus(app: &mut App, output_name: &str) {
    let group = app.active_group.clone();
    let Some((workspace, windows)) = app.shell_workspace_snapshot(&group) else {
        return;
    };
    app.shell.sync_workspace(output_name, workspace, windows);
    let Some(next_id) = app.shell.cycle_focus(output_name) else {
        return;
    };
    focus_window_id(app, next_id);
}

fn focus_window_id(app: &mut App, window_id: u64) {
    let Some((next_surface, next_focus)) = app
        .active_canvas()
        .window_by_id(window_id)
        .and_then(|window| Some((window.wl_surface()?, keyboard_target(window)?)))
    else {
        return;
    };
    app.active_canvas_mut().raise(&next_surface);
    if let Some(keyboard) = app.seat.get_keyboard() {
        keyboard.set_focus(app, Some(next_focus), SERIAL_COUNTER.next_serial());
    }
}

pub(crate) fn set_mark(app: &mut App, digit: u8) {
    let canvas = app.active_view().canvas.clone();
    let center = app.active_view().viewport.center;
    app.marks
        .entry(canvas.clone())
        .or_default()
        .insert(digit, center);
    marks::save(&app.marks);
    println!("nkdhr-canvas: set mark {digit} on canvas {canvas:?} at {center:?}");
}

pub(crate) fn jump_to_mark(app: &mut App, digit: u8) {
    let canvas = app.active_view().canvas.clone();
    let Some(&center) = app.marks.get(&canvas).and_then(|marks| marks.get(&digit)) else {
        return;
    };
    let grid = { app.interaction_settings.lock().unwrap().grid };
    let view = app.active_view_mut();
    let target = snapped_viewport(grid, Viewport { center, zoom: 1.0 });
    view.animation = Some(Animation::new(view.viewport, target, TRANSITION));
    view.in_overview = false;
}

pub(crate) fn toggle_overview(
    app: &mut App,
    group_size: Size<i32, Logical>,
    canvas_anchor: Point<f64, Logical>,
) {
    if app.active_view().in_overview {
        exit_overview(app, None);
        return;
    }
    let viewport = app.active_view().viewport;
    let target = app
        .active_canvas()
        .bounding_rect()
        .map_or(viewport, |rect| {
            Viewport::fit_group(rect, group_size, canvas_anchor)
        });
    let view = app.active_view_mut();
    view.pre_overview_viewport = viewport;
    view.animation = Some(Animation::new(viewport, target, TRANSITION));
    view.in_overview = true;
}

pub(crate) fn exit_overview(app: &mut App, target: Option<Viewport>) {
    let grid = { app.interaction_settings.lock().unwrap().grid };
    let view = app.active_view_mut();
    let target = snapped_viewport(grid, target.unwrap_or(view.pre_overview_viewport));
    view.animation = Some(Animation::new(view.viewport, target, TRANSITION));
    view.in_overview = false;
}

fn handle_pointer_button<B: InputBackend>(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    pointer: &PointerHandle<App>,
    group: &InputGroup,
    event: &B::PointerButtonEvent,
) {
    let button_state = event.state();
    let button_code = event.button_code();

    if app.placement.is_some() {
        if button_state == ButtonState::Pressed && button_code == BTN_LEFT {
            app.placement_pointer(local_point(group, pointer.current_location()));
            app.commit_placement();
            app.suppress_pointer_release = true;
        } else if button_state == ButtonState::Pressed && button_code == BTN_RIGHT {
            app.cancel_placement();
            app.suppress_pointer_release = true;
        }
        return;
    }

    if button_state == ButtonState::Released && std::mem::take(&mut app.suppress_pointer_release) {
        return;
    }
    if button_state == ButtonState::Pressed {
        app.suppress_pointer_release = false;
    }
    if button_state == ButtonState::Released && app.pointer_action.is_some() {
        crate::actions::end_pointer(app);
        return;
    }

    let shell_position = pointer.current_location() - group.output_global_location.to_f64();
    let shell_handled = app.shell.pointer_button(
        &group.output_name,
        shell_position,
        button_code,
        button_state,
        ui_modifiers(keyboard.modifier_state()),
        1,
    );
    if let Some(window) = app.shell.take_requested_window_focus(&group.output_name) {
        focus_window_id(app, window);
    }
    if shell_handled {
        return;
    }

    if app.active_view().in_overview {
        if button_state == ButtonState::Pressed && button_code == BTN_LEFT {
            handle_overview_click(app, pointer, group);
        }
        return;
    }

    let pointer_pos = pointer.current_location();
    if button_state == ButtonState::Pressed {
        app.active_canvas_mut().clear_pinned_keyboard_focus();
    }
    if !has_active_pointer_constraint(pointer)
        && dispatch_pinned_pointer(app, group, pointer_pos, |position| {
            PinnedPointerEvent::Button {
                position,
                button: button_code,
                state: button_state,
                modifiers: ui_modifiers(keyboard.modifier_state()),
                time: event.time_msec(),
            }
        }) == InputHandled::Captured
    {
        if pointer.current_focus().is_some() {
            pointer.motion(
                app,
                None,
                &MotionEvent {
                    location: pointer_pos,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
            pointer.frame(app);
        }
        return;
    }

    if button_state == ButtonState::Pressed {
        let modifiers = keyboard.modifier_state();
        let viewport = app.active_view().viewport;
        let world_pos =
            viewport.group_logical_to_world(local_point(group, pointer_pos), group.canvas_anchor);
        let hit = app.active_canvas().window_at(world_pos).and_then(|window| {
            Some((
                window.wl_surface()?,
                window.position,
                window.size(),
                keyboard_target(window)?,
                window.content_rect().contains(world_pos),
            ))
        });

        if button_code == BTN_LEFT
            && !modifiers.logo
            && let Some((surface, _, _, focus, _)) = hit.as_ref()
        {
            app.active_canvas_mut().raise(surface);
            keyboard.set_focus(app, Some(focus.clone()), SERIAL_COUNTER.next_serial());
        }

        let origin = hit.as_ref().map_or(
            nkdhr_ui::GestureOrigin::EmptyCanvas,
            |(_, _, _, _, content_hit)| {
                if *content_hit {
                    nkdhr_ui::GestureOrigin::Window
                } else {
                    nkdhr_ui::GestureOrigin::WindowFrame
                }
            },
        );
        if let Some(button) = action_button(button_code)
            && let Some(binding) = find_binding(
                app,
                binding_context_for_origin(app, origin),
                &nkdhr_ui::RuntimeTrigger::Button {
                    button,
                    modifiers: action_modifiers(modifiers),
                    device: pointer_device_class::<B, _>(event),
                    origin,
                    phase: nkdhr_ui::KeyPhase::Press,
                },
            )
        {
            let target =
                hit.as_ref().map(
                    |(surface, position, size, _, _)| crate::actions::PointerTarget {
                        surface: surface.clone(),
                        position: *position,
                        size: (
                            size.w.round().max(1.0) as i32,
                            size.h.round().max(1.0) as i32,
                        )
                            .into(),
                    },
                );
            if crate::actions::begin_pointer_binding(
                app,
                &binding,
                crate::actions::CanvasActionPayload::PointerStart {
                    pointer: pointer_pos,
                    target,
                    viewport,
                    resize_edge: None,
                },
            ) {
                return;
            }
        }

        if button_code == BTN_LEFT
            && let Some((surface, _, _, focus, content_hit)) = hit
        {
            app.active_canvas_mut().raise(&surface);
            keyboard.set_focus(app, Some(focus), SERIAL_COUNTER.next_serial());
            if !content_hit {
                return;
            }
        }
    }

    pointer.button(
        app,
        &ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: event.time_msec(),
            button: button_code,
            state: button_state,
        },
    );
    pointer.frame(app);
}

fn handle_pointer_axis<B: InputBackend>(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    pointer: &PointerHandle<App>,
    group: &InputGroup,
    event: &B::PointerAxisEvent,
) {
    if app.active_view().in_overview || app.drag.is_some() {
        return;
    }
    let dx = event
        .amount(Axis::Horizontal)
        .or_else(|| event.amount_v120(Axis::Horizontal).map(|v120| v120 / 6.0))
        .unwrap_or(0.0);
    let dy = event
        .amount(Axis::Vertical)
        .or_else(|| event.amount_v120(Axis::Vertical).map(|v120| v120 / 6.0))
        .unwrap_or(0.0);
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    let shell_position = pointer.current_location() - group.output_global_location.to_f64();
    if app.shell.pointer_axis(
        &group.output_name,
        shell_position,
        dx,
        dy,
        ui_modifiers(keyboard.modifier_state()),
    ) {
        return;
    }
    if !has_active_pointer_constraint(pointer)
        && dispatch_pinned_pointer(app, group, pointer.current_location(), |position| {
            PinnedPointerEvent::Axis {
                position,
                horizontal: dx,
                vertical: dy,
                modifiers: ui_modifiers(keyboard.modifier_state()),
                time: event.time_msec(),
            }
        }) == InputHandled::Captured
    {
        return;
    }
    pointer.axis(app, axis_frame::<B>(event));
    pointer.frame(app);
}

fn ui_modifiers(state: smithay::input::keyboard::ModifiersState) -> nkdhr_ui::Modifiers {
    nkdhr_ui::Modifiers {
        shift: state.shift,
        control: state.ctrl,
        alt: state.alt,
        logo: state.logo,
    }
}

fn action_modifiers(state: smithay::input::keyboard::ModifiersState) -> nkdhr_ui::ModifierSet {
    let mut modifiers = Vec::with_capacity(4);
    if state.ctrl {
        modifiers.push(nkdhr_ui::Modifier::Control);
    }
    if state.alt {
        modifiers.push(nkdhr_ui::Modifier::Alt);
    }
    if state.shift {
        modifiers.push(nkdhr_ui::Modifier::Shift);
    }
    if state.logo {
        modifiers.push(nkdhr_ui::Modifier::Logo);
    }
    nkdhr_ui::ModifierSet::new(modifiers)
}

fn action_button(button: u32) -> Option<nkdhr_ui::ButtonCode> {
    match button {
        BTN_LEFT => Some(nkdhr_ui::ButtonCode::Primary),
        BTN_RIGHT => Some(nkdhr_ui::ButtonCode::Secondary),
        0x112 => Some(nkdhr_ui::ButtonCode::Middle),
        0x116 => Some(nkdhr_ui::ButtonCode::Back),
        0x115 => Some(nkdhr_ui::ButtonCode::Forward),
        _ => None,
    }
}

fn pointer_device_class<B: InputBackend, E: Event<B>>(event: &E) -> nkdhr_ui::DeviceClass {
    if event.device().has_capability(DeviceCapability::Gesture) {
        nkdhr_ui::DeviceClass::Touchpad
    } else {
        nkdhr_ui::DeviceClass::Mouse
    }
}

fn ui_key(sym: Keysym) -> nkdhr_ui::Key {
    match sym {
        Keysym::Tab => nkdhr_ui::Key::Tab,
        Keysym::Return | Keysym::KP_Enter => nkdhr_ui::Key::Enter,
        Keysym::space => nkdhr_ui::Key::Space,
        Keysym::Escape => nkdhr_ui::Key::Escape,
        Keysym::Left => nkdhr_ui::Key::ArrowLeft,
        Keysym::Right => nkdhr_ui::Key::ArrowRight,
        Keysym::Up => nkdhr_ui::Key::ArrowUp,
        Keysym::Down => nkdhr_ui::Key::ArrowDown,
        Keysym::Home => nkdhr_ui::Key::Home,
        Keysym::End => nkdhr_ui::Key::End,
        Keysym::Page_Up => nkdhr_ui::Key::PageUp,
        Keysym::Page_Down => nkdhr_ui::Key::PageDown,
        Keysym::BackSpace => nkdhr_ui::Key::Backspace,
        Keysym::Delete => nkdhr_ui::Key::Delete,
        _ => sym.key_char().map_or_else(
            || nkdhr_ui::Key::Named(format!("{sym:?}")),
            |character| nkdhr_ui::Key::Character(character.to_string()),
        ),
    }
}

fn axis_frame<B: InputBackend>(event: &B::PointerAxisEvent) -> AxisFrame {
    let source = event.source();
    let mut frame = AxisFrame::new(event.time_msec()).source(source);
    for axis in [Axis::Horizontal, Axis::Vertical] {
        frame = frame.relative_direction(axis, event.relative_direction(axis));
        if let Some(amount) = event.amount(axis) {
            if matches!(source, AxisSource::Finger) && amount == 0.0 {
                frame = frame.stop(axis);
            } else {
                frame = frame.value(axis, amount);
            }
        }
        if let Some(v120) = event.amount_v120(axis) {
            frame = frame.v120(axis, v120.round() as i32);
        }
    }
    frame
}

pub(crate) fn gesture_pan_center(
    center: Point<f64, World>,
    delta: Point<f64, Logical>,
    zoom: f64,
) -> Point<f64, World> {
    (center.x - delta.x / zoom, center.y - delta.y / zoom).into()
}

fn handle_overview_click(app: &mut App, pointer: &PointerHandle<App>, group: &InputGroup) {
    let pointer_pos = pointer.current_location();
    let viewport = app.active_view().viewport;
    let world_pos =
        viewport.group_logical_to_world(local_point(group, pointer_pos), group.canvas_anchor);
    let Some((surface, center, focus)) =
        app.active_canvas().window_at(world_pos).and_then(|window| {
            Some((
                window.wl_surface()?,
                window.center(),
                keyboard_target(window)?,
            ))
        })
    else {
        exit_overview(app, None);
        return;
    };
    let target = Viewport { center, zoom: 1.0 };
    app.active_canvas_mut().raise(&surface);
    exit_overview(app, Some(target));
    if let Some(keyboard) = app.seat.get_keyboard() {
        keyboard.set_focus(app, Some(focus), SERIAL_COUNTER.next_serial());
    }
}

pub(crate) fn apply_drag(app: &mut App, drag: &Drag, pointer_pos: Point<f64, Logical>) {
    let grid = { app.interaction_settings.lock().unwrap().grid };
    match drag {
        Drag::Move {
            surface,
            window_start,
            pointer_start,
        } => {
            let delta = app
                .active_view()
                .viewport
                .to_world_delta(pointer_pos - *pointer_start);
            let position = *window_start + delta;
            app.active_canvas_mut().set_position(surface, position);
        }
        Drag::Resize {
            surface,
            size_start,
            window_start,
            pointer_start,
            edge,
        } => {
            let delta = app
                .active_view()
                .viewport
                .to_world_delta(pointer_pos - *pointer_start);
            let (new_position, new_size) =
                resize_geometry(*window_start, *size_start, delta, *edge, grid);
            app.active_canvas_mut().set_position(surface, new_position);
            if let Some(toplevel) = app
                .active_canvas()
                .windows()
                .iter()
                .find(|window| window.matches_surface(surface))
            {
                toplevel.request_size(new_size);
            }
        }
        Drag::Pan {
            viewport_start,
            pointer_start,
            zoom,
        } => {
            let delta = pointer_pos - *pointer_start;
            app.active_view_mut().viewport.center = (
                viewport_start.x - delta.x / zoom,
                viewport_start.y - delta.y / zoom,
            )
                .into();
        }
    }
}

pub(crate) fn finish_drag(app: &mut App, drag: &Drag) {
    let Drag::Move { surface, .. } = drag else {
        if matches!(drag, Drag::Pan { .. }) {
            animate_viewport_snap(app);
        }
        return;
    };
    let grid = { app.interaction_settings.lock().unwrap().grid };
    if !grid.enabled {
        return;
    }
    let Some(position) = app
        .active_canvas()
        .windows()
        .iter()
        .find(|window| window.matches_surface(surface))
        .map(|window| window.position)
    else {
        return;
    };
    let target = (grid.snap(position.x), grid.snap(position.y)).into();
    app.active_canvas_mut()
        .animate_position(surface, target, WINDOW_SNAP_TRANSITION);
}

pub(crate) fn snapped_viewport(grid: GridSettings, viewport: Viewport) -> Viewport {
    Viewport {
        center: (grid.snap(viewport.center.x), grid.snap(viewport.center.y)).into(),
        ..viewport
    }
}

pub(crate) fn animate_viewport_snap(app: &mut App) {
    if app.active_view().in_overview {
        return;
    }
    let grid = { app.interaction_settings.lock().unwrap().grid };
    if !grid.enabled {
        return;
    }
    let view = app.active_view_mut();
    let target = snapped_viewport(grid, view.viewport);
    if target == view.viewport {
        return;
    }
    view.animation = Some(Animation::new(
        view.viewport,
        target,
        VIEWPORT_SNAP_TRANSITION,
    ));
}

fn resize_geometry(
    window_start: Point<f64, World>,
    size_start: Size<i32, Logical>,
    delta: Point<f64, World>,
    edge: ResizeEdge,
    grid: GridSettings,
) -> (Point<f64, World>, Size<i32, Logical>) {
    let mut left = window_start.x;
    let mut top = window_start.y;
    let mut right = left + f64::from(size_start.w);
    let mut bottom = top + f64::from(size_start.h);

    if edge.left() {
        left = grid.snap(left + delta.x);
    } else if edge.right() {
        right = grid.snap(right + delta.x);
    }
    if edge.top() {
        top = grid.snap(top + delta.y);
    } else if edge.bottom() {
        bottom = grid.snap(bottom + delta.y);
    }

    let new_size = Size::from((
        (right - left).round().max(1.0) as i32,
        (bottom - top).round().max(1.0) as i32,
    ));
    let new_position = (
        if edge.left() {
            right - f64::from(new_size.w)
        } else {
            left
        },
        if edge.top() {
            bottom - f64::from(new_size.h)
        } else {
            top
        },
    )
        .into();
    (new_position, new_size)
}

/// Begin a client-requested move using the compositor's existing canvas
/// drag machinery. The protocol handler validates the implicit grab.
pub fn begin_client_move(app: &mut App, surface: &WlSurface) -> bool {
    if app.session_locked() {
        return false;
    }
    let Some(pointer) = app.seat.get_pointer() else {
        return false;
    };
    let Some(pointer_focus) = pointer.current_focus() else {
        return false;
    };
    let Some(target) = app
        .active_canvas()
        .windows()
        .iter()
        .find(|window| window.matches_surface(surface) && window.matches_surface(&pointer_focus))
        .map(|window| crate::actions::PointerTarget {
            surface: surface.clone(),
            position: window.position,
            size: (
                window.size().w.round().max(1.0) as i32,
                window.size().h.round().max(1.0) as i32,
            )
                .into(),
        })
    else {
        return false;
    };
    crate::actions::begin_named_pointer(
        app,
        "canvas.window.move",
        crate::actions::CanvasActionPayload::PointerStart {
            pointer: pointer.current_location(),
            target: Some(target),
            viewport: app.active_view().viewport,
            resize_edge: None,
        },
    )
}

/// Begin a client-requested edge resize after the shell protocol has
/// validated its implicit pointer grab.
pub fn begin_client_resize(app: &mut App, surface: &WlSurface, edge: ResizeEdge) -> bool {
    if app.session_locked() {
        return false;
    }
    let Some(pointer) = app.seat.get_pointer() else {
        return false;
    };
    let Some(pointer_focus) = pointer.current_focus() else {
        return false;
    };
    let Some(target) = app
        .active_canvas()
        .windows()
        .iter()
        .find(|window| window.matches_surface(surface) && window.matches_surface(&pointer_focus))
        .map(|window| crate::actions::PointerTarget {
            surface: surface.clone(),
            position: window.position,
            size: (
                window.size().w.round().max(1.0) as i32,
                window.size().h.round().max(1.0) as i32,
            )
                .into(),
        })
    else {
        return false;
    };
    crate::actions::begin_named_pointer(
        app,
        "canvas.window.resize",
        crate::actions::CanvasActionPayload::PointerStart {
            pointer: pointer.current_location(),
            target: Some(target),
            viewport: app.active_view().viewport,
            resize_edge: Some(edge),
        },
    )
}

pub(crate) fn focused_surface(app: &App) -> Option<WlSurface> {
    app.seat
        .get_keyboard()?
        .current_focus()?
        .wl_surface()
        .map(|surface| surface.into_owned())
}

fn keyboard_target(window: &ManagedWindow) -> Option<KeyboardFocusTarget> {
    if let Some(surface) = window.window.x11_surface() {
        Some(KeyboardFocusTarget::X11(surface.clone()))
    } else {
        window.wl_surface().map(KeyboardFocusTarget::Wayland)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_function_keys_to_linux_vts() {
        for (sym, vt) in [
            (Keysym::F1, 1),
            (Keysym::F2, 2),
            (Keysym::F3, 3),
            (Keysym::F4, 4),
            (Keysym::F5, 5),
            (Keysym::F6, 6),
            (Keysym::F7, 7),
            (Keysym::F8, 8),
            (Keysym::F9, 9),
            (Keysym::F10, 10),
            (Keysym::F11, 11),
            (Keysym::F12, 12),
        ] {
            assert_eq!(function_key_vt_number(sym), Some(vt));
        }
        assert_eq!(function_key_vt_number(Keysym::Escape), None);
        assert_eq!(xf86_vt_number(Keysym::XF86_Switch_VT_2), Some(2));
        assert_eq!(xf86_vt_number(Keysym::F2), None);
    }

    #[test]
    fn resize_snaps_only_the_moving_edges() {
        let (position, size) = resize_geometry(
            (96.0, 96.0).into(),
            (250, 200).into(),
            (17.0, 19.0).into(),
            ResizeEdge::BottomRight,
            GridSettings::default(),
        );
        assert_eq!(position, (96.0, 96.0).into());
        assert_eq!(size, (256, 224).into());

        let (position, size) = resize_geometry(
            (96.0, 96.0).into(),
            (256, 224).into(),
            (-45.0, -33.0).into(),
            ResizeEdge::TopLeft,
            GridSettings::default(),
        );
        assert_eq!(position, (64.0, 64.0).into());
        assert_eq!(size, (288, 256).into());
    }

    #[test]
    fn resize_preserves_exact_delta_when_grid_is_disabled() {
        let grid = GridSettings {
            enabled: false,
            ..GridSettings::default()
        };
        let (position, size) = resize_geometry(
            (100.0, 100.0).into(),
            (250, 200).into(),
            (17.0, 19.0).into(),
            ResizeEdge::BottomRight,
            grid,
        );
        assert_eq!(position, (100.0, 100.0).into());
        assert_eq!(size, (267, 219).into());
    }

    #[test]
    fn three_finger_pan_tracks_the_gesture_at_current_zoom() {
        assert_eq!(
            gesture_pan_center((100.0, -50.0).into(), (12.0, -8.0).into(), 2.0),
            (94.0, -46.0).into()
        );
    }

    #[test]
    fn work_viewport_center_snaps_to_the_world_grid() {
        let viewport = Viewport {
            center: (47.0, -49.0).into(),
            zoom: 1.0,
        };

        assert_eq!(
            snapped_viewport(GridSettings::default(), viewport).center,
            (32.0, -64.0).into()
        );
    }

    #[test]
    fn disabled_grid_preserves_the_exact_viewport_center() {
        let viewport = Viewport {
            center: (47.25, -49.75).into(),
            zoom: 1.0,
        };
        let grid = GridSettings {
            enabled: false,
            ..GridSettings::default()
        };

        assert_eq!(snapped_viewport(grid, viewport), viewport);
    }
}
