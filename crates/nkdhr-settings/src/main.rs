use std::error::Error;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nkdhr_render::gles::GlesBackend;
use nkdhr_settings::AppearanceSurface;
use nkdhr_ui::{
    ClipboardRequest, Key, MaterialCapabilities, Modifiers, PointerButton, Size, UiEvent, UiSurface,
};
use smithay::backend::egl::context::{GlAttributes, PixelFormatRequirements};
use smithay::backend::egl::display::EGLDisplay;
use smithay::backend::egl::{EGLContext, EGLSurface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Bind, Color32F, Frame, Renderer};
use smithay::reexports::winit::application::ApplicationHandler;
use smithay::reexports::winit::dpi::{LogicalSize, PhysicalPosition};
use smithay::reexports::winit::event::{
    ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent,
};
use smithay::reexports::winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use smithay::reexports::winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use smithay::reexports::winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use smithay::reexports::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use smithay::reexports::winit::window::{Window, WindowId};
use smithay::utils::{Physical, Rectangle, Transform};
use wayland_egl::WlEglSurface;

const BACKGROUND: Color32F = Color32F::new(0.055, 0.06, 0.08, 1.0);

fn main() -> Result<(), Box<dyn Error>> {
    let mut event_loop = EventLoop::builder().build()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    #[allow(deprecated)]
    let window = Arc::new(
        event_loop.create_window(
            Window::default_attributes()
                .with_title("nkdhr Settings")
                .with_inner_size(LogicalSize::new(1160.0, 720.0))
                .with_visible(true),
        )?,
    );
    window.set_ime_allowed(true);

    let display = unsafe { EGLDisplay::new(Arc::clone(&window))? };
    let attributes = GlAttributes {
        version: (3, 0),
        profile: None,
        debug: cfg!(debug_assertions),
        vsync: true,
    };
    let context =
        EGLContext::new_with_config(&display, attributes, PixelFormatRequirements::_10_bit())
            .or_else(|_| {
                EGLContext::new_with_config(&display, attributes, PixelFormatRequirements::_8_bit())
            })?;
    let raw = window.window_handle()?.as_raw();
    let RawWindowHandle::Wayland(handle) = raw else {
        return Err("nkdhr Settings standalone host requires a Wayland window".into());
    };
    let physical = window.inner_size();
    let native_surface = unsafe {
        WlEglSurface::new_from_raw(
            handle.surface.as_ptr() as *mut _,
            physical.width as i32,
            physical.height as i32,
        )
    }
    .map_err(|error| format!("could not create Wayland EGL surface: {error}"))?;
    let mut egl_surface = unsafe {
        EGLSurface::new(
            &display,
            context
                .pixel_format()
                .ok_or("EGL context has no pixel format")?,
            context.config_id(),
            native_surface,
        )?
    };
    let _ = context.unbind();
    let mut renderer = unsafe { GlesRenderer::new(context)? };
    let mut backend = GlesBackend::new(&mut renderer)?;

    let mut input = StandaloneInput::default();
    let initial_scale = window.scale_factor() as f32;
    let initial_size = logical_size(&window);
    let mut surface = AppearanceSurface::new(
        initial_size,
        initial_scale,
        MaterialCapabilities {
            backdrop_blur: true,
            reduced_transparency: false,
            high_contrast: false,
        },
    )?;
    let mut redraw = true;
    let mut running = true;

    while running {
        let mut events = Vec::new();
        let mut close_requested = false;
        let status = event_loop.pump_app_events(
            Some(Duration::from_millis(16)),
            &mut EventCollector {
                events: &mut events,
                close_requested: &mut close_requested,
            },
        );
        if close_requested || matches!(status, PumpStatus::Exit(_)) {
            running = false;
            continue;
        }

        for event in events {
            match event {
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => redraw = true,
                WindowEvent::RedrawRequested => redraw = true,
                WindowEvent::Focused(focused) => {
                    dispatch(&mut surface, UiEvent::FocusChanged(focused));
                    redraw = true;
                }
                WindowEvent::CursorMoved { position, .. } => {
                    input.pointer = logical_position(position, window.scale_factor());
                    dispatch(
                        &mut surface,
                        UiEvent::PointerMoved {
                            position: input.pointer,
                        },
                    );
                    redraw = true;
                }
                WindowEvent::CursorLeft { .. } => {
                    dispatch(&mut surface, UiEvent::PointerLeft);
                    redraw = true;
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    let button = pointer_button(button);
                    let click_count = input.click_count(button, state);
                    let event = match state {
                        ElementState::Pressed => UiEvent::PointerDown {
                            position: input.pointer,
                            button,
                            modifiers: input.modifiers,
                            click_count,
                        },
                        ElementState::Released => UiEvent::PointerUp {
                            position: input.pointer,
                            button,
                            modifiers: input.modifiers,
                            click_count,
                        },
                    };
                    dispatch(&mut surface, event);
                    redraw = true;
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    let (delta_x, delta_y) = scroll_delta(delta, window.scale_factor());
                    dispatch(
                        &mut surface,
                        UiEvent::PointerScroll {
                            position: input.pointer,
                            delta_x,
                            delta_y,
                            modifiers: input.modifiers,
                        },
                    );
                    redraw = true;
                }
                WindowEvent::ModifiersChanged(modifiers) => {
                    input.modifiers = ui_modifiers(modifiers.state());
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    let key = ui_key(&event.logical_key);
                    let ui_event = match event.state {
                        ElementState::Pressed => UiEvent::KeyDown {
                            key,
                            modifiers: input.modifiers,
                            repeat: event.repeat,
                        },
                        ElementState::Released => UiEvent::KeyUp {
                            key,
                            modifiers: input.modifiers,
                        },
                    };
                    dispatch(&mut surface, ui_event);
                    if event.state == ElementState::Pressed
                        && !input.modifiers.control
                        && !input.modifiers.alt
                        && !input.modifiers.logo
                        && let Some(text) = event.text
                        && !text.chars().any(char::is_control)
                    {
                        dispatch(&mut surface, UiEvent::TextInput(text.to_string()));
                    }
                    redraw = true;
                }
                WindowEvent::Ime(Ime::Preedit(text, selection)) => {
                    dispatch(&mut surface, UiEvent::ImePreedit { text, selection });
                    redraw = true;
                }
                WindowEvent::Ime(Ime::Commit(text)) => {
                    dispatch(&mut surface, UiEvent::ImeCommit(text));
                    redraw = true;
                }
                WindowEvent::Ime(Ime::Disabled) => {
                    dispatch(
                        &mut surface,
                        UiEvent::ImePreedit {
                            text: String::new(),
                            selection: None,
                        },
                    );
                    redraw = true;
                }
                _ => {}
            }
        }

        redraw |= surface.frame_requested();
        if !redraw {
            continue;
        }
        redraw = false;
        let target = target_size(&window);
        if target.w <= 0 || target.h <= 0 {
            continue;
        }
        let scale = window.scale_factor() as f32;
        let logical = logical_size(&window);
        surface.render(logical, scale)?;
        egl_surface.resize(target.w, target.h, 0, 0);
        let mut framebuffer = renderer.bind(&mut egl_surface)?;
        let prepared = backend.prepare(
            &mut renderer,
            surface.display_list(),
            surface.textures(),
            target,
            scale,
        )?;
        let damage = Rectangle::from_size(target);
        let mut frame = renderer.render(&mut framebuffer, target, Transform::Flipped180)?;
        frame.clear(BACKGROUND, &[damage])?;
        backend.draw(&mut frame, &prepared, &[damage])?;
        let _ = frame.finish()?;
        drop(framebuffer);
        window.pre_present_notify();
        egl_surface.swap_buffers(None)?;
    }

    backend.destroy(&mut renderer)?;
    drop(display);
    Ok(())
}

