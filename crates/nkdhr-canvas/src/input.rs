use std::time::Duration;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent,
    GestureEndEvent, GestureSwipeUpdateEvent, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
    PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};
use smithay::input::keyboard::{FilterResult, KeyboardHandle, Keysym, ModifiersState, keysyms};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureSwipeBeginEvent as PointerSwipeBeginEvent,
    GestureSwipeEndEvent as PointerSwipeEndEvent,
    GestureSwipeUpdateEvent as PointerSwipeUpdateEvent, MotionEvent, PointerHandle,
    RelativeMotionEvent,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Size};
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};
use smithay::wayland::seat::WaylandFocus;

use crate::canvas::marks;
use crate::canvas::output_group::OutputLayout;
use crate::canvas::world::{Animation, Drag, ManagedWindow, ResizeEdge, Viewport, World};
use crate::settings::GridSettings;
use crate::state::{App, KeyboardFocusTarget};
use crate::widget_host::{InputHandled, PinnedLayer, PinnedPointerEvent};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const TRANSITION: Duration = Duration::from_millis(250);
const KEYBOARD_PAN_TRANSITION: Duration = Duration::from_millis(140);
const WINDOW_SNAP_TRANSITION: Duration = Duration::from_millis(120);
const PAN_STEP: f64 = 80.0;
type RelativeMotion = (Point<f64, Logical>, Point<f64, Logical>, u64);

#[derive(Clone)]
struct InputGroup {
    origin: Point<i32, Logical>,
    size: Size<i32, Logical>,
}

