use std::fs;
use std::path::{Path, PathBuf};

use nkdhr_ipc::BrightnessStatus;
use zbus::blocking::Connection;
use zbus::message::Header;

use crate::backends::logind::{self, SessionProxyBlocking};
use crate::backends::polkit;

pub struct Brightness {
    system: Connection,
    device: PathBuf,
}

impl Brightness {
    /// Picks the first backlight device under `/sys/class/backlight`.
    /// Fails (and the module is left unregistered by `main`) on hardware
    /// with no backlight, e.g. a desktop.
    pub fn new(system: Connection) -> zbus::fdo::Result<Self> {
        let mut entries = fs::read_dir("/sys/class/backlight")
            .map_err(|err| zbus::fdo::Error::Failed(format!("no backlight device: {err}")))?;
        let entry = entries
            .next()
            .ok_or_else(|| zbus::fdo::Error::Failed("no backlight device found".to_owned()))?
            .map_err(|err| {
                zbus::fdo::Error::Failed(format!("reading /sys/class/backlight: {err}"))
            })?;

        Ok(Self {
            system,
            device: entry.path(),
        })
    }

    fn device_name(&self) -> zbus::fdo::Result<&str> {
        self.device
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                zbus::fdo::Error::Failed(format!(
                    "backlight device path {} has no valid name",
                    self.device.display()
                ))
            })
    }
}

#[zbus::interface(name = "org.nkdhr.Brightness1")]
impl Brightness {
    fn get_status(&self) -> zbus::fdo::Result<BrightnessStatus> {
        let brightness = read_u32(&self.device.join("brightness"))?;
        let max = read_u32(&self.device.join("max_brightness"))?;
        let percent = (brightness * 100 + max / 2)
            .checked_div(max)
            .unwrap_or(0)
            .min(100) as u8;

        Ok(BrightnessStatus { percent })
    }

    /// Sets brightness via `logind`'s `Session.SetBrightness` on the
    /// primary seat's active session, rather than writing
    /// `/sys/class/backlight/*/brightness` directly, so `nkdhrd` never
    /// needs elevated file permissions — `logind` already brokers this
    /// per-seat.
    async fn set(
        &self,
        percent: u8,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        polkit::check_authorization(
            &self.system,
            connection,
            &header,
            "org.nkdhr.policy.brightness-set",
        )
        .await?;

        let max = read_u32(&self.device.join("max_brightness"))?;
        let value = (u32::from(percent.min(100)) * max + 50) / 100;

        let session_path = logind::active_session_path(&self.system)?;
        let session = SessionProxyBlocking::builder(&self.system)
            .path(session_path)?
            .build()?;
        Ok(session.set_brightness("backlight", self.device_name()?, value)?)
    }
}

fn read_u32(path: &Path) -> zbus::fdo::Result<u32> {
    fs::read_to_string(path)
        .map_err(|err| zbus::fdo::Error::Failed(format!("reading {}: {err}", path.display())))?
        .trim()
        .parse()
        .map_err(|err| zbus::fdo::Error::Failed(format!("parsing {}: {err}", path.display())))
}