struct EventCollector<'a> {
    events: &'a mut Vec<WindowEvent>,
    close_requested: &'a mut bool,
}

impl ApplicationHandler for EventCollector<'_> {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
            *self.close_requested = true;
        } else {
            self.events.push(event);
        }
    }
}

#[derive(Default)]
struct StandaloneInput {
    pointer: nkdhr_render::Point,
    modifiers: Modifiers,
    last_click: Option<(PointerButton, nkdhr_render::Point, Instant)>,
    click_count: u8,
}

impl StandaloneInput {
    fn click_count(&mut self, button: PointerButton, state: ElementState) -> u8 {
        if state == ElementState::Released {
            return self.click_count.max(1);
        }
        let now = Instant::now();
        let continues = self.last_click.is_some_and(|(last_button, point, at)| {
            last_button == button
                && now.duration_since(at) <= Duration::from_millis(500)
                && (self.pointer.x - point.x).abs() <= 5.0
                && (self.pointer.y - point.y).abs() <= 5.0
        });
        self.click_count = if continues {
            self.click_count.saturating_add(1).min(3)
        } else {
            1
        };
        self.last_click = Some((button, self.pointer, now));
        self.click_count
    }
}

fn dispatch(surface: &mut AppearanceSurface, event: UiEvent) {
    match surface.dispatch(&event) {
        Ok(result) => service_clipboard(surface, result.clipboard),
        Err(error) => eprintln!("nkdhr-settings: input dispatch failed: {error}"),
    }
}

