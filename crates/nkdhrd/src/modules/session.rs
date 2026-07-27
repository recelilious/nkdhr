use nkdhr_ipc::SessionStatus;
use zbus::blocking::Connection;

use crate::backends::logind::{self, SessionProxyBlocking};

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
        let session_path = logind::active_session_path(&self.system)?;
        let session = SessionProxyBlocking::builder(&self.system)
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
}
