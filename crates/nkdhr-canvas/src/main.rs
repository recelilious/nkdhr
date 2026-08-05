mod backends;
mod canvas;
mod cursor;
mod input;
mod protocols;
mod render;
mod settings;
mod state;
mod widget_host;

use backends::Backend;

fn main() -> backends::BackendResult {
    match std::env::args().nth(1).as_deref() {
        Some("--nested") => run_nested(),
        Some("--tty") => run_tty(),
        Some(argument) => {
            Err(format!("unknown argument {argument:?}; expected --nested or --tty").into())
        }
        None if std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("DISPLAY").is_some() =>
        {
            run_nested()
        }
        None => run_tty(),
    }
}

#[cfg(feature = "nested")]
fn run_nested() -> backends::BackendResult {
    backends::winit::WinitBackend.run()
}

#[cfg(not(feature = "nested"))]
fn run_nested() -> backends::BackendResult {
    Err("the nested backend is not included in this build".into())
}

#[cfg(feature = "tty")]
fn run_tty() -> backends::BackendResult {
    backends::tty::TtyBackend.run()
}

#[cfg(not(feature = "tty"))]
fn run_tty() -> backends::BackendResult {
    Err("the TTY backend is not included in this build; rebuild with --features tty".into())
}
