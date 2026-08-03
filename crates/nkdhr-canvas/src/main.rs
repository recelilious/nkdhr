mod keybindings;
mod marks;
mod state;
mod world;

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent,
    PointerAxisEvent, PointerButtonEvent,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, ImportDma, Renderer};
use smithay::backend::winit::{
    self as smithay_winit, WinitEvent, WinitInput, WinitKeyboardInputEvent, WinitMouseInputEvent,
    WinitMouseWheelEvent,
};
use smithay::input::SeatState;
use smithay::input::keyboard::{FilterResult, KeyboardHandle, Keysym, keysyms};
use smithay::input::pointer::{ButtonEvent, MotionEvent, PointerHandle};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Size, Transform};
use smithay::wayland::compositor::{
    CompositorState, SurfaceAttributes, TraversalAction, with_surface_tree_downward,
};
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;
use world::{Animation, Canvas, Drag, Viewport};

use state::{App, ClientState};

/// Linux input-event-codes (`linux/input-event-codes.h`) for the two
/// pointer buttons `super`-modified drags use. Smithay doesn't name these
/// itself — see `PointerButtonEvent::button_code`'s own winit-backend
/// implementation, which maps to exactly these values.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// Flat mid-tone blue-grey; there is no theming system yet (that's UI-4,
/// Phase 3) — this exists only to make "the canvas is actually rendering"
/// visible while nested, not as a real design choice.
const CANVAS_BACKGROUND: Color32F = Color32F::new(0.11, 0.12, 0.16, 1.0);

/// How long an overview enter/exit or mark jump takes to animate. Not
/// configurable yet — nothing has asked for that, and a single constant
/// is enough to prove animated transitions work at all (ROADMAP.md's
/// COMP-4 bullet).
const TRANSITION: Duration = Duration::from_millis(250);

/// Scroll-wheel/trackpad-scroll pan speed: world units panned per pixel
/// of `PointerAxisEvent::amount`. Picked to feel roughly like panning a
/// map at 1:1 zoom; not derived from anything more principled.
const SCROLL_PAN_SPEED: f64 = 1.0;

