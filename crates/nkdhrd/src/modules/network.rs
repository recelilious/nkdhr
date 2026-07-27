use std::collections::HashMap;

use nkdhr_ipc::NetworkStatus;
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

use crate::backends::network_manager::{
    AccessPointProxyBlocking, ActiveConnectionProxyBlocking, DeviceProxyBlocking,
    IP4ConfigProxyBlocking, NetworkManagerProxyBlocking, WirelessProxyBlocking,
};

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
