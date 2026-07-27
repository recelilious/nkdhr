use nkdhr_ipc::PowerStatus;
use zbus::blocking::Connection;
use zbus::zvariant::Optional;

use crate::backends::upower::DisplayDeviceProxyBlocking;

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
        let device = DisplayDeviceProxyBlocking::new(&self.system)?;

        Ok(PowerStatus {
            is_present: device.is_present()?,
            percentage: device.percentage()?,
            state: friendly_state(device.state()?),
            time_to_empty_secs: seconds(device.time_to_empty()?),
            time_to_full_secs: seconds(device.time_to_full()?),
        })
    }
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
