use std::time::Duration;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, ButtonState, Event, InputBackend, InputEvent, KeyState,
    KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};
use smithay::input::keyboard::{FilterResult, KeyboardHandle, Keysym, keysyms};
use smithay::input::pointer::{ButtonEvent, MotionEvent, PointerHandle};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Size};

use crate::canvas::marks;
use crate::canvas::output_group::OutputLayout;
use crate::canvas::world::{Animation, Drag, Viewport};
use crate::state::App;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const TRANSITION: Duration = Duration::from_millis(250);
const SCROLL_PAN_SPEED: f64 = 1.0;
const PAN_STEP: f64 = 80.0;

#[derive(Clone)]
struct InputGroup {
    origin: Point<i32, Logical>,
    size: Size<i32, Logical>,
}

/// Backend-independent compositor input dispatch. Backend code supplies the
/// resolved output layout; this layer maps compositor-global pointer
/// coordinates into the active output group's independent canvas view.
pub fn handle<B: InputBackend>(app: &mut App, layout: &OutputLayout, event: InputEvent<B>) {
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
            activate_group_at(app, layout, pointer_pos);
            if let Some(group) = active_group(app, layout) {
                handle_pointer_motion(app, &pointer, &group, pointer_pos, event.time_msec());
            }
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let pointer_pos = (event.x_transformed(extent.w), event.y_transformed(extent.h)).into();
            activate_group_at(app, layout, pointer_pos);
            if let Some(group) = active_group(app, layout) {
                handle_pointer_motion(app, &pointer, &group, pointer_pos, event.time_msec());
            }
        }
        InputEvent::PointerButton { event } => {
            activate_group_at(app, layout, pointer.current_location());
            if let Some(group) = active_group(app, layout) {
                handle_pointer_button::<B>(app, &keyboard, &pointer, &group, &event);
            }
        }
        InputEvent::PointerAxis { event } => handle_pointer_axis::<B>(app, &event),
        _ => {}
    }
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

fn local_point(group: &InputGroup, global: Point<f64, Logical>) -> Point<f64, Logical> {
    global - group.origin.to_f64()
}

