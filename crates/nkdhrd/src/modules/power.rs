use std::thread;

use nkdhr_ipc::{POWER_OBJECT_PATH, PowerStatus};
use zbus::blocking::Connection;
use zbus::message::Header;
use zbus::zvariant::Optional;

use crate::backends::logind::ManagerProxyBlocking;
use crate::backends::upower::DisplayDeviceProxyBlocking;
use crate::backends::{dbus_properties, polkit};

/// UPower's own well-known name and the single, stable object path of its
/// `DisplayDevice` aggregate — the same object [`read_status`] reads and
/// [`spawn_watcher`] watches for `PropertiesChanged`.
const UPOWER_SERVICE: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

pub struct Power {
    system: Connection,
}

impl Power {
    pub fn new(system: Connection) -> Self {
        Self { system }
    }
}

#[zbus::interface(name = "org.nkdhr.Power1")]
impl Power {
    fn get_status(&self) -> zbus::fdo::Result<PowerStatus> {
        read_status(&self.system)
    }

    async fn power_off(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        polkit::check_authorization(
            &self.system,
            connection,
            &header,
            "org.nkdhr.policy.power-off",
        )
        .await?;
        Ok(ManagerProxyBlocking::new(&self.system)?.power_off(false)?)
    }

    async fn reboot(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        polkit::check_authorization(&self.system, connection, &header, "org.nkdhr.policy.reboot")
            .await?;
        Ok(ManagerProxyBlocking::new(&self.system)?.reboot(false)?)
    }

    async fn suspend(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        polkit::check_authorization(
            &self.system,
            connection,
            &header,
            "org.nkdhr.policy.suspend",
        )
        .await?;
        Ok(ManagerProxyBlocking::new(&self.system)?.suspend(false)?)
    }

    /// Fired whenever [`spawn_watcher`] observes a real change to the
    /// status [`get_status`](Self::get_status) would now return.
    #[zbus(signal)]
    async fn changed(
        signal_emitter: &zbus::object_server::SignalEmitter<'_>,
        status: PowerStatus,
    ) -> zbus::Result<()>;
}

fn read_status(system: &Connection) -> zbus::fdo::Result<PowerStatus> {
    let device = DisplayDeviceProxyBlocking::new(system)?;

    Ok(PowerStatus {
        is_present: device.is_present()?,
        percentage: device.percentage()?,
        state: friendly_state(device.state()?),
        time_to_empty_secs: seconds(device.time_to_empty()?),
        time_to_full_secs: seconds(device.time_to_full()?),
    })
}

/// Spawns the background thread that emits `Power1.Changed` whenever
/// UPower reports a real change to the `DisplayDevice` aggregate. Never
/// polls: the thread blocks on UPower's own `PropertiesChanged` signal
/// between events.
pub fn spawn_watcher(system: Connection, session: Connection) {
    thread::spawn(move || {
        if let Err(err) = watch(&system, &session) {
            eprintln!("nkdhrd: power watcher exited: {err}");
        }
    });
}

fn watch(system: &Connection, session: &Connection) -> zbus::Result<()> {
    let events = dbus_properties::watch(system, UPOWER_SERVICE, DISPLAY_DEVICE_PATH)?;
    let iface = session
        .object_server()
        .interface::<_, Power>(POWER_OBJECT_PATH)?;

    let mut last = None;
    for _event in events {
        let status = read_status(system)?;
        if last.as_ref() != Some(&status) {
            zbus::block_on(Power::changed(iface.signal_emitter(), status.clone()))?;
            last = Some(status);
        }
    }
    Ok(())
}

fn friendly_state(state: u32) -> String {
    match state {
        1 => "charging",
        2 => "discharging",
        3 => "empty",
        4 => "full",
        5 => "pending-charge",
        6 => "pending-discharge",
        _ => "unknown",
    }
    .to_owned()
}

/// UPower reports `0` (or negative, per its own docs, for "not being
/// calculated yet") for a time estimate that doesn't apply; treat both as
/// absent rather than a real zero.
fn seconds(value: i64) -> Optional<u32> {
    if value <= 0 {
        None
    } else {
        u32::try_from(value).ok()
    }
    .into()
}
