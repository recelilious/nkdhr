//! Minimal proxies for the parts of `org.freedesktop.NetworkManager` the
//! Network module needs. Not a general NetworkManager binding — only the
//! primary active connection's identity, device, Wi-Fi signal and IPv4
//! address.

use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
pub trait NetworkManager {
    #[zbus(property)]
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;

    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// `specific_object` is `"/"` (no specific access point) for a normal
    /// connect-by-SSID. Returns the new connection's settings path and its
    /// active-connection path.
    fn add_and_activate_connection(
        &self,
        connection: HashMap<String, HashMap<String, OwnedValue>>,
        device: &OwnedObjectPath,
        specific_object: &OwnedObjectPath,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
pub trait ActiveConnection {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn r#type(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(property)]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
pub trait Device {
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;

    /// `NM_DEVICE_TYPE_WIFI` is `2`; see `nm-dbus-interface.h`. No typed
    /// enum for this in nkdhr — the base feature only ever compares
    /// against that one constant.
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
pub trait Wireless {
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
pub trait AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;

    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.IP4Config",
    default_service = "org.freedesktop.NetworkManager"
)]
pub trait IP4Config {
    #[zbus(property)]
    fn address_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
}