/// Backend-independent compositor input dispatch. Backend code supplies the
/// resolved output layout; this layer maps compositor-global pointer
/// coordinates into the active output group's independent canvas view.
pub fn handle<B: InputBackend>(app: &mut App, layout: &OutputLayout, event: InputEvent<B>) {
    if app.session_locked() {
        handle_session_lock_input(app, layout, event);
        return;
    }

    let keyboard = app
        .seat
        .get_keyboard()
        .expect("App::new always creates a keyboard");
    let pointer = app
        .seat
        .get_pointer()
        .expect("App::new always creates a pointer");
    let extent = layout.logical_extent();

    match event {
        InputEvent::Keyboard { event } => {
            if let Some(group) = active_group(app, layout) {
                handle_keyboard::<B>(app, &keyboard, group.size, event);
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
                handle_pointer_axis::<B>(app, &pointer, &group, &event);
            }
        }
        InputEvent::GestureSwipeBegin { event } => {
            activate_group_at(app, layout, pointer.current_location());
            // Three fingers are the compositor's global canvas gesture.
            // Client focus, grabs and pointer constraints must not turn it
            // back into an application gesture; session lock has its own
            // fail-closed input path above this dispatcher.
            let compositor_owned = event.fingers() == 3 && app.drag.is_none();
            app.canvas_swipe_active = compositor_owned;
            if compositor_owned {
                app.active_view_mut().animation = None;
            } else {
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
            if app.canvas_swipe_active {
                let view = app.active_view_mut();
                view.viewport.center =
                    gesture_pan_center(view.viewport.center, event.delta(), view.viewport.zoom);
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
        InputEvent::GestureSwipeEnd { event } if !std::mem::take(&mut app.canvas_swipe_active) => {
            pointer.gesture_swipe_end(
                app,
                &PointerSwipeEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                },
            );
        }
        InputEvent::GestureSwipeEnd { .. } => {}
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
        .map(|group| InputGroup {
            origin: group.global_location,
            size: group.logical_size,
        })
}

fn activate_group_at(app: &mut App, layout: &OutputLayout, point: Point<f64, Logical>) {
    if app.drag.is_some() {
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

fn handle_pointer_motion(
    app: &mut App,
    pointer: &PointerHandle<App>,
    group: &InputGroup,
    pointer_pos: Point<f64, Logical>,
    time: u32,
    relative: Option<RelativeMotion>,
) {
    if let Some(drag) = app.drag.clone() {
        apply_drag(app, &drag, pointer_pos);
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
    let world_pos = viewport.group_logical_to_world(local_point(group, pointer_pos), group.size);
    let (window, surface, surface_offset) = app.active_canvas().surface_at(world_pos)?;
    let root_offset = viewport.to_group_logical(window.position, group.size);
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
    let world_pos = viewport.group_logical_to_world(local_point(group, pointer_pos), group.size);

    if app
        .active_canvas_mut()
        .dispatch_pinned_pointer(world_pos, PinnedLayer::AboveWindows, &event)
        == InputHandled::Captured
    {
        return InputHandled::Captured;
    }
    if app.active_canvas().window_at(world_pos).is_some() {
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
    let world_pos = viewport.group_logical_to_world(local_point(group, pointer_pos), group.size);
    let (window, candidate, surface_offset) = app.active_canvas().surface_at(world_pos)?;
    if &candidate != surface {
        return None;
    }
    let root_offset = viewport.to_group_logical(window.position, group.size);
    let surface_offset = surface_offset.to_f64().upscale(viewport.zoom);
    Some((
        surface.clone(),
        group.origin.to_f64() + root_offset + surface_offset,
    ))
}

fn handle_keyboard<B: InputBackend>(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    group_size: Size<i32, Logical>,
    event: B::KeyboardKeyEvent,
) {
    let serial = SERIAL_COUNTER.next_serial();
    let key_state = event.state();
    let bindings = app.interaction_settings.lock().unwrap().keybindings;
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

            if handle_vt_switch(app, modifiers, sym, &raw_syms, pressed) {
                return FilterResult::Intercept(());
            }
            if sym == Keysym::Escape {
                if pressed && app.active_view().in_overview {
                    exit_overview(app, None);
                }
                return FilterResult::Intercept(());
            }
            if modifiers.logo && sym == bindings.overview {
                if pressed {
                    toggle_overview(app, group_size);
                }
                return FilterResult::Intercept(());
            }
            if modifiers.logo && sym == bindings.close_window {
                if pressed {
                    close_focused_window(app);
                }
                return FilterResult::Intercept(());
            }
            if modifiers.alt && sym == bindings.cycle_focus {
                if pressed {
                    cycle_focus(app);
                }
                return FilterResult::Intercept(());
            }
            if modifiers.logo && !app.active_view().in_overview {
                let step = match sym {
                    Keysym::Left => Some((-PAN_STEP, 0.0)),
                    Keysym::Right => Some((PAN_STEP, 0.0)),
                    Keysym::Up => Some((0.0, -PAN_STEP)),
                    Keysym::Down => Some((0.0, PAN_STEP)),
                    _ => None,
                };
                if let Some((dx, dy)) = step {
                    if pressed {
                        let view = app.active_view_mut();
                        let base = view
                            .animation
                            .as_ref()
                            .map(Animation::target)
                            .unwrap_or(view.viewport);
                        let target = Viewport {
                            center: (base.center.x + dx, base.center.y + dy).into(),
                            zoom: base.zoom,
                        };
                        view.animation = Some(Animation::new(
                            view.viewport,
                            target,
                            KEYBOARD_PAN_TRANSITION,
                        ));
                    }
                    return FilterResult::Intercept(());
                }
            }
            if modifiers.logo
                && let Some(digit) = keysym.raw_syms().first().and_then(|sym| digit_value(*sym))
            {
                if pressed {
                    if modifiers.shift {
                        set_mark(app, digit);
                    } else {
                        jump_to_mark(app, digit);
                    }
                }
                return FilterResult::Intercept(());
            }
            FilterResult::Forward
        },
    );
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

fn digit_value(sym: Keysym) -> Option<u8> {
    let raw = sym.raw();
    if (keysyms::KEY_0..=keysyms::KEY_9).contains(&raw) {
        Some((raw - keysyms::KEY_0) as u8)
    } else {
        None
    }
}

fn close_focused_window(app: &mut App) {
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

fn cycle_focus(app: &mut App) {
    let current = focused_surface(app);
    let Some((next_surface, next_focus)) = app
        .active_canvas()
        .next_after(current.as_ref())
        .and_then(|window| Some((window.wl_surface()?, keyboard_target(window)?)))
    else {
        return;
    };
    app.active_canvas_mut().raise(&next_surface);
    if let Some(keyboard) = app.seat.get_keyboard() {
        keyboard.set_focus(app, Some(next_focus), SERIAL_COUNTER.next_serial());
    }
}

fn set_mark(app: &mut App, digit: u8) {
    let canvas = app.active_view().canvas.clone();
    let center = app.active_view().viewport.center;
    app.marks
        .entry(canvas.clone())
        .or_default()
        .insert(digit, center);
    marks::save(&app.marks);
    println!("nkdhr-canvas: set mark {digit} on canvas {canvas:?} at {center:?}");
}

fn jump_to_mark(app: &mut App, digit: u8) {
    let canvas = app.active_view().canvas.clone();
    let Some(&center) = app.marks.get(&canvas).and_then(|marks| marks.get(&digit)) else {
        return;
    };
    let view = app.active_view_mut();
    let target = Viewport { center, zoom: 1.0 };
    view.animation = Some(Animation::new(view.viewport, target, TRANSITION));
    view.in_overview = false;
}

fn toggle_overview(app: &mut App, group_size: Size<i32, Logical>) {
    if app.active_view().in_overview {
        exit_overview(app, None);
        return;
    }
    let viewport = app.active_view().viewport;
    let target = app
        .active_canvas()
        .bounding_rect()
        .map_or(viewport, |rect| Viewport::fit_group(rect, group_size));
    let view = app.active_view_mut();
    view.pre_overview_viewport = viewport;
    view.animation = Some(Animation::new(viewport, target, TRANSITION));
    view.in_overview = true;
}

fn exit_overview(app: &mut App, target: Option<Viewport>) {
    let view = app.active_view_mut();
    let target = target.unwrap_or(view.pre_overview_viewport);
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

    if app.active_view().in_overview {
        if button_state == ButtonState::Pressed && button_code == BTN_LEFT {
            handle_overview_click(app, pointer, group);
        }
        return;
    }

    if button_state == ButtonState::Released
        && let Some(drag) = app.drag.take()
    {
        finish_drag(app, &drag);
        return;
    }

    let pointer_pos = pointer.current_location();
    if !has_active_pointer_constraint(pointer)
        && dispatch_pinned_pointer(app, group, pointer_pos, |position| {
            PinnedPointerEvent::Button {
                position,
                button: button_code,
                state: button_state,
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
            viewport.group_logical_to_world(local_point(group, pointer_pos), group.size);
        let hit = app.active_canvas().window_at(world_pos).and_then(|window| {
            Some((
                window.wl_surface()?,
                window.position,
                window.size(),
                keyboard_target(window)?,
                window.content_rect().contains(world_pos),
            ))
        });

        if modifiers.logo && button_code == BTN_LEFT {
            if let Some((surface, position, _, _, _)) = hit {
                app.drag = Some(Drag::Move {
                    surface,
                    window_start: position,
                    pointer_start: pointer_pos,
                });
            }
            return;
        }
        if modifiers.logo && button_code == BTN_RIGHT {
            if let Some((surface, position, size, _, _)) = hit {
                app.drag = Some(Drag::Resize {
                    surface,
                    size_start: (size.w.max(1.0) as i32, size.h.max(1.0) as i32).into(),
                    window_start: position,
                    pointer_start: pointer_pos,
                    edge: ResizeEdge::BottomRight,
                });
            }
            return;
        }
        if button_code == BTN_LEFT {
            if let Some((surface, position, _, focus, content_hit)) = hit {
                app.active_canvas_mut().raise(&surface);
                keyboard.set_focus(app, Some(focus), SERIAL_COUNTER.next_serial());
                if !content_hit {
                    app.drag = Some(Drag::Move {
                        surface,
                        window_start: position,
                        pointer_start: pointer_pos,
                    });
                    return;
                }
            } else {
                app.drag = Some(Drag::Pan {
                    viewport_start: viewport.center,
                    pointer_start: pointer_pos,
                    zoom: viewport.zoom,
                });
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
    if !has_active_pointer_constraint(pointer)
        && dispatch_pinned_pointer(app, group, pointer.current_location(), |position| {
            PinnedPointerEvent::Axis {
                position,
                horizontal: dx,
                vertical: dy,
                time: event.time_msec(),
            }
        }) == InputHandled::Captured
    {
        return;
    }
    pointer.axis(app, axis_frame::<B>(event));
    pointer.frame(app);
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

fn gesture_pan_center(
    center: Point<f64, World>,
    delta: Point<f64, Logical>,
    zoom: f64,
) -> Point<f64, World> {
    (center.x - delta.x / zoom, center.y - delta.y / zoom).into()
}

fn handle_overview_click(app: &mut App, pointer: &PointerHandle<App>, group: &InputGroup) {
    let pointer_pos = pointer.current_location();
    let viewport = app.active_view().viewport;
    let world_pos = viewport.group_logical_to_world(local_point(group, pointer_pos), group.size);
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

fn apply_drag(app: &mut App, drag: &Drag, pointer_pos: Point<f64, Logical>) {
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

fn finish_drag(app: &mut App, drag: &Drag) {
    let Drag::Move { surface, .. } = drag else {
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
    let Some(window) =
        app.active_canvas().windows().iter().find(|window| {
            window.matches_surface(surface) && window.matches_surface(&pointer_focus)
        })
    else {
        return false;
    };
    app.drag = Some(Drag::Move {
        surface: surface.clone(),
        window_start: window.position,
        pointer_start: pointer.current_location(),
    });
    true
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
    let Some(window) =
        app.active_canvas().windows().iter().find(|window| {
            window.matches_surface(surface) && window.matches_surface(&pointer_focus)
        })
    else {
        return false;
    };
    let size = window.size();
    app.drag = Some(Drag::Resize {
        surface: surface.clone(),
        size_start: (
            size.w.round().max(1.0) as i32,
            size.h.round().max(1.0) as i32,
        )
            .into(),
        window_start: window.position,
        pointer_start: pointer.current_location(),
        edge,
    });
    true
}

fn focused_surface(app: &App) -> Option<WlSurface> {
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
}