fn service_clipboard(surface: &mut AppearanceSurface, requests: Vec<ClipboardRequest>) {
    for request in requests {
        match request {
            ClipboardRequest::WriteText(text) => {
                let result = Command::new("wl-copy")
                    .stdin(Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        child
                            .stdin
                            .take()
                            .ok_or_else(|| std::io::Error::other("wl-copy stdin unavailable"))?
                            .write_all(text.as_bytes())?;
                        child.wait().map(|_| ())
                    });
                if let Err(error) = result {
                    eprintln!("nkdhr-settings: clipboard write failed: {error}");
                }
            }
            ClipboardRequest::ReadText { target } => {
                match Command::new("wl-paste")
                    .args(["--no-newline", "--type", "text"])
                    .output()
                {
                    Ok(output) if output.status.success() => {
                        let text = String::from_utf8_lossy(&output.stdout).into_owned();
                        if let Err(error) =
                            surface.dispatch(&UiEvent::ClipboardText { target, text })
                        {
                            eprintln!("nkdhr-settings: clipboard response failed: {error}");
                        }
                    }
                    Ok(output) => eprintln!(
                        "nkdhr-settings: clipboard read failed with {}",
                        output.status
                    ),
                    Err(error) => eprintln!("nkdhr-settings: clipboard read failed: {error}"),
                }
            }
        }
    }
}

fn logical_size(window: &Window) -> Size {
    let physical = window.inner_size();
    let scale = window.scale_factor() as f32;
    Size::new(
        physical.width as f32 / scale,
        physical.height as f32 / scale,
    )
}

fn target_size(window: &Window) -> smithay::utils::Size<i32, Physical> {
    let size = window.inner_size();
    (size.width as i32, size.height as i32).into()
}

fn logical_position(position: PhysicalPosition<f64>, scale: f64) -> nkdhr_render::Point {
    nkdhr_render::Point::new((position.x / scale) as f32, (position.y / scale) as f32)
}

fn pointer_button(button: MouseButton) -> PointerButton {
    match button {
        MouseButton::Left => PointerButton::Primary,
        MouseButton::Right => PointerButton::Secondary,
        MouseButton::Middle => PointerButton::Middle,
        MouseButton::Back => PointerButton::Other(4),
        MouseButton::Forward => PointerButton::Other(5),
        MouseButton::Other(button) => PointerButton::Other(button),
    }
}

fn scroll_delta(delta: MouseScrollDelta, scale: f64) -> (f32, f32) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (x * 32.0, y * 32.0),
        MouseScrollDelta::PixelDelta(position) => {
            ((position.x / scale) as f32, (position.y / scale) as f32)
        }
    }
}

fn ui_modifiers(state: ModifiersState) -> Modifiers {
    Modifiers {
        shift: state.shift_key(),
        control: state.control_key(),
        alt: state.alt_key(),
        logo: state.super_key(),
    }
}

fn ui_key(key: &WinitKey) -> Key {
    match key {
        WinitKey::Named(NamedKey::Tab) => Key::Tab,
        WinitKey::Named(NamedKey::Enter) => Key::Enter,
        WinitKey::Named(NamedKey::Space) => Key::Space,
        WinitKey::Named(NamedKey::Escape) => Key::Escape,
        WinitKey::Named(NamedKey::ArrowLeft) => Key::ArrowLeft,
        WinitKey::Named(NamedKey::ArrowRight) => Key::ArrowRight,
        WinitKey::Named(NamedKey::ArrowUp) => Key::ArrowUp,
        WinitKey::Named(NamedKey::ArrowDown) => Key::ArrowDown,
        WinitKey::Named(NamedKey::Home) => Key::Home,
        WinitKey::Named(NamedKey::End) => Key::End,
        WinitKey::Named(NamedKey::PageUp) => Key::PageUp,
        WinitKey::Named(NamedKey::PageDown) => Key::PageDown,
        WinitKey::Named(NamedKey::Backspace) => Key::Backspace,
        WinitKey::Named(NamedKey::Delete) => Key::Delete,
        WinitKey::Character(text) => Key::Character(text.to_string()),
        WinitKey::Named(named) => Key::Named(format!("{named:?}")),
        WinitKey::Unidentified(_) | WinitKey::Dead(_) => Key::Named(format!("{key:?}")),
    }
}
