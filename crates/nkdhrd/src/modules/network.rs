use std::collections::HashMap;

use nkdhr_ipc::NetworkStatus;
use zbus::blocking::Connection;
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::backends::network_manager::{
    AccessPointProxyBlocking, ActiveConnectionProxyBlocking, DeviceProxyBlocking,
    IP4ConfigProxyBlocking, NetworkManagerProxyBlocking, WirelessProxyBlocking,
};
use crate::backends::polkit;

/// `NM_DEVICE_TYPE_WIFI`; see `NetworkManager.h`. No typed enum for this
/// in nkdhr — the base feature only ever compares against this one
/// constant.
const NM_DEVICE_TYPE_WIFI: u32 = 2;

/// The object path NetworkManager uses in place of `None` for
/// object-path-valued properties.
const NO_OBJECT: &str = "/";

pub struct Network {
    system: Connection,
}

impl Network {
    pub fn new(system: Connection) -> Self {
        Self { system }
    }

    fn disconnected() -> NetworkStatus {
        NetworkStatus {
            connected: false,
            kind: "none".to_owned(),
            interface: None.into(),
            ssid: None.into(),
            signal_percent: None.into(),
            ip4_address: None.into(),
        }
    }
}

#[zbus::interface(name = "org.nkdhr.Network1")]
impl Network {
    fn get_status(&self) -> zbus::fdo::Result<NetworkStatus> {
        let nm = NetworkManagerProxyBlocking::new(&self.system)?;
        let primary = nm.primary_connection()?;
        if primary.as_str() == NO_OBJECT {
            return Ok(Self::disconnected());
        }

        let active = ActiveConnectionProxyBlocking::builder(&self.system)
            .path(primary)?
            .build()?;
        let kind = active.r#type()?;
        let device_path = active.devices()?.into_iter().next();

        let interface = device_path
            .clone()
            .map(|path| -> zbus::Result<String> {
                let device = DeviceProxyBlocking::builder(&self.system)
                    .path(path)?
                    .build()?;
                device.interface()
            })
            .transpose()?;

        let (ssid, signal_percent) = if kind == "802-11-wireless" {
            wifi_details(&self.system, device_path)?
        } else {
            (None, None)
        };

        let ip4_config = active.ip4_config()?;
        let ip4_address = if ip4_config.as_str() == NO_OBJECT {
            None
        } else {
            let ip4 = IP4ConfigProxyBlocking::builder(&self.system)
                .path(ip4_config)?
                .build()?;
            first_address(&ip4.address_data()?)
        };

        Ok(NetworkStatus {
            connected: true,
            kind: friendly_kind(&kind),
            interface: interface.into(),
            ssid: ssid.into(),
            signal_percent: signal_percent.into(),
            ip4_address: ip4_address.into(),
        })
    }

    /// Connects to a Wi-Fi network by SSID, creating a new NetworkManager
    /// connection profile for it. `password` is WPA/WPA2 personal (PSK);
    /// pass an empty string for an open network. Reusing an existing
    /// saved profile for the same SSID, and other security types
    /// (WPA-Enterprise, WEP), are out of scope for the base feature.
    async fn connect(
        &self,
        ssid: &str,
        password: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        polkit::check_authorization(
            &self.system,
            connection,
            &header,
            "org.nkdhr.policy.network-connect",
        )
        .await?;

        let nm = NetworkManagerProxyBlocking::new(&self.system)?;
        let device_path = wifi_device_path(&self.system, &nm)?
            .ok_or_else(|| zbus::fdo::Error::Failed("no Wi-Fi device found".to_owned()))?;
        let no_object = OwnedObjectPath::try_from(NO_OBJECT)
            .expect("\"/\" is always a valid D-Bus object path");

        let settings = connection_settings(ssid, password)?;
        nm.add_and_activate_connection(settings, &device_path, &no_object)?;
        Ok(())
    }
}

fn wifi_device_path(
    system: &Connection,
    nm: &NetworkManagerProxyBlocking,
) -> zbus::fdo::Result<Option<OwnedObjectPath>> {
    for path in nm.devices()? {
        let device = DeviceProxyBlocking::builder(system)
            .path(path.clone())?
            .build()?;
        if device.device_type()? == NM_DEVICE_TYPE_WIFI {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn connection_settings(
    ssid: &str,
    password: &str,
) -> zbus::fdo::Result<HashMap<String, HashMap<String, OwnedValue>>> {
    let mut settings = HashMap::new();

    let mut connection = HashMap::new();
    connection.insert("type".to_owned(), owned_value("802-11-wireless")?);
    connection.insert("id".to_owned(), owned_value(ssid)?);
    settings.insert("connection".to_owned(), connection);

    let mut wireless = HashMap::new();
    wireless.insert("ssid".to_owned(), owned_value(ssid.as_bytes())?);
    settings.insert("802-11-wireless".to_owned(), wireless);

    if !password.is_empty() {
        let mut security = HashMap::new();
        security.insert("key-mgmt".to_owned(), owned_value("wpa-psk")?);
        security.insert("psk".to_owned(), owned_value(password)?);
        settings.insert("802-11-wireless-security".to_owned(), security);
    }

    Ok(settings)
}

fn owned_value<'v>(value: impl Into<Value<'v>>) -> zbus::fdo::Result<OwnedValue> {
    value
        .into()
        .try_to_owned()
        .map_err(|err| zbus::fdo::Error::Failed(format!("building connection settings: {err}")))
}

fn wifi_details(
    system: &Connection,
    device_path: Option<zbus::zvariant::OwnedObjectPath>,
) -> zbus::Result<(Option<String>, Option<u8>)> {
    let Some(device_path) = device_path else {
        return Ok((None, None));
    };

    let wireless = WirelessProxyBlocking::builder(system)
        .path(device_path)?
        .build()?;
    let ap_path = wireless.active_access_point()?;
    if ap_path.as_str() == NO_OBJECT {
        return Ok((None, None));
    }

    let ap = AccessPointProxyBlocking::builder(system)
        .path(ap_path)?
        .build()?;
    let ssid = String::from_utf8(ap.ssid()?).ok();
    let strength = ap.strength()?;

    Ok((ssid, Some(strength)))
}

fn friendly_kind(nm_type: &str) -> String {
    match nm_type {
        "802-11-wireless" => "wifi".to_owned(),
        "802-3-ethernet" => "wired".to_owned(),
        other => other.to_owned(),
    }
}

fn first_address(address_data: &[HashMap<String, OwnedValue>]) -> Option<String> {
    let value = address_data.first()?.get("address")?.clone();
    String::try_from(value).ok()
}
