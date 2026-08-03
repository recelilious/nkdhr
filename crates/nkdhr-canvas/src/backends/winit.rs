use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, ImportDma, Renderer};
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Rectangle, Transform};
use smithay::wayland::dmabuf::DmabufState;

use crate::backends::{Backend, BackendResult};
use crate::canvas::output_group::{ConnectedOutput, OutputConfig, OutputLayout};
use crate::input;
use crate::render;
use crate::state::{App, ClientState};

const CANVAS_BACKGROUND: Color32F = Color32F::new(0.11, 0.12, 0.16, 1.0);
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
    let mut clients: Vec<Client> = Vec::new();

    let mut frame_count: u32 = 0;
    let mut fps_window_start = Instant::now();

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
            let canvas = app
                .canvases
                .get(&view.canvas)
                .expect("resolved output group must have a canvas");
            let elements = canvas
                .windows()
                .iter()
                .flat_map(|window| {
                    let group_point = view
                        .viewport
                        .to_group_logical(window.position, group.logical_size);
                    let local = group_point - resolved.group_location.to_f64();
                    let offset = local.to_physical(resolved.scale).to_i32_round();
                    render_elements_from_surface_tree(
                        renderer,
                        window.surface.wl_surface(),
                        offset,
                        view.viewport.zoom * resolved.scale,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();

            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(CANVAS_BACKGROUND, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
            let _ = frame.finish()?;
        }
        backend.submit(Some(&[damage]))?;

        let frame_time = app.start_time.elapsed().as_millis() as u32;
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
            render::send_frame_callbacks(window.surface.wl_surface(), frame_time);
        }

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
            let client = display_handle.insert_client(stream, Arc::new(ClientState::default()))?;
            clients.push(client);
        }

        display.dispatch_clients(&mut app)?;
        display.flush_clients()?;
    }
}
