//! The nested backend: the compositor as one window inside an existing X11
//! or Wayland desktop, for development.
//!
//! The window is one output. Every iteration redraws the whole window and
//! swaps as fast as the host accepts; there is no damage tracking or frame
//! pacing, which is acceptable for a development backend.

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{ExportMem, Frame, ImportDma, Renderer, TextureMapping};
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::EventLoop as CalloopEventLoop;
use smithay::reexports::wayland_server::{Display, ListeningSocket};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Physical, Rectangle, Size, Transform};
use smithay::wayland::dmabuf::DmabufState;
use smithay::xwayland::{X11Wm, XWayland, XWaylandEvent};

use crate::backends::{Backend, BackendResult, SourceGuard};
use crate::input;
use crate::outputs::{ConnectedOutput, OutputLayout, OutputsConfig};
use crate::protocols::SCREENCOPY_FORMAT;
use crate::render;
use crate::shutdown::{self, ShutdownSignal};
use crate::state::{App, ClientState};

const NESTED_OUTPUT_NAME: &str = "nkdhr-compositor-nested-0";

pub struct WinitBackend;

impl Backend for WinitBackend {
    fn run(self, shutdown: &ShutdownSignal) -> BackendResult {
        run(shutdown)
    }
}

fn resolve_layout(config: &OutputsConfig, size: Size<i32, Physical>) -> OutputLayout {
    OutputLayout::resolve(
        &config.snapshot(),
        &[ConnectedOutput {
            name: NESTED_OUTPUT_NAME.to_owned(),
            physical_size: size,
        }],
    )
}

/// Advertises the nested output's mode, scale and position, and hands the
/// layout to `App`.
fn apply_layout(app: &mut App, output: &Output, layout: &OutputLayout, size: Size<i32, Physical>) {
    let resolved = layout
        .output(NESTED_OUTPUT_NAME)
        .expect("the connected nested output always resolves");
    let mode = Mode {
        size,
        refresh: 60_000,
    };
    output.change_current_state(
        Some(mode),
        None,
        Some(Scale::Fractional(resolved.scale)),
        Some(resolved.global_location),
    );
    output.set_preferred(mode);
    app.set_output_layout(layout.clone());
    println!(
        "nkdhr-compositor: output {NESTED_OUTPUT_NAME} is {}x{} at scale {}",
        size.w, size.h, resolved.scale
    );
}

