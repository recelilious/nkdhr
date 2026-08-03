mod keybindings;
mod state;
mod world;

use std::sync::Arc;
use std::time::Instant;

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent,
    PointerButtonEvent,
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
};
use smithay::input::SeatState;
use smithay::input::keyboard::{FilterResult, KeyboardHandle};
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
use world::{Canvas, Drag, logical_delta_to_world, logical_to_world};

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
    };

    let listener = ListeningSocket::bind_auto("wayland", 0..32)?;
    let socket_name = listener
        .socket_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    println!("nkdhr-canvas: listening on WAYLAND_DISPLAY={socket_name}");
    let mut clients: Vec<Client> = Vec::new();

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

        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend.bind()?;
            let elements = app
                .canvas
                .windows()
                .iter()
                .flat_map(|window| {
                    // COMP-3 has no viewport yet (fixed at world origin,
                    // 1:1 — COMP-4 introduces pan/zoom), so a window's
                    // world position *is* its on-screen offset today.
                    let offset: Point<i32, Physical> = (
                        window.position.x.round() as i32,
                        window.position.y.round() as i32,
                    )
                        .into();
                    render_elements_from_surface_tree(
                        renderer,
                        window.surface.wl_surface(),
                        offset,
                        1.0,
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

fn handle_input(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    pointer: &PointerHandle<App>,
    window_size: Size<i32, Physical>,
    event: InputEvent<WinitInput>,
) {
    match event {
        InputEvent::Keyboard { event } => handle_keyboard(app, keyboard, event),
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
            handle_pointer_button(app, keyboard, pointer, &event);
        }
        InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } => {}
        other => println!("nkdhr-canvas: input {other:?}"),
    }
}

/// Intercepts the `close_window`/`cycle_focus` keybindings (CTRL-5's
/// `canvas` namespace, hot-reloadable — see `crate::keybindings`) before
/// forwarding anything else to the focused client as normal.
fn handle_keyboard(app: &mut App, keyboard: &KeyboardHandle<App>, event: WinitKeyboardInputEvent) {
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
            if modifiers.logo && sym == bindings.close_window {
                if key_state == KeyState::Pressed {
                    close_focused_window(app);
                }
                return FilterResult::Intercept(());
            }
            if modifiers.alt && sym == bindings.cycle_focus {
                if key_state == KeyState::Pressed {
                    cycle_focus(app);
                }
                return FilterResult::Intercept(());
            }
            FilterResult::Forward
        },
    );
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

fn handle_pointer_button(
    app: &mut App,
    keyboard: &KeyboardHandle<App>,
    pointer: &PointerHandle<App>,
    event: &WinitMouseInputEvent,
) {
    let button_state = event.state();
    let button_code = event.button_code();

    if button_state == ButtonState::Released {
        if app.drag.take().is_some() {
            return;
        }
    } else {
        let modifiers = keyboard.modifier_state();
        let pointer_pos = pointer.current_location();
        let world_pos = logical_to_world(pointer_pos);

        if modifiers.logo && button_code == BTN_LEFT {
            if let Some(window) = app.canvas.window_at(world_pos) {
                app.drag = Some(Drag::Move {
                    surface: window.surface.wl_surface().clone(),
                    window_start: window.position,
                    pointer_start: pointer_pos,
                });
            }
            return;
        }
        if modifiers.logo && button_code == BTN_RIGHT {
            if let Some(window) = app.canvas.window_at(world_pos) {
                let size = window.size();
                app.drag = Some(Drag::Resize {
                    surface: window.surface.wl_surface().clone(),
                    size_start: (size.w.max(1.0) as i32, size.h.max(1.0) as i32).into(),
                    pointer_start: pointer_pos,
                });
            }
            return;
        }
        if button_code == BTN_LEFT
            && let Some(window) = app.canvas.window_at(world_pos)
        {
            let surface = window.surface.wl_surface().clone();
            app.canvas.raise(&surface);
            let serial = SERIAL_COUNTER.next_serial();
            keyboard.set_focus(app, Some(surface), serial);
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

/// Applies one motion update to an in-progress `super+drag` move or
/// resize — moving updates the window's world position directly; resizing
/// re-requests an `xdg_toplevel` configure at the new size and lets the
/// client's own next commit (picked up automatically by the render loop's
/// `ManagedWindow::size`) supply the actual new buffer.
fn apply_drag(app: &mut App, drag: &Drag, pointer_pos: Point<f64, Logical>) {
    match drag {
        Drag::Move {
            surface,
            window_start,
            pointer_start,
        } => {
            let delta = pointer_pos - *pointer_start;
            let new_position = *window_start + logical_delta_to_world(delta);
            app.canvas.set_position(surface, new_position);
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
