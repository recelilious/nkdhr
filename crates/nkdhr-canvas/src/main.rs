use smithay::backend::input::InputEvent;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::{self as smithay_winit, WinitEvent, WinitInput};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Rectangle, Transform};

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

    loop {
        let mut should_exit = false;
        // `WinitEvent::CloseRequested` does not exit the loop by itself —
        // it is just delivered like any other event — so the exit flag
        // above is what actually makes the window's close button work.
        let status = event_loop.dispatch_new_events(|event| match event {
            WinitEvent::CloseRequested => should_exit = true,
            WinitEvent::Resized { size, scale_factor } => {
                println!("nkdhr-canvas: resized to {size:?} @ {scale_factor}x");
            }
            WinitEvent::Input(event) => log_input(&event),
            WinitEvent::Focus(_) | WinitEvent::Redraw => {}
        });
        if should_exit || matches!(status, PumpStatus::Exit(_)) {
            return Ok(());
        }

        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend.bind()?;
            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(CANVAS_BACKGROUND, &[damage])?;
            // The returned `SyncPoint` matters for damage-tracked partial
            // redraws to know when the GPU is actually done; COMP-1 clears
            // and redraws in full every frame, so there is nothing to wait
            // on it for yet.
            let _ = frame.finish()?;
        }
        backend.submit(Some(&[damage]))?;
    }
}

fn log_input(event: &InputEvent<WinitInput>) {
    match event {
        InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } => {}
        other => println!("nkdhr-canvas: input {other:?}"),
    }
}