/// World units `super+arrow` pans per keypress. Fixed-step, not
/// smooth/held-repeat panning — Smithay's per-key repeat machinery is
/// about re-delivering a key *to a client*, not about the compositor
/// itself reacting continuously to a held intercepted key, and building a
/// separate repeat timer for this one binding isn't worth it while
/// pointer-drag and scroll already cover smooth panning.
const PAN_STEP: f64 = 80.0;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, mut event_loop) = smithay_winit::init_from_attributes::<GlesRenderer>(
        WinitWindow::default_attributes()
            .with_title("nkdhr-canvas")
            .with_inner_size(LogicalSize::new(1280.0, 800.0)),
    )?;

    let mut display: Display<App> = Display::new()?;
    let dh = display.handle();

    // Some clients (`foot`, at least) refuse to render at all without at
    // least one `wl_output` — real per-output modelling (multiple
    // outputs, output groups) is COMP-4/5's job; this is the smallest
    // real output that satisfies clients today, one output matching the
    // nested window's own size.
    let output = Output::new(
        "nkdhr-canvas-nested-0".to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "nkdhr".to_owned(),
            model: "nested".to_owned(),
        },
    );
    let initial_size = backend.window_size();
    output.change_current_state(
        Some(Mode {
            size: initial_size,
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    output.set_preferred(Mode {
        size: initial_size,
        refresh: 60_000,
    });
    output.create_global::<App>(&dh);

    let dmabuf_formats = backend.renderer().dmabuf_formats();
    let mut dmabuf_state = DmabufState::new();
    dmabuf_state.create_global::<App>(&dh, dmabuf_formats);

    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "nkdhr-canvas");
    let keyboard = seat.add_keyboard(Default::default(), 200, 200)?;
    let pointer = seat.add_pointer();

    let mut app = App {
        start_time: Instant::now(),
        compositor_state: CompositorState::new::<App>(&dh),
        xdg_shell_state: XdgShellState::new::<App>(&dh),
        shm_state: ShmState::new::<App>(&dh, Vec::new()),
        dmabuf_state,
        seat_state,
        data_device_state: DataDeviceState::new::<App>(&dh),
        seat,
        canvas: Canvas::new(),
        drag: None,
        keybindings: keybindings::watch(),
        viewport: Viewport::WORK,
        in_overview: false,
        pre_overview_viewport: Viewport::WORK,
        animation: None,
        marks: marks::load(),
    };
    println!("nkdhr-canvas: loaded {} saved mark(s)", app.marks.len());

    let listener = ListeningSocket::bind_auto("wayland", 0..32)?;
    let socket_name = listener
        .socket_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    println!("nkdhr-canvas: listening on WAYLAND_DISPLAY={socket_name}");
    let mut clients: Vec<Client> = Vec::new();

    // Lightweight, always-on frame-time reporting — not a one-off test
    // hook: a compositor's frame pacing is exactly the kind of thing
    // worth being able to check at a glance later too, not just during
    // COMP-4's own "measured frame times" verification (ROADMAP.md).
    let mut frame_count: u32 = 0;
    let mut fps_window_start = Instant::now();

    loop {
        let mut should_exit = false;
        // `WinitEvent::CloseRequested` does not exit the loop by itself —
        // it is just delivered like any other event — so the exit flag
        // above is what actually makes the window's close button work.
        let window_size = backend.window_size();
        let status = event_loop.dispatch_new_events(|event| match event {
            WinitEvent::CloseRequested => should_exit = true,
            WinitEvent::Resized { size, scale_factor } => {
                println!("nkdhr-canvas: resized to {size:?} @ {scale_factor}x");
            }
            WinitEvent::Input(event) => {
                handle_input(&mut app, &keyboard, &pointer, window_size, event)
            }
            WinitEvent::Focus(_) | WinitEvent::Redraw => {}
        });
        if should_exit || matches!(status, PumpStatus::Exit(_)) {
            return Ok(());
        }

        advance_animation(&mut app);

        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend.bind()?;
            let elements = app
                .canvas
                .windows()
                .iter()
                .flat_map(|window| {
                    let offset = app.viewport.to_screen(window.position, size);
                    render_elements_from_surface_tree(
                        renderer,
                        window.surface.wl_surface(),
                        offset,
                        app.viewport.zoom,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();

            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(CANVAS_BACKGROUND, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
            // The returned `SyncPoint` matters for damage-tracked partial
            // redraws to know when the GPU is actually done; nkdhr-canvas
            // clears and redraws in full every frame, so there is nothing
            // to wait on it for yet.
            let _ = frame.finish()?;
        }
        backend.submit(Some(&[damage]))?;

        let frame_time = app.start_time.elapsed().as_millis() as u32;
        for window in app.canvas.windows() {
            send_frame_callbacks(window.surface.wl_surface(), frame_time);
        }

        frame_count += 1;
        let fps_elapsed = fps_window_start.elapsed();
        if fps_elapsed >= Duration::from_secs(5) {
            let fps = f64::from(frame_count) / fps_elapsed.as_secs_f64();
            let avg_frame_ms = fps_elapsed.as_secs_f64() * 1000.0 / f64::from(frame_count);
            println!(
                "nkdhr-canvas: {fps:.1} fps ({avg_frame_ms:.2} ms/frame avg over {frame_count} frames, {} windows)",
                app.canvas.windows().len()
            );
            frame_count = 0;
            fps_window_start = Instant::now();
        }

        if let Some(stream) = listener.accept()? {
            let client = display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
            clients.push(client);
        }

        display.dispatch_clients(&mut app)?;
        display.flush_clients()?;
    }
}

/// Advances `app.animation` by one frame, snapping to its target and
/// clearing it once done. Called once per render loop iteration, before
/// rendering — this is COMP-4's whole "animated transitions" mechanism,
/// no separate timer or animation engine.
fn advance_animation(app: &mut App) {
    let Some(animation) = &app.animation else {
        return;
    };
    match animation.advance(Instant::now()) {
        Some(viewport) => app.viewport = viewport,
        None => {
            app.viewport = animation.target();
            app.animation = None;
        }
    }
}

fn handle_input(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    pointer: &PointerHandle<App>,
    window_size: Size<i32, Physical>,
    event: InputEvent<WinitInput>,
) {
    match event {
        InputEvent::Keyboard { event } => handle_keyboard(app, keyboard, window_size, event),
        InputEvent::PointerMotionAbsolute { event } => {
            let pointer_pos: Point<f64, Logical> = (
                event.x_transformed(window_size.w),
                event.y_transformed(window_size.h),
            )
                .into();

            if let Some(drag) = app.drag.clone() {
                apply_drag(app, &drag, pointer_pos);
                return;
            }
            if app.in_overview {
                // No client should react to pointer motion while the
                // overview is showing — there's nothing to interact with
                // until a window is picked (or the overview is cancelled).
                return;
            }

            let focus = focused_surface(app).map(|surface| (surface, (0.0, 0.0).into()));
            pointer.motion(
                app,
                focus,
                &MotionEvent {
                    location: pointer_pos,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
            pointer.frame(app);
        }
        InputEvent::PointerButton { event } => {
            handle_pointer_button(app, keyboard, pointer, window_size, &event);
        }
        InputEvent::PointerAxis { event } => handle_pointer_axis(app, &event),
        InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } => {}
        other => println!("nkdhr-canvas: input {other:?}"),
    }
}

/// Intercepts every compositor-level keybinding — `Escape` (fixed, cancels
/// overview), `overview`/`close_window`/`cycle_focus` (CTRL-5's `canvas`
/// namespace, hot-reloadable — `crate::keybindings`), and the mark
/// set/jump bindings (`super+shift+<0-9>`/`super+<0-9>`, fixed digits,
/// only the modifier-vs-action split is meaningful to make configurable)
/// — before forwarding anything else to the focused client as normal.
fn handle_keyboard(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    window_size: Size<i32, Physical>,
    event: WinitKeyboardInputEvent,
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
                if pressed && app.in_overview {
                    exit_overview(app, None);
                }
                return FilterResult::Intercept(());
            }
            if modifiers.logo && sym == bindings.overview {
                if pressed {
                    toggle_overview(app, window_size);
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
            if modifiers.logo && !app.in_overview {
                // `super+arrow` is the keyboard half of ROADMAP.md's
                // COMP-4 pan bullet ("keyboard / pointer / touchpad
                // gestures") — deliberately modifier-gated rather than
                // bare arrow keys, which any focused client (a text
                // field, a terminal's readline, ...) already needs for
                // its own cursor movement.
                let step = match sym {
                    Keysym::Left => Some((-PAN_STEP, 0.0)),
                    Keysym::Right => Some((PAN_STEP, 0.0)),
                    Keysym::Up => Some((0.0, -PAN_STEP)),
                    Keysym::Down => Some((0.0, PAN_STEP)),
                    _ => None,
                };
                if let Some((dx, dy)) = step {
                    if pressed {
                        app.viewport.center.x += dx;
                        app.viewport.center.y += dy;
                    }
                    return FilterResult::Intercept(());
                }
            }
            if modifiers.logo {
                // Digits are matched on their *raw*, unshifted level (not
                // `modified_sym`) so `super+shift+3` still means "mark 3",
                // not whatever shift turns "3" into on the active layout.
                if let Some(digit) = keysym.raw_syms().first().and_then(|sym| digit_value(*sym)) {
                    if pressed {
                        if modifiers.shift {
                            set_mark(app, digit);
                        } else {
                            jump_to_mark(app, digit);
                        }
                    }
                    return FilterResult::Intercept(());
                }
            }
            FilterResult::Forward
        },
    );
}

/// `Keysym::KEY_0..=KEY_9` are contiguous (`0x30..=0x39`, matching ASCII)
/// — see `xkbcommon::xkb::keysyms`' own doc comments for those constants.
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
        .canvas
        .windows()
        .iter()
        .find(|window| *window.surface.wl_surface() == focused)
    {
        window.surface.send_close();
    }
}

fn cycle_focus(app: &mut App) {
    let current = focused_surface(app);
    let Some(next) = app.canvas.next_after(current.as_ref()) else {
        return;
    };
    let next_surface = next.surface.wl_surface().clone();
    app.canvas.raise(&next_surface);
    if let Some(keyboard) = app.seat.get_keyboard() {
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(app, Some(next_surface), serial);
    }
}

/// `super+<digit>`: records the current viewport center under `digit` and
/// persists it (`crate::marks::save`) so it survives a restart.
fn set_mark(app: &mut App, digit: u8) {
    app.marks.insert(digit, app.viewport.center);
    marks::save(&app.marks);
    println!(
        "nkdhr-canvas: set mark {digit} at {:?}",
        app.viewport.center
    );
}

/// `super+shift+<digit>`: animates the viewport to a previously set mark.
/// A jump while in overview also exits it, landing back in the normal
/// work state at that mark — matching how clicking a window in overview
/// behaves.
fn jump_to_mark(app: &mut App, digit: u8) {
    let Some(&center) = app.marks.get(&digit) else {
        return;
    };
    let target = Viewport { center, zoom: 1.0 };
    app.animation = Some(Animation::new(app.viewport, target, TRANSITION));
    app.in_overview = false;
}

/// Enters or exits the zoomed-out overview state, animated either way —
/// `super+overview` (default `o`)'s entire implementation.
fn toggle_overview(app: &mut App, window_size: Size<i32, Physical>) {
    if app.in_overview {
        exit_overview(app, None);
        return;
    }
    app.pre_overview_viewport = app.viewport;
    let target = app
        .canvas
        .bounding_rect()
        .map_or(app.viewport, |rect| Viewport::fit(rect, window_size));
    app.animation = Some(Animation::new(app.viewport, target, TRANSITION));
    app.in_overview = true;
}

/// Leaves overview, animating back to `target` — the viewport centered on
/// a picked window (`Some`, from clicking in overview) or back to
/// wherever the view was before overview was entered (`None`, from
/// `Escape` or clicking empty space).
fn exit_overview(app: &mut App, target: Option<Viewport>) {
    let target = target.unwrap_or(app.pre_overview_viewport);
    app.animation = Some(Animation::new(app.viewport, target, TRANSITION));
    app.in_overview = false;
}

fn handle_pointer_button(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    pointer: &PointerHandle<App>,
    window_size: Size<i32, Physical>,
    event: &WinitMouseInputEvent,
) {
    let button_state = event.state();
    let button_code = event.button_code();

    if app.in_overview {
        if button_state == ButtonState::Pressed && button_code == BTN_LEFT {
            handle_overview_click(app, pointer, window_size);
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
        let world_pos = app.viewport.to_world(pointer_pos, window_size);
        let window = app.canvas.window_at(world_pos);

        if modifiers.logo && button_code == BTN_LEFT {
            if let Some(window) = window {
                app.drag = Some(Drag::Move {
                    surface: window.surface.wl_surface().clone(),
                    window_start: window.position,
                    pointer_start: pointer_pos,
                });
            }
            return;
        }
        if modifiers.logo && button_code == BTN_RIGHT {
            if let Some(window) = window {
                let size = window.size();
                app.drag = Some(Drag::Resize {
                    surface: window.surface.wl_surface().clone(),
                    size_start: (size.w.max(1.0) as i32, size.h.max(1.0) as i32).into(),
                    pointer_start: pointer_pos,
                });
            }
            return;
        }
        if button_code == BTN_LEFT {
            if let Some(window) = window {
                let surface = window.surface.wl_surface().clone();
                app.canvas.raise(&surface);
                let serial = SERIAL_COUNTER.next_serial();
                keyboard.set_focus(app, Some(surface), serial);
            } else {
                // A plain drag on empty canvas pans — pointer/touchpad
                // panning's whole implementation (keyboard/scroll pan are
                // handled separately, see `handle_pointer_axis`).
                app.drag = Some(Drag::Pan {
                    viewport_start: app.viewport.center,
                    pointer_start: pointer_pos,
                    zoom: app.viewport.zoom,
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

/// Scroll-wheel or trackpad two-finger scroll pans the viewport — the
/// "touchpad gestures" half of ROADMAP.md's COMP-4 pan bullet. The winit
/// backend doesn't deliver real multi-touch gesture events (its
/// `InputBackend` impl types every `Gesture*Event` as `UnusedEvent` — real
/// gesture support needs COMP-5's TTY/libinput backend), but two-finger
/// trackpad scrolling already arrives as ordinary scroll/axis events at
/// the OS level, which this handles today.
fn handle_pointer_axis(app: &mut App, event: &WinitMouseWheelEvent) {
    if app.in_overview || app.drag.is_some() {
        return;
    }
    let dx = event
        .amount(smithay::backend::input::Axis::Horizontal)
        .or_else(|| {
            event
                .amount_v120(smithay::backend::input::Axis::Horizontal)
                .map(|v120| v120 / 120.0 * 20.0)
        })
        .unwrap_or(0.0);
    let dy = event
        .amount(smithay::backend::input::Axis::Vertical)
        .or_else(|| {
            event
                .amount_v120(smithay::backend::input::Axis::Vertical)
                .map(|v120| v120 / 120.0 * 20.0)
        })
        .unwrap_or(0.0);
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    app.viewport.center.x += dx * SCROLL_PAN_SPEED / app.viewport.zoom;
    app.viewport.center.y += dy * SCROLL_PAN_SPEED / app.viewport.zoom;
}

/// Hit-tests a click against the overview's current (possibly
/// mid-animation) viewport: a window under the pointer zooms to it at 1:1
/// and exits overview; empty space cancels overview instead, same as
/// `Escape`.
fn handle_overview_click(
    app: &mut App,
    pointer: &PointerHandle<App>,
    window_size: Size<i32, Physical>,
) {
    let pointer_pos = pointer.current_location();
    let world_pos = app.viewport.to_world(pointer_pos, window_size);
    let Some(window) = app.canvas.window_at(world_pos) else {
        exit_overview(app, None);
        return;
    };
    let target = Viewport {
        center: window.center(),
        zoom: 1.0,
    };
    let surface = window.surface.wl_surface().clone();
    app.canvas.raise(&surface);
    exit_overview(app, Some(target));
    if let Some(keyboard) = app.seat.get_keyboard() {
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(app, Some(surface), serial);
    }
}

/// Applies one motion update to an in-progress `super+drag` move/resize
/// or empty-canvas pan — moving updates the window's world position
/// directly; resizing re-requests an `xdg_toplevel` configure at the new
/// size and lets the client's own next commit (picked up automatically by
/// the render loop's `ManagedWindow::size`) supply the actual new buffer;
/// panning updates the viewport's center the same way moving updates a
/// window's position.
fn apply_drag(app: &mut App, drag: &Drag, pointer_pos: Point<f64, Logical>) {
    match drag {
        Drag::Move {
            surface,
            window_start,
            pointer_start,
        } => {
            let delta = app.viewport.to_world_delta(pointer_pos - *pointer_start);
            app.canvas.set_position(surface, *window_start + delta);
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
                .canvas
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
            // Dragging right should reveal content to the left — the
            // viewport moves opposite the drag, like grabbing a map.
            app.viewport.center = (
                viewport_start.x - delta.x / zoom,
                viewport_start.y - delta.y / zoom,
            )
                .into();
        }
    }
}

/// The currently keyboard-focused surface, if any — where plain (no
/// modifier) pointer motion routes to when no `super+drag` interaction is
/// in progress.
fn focused_surface(app: &App) -> Option<WlSurface> {
    app.seat.get_keyboard()?.current_focus()
}

/// Walks a surface's (sub)surface tree and fires every pending
/// `wl_surface.frame` callback, so clients keep rendering instead of
/// blocking on a callback that will never come.
fn send_frame_callbacks(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}
