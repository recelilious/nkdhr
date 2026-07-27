use std::fs;
use std::path::{Path, PathBuf};

use nkdhr_ipc::BrightnessStatus;

pub struct Brightness {
    device: PathBuf,
}

impl Brightness {
    /// Picks the first backlight device under `/sys/class/backlight`.
    /// Fails (and the module is left unregistered by `main`) on hardware
    /// with no backlight, e.g. a desktop.
    pub fn new() -> zbus::fdo::Result<Self> {
        let mut entries = fs::read_dir("/sys/class/backlight")
            .map_err(|err| zbus::fdo::Error::Failed(format!("no backlight device: {err}")))?;
        let entry = entries
            .next()
            .ok_or_else(|| zbus::fdo::Error::Failed("no backlight device found".to_owned()))?
            .map_err(|err| {
                zbus::fdo::Error::Failed(format!("reading /sys/class/backlight: {err}"))
            })?;

        Ok(Self {
            device: entry.path(),
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
}

fn read_u32(path: &Path) -> zbus::fdo::Result<u32> {
    fs::read_to_string(path)
        .map_err(|err| zbus::fdo::Error::Failed(format!("reading {}: {err}", path.display())))?
        .trim()
        .parse()
        .map_err(|err| zbus::fdo::Error::Failed(format!("parsing {}: {err}", path.display())))
}
