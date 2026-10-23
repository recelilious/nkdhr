//! Stopping the compositor on `SIGTERM`, `SIGINT` and `SIGHUP`.
//!
//! A signal only records that the compositor should stop. Each backend
//! polls [`ShutdownSignal::received`] once per loop iteration and leaves its
//! loop, so the teardown (removing the Wayland and X11 sockets and lock
//! files, handing the displays and the seat back) runs on the main thread
//! like any other code, never in signal context.
//!
//! The handlers are ordinary `sigaction` handlers installed through
//! `signal-hook`, not calloop's `Signals` source. That source is built on
//! `signalfd`, which needs the signals blocked, and it blocks them only in
//! the thread that creates it. A signal mask is inherited by every thread
//! created afterwards and survives `fork` and `exec`, so Xwayland and every
//! other child would start with these signals blocked and could not be
//! stopped by them, while a thread created earlier would not block them,
//! and a signal the kernel delivered to that thread would still end the
//! process without any cleanup. A handler changes no mask, and `exec`
//! resets it to the default in children.
//!
//! The kernel delivers a signal sent to the process to any thread that does
//! not block it, and when two signals arrive together it may run their
//! handlers on two threads at once, in either order. Nothing here relies on
//! the order in which two different signals are handled.

use std::ffi::c_int;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::{mem, ptr};

use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;
use signal_hook::low_level::signal_name;

/// A signal that asks the compositor to stop.
struct StopSignal {
    number: c_int,
    /// Whether this signal, arriving after an earlier signal that forces the
    /// exit, ends the process at once: the way out of a teardown that hangs.
    forces_exit: bool,
}

/// The signals that stop the compositor.
///
/// `SIGHUP` neither forces the exit nor arms it for a later signal, because
/// hang-ups routinely arrive right behind another signal. logind stops a
/// login session's scope with `SendSIGHUP=yes`, so ending the session
/// (`loginctl terminate-session`, logging out, shutting the system down)
/// delivers `SIGTERM` immediately followed by `SIGHUP`, and closing a
/// terminal can deliver `SIGHUP` both from the shell and from the kernel.
/// If a hang-up took part in the forced exit, exactly those ordinary ways
/// of stopping could skip the cleanup.
const STOP_SIGNALS: [StopSignal; 3] = [
    StopSignal {
        number: SIGTERM,
        forces_exit: true,
    },
    StopSignal {
        number: SIGINT,
        forces_exit: true,
    },
    StopSignal {
        number: SIGHUP,
        forces_exit: false,
    },
];

/// The exit status of a forced exit, so that it is told apart from the 0 of
/// a clean shutdown.
const FORCED_EXIT_STATUS: c_int = 1;

/// What a received-signal record holds before such a signal arrived. No
/// signal has the number 0.
const NONE_RECEIVED: usize = 0;

/// The stop request the signal handlers record, polled by the backends.
pub struct ShutdownSignal {
    /// The number of a received stop signal that forces the exit, or
    /// [`NONE_RECEIVED`].
    forcing: Arc<AtomicUsize>,
    /// The number of a received stop signal that does not force the exit,
    /// or [`NONE_RECEIVED`].
    other: Arc<AtomicUsize>,
}

impl ShutdownSignal {
    /// Installs the handlers for `SIGTERM`, `SIGINT` and `SIGHUP`, except for
    /// those that are ignored when the compositor starts (see
    /// [`is_ignored`]).
    ///
    /// `main` calls this before anything else, in particular before a
    /// backend creates a socket, a lock file, a thread or a child process.
    /// There is then no moment at which those exist and one of these
    /// signals would still end the process with its default action, leaving
    /// them behind.
    pub fn install() -> io::Result<Self> {
        Self::install_for(&STOP_SIGNALS)
    }

