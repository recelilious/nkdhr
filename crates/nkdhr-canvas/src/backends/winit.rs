use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, ExportMem, Frame, ImportDma, Renderer, TextureMapping};
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::EventLoop as CalloopEventLoop;
use smithay::reexports::wayland_server::{Display, ListeningSocket};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Rectangle, Transform};
use smithay::wayland::dmabuf::DmabufState;
use smithay::xwayland::{X11Wm, XWayland, XWaylandEvent};

use crate::backends::{Backend, BackendResult};
use crate::canvas::output_group::{ConnectedOutput, OutputConfig, OutputLayout};
use crate::input;
use crate::protocols::SCREENCOPY_FORMAT;
use crate::render;
use crate::state::{App, ClientState};
use crate::ui_render::PinnedGlesRenderer;
use crate::widget_host::PinnedLayer;

const CANVAS_BACKGROUND: Color32F = Color32F::new(0.11, 0.12, 0.16, 1.0);
const LOCK_BACKGROUND: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);
const NESTED_OUTPUT_NAME: &str = "nkdhr-canvas-nested-0";

pub struct WinitBackend;

impl Backend for WinitBackend {
    fn run(self) -> BackendResult {
        run()
    }
}

fn run() -> BackendResult {
    let (mut backend, mut event_loop) = smithay_winit::init_from_attributes::<GlesRenderer>(
        WinitWindow::default_attributes()
            .with_title("nkdhr-canvas")
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
    let initial_size = backend.window_size();
    let initial_mode = Mode {
        size: initial_size,
        refresh: 60_000,
    };
    output.change_current_state(Some(initial_mode), None, None, None);
    output.set_preferred(initial_mode);
    output.create_global::<App>(&display_handle);

    let output_config = OutputConfig::watch();
    let mut output_config_generation = output_config.generation();
    let mut output_layout = OutputLayout::resolve(
        &output_config.snapshot(),
        &[ConnectedOutput {
            name: NESTED_OUTPUT_NAME.to_owned(),
            physical_size: initial_size,
        }],
    );

    let dmabuf_formats = backend.renderer().dmabuf_formats();
    let mut dmabuf_state = DmabufState::new();
    dmabuf_state.create_global::<App>(&display_handle, dmabuf_formats);
    let mut app = App::new(&display_handle, dmabuf_state)?;
    app.reconcile_output_layout(&output_layout);
    let mut xwayland_event_loop: CalloopEventLoop<App> = CalloopEventLoop::try_new()?;
    let xwayland_handle = xwayland_event_loop.handle();
    match XWayland::spawn(
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
            xwayland_handle.insert_source(xwayland, move |event, _, app| match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => match X11Wm::start_wm(wm_handle.clone(), x11_socket, client.clone()) {
                    Ok(xwm) => app.install_xwm(xwm, display_number, wm_handle.clone()),
                    Err(error) => eprintln!("nkdhr-canvas: failed to start XWM: {error}"),
                },
                XWaylandEvent::Error => {
                    eprintln!("nkdhr-canvas: XWayland exited during startup")
                }
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("nkdhr-canvas: Xwayland is not installed; X11 compatibility disabled");
        }
        Err(error) => return Err(error.into()),
    }
    let resolved = output_layout
        .output(NESTED_OUTPUT_NAME)
        .expect("the connected nested output must resolve");
    output.change_current_state(
        Some(initial_mode),
        None,
        Some(Scale::Fractional(resolved.scale)),
        Some(resolved.global_location),
    );

    let listener = ListeningSocket::bind_auto("wayland", 0..32)?;
    let socket_name = listener
        .socket_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    println!("nkdhr-canvas: listening on WAYLAND_DISPLAY={socket_name}");
    let mut frame_count: u32 = 0;
    let mut fps_window_start = Instant::now();
    let mut pinned_ui_renderer = PinnedGlesRenderer::default();

    loop {
        let mut should_exit = false;
        let status = event_loop.dispatch_new_events(|event| match event {
            WinitEvent::CloseRequested => should_exit = true,
            WinitEvent::Resized { size, scale_factor } => {
                println!("nkdhr-canvas: resized to {size:?} @ {scale_factor}x");
            }
            WinitEvent::Input(event) => input::handle(&mut app, &output_layout, event),
            WinitEvent::Focus(_) | WinitEvent::Redraw => {}
        });
        if should_exit || matches!(status, PumpStatus::Exit(_)) {
            return Ok(());
        }

        let size = backend.window_size();
        let config_generation = output_config.generation();
        let layout_size = output_layout
            .output(NESTED_OUTPUT_NAME)
            .map(|output| output.physical_size);
        if layout_size != Some(size) || config_generation != output_config_generation {
            output_layout = OutputLayout::resolve(
                &output_config.snapshot(),
                &[ConnectedOutput {
                    name: NESTED_OUTPUT_NAME.to_owned(),
                    physical_size: size,
                }],
            );
            output_config_generation = config_generation;
            let mode = Mode {
                size,
                refresh: 60_000,
            };
            let resolved = output_layout
                .output(NESTED_OUTPUT_NAME)
                .expect("the connected nested output must resolve");
            let group = output_layout
                .group_for_output(NESTED_OUTPUT_NAME)
                .expect("the connected nested output must belong to a group");
            output.change_current_state(
                Some(mode),
                None,
                Some(Scale::Fractional(resolved.scale)),
                Some(resolved.global_location),
            );
            output.set_preferred(mode);
            app.reconcile_output_layout(&output_layout);
            println!(
                "nkdhr-canvas: nested output resolved to group {:?}, canvas {:?}",
                group.name, group.canvas
            );
        }

        render::advance_animations(&mut app);

        let damage = Rectangle::from_size(size);
        let screencopies = app.take_pending_screencopies(NESTED_OUTPUT_NAME);
        let include_cursor = screencopies
            .first()
            .is_none_or(|request| request.overlay_cursor());
        let restore_surface_after_screencopy = !screencopies.is_empty();
        {
            let (renderer, mut framebuffer) = backend.bind()?;
            let resolved = output_layout
                .output(NESTED_OUTPUT_NAME)
                .expect("the connected nested output must resolve");
            let group = output_layout
                .group_for_output(NESTED_OUTPUT_NAME)
                .expect("the connected nested output must belong to a group");
            let view = app
                .group_views
                .get(&group.name)
                .expect("resolved output group must have view state");
            let canvas_name = view.canvas.clone();
            let viewport = view.viewport;
            let workspace_fade = view.workspace_fade.clone();
            render::update_workspace_output_membership(
                &app,
                &output,
                group,
                resolved,
                &canvas_name,
                viewport,
                workspace_fade.as_ref(),
            );
            let locked = app.session_locked();
            let lock_surface = app.lock_surface_for_output(NESTED_OUTPUT_NAME);
            let mut elements = if include_cursor {
                render::cursor_render_elements(
                    renderer,
                    &app,
                    resolved.global_location,
                    resolved.scale,
                )
            } else {
                Vec::new()
            };
            if !locked {
                elements.extend(render::dnd_icon_render_elements(
                    renderer,
                    &app,
                    resolved.global_location,
                    resolved.scale,
                ));
                elements.extend(render::shell_render_elements(
                    &mut pinned_ui_renderer,
                    renderer,
                    &mut app.shell,
                    NESTED_OUTPUT_NAME,
                    size,
                    resolved.scale,
                ));
                elements.extend(render::placement_preview_render_elements(
                    &app,
                    viewport,
                    group.canvas_anchor,
                    resolved.group_location,
                    resolved.scale,
                ));
            }
            if locked {
                elements.extend(lock_surface.iter().flat_map(
                    |surface| -> Vec<render::CanvasRenderElement<GlesRenderer>> {
                        render_elements_from_surface_tree(
                            renderer,
                            surface,
                            (0, 0),
                            resolved.scale,
                            1.0,
                            Kind::Unspecified,
                        )
                    },
                ));
            } else {
                elements.extend(render::pinned_render_elements(
                    &mut pinned_ui_renderer,
                    renderer,
                    app.canvases
                        .get_mut(&canvas_name)
                        .expect("resolved output group must have a canvas"),
                    PinnedLayer::AboveWindows,
                    render::PinnedOutputPlacement {
                        viewport,
                        canvas_anchor: group.canvas_anchor,
                        output_group_location: resolved.group_location,
                        output_scale: resolved.scale,
                        target: size,
                    },
                ));
                elements.extend(app.canvases[&canvas_name].windows().iter().rev().flat_map(
                    |window| {
                        render::window_render_elements(
                            renderer,
                            window,
                            viewport,
                            group.canvas_anchor,
                            resolved.group_location,
                            resolved.scale,
                            workspace_fade
                                .as_ref()
                                .map_or(1.0, crate::state::WorkspaceFade::progress),
                        )
                    },
                ));
                elements.extend(render::pinned_render_elements(
                    &mut pinned_ui_renderer,
                    renderer,
                    app.canvases
                        .get_mut(&canvas_name)
                        .expect("resolved output group must have a canvas"),
                    PinnedLayer::BehindWindows,
                    render::PinnedOutputPlacement {
                        viewport,
                        canvas_anchor: group.canvas_anchor,
                        output_group_location: resolved.group_location,
                        output_scale: resolved.scale,
                        target: size,
                    },
                ));
                if let Some(fade) = workspace_fade {
                    let alpha = 1.0 - fade.progress();
                    if alpha > 0.0
                        && let Some(canvas) = app.canvases.get(&fade.canvas)
                    {
                        elements.extend(canvas.windows().iter().rev().flat_map(|window| {
                            render::window_render_elements(
                                renderer,
                                window,
                                fade.viewport,
                                group.canvas_anchor,
                                resolved.group_location,
                                resolved.scale,
                                alpha,
                            )
                        }));
                    }
                }
            }

            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(
                if locked {
                    LOCK_BACKGROUND
                } else {
                    CANVAS_BACKGROUND
                },
                &[damage],
            )?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
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
                                    eprintln!("nkdhr-canvas: nested screencopy failed: {error}");
                                }
                            }
                            Err(error) => {
                                let message = error.to_string();
                                let _ = request.fail(message.clone());
                                eprintln!(
                                    "nkdhr-canvas: nested screencopy mapping failed: {message}"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = request.fail(message.clone());
                        eprintln!("nkdhr-canvas: nested screencopy readback failed: {message}");
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

        let frame_time = app.start_time.elapsed().as_millis() as u32;
        if app.session_locked() {
            if let Some(surface) = app.lock_surface_for_output(NESTED_OUTPUT_NAME) {
                render::send_frame_callbacks(&surface, frame_time);
            }
            app.note_protected_frame(NESTED_OUTPUT_NAME);
        } else {
            let group = output_layout
                .group_for_output(NESTED_OUTPUT_NAME)
                .expect("the connected nested output must belong to a group");
            let view = app
                .group_views
                .get(&group.name)
                .expect("resolved output group must have view state");
            let canvas = app
                .canvases
                .get(&view.canvas)
                .expect("resolved output group must have a canvas");
            for window in canvas.windows() {
                if let Some(surface) = window.wl_surface() {
                    render::send_frame_callbacks(&surface, frame_time);
                }
            }
        }
        render::send_pointer_frame_callbacks(&app, frame_time);

        frame_count += 1;
        let fps_elapsed = fps_window_start.elapsed();
        if fps_elapsed >= Duration::from_secs(5) {
            let fps = f64::from(frame_count) / fps_elapsed.as_secs_f64();
            let avg_frame_ms = fps_elapsed.as_secs_f64() * 1000.0 / f64::from(frame_count);
            println!(
                "nkdhr-canvas: {fps:.1} fps ({avg_frame_ms:.2} ms/frame avg over {frame_count} frames, {} windows)",
                app.canvases
                    .values()
                    .map(|canvas| canvas.windows().len())
                    .sum::<usize>()
            );
            frame_count = 0;
            fps_window_start = Instant::now();
        }

        if let Some(stream) = listener.accept()? {
            display_handle.insert_client(stream, Arc::new(ClientState::default()))?;
        }

        xwayland_event_loop.dispatch(Duration::ZERO, &mut app)?;
        display.dispatch_clients(&mut app)?;
        display.flush_clients()?;
    }
}