fn handle_pointer_motion(
    app: &mut App,
    pointer: &PointerHandle<App>,
    group: &InputGroup,
    pointer_pos: Point<f64, Logical>,
    time: u32,
) {
    if let Some(drag) = app.drag.clone() {
        apply_drag(app, &drag, pointer_pos);
        return;
    }
    if app.active_view().in_overview {
        return;
    }

    let focus = pointer_focus(app, group);
    pointer.motion(
        app,
        focus,
        &MotionEvent {
            location: pointer_pos,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    pointer.frame(app);
}

fn pointer_focus(app: &App, group: &InputGroup) -> Option<(WlSurface, Point<f64, Logical>)> {
    let pointer_pos = app.seat.get_pointer()?.current_location();
    let viewport = app.active_view().viewport;
    let world_pos = viewport.group_logical_to_world(local_point(group, pointer_pos), group.size);
    let window = app.active_canvas().window_at(world_pos)?;
    let local_offset = viewport.to_group_logical(window.position, group.size);
    let offset = group.origin.to_f64() + local_offset;
    Some((window.surface.wl_surface().clone(), offset))
}

fn handle_keyboard<B: InputBackend>(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    group_size: Size<i32, Logical>,
    event: B::KeyboardKeyEvent,
) {
    let serial = SERIAL_COUNTER.next_serial();
    let key_state = event.state();
    let bindings = *app.keybindings.lock().unwrap();
    keyboard.input::<(), _>(
        app,
        event.key_code(),
        key_state,
        serial,
        event.time_msec(),
        move |app, modifiers, keysym| {
            let sym = keysym.modified_sym();
            let pressed = key_state == KeyState::Pressed;

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
                        view.viewport.center.x += dx;
                        view.viewport.center.y += dy;
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
        .find(|window| *window.surface.wl_surface() == focused)
    {
        window.surface.send_close();
    }
}

fn cycle_focus(app: &mut App) {
    let current = focused_surface(app);
    let Some(next_surface) = app
        .active_canvas()
        .next_after(current.as_ref())
        .map(|window| window.surface.wl_surface().clone())
    else {
        return;
    };
    app.active_canvas_mut().raise(&next_surface);
    if let Some(keyboard) = app.seat.get_keyboard() {
        keyboard.set_focus(app, Some(next_surface), SERIAL_COUNTER.next_serial());
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

    if button_state == ButtonState::Released {
        if app.drag.take().is_some() {
            return;
        }
    } else {
        let modifiers = keyboard.modifier_state();
        let pointer_pos = pointer.current_location();
        let viewport = app.active_view().viewport;
        let world_pos =
            viewport.group_logical_to_world(local_point(group, pointer_pos), group.size);
        let hit = app.active_canvas().window_at(world_pos).map(|window| {
            (
                window.surface.wl_surface().clone(),
                window.position,
                window.size(),
            )
        });

        if modifiers.logo && button_code == BTN_LEFT {
            if let Some((surface, position, _)) = hit {
                app.drag = Some(Drag::Move {
                    surface,
                    window_start: position,
                    pointer_start: pointer_pos,
                });
            }
            return;
        }
        if modifiers.logo && button_code == BTN_RIGHT {
            if let Some((surface, _, size)) = hit {
                app.drag = Some(Drag::Resize {
                    surface,
                    size_start: (size.w.max(1.0) as i32, size.h.max(1.0) as i32).into(),
                    pointer_start: pointer_pos,
                });
            }
            return;
        }
        if button_code == BTN_LEFT {
            if let Some((surface, _, _)) = hit {
                app.active_canvas_mut().raise(&surface);
                keyboard.set_focus(app, Some(surface), SERIAL_COUNTER.next_serial());
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

fn handle_pointer_axis<B: InputBackend>(app: &mut App, event: &B::PointerAxisEvent) {
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
    let view = app.active_view_mut();
    view.viewport.center.x += dx * SCROLL_PAN_SPEED / view.viewport.zoom;
    view.viewport.center.y += dy * SCROLL_PAN_SPEED / view.viewport.zoom;
}

fn handle_overview_click(app: &mut App, pointer: &PointerHandle<App>, group: &InputGroup) {
    let pointer_pos = pointer.current_location();
    let viewport = app.active_view().viewport;
    let world_pos = viewport.group_logical_to_world(local_point(group, pointer_pos), group.size);
    let Some((surface, center)) = app
        .active_canvas()
        .window_at(world_pos)
        .map(|window| (window.surface.wl_surface().clone(), window.center()))
    else {
        exit_overview(app, None);
        return;
    };
    let target = Viewport { center, zoom: 1.0 };
    app.active_canvas_mut().raise(&surface);
    exit_overview(app, Some(target));
    if let Some(keyboard) = app.seat.get_keyboard() {
        keyboard.set_focus(app, Some(surface), SERIAL_COUNTER.next_serial());
    }
}

fn apply_drag(app: &mut App, drag: &Drag, pointer_pos: Point<f64, Logical>) {
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
            app.active_canvas_mut()
                .set_position(surface, *window_start + delta);
        }
        Drag::Resize {
            surface,
            size_start,
            pointer_start,
        } => {
            let delta = pointer_pos - *pointer_start;
            let new_size = Size::from((
                (f64::from(size_start.w) + delta.x).max(1.0) as i32,
                (f64::from(size_start.h) + delta.y).max(1.0) as i32,
            ));
            if let Some(toplevel) = app
                .active_canvas()
                .windows()
                .iter()
                .find(|window| window.surface.wl_surface() == surface)
                .map(|window| window.surface.clone())
            {
                toplevel.with_pending_state(|state| state.size = Some(new_size));
                toplevel.send_configure();
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

fn focused_surface(app: &App) -> Option<WlSurface> {
    app.seat.get_keyboard()?.current_focus()
}
