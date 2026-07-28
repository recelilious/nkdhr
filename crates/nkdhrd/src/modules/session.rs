use std::thread;

use nkdhr_ipc::{SESSION_OBJECT_PATH, SessionStatus};
use zbus::blocking::Connection;

use crate::backends::dbus_properties;
use crate::backends::logind::{self, SessionProxyBlocking};

/// `logind`'s own well-known name, used both by [`logind::active_session_path`]
/// and by [`spawn_watcher`] to watch the resolved session object.
const LOGIND_SERVICE: &str = "org.freedesktop.login1";

pub struct Session {
    system: Connection,
}

impl Session {
    pub fn new(system: Connection) -> Self {
        Self { system }
    }
}

#[zbus::interface(name = "org.nkdhr.Session1")]
impl Session {
    /// Reports the active session on the primary seat, not "the session
    /// nkdhrd's own process happens to run in" — `nkdhrd` runs as a
    /// generic `systemd --user` service and isn't itself a member of any
    /// login session, so a PID-based lookup on its own process would
    /// always fail.
    fn get_status(&self) -> zbus::fdo::Result<SessionStatus> {
        read_status(&self.system)
    }

    /// Fired whenever [`spawn_watcher`] observes a real change to the
    /// status [`get_status`](Self::get_status) would now return.
    #[zbus(signal)]
    async fn changed(
        signal_emitter: &zbus::object_server::SignalEmitter<'_>,
        status: SessionStatus,
    ) -> zbus::Result<()>;
}

fn read_status(system: &Connection) -> zbus::fdo::Result<SessionStatus> {
    let session_path = logind::active_session_path(system)?;
    let session = SessionProxyBlocking::builder(system)
        .path(session_path)?
        .build()?;

    Ok(SessionStatus {
        id: session.id()?,
        seat: session.seat()?.0,
        active: session.active()?,
        idle: session.idle_hint()?,
        locked: session.locked_hint()?,
    })
}

/// Spawns the background thread that emits `Session1.Changed` whenever
/// `logind` reports a real change to the active session — never on a
/// timer. Never polls: the thread blocks on `logind`'s own
/// `PropertiesChanged` signal between events.
///
/// **Scope gap, deliberate**: the session watched is the one active on the
/// primary seat *when this function is called* (daemon startup). A full
/// session hand-off afterwards — fast user switching, or a fresh login on
/// the same seat replacing the current session — is not observed until
/// `nkdhrd` restarts, since the watcher would need to notice the seat's
/// `ActiveSession` pointer itself changing and re-subscribe to the new
/// session object, which this single-seat, single-user development target
/// does not yet need. `get_status` is unaffected: it always re-resolves
/// the active session fresh on every call.
pub fn spawn_watcher(system: Connection, session: Connection) {
    thread::spawn(move || {
        if let Err(err) = watch(&system, &session) {
            eprintln!("nkdhrd: session watcher exited: {err}");
        }
    });
}

fn watch(system: &Connection, session: &Connection) -> zbus::Result<()> {
    let session_path = logind::active_session_path(system)?;
    let events = dbus_properties::watch(system, LOGIND_SERVICE, session_path.as_str())?;
    let iface = session
        .object_server()
        .interface::<_, Session>(SESSION_OBJECT_PATH)?;

    let mut last = None;
    for _event in events {
        let status = read_status(system)?;
        if last.as_ref() != Some(&status) {
            zbus::block_on(Session::changed(iface.signal_emitter(), status.clone()))?;
            last = Some(status);
        }
    }
    Ok(())
}
