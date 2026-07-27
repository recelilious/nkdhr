use serde::{Deserialize, Serialize};
use zbus::zvariant::{Optional, Type};

/// Object path at which `nkdhrd` serves `org.nkdhr.Network1`.
pub const NETWORK_OBJECT_PATH: &str = "/org/nkdhr/Network1";

/// Snapshot returned by [`NetworkProxy::get_status`], describing
/// NetworkManager's primary active connection only.
///
/// `interface`, `ssid`, `signal_percent` and `ip4_address` use
/// [`Optional`] rather than `Option` because classic D-Bus (unlike
/// GVariant) has no "maybe" type; `Optional` encodes absence as the
/// field's default value (`""` or `0`) instead.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct NetworkStatus {
    pub connected: bool,
    pub kind: String,
    pub interface: Optional<String>,
    pub ssid: Optional<String>,
    pub signal_percent: Optional<u8>,
    pub ip4_address: Optional<String>,
}

/// Client-side contract for `org.nkdhr.Network1`. See `daemon.rs` for why
/// the interface/service/path strings are duplicated as literals here.
#[zbus::proxy(
    interface = "org.nkdhr.Network1",
    default_service = "org.nkdhr.Daemon1",
    default_path = "/org/nkdhr/Network1"
)]
pub trait Network {
    async fn get_status(&self) -> zbus::Result<NetworkStatus>;

    /// `password` is WPA/WPA2 personal (PSK); pass an empty string for an
    /// open network.
    async fn connect(&self, ssid: &str, password: &str) -> zbus::Result<()>;
}
