//! Platform backends. Each one owns its event loop, renderer, Smithay
//! `Output` objects and presentation, and calls the shared layers at fixed
//! points: `input::handle` for every input event, `App::finish_client_dispatch`
//! right after each client dispatch, `render::refresh` once per loop
//! iteration, `render::output_elements` to compose each output, the
//! output-membership and frame-callback helpers after each output's render
//! pass, and `App::set_output_layout` whenever it re-resolves the layout.
//! Each also polls the [`ShutdownSignal`] once per loop iteration.

use std::error::Error;

use smithay::reexports::calloop::{LoopHandle, RegistrationToken};

use crate::shutdown::ShutdownSignal;

pub type BackendResult = Result<(), Box<dyn Error>>;

/// A way of running the compositor: nested inside another desktop, or on a
/// virtual terminal with DRM/KMS. Neither is scaffolding for the other.
pub trait Backend {
    /// Runs the compositor until it stops: its window closes (nested), a
    /// stop signal arrives, or an error ends it. However it stops, what the
    /// compositor holds outside the process (socket and lock files, and on
    /// a TTY the displays and the seat) is released before this returns.
    fn run(self, shutdown: &ShutdownSignal) -> BackendResult;
}

/// An event source that is removed from its loop when the guard is dropped.
///
/// calloop frees a loop's sources only once the last `LoopHandle` to the
/// loop is gone. A callback that holds a handle to its own loop is a
/// reference cycle that keeps the loop, and every source in it, alive after
/// the `EventLoop` is dropped, and the loop that runs XWayland has one as
/// soon as XWayland is started: nkdhr's XWayland callback holds a handle to
/// start the window manager, and the X11 event source that
/// `X11Wm::start_wm` registers holds one too. A source whose `Drop`
/// releases something outside the process (the Wayland socket, the X11
/// lock, a DRM device, the libseat seat) is therefore owned through this
/// guard, which releases it on every way out of a backend's `run`: a stop
/// signal, the window closing, an error, a startup failure after the source
/// was registered, or a panic.
pub struct SourceGuard<Data: 'static> {
    handle: LoopHandle<'static, Data>,
    token: RegistrationToken,
}

impl<Data> SourceGuard<Data> {
    pub fn new(handle: LoopHandle<'static, Data>, token: RegistrationToken) -> Self {
        Self { handle, token }
    }
}

/// `LoopHandle::remove` drops the source, and its callback, at once.
impl<Data> Drop for SourceGuard<Data> {
    fn drop(&mut self) {
        self.handle.remove(self.token);
    }
}

#[cfg(feature = "tty")]
pub mod tty;
#[cfg(feature = "nested")]
pub mod winit;

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::calloop::ping::make_ping;

    use super::*;

    /// Records that it was dropped.
    struct DropRecorder(Rc<Cell<bool>>);

    impl Drop for DropRecorder {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    /// Registers a source whose callback holds a handle to its own loop, as
    /// the XWayland callbacks do, and owns a value that records being
    /// dropped.
    fn insert_self_referencing_source(
        event_loop: &EventLoop<'static, ()>,
        dropped: &Rc<Cell<bool>>,
    ) -> RegistrationToken {
        let (_ping, source) = make_ping().unwrap();
        let own_loop = event_loop.handle();
        let recorder = DropRecorder(Rc::clone(dropped));
        event_loop
            .handle()
            .insert_source(source, move |(), (), ()| {
                let _ = (&own_loop, &recorder);
            })
            .unwrap()
    }

    /// The reason for [`SourceGuard`]: if calloop ever frees such a source
    /// with the `EventLoop`, this fails, and the guards are no longer needed
    /// for it.
    #[test]
    fn dropping_the_event_loop_keeps_a_source_that_holds_its_own_loop() {
        let dropped = Rc::new(Cell::new(false));
        let event_loop = EventLoop::try_new().unwrap();
        insert_self_referencing_source(&event_loop, &dropped);

        drop(event_loop);
        assert!(!dropped.get());
    }

    #[test]
    fn dropping_the_guard_drops_its_source_at_once() {
        let dropped = Rc::new(Cell::new(false));
        let event_loop = EventLoop::try_new().unwrap();
        let token = insert_self_referencing_source(&event_loop, &dropped);
        let guard = SourceGuard::new(event_loop.handle(), token);
        assert!(!dropped.get());

        drop(guard);
        assert!(dropped.get());
    }
}
