use serde::{Deserialize, Serialize};
use zbus::zvariant::{Optional, Type};

/// Object path at which `nkdhrd` serves `org.nkdhr.Power1`.
pub const POWER_OBJECT_PATH: &str = "/org/nkdhr/Power1";

/// Snapshot returned by [`PowerProxy::get_status`], sourced from UPower's
/// `DisplayDevice` (its own aggregate across every battery in the system).
///
/// `time_to_empty_secs` and `time_to_full_secs` use [`Optional`] because
/// UPower reports `0` for "not applicable" (e.g. `time_to_full_secs` while
/// discharging) rather than omitting the value; `Optional` turns that back
/// into an absence a client can tell apart from a real zero.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct PowerStatus {
    pub is_present: bool,
    pub percentage: f64,
    pub state: String,
    pub time_to_empty_secs: Optional<u32>,
    pub time_to_full_secs: Optional<u32>,
}

/// Client-side contract for `org.nkdhr.Power1`. See `daemon.rs` for why the
/// interface/service/path strings are duplicated as literals here.
#[zbus::proxy(
    interface = "org.nkdhr.Power1",
    default_service = "org.nkdhr.Daemon1",
    default_path = "/org/nkdhr/Power1"
)]
pub trait Power {
    async fn get_status(&self) -> zbus::Result<PowerStatus>;
    async fn power_off(&self) -> zbus::Result<()>;
    async fn reboot(&self) -> zbus::Result<()>;
    async fn suspend(&self) -> zbus::Result<()>;
}
