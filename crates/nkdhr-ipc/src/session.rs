use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

/// Object path at which `nkdhrd` serves `org.nkdhr.Session1`.
pub const SESSION_OBJECT_PATH: &str = "/org/nkdhr/Session1";

/// Snapshot returned by [`SessionProxy::get_status`], sourced from the
/// caller's own `logind` session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SessionStatus {
    pub id: String,
    pub seat: String,
    pub active: bool,
    pub idle: bool,
    pub locked: bool,
}

/// Client-side contract for `org.nkdhr.Session1`. See `daemon.rs` for why
/// the interface/service/path strings are duplicated as literals here.
#[zbus::proxy(
    interface = "org.nkdhr.Session1",
    default_service = "org.nkdhr.Daemon1",
    default_path = "/org/nkdhr/Session1"
)]
pub trait Session {
    async fn get_status(&self) -> zbus::Result<SessionStatus>;

    /// Fired whenever `logind` reports a real change to the active
    /// session's active/idle/locked state — never on a timer. `nkdhrctl
    /// watch session` is a thin loop over this signal.
    #[zbus(signal)]
    fn changed(&self, status: SessionStatus) -> zbus::Result<()>;
}
