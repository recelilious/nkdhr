use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

/// Object path at which `nkdhrd` serves `org.nkdhr.Brightness1`.
pub const BRIGHTNESS_OBJECT_PATH: &str = "/org/nkdhr/Brightness1";

/// Snapshot returned by [`BrightnessProxy::get_status`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct BrightnessStatus {
    pub percent: u8,
}

/// Client-side contract for `org.nkdhr.Brightness1`. See `daemon.rs` for why
/// the interface/service/path strings are duplicated as literals here.
#[zbus::proxy(
    interface = "org.nkdhr.Brightness1",
    default_service = "org.nkdhr.Daemon1",
    default_path = "/org/nkdhr/Brightness1"
)]
pub trait Brightness {
    async fn get_status(&self) -> zbus::Result<BrightnessStatus>;
    async fn set(&self, percent: u8) -> zbus::Result<()>;

    /// Fired whenever the backlight device reports a real change — never on
    /// a timer. `nkdhrctl watch brightness` is a thin loop over this
    /// signal.
    #[zbus(signal)]
    fn changed(&self, status: BrightnessStatus) -> zbus::Result<()>;
}