    /// Registers `signal-hook` flag actions for each of `signals` that is
    /// not ignored. The actions for one signal run in registration order.
    ///
    /// A signal that forces the exit gets three: an exit conditional on
    /// `forcing_seen`, which finds it still false on the first such signal
    /// and ends the process with `_exit` on any later one; then the action
    /// that sets `forcing_seen`; then the one that records the signal's
    /// number in `forcing`. Only these signals set `forcing_seen`. Were a
    /// `SIGHUP` to set it too, logind's `SIGTERM` could be handled on one
    /// thread just after the `SIGHUP` that follows it was handled on
    /// another, find the flag set and end the process without cleanup.
    ///
    /// A signal that does not force the exit gets only the action that
    /// records its number in `other`.
    ///
    /// The actions only touch atomics and call `_exit`, which are
    /// async-signal-safe; nothing else of the compositor runs in signal
    /// context.
    fn install_for(signals: &[StopSignal]) -> io::Result<Self> {
        let forcing_seen = Arc::new(AtomicBool::new(false));
        let forcing = Arc::new(AtomicUsize::new(NONE_RECEIVED));
        let other = Arc::new(AtomicUsize::new(NONE_RECEIVED));
        for signal in signals {
            if is_ignored(signal.number)? {
                continue;
            }
            // Signal numbers are small positive integers.
            let number = signal.number as usize;
            if signal.forces_exit {
                // The handler runs these actions in registration order, and
                // a signal can arrive between two registrations. Recording
                // the request first means every partly registered state
                // either records a stop request or does nothing; arming
                // `forcing_seen` before the record existed could make a
                // signal arriving in between force the exit on the next
                // ordinary stop request, without cleanup.
                flag::register_usize(signal.number, Arc::clone(&forcing), number)?;
                flag::register_conditional_shutdown(
                    signal.number,
                    FORCED_EXIT_STATUS,
                    Arc::clone(&forcing_seen),
                )?;
                flag::register(signal.number, Arc::clone(&forcing_seen))?;
            } else {
                flag::register_usize(signal.number, Arc::clone(&other), number)?;
            }
        }
        Ok(Self { forcing, other })
    }

    /// The name of the stop signal the compositor stops for, such as
    /// `SIGTERM`, or `None` while none has arrived.
    ///
    /// A signal that forces the exit is named in preference to a `SIGHUP`.
    /// When logind ends a session, the `SIGTERM` is what stops the
    /// compositor and the `SIGHUP` right behind it only a side effect, and
    /// as the two may be handled in either order, neither the first nor the
    /// last signal recorded is reliably the `SIGTERM`.
    pub fn received(&self) -> Option<&'static str> {
        [&self.forcing, &self.other]
            .into_iter()
            .map(|record| record.load(Ordering::SeqCst))
            .find(|&number| number != NONE_RECEIVED)
            .map(|number| {
                c_int::try_from(number)
                    .ok()
                    .and_then(signal_name)
                    .unwrap_or("a stop signal")
            })
    }
}

