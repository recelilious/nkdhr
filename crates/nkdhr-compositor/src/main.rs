mod backends;
mod cursor;
mod grabs;
mod input;
mod outputs;
mod protocols;
mod render;
mod shutdown;
mod state;
mod windows;

use backends::Backend;
use shutdown::ShutdownSignal;

fn main() -> backends::BackendResult {
    // First of all: no socket, lock file, thread or child process may exist
    // while a stop signal would still end the process without cleanup.
    let shutdown = ShutdownSignal::install()?;
    match std::env::args().nth(1).as_deref() {
        Some("--nested") => run_nested(&shutdown),
        Some("--tty") => run_tty(&shutdown),
        Some(argument) => {
            Err(format!("unknown argument {argument:?}; expected --nested or --tty").into())
        }
        None if std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("DISPLAY").is_some() =>
        {
            run_nested(&shutdown)
        }
        None => run_tty(&shutdown),
    }
}

#[cfg(feature = "nested")]
fn run_nested(shutdown: &ShutdownSignal) -> backends::BackendResult {
    backends::winit::WinitBackend.run(shutdown)
}

#[cfg(not(feature = "nested"))]
fn run_nested(_shutdown: &ShutdownSignal) -> backends::BackendResult {
    Err("the nested backend is not included in this build".into())
}

#[cfg(feature = "tty")]
fn run_tty(shutdown: &ShutdownSignal) -> backends::BackendResult {
    backends::tty::TtyBackend.run(shutdown)
}

#[cfg(not(feature = "tty"))]
fn run_tty(_shutdown: &ShutdownSignal) -> backends::BackendResult {
    Err("the TTY backend is not included in this build; rebuild with --features tty".into())
}
