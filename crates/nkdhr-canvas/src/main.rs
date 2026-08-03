mod state;

use std::sync::Arc;
use std::time::Instant;

use smithay::backend::input::{
    AbsolutePositionEvent, Event, InputEvent, KeyboardKeyEvent, PointerButtonEvent,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, ImportDma, Renderer};
use smithay::backend::winit::{self as smithay_winit, WinitEvent, WinitInput};
use smithay::input::SeatState;
use smithay::input::keyboard::{FilterResult, KeyboardHandle};
use smithay::input::pointer::{ButtonEvent, MotionEvent, PointerHandle};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Physical, Rectangle, SERIAL_COUNTER, Size, Transform};
use smithay::wayland::compositor::{
    CompositorState, SurfaceAttributes, TraversalAction, with_surface_tree_downward,
};
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;

use state::{App, ClientState};

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
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .flat_map(|surface| {
                    render_elements_from_surface_tree(
                        renderer,
                        surface.wl_surface(),
                        (0, 0),
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
        for surface in app.xdg_shell_state.toplevel_surfaces() {
            send_frame_callbacks(surface.wl_surface(), frame_time);
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
        InputEvent::Keyboard { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            keyboard.input::<(), _>(
                app,
                event.key_code(),
                event.state(),
                serial,
                event.time_msec(),
                |_, _, _| FilterResult::Forward,
            );
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let location = (
                event.x_transformed(window_size.w),
                event.y_transformed(window_size.h),
            )
                .into();
            let focus = focused_surface(app).map(|surface| (surface, (0.0, 0.0).into()));
            pointer.motion(
                app,
                focus,
                &MotionEvent {
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
            pointer.frame(app);
        }
        InputEvent::PointerButton { event } => {
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
        InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } => {}
        other => println!("nkdhr-canvas: input {other:?}"),
    }
}

/// COMP-2 has no click-to-focus or window placement yet (COMP-3), so
/// pointer events target whichever surface currently has keyboard focus —
/// consistent with [`state::App::new_toplevel`]'s "newest window wins"
/// placeholder policy.
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