/// Whether `signal` is ignored (`SIG_IGN`), as the process that started the
/// compositor may have left it.
///
/// `signal-hook` installs its handler whatever the disposition was, and
/// chains to the previous one only if that was a handler function, so
/// registering an ignored signal would make it stop the compositor. But a
/// signal ignored at startup is the starter's decision, and a deliberate
/// one: `nohup` ignores `SIGHUP` so that the program outlives its terminal,
/// and a shell without job control (a script) ignores `SIGINT` in the
/// commands it starts in the background, so that a `Ctrl+C` aimed at the
/// script does not reach them. Such a signal is left ignored.
fn is_ignored(signal: c_int) -> io::Result<bool> {
    // SAFETY: `sigaction` is a C struct of integers and integer-sized
    // handler addresses, for which all zeroes is a valid value. It must
    // start initialized because the C library may leave part of it
    // unwritten: glibc copies only the kernel's shorter signal mask.
    let mut current: libc::sigaction = unsafe { mem::zeroed() };
    // SAFETY: with a null new action, `sigaction` changes nothing and only
    // writes the current action to `current`, a valid, writable `sigaction`.
    if unsafe { libc::sigaction(signal, ptr::null(), &mut current) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(current.sa_sigaction == libc::SIG_IGN)
}

/// Logs that the compositor stops because `signal` arrived.
///
/// Written without `println!`, which panics when stdout is gone: a `SIGHUP`
/// often means that the terminal stdout points to has just hung up, and the
/// shutdown goes on all the same.
pub fn log_shutdown(signal: &str) {
    let _ = writeln!(
        io::stdout(),
        "nkdhr-compositor: received {signal}, shutting down"
    );
}

#[cfg(test)]
mod tests {
    use signal_hook::consts::signal::{SIGALRM, SIGUSR1, SIGUSR2, SIGVTALRM, SIGXCPU};
    use signal_hook::low_level::raise;

    use super::*;

    fn forces_exit(number: c_int) -> Option<bool> {
        STOP_SIGNALS
            .iter()
            .find(|signal| signal.number == number)
            .map(|signal| signal.forces_exit)
    }

    #[test]
    fn a_repeated_termination_or_interrupt_forces_the_exit_but_a_hang_up_never_does() {
        assert_eq!(forces_exit(SIGTERM), Some(true));
        assert_eq!(forces_exit(SIGINT), Some(true));
        assert_eq!(forces_exit(SIGHUP), Some(false));
        assert_eq!(STOP_SIGNALS.len(), 3);
    }

    #[test]
    fn every_stop_signal_is_logged_by_its_name() {
        for signal in &STOP_SIGNALS {
            assert!(signal_name(signal.number).is_some_and(|name| name.starts_with("SIG")));
        }
    }

    // The tests below install real handlers in the test process, each for
    // signals that no other test raises. `raise` sends the signal to the
    // calling thread and returns only after the handler has run.

    #[test]
    fn the_first_forcing_signal_only_requests_the_shutdown() {
        let shutdown = ShutdownSignal::install_for(&[StopSignal {
            number: SIGUSR1,
            forces_exit: true,
        }])
        .unwrap();
        assert_eq!(shutdown.received(), None);

        // Had the conditional exit been registered after the action that
        // sets its condition, this would end the test process.
        raise(SIGUSR1).unwrap();
        assert_eq!(shutdown.received(), Some("SIGUSR1"));
    }

    #[test]
    fn a_repeated_signal_that_does_not_force_the_exit_only_requests_the_shutdown() {
        let shutdown = ShutdownSignal::install_for(&[StopSignal {
            number: SIGUSR2,
            forces_exit: false,
        }])
        .unwrap();

        raise(SIGUSR2).unwrap();
        raise(SIGUSR2).unwrap();
        assert_eq!(shutdown.received(), Some("SIGUSR2"));
    }

    /// logind's `SIGTERM` and `SIGHUP`, in the order in which the forced
    /// exit would go wrong: the hang-up handled first.
    #[test]
    fn a_signal_that_does_not_force_the_exit_never_arms_the_forced_exit() {
        let shutdown = ShutdownSignal::install_for(&[
            StopSignal {
                number: SIGVTALRM,
                forces_exit: true,
            },
            StopSignal {
                number: SIGALRM,
                forces_exit: false,
            },
        ])
        .unwrap();

        raise(SIGALRM).unwrap();
        assert_eq!(shutdown.received(), Some("SIGALRM"));
        // Had the first signal armed the forced exit, this would end the
        // test process.
        raise(SIGVTALRM).unwrap();
        assert_eq!(shutdown.received(), Some("SIGVTALRM"));
        // The forcing signal stays the one named.
        raise(SIGALRM).unwrap();
        assert_eq!(shutdown.received(), Some("SIGVTALRM"));
    }

    #[test]
    fn a_signal_ignored_at_startup_stays_ignored() {
        // SAFETY: ignoring a signal that no other test uses affects nothing
        // else in the test process.
        let previous = unsafe { libc::signal(SIGXCPU, libc::SIG_IGN) };
        assert_ne!(previous, libc::SIG_ERR);

        let shutdown = ShutdownSignal::install_for(&[StopSignal {
            number: SIGXCPU,
            forces_exit: true,
        }])
        .unwrap();
        assert!(is_ignored(SIGXCPU).unwrap());

        // Had it been registered, the second one would end the test process.
        raise(SIGXCPU).unwrap();
        raise(SIGXCPU).unwrap();
        assert_eq!(shutdown.received(), None);
    }
}
