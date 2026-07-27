//! Shared D-Bus contract for the nkdhr control plane: bus, path and
//! interface identifiers, wire types, and client proxies used by `nkdhrd`,
//! `nkdhrctl`, and any other process that talks to the daemon.

use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

/// Well-known bus name `nkdhrd` requests on the session bus.
pub const BUS_NAME: &str = "org.nkdhr.Daemon1";

/// Object path at which `nkdhrd` serves the [`BUS_NAME`] interface.
pub const OBJECT_PATH: &str = "/org/nkdhr/Daemon1";

/// Snapshot returned by [`DaemonProxy::get_status`].
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct DaemonStatus {
    pub version: String,
    pub uptime_secs: u64,
    pub modules: Vec<String>,
}

/// Client-side contract for `org.nkdhr.Daemon1`.
///
/// The interface, service and path strings here must match the
/// `#[interface(...)]` implementation in `nkdhrd` by construction: zbus's
/// macros take them as literals, so [`BUS_NAME`] and [`OBJECT_PATH`] can't
/// be referenced from within the attribute itself.
#[zbus::proxy(
    interface = "org.nkdhr.Daemon1",
    default_service = "org.nkdhr.Daemon1",
    default_path = "/org/nkdhr/Daemon1"
)]
pub trait Daemon {
    async fn ping(&self) -> zbus::Result<String>;
    async fn get_status(&self) -> zbus::Result<DaemonStatus>;
    async fn get_version(&self) -> zbus::Result<String>;
}