/// What must be released when the compositor stops is owned by locals of
/// this function, so that returning from it in any way releases it: the
/// listening socket is a plain `ListeningSocket` outside any event loop,
/// and the XWayland source, which owns the X11 lock, is held through a
/// [`SourceGuard`], because the XWayland loop outlives this function once
/// the X11 window manager has started. Both are declared late and are
/// therefore dropped first.
fn run(shutdown: &ShutdownSignal) -> BackendResult {
    let (mut backend, mut event_loop) = smithay_winit::init_from_attributes::<GlesRenderer>(
        WinitWindow::default_attributes()
            .with_title("nkdhr-compositor")
            .with_inner_size(LogicalSize::new(1280.0, 800.0)),
    )?;
    backend.window().set_cursor_visible(false);

    let mut display: Display<App> = Display::new()?;
    let mut display_handle = display.handle();

    let output = Output::new(
        NESTED_OUTPUT_NAME.to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "nkdhr".to_owned(),
            model: "nested".to_owned(),
        },
    );
    let mut size = backend.window_size();
    output.change_current_state(
        Some(Mode {
            size,
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    output.create_global::<App>(&display_handle);

    let dmabuf_formats = backend.renderer().dmabuf_formats();
    let mut dmabuf_state = DmabufState::new();
    dmabuf_state.create_global::<App>(&display_handle, dmabuf_formats);
    let mut app = App::new(&display_handle, dmabuf_state)?;

    let outputs_config = OutputsConfig::watch();
    let mut outputs_generation = outputs_config.generation();
    let mut output_layout = resolve_layout(&outputs_config, size);
    apply_layout(&mut app, &output, &output_layout, size);

    let mut xwayland_event_loop: CalloopEventLoop<App> = CalloopEventLoop::try_new()?;
    let xwayland_handle = xwayland_event_loop.handle();
    let _xwayland_source = match XWayland::spawn(
        &display_handle,
        None,
        crate::protocols::xwayland_environment(),
        true,
        Stdio::null(),
        Stdio::inherit(),
        |_| {},
    ) {
        Ok((xwayland, client)) => {
            let wm_handle = xwayland_handle.clone();
            let token =
                xwayland_handle.insert_source(xwayland, move |event, _, app| match event {
                    XWaylandEvent::Ready {
                        x11_socket,
                        display_number,
                    } => match X11Wm::start_wm(wm_handle.clone(), x11_socket, client.clone()) {
                        Ok(xwm) => app.install_xwm(xwm, display_number, wm_handle.clone()),
                        Err(error) => eprintln!("nkdhr-compositor: failed to start XWM: {error}"),
                    },
                    XWaylandEvent::Error => {
                        eprintln!("nkdhr-compositor: XWayland exited during startup")
                    }
                })?;
            Some(SourceGuard::new(xwayland_handle.clone(), token))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("nkdhr-compositor: Xwayland is not installed; X11 compatibility disabled");
            None
        }
        Err(error) => return Err(error.into()),
    };

    let listener = ListeningSocket::bind_auto("wayland", 0..32)?;
    let socket_name = listener
        .socket_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    println!("nkdhr-compositor: listening on WAYLAND_DISPLAY={socket_name}");
    let mut frame_count: u32 = 0;
    let mut fps_window_start = Instant::now();

    loop {
        let mut should_exit = false;
        // Smithay's minimal example ignores `CloseRequested`, which would
        // leave the process running after its window closes; `PumpStatus::
        // Exit` only reports winit's own loop terminating.
        let status = event_loop.dispatch_new_events(|event| match event {
            WinitEvent::CloseRequested => should_exit = true,
            WinitEvent::Input(event) => input::handle(&mut app, &output_layout, event),
            WinitEvent::Resized { .. } | WinitEvent::Focus(_) | WinitEvent::Redraw => {}
        });
        if should_exit || matches!(status, PumpStatus::Exit(_)) {
            return Ok(());
        }
        if let Some(signal) = shutdown.received() {
            shutdown::log_shutdown(signal);
            return Ok(());
        }

        // `WinitGraphicsBackend::bind` re-queries the window size and
        // resizes its EGL surface every frame; only the layout needs to
        // follow a resize.
        let window_size = backend.window_size();
        let generation = outputs_config.generation();
        if window_size != size || generation != outputs_generation {
            size = window_size;
            outputs_generation = generation;
            output_layout = resolve_layout(&outputs_config, size);
            apply_layout(&mut app, &output, &output_layout, size);
        }
        let resolved = output_layout
            .output(NESTED_OUTPUT_NAME)
            .expect("the connected nested output always resolves");

        render::refresh(&mut app);

        let damage = Rectangle::from_size(size);
        let screencopies = app.take_pending_screencopies(NESTED_OUTPUT_NAME);
        let include_cursor = screencopies
            .first()
            .is_none_or(|request| request.overlay_cursor());
        let restore_surface_after_screencopy = !screencopies.is_empty();
        {
            let (renderer, mut framebuffer) = backend.bind()?;
            let elements = render::output_elements(renderer, &app, resolved, include_cursor);
            // GL's framebuffer origin is bottom-left; Smithay's logical
            // convention is top-left.
            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(render::clear_color(&app), &[damage])?;
            // Elements size themselves from this scale: a surface's
            // destination is its logical (or viewport) size times the scale,
            // while their locations are already physical. It must be the
            // output's scale, as the TTY backend's DRM compositor uses.
            draw_render_elements(&mut frame, resolved.scale, &elements, &[damage])?;
            let _ = frame.finish()?;

            for request in screencopies {
                match renderer.copy_framebuffer(&framebuffer, request.region, SCREENCOPY_FORMAT) {
                    Ok(mapping) => {
                        let flipped = mapping.flipped();
                        match renderer.map_texture(&mapping) {
                            Ok(pixels) => {
                                if let Err(error) =
                                    request.complete(pixels, flipped, app.start_time.elapsed())
                                {
                                    eprintln!(
                                        "nkdhr-compositor: nested screencopy failed: {error}"
                                    );
                                }
                            }
                            Err(error) => {
                                let message = error.to_string();
                                let _ = request.fail(message.clone());
                                eprintln!(
                                    "nkdhr-compositor: nested screencopy mapping failed: {message}"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = request.fail(message.clone());
                        eprintln!("nkdhr-compositor: nested screencopy readback failed: {message}");
                    }
                }
            }
            if restore_surface_after_screencopy {
                // `GlesRenderer::map_texture` intentionally makes the EGL
                // context current without a surface. Winit must have its
                // window surface rebound before `swap_buffers`, otherwise
                // Wayland EGL reports BadSurface and its attempted recovery
                // cannot allocate a second surface for the same native
                // window. A no-op render bind preserves the composed pixels.
                let frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
                let _ = frame.finish()?;
            }
        }
        backend.submit(Some(&[damage]))?;

        render::update_output_membership(&app, &output, resolved);
        render::send_frame_callbacks(&app, &output, resolved);
        if app.session_locked() {
            app.note_protected_frame(NESTED_OUTPUT_NAME);
        }

        frame_count += 1;
        let fps_elapsed = fps_window_start.elapsed();
        if fps_elapsed >= Duration::from_secs(5) {
            let fps = f64::from(frame_count) / fps_elapsed.as_secs_f64();
            let avg_frame_ms = fps_elapsed.as_secs_f64() * 1000.0 / f64::from(frame_count);
            println!(
                "nkdhr-compositor: {fps:.1} fps ({avg_frame_ms:.2} ms/frame avg over {frame_count} frames, {} windows)",
                app.windows.len()
            );
            frame_count = 0;
            fps_window_start = Instant::now();
        }

        if let Some(stream) = listener.accept()? {
            display_handle.insert_client(stream, Arc::new(ClientState::default()))?;
        }

        xwayland_event_loop.dispatch(Duration::ZERO, &mut app)?;
        display.dispatch_clients(&mut app)?;
        app.finish_client_dispatch();
        display.flush_clients()?;
    }
}
