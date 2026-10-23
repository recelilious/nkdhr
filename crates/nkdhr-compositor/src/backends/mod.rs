//! Platform backends. Each one owns its event loop, renderer, Smithay
//! `Output` objects and presentation, and calls the shared layers at fixed
//! points: `input::handle` for every input event, `App::finish_client_dispatch`
//! right after each client dispatch, `render::refresh` once per loop
//! iteration, `render::output_elements` to compose each output, the
//! output-membership and frame-callback helpers after each output's render
//! pass, and `App::set_output_layout` whenever it re-resolves the layout.

use std::error::Error;

pub type BackendResult = Result<(), Box<dyn Error>>;

/// A way of running the compositor: nested inside another desktop, or on a
/// virtual terminal with DRM/KMS. Neither is scaffolding for the other.
pub trait Backend {
    fn run(self) -> BackendResult;
}

#[cfg(feature = "tty")]
pub mod tty;
#[cfg(feature = "nested")]
pub mod winit;
