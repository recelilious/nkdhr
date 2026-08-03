use std::error::Error;

pub type BackendResult = Result<(), Box<dyn Error>>;

pub trait Backend {
    fn run(self) -> BackendResult;
}

#[cfg(feature = "tty")]
pub mod tty;
#[cfg(feature = "nested")]
pub mod winit;
