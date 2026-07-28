use std::path::{Path, PathBuf};
use std::thread;
use std::{fs, io};

use inotify::{Inotify, WatchMask};
use nkdhr_ipc::{BRIGHTNESS_OBJECT_PATH, BrightnessStatus};
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

    /// The backlight device directory this instance reads from, for
    /// [`spawn_watcher`] to watch without re-running backlight-device
    /// discovery a second time (and risking picking a different device on
    /// hardware with more than one).
    pub fn device_path(&self) -> &Path {
        &self.device
    }
}

#[zbus::interface(name = "org.nkdhr.Brightness1")]
impl Brightness {
    fn get_status(&self) -> zbus::fdo::Result<BrightnessStatus> {
        read_status(&self.device)
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

    /// Fired whenever [`spawn_watcher`] observes a real change to the
    /// status [`get_status`](Self::get_status) would now return.
    #[zbus(signal)]
    async fn changed(
        signal_emitter: &zbus::object_server::SignalEmitter<'_>,
        status: BrightnessStatus,
    ) -> zbus::Result<()>;
}

fn read_u32(path: &Path) -> zbus::fdo::Result<u32> {
    fs::read_to_string(path)
        .map_err(|err| zbus::fdo::Error::Failed(format!("reading {}: {err}", path.display())))?
        .trim()
        .parse()
        .map_err(|err| zbus::fdo::Error::Failed(format!("parsing {}: {err}", path.display())))
}

fn read_status(device: &Path) -> zbus::fdo::Result<BrightnessStatus> {
    let brightness = read_u32(&device.join("brightness"))?;
    let max = read_u32(&device.join("max_brightness"))?;
    let percent = (brightness * 100 + max / 2)
        .checked_div(max)
        .unwrap_or(0)
        .min(100) as u8;

    Ok(BrightnessStatus { percent })
}

/// Spawns the background thread that emits `Brightness1.Changed` whenever
/// the backlight device's `brightness` file reports a real change — never
/// on a timer. Sysfs has no D-Bus signal of its own, so this watches the
/// file directly via `inotify` rather than polling: the kernel backlight
/// driver writes this file (via `sysfs_notify`) on every change, whether
/// triggered by `nkdhrd` itself (through `logind`'s `SetBrightness`), a
/// hotkey, or another process, so this catches every source, not just
/// nkdhr's own.
pub fn spawn_watcher(device: PathBuf, session: Connection) {
    thread::spawn(move || {
        if let Err(err) = watch(&device, &session) {
            eprintln!("nkdhrd: brightness watcher exited: {err}");
        }
    });
}

fn watch(device: &Path, session: &Connection) -> io::Result<()> {
    let mut inotify = Inotify::init()?;
    inotify
        .watches()
        .add(device.join("brightness"), WatchMask::MODIFY)?;

    let iface = session
        .object_server()
        .interface::<_, Brightness>(BRIGHTNESS_OBJECT_PATH)
        .map_err(io::Error::other)?;

    let mut last = None;
    let mut buffer = [0; 1024];
    loop {
        // Blocks on the inotify file descriptor until the kernel reports at
        // least one event; never polls.
        for _event in inotify.read_events_blocking(&mut buffer)? {
            let status = read_status(device).map_err(io::Error::other)?;
            if last.as_ref() != Some(&status) {
                zbus::block_on(Brightness::changed(iface.signal_emitter(), status.clone()))
                    .map_err(io::Error::other)?;
                last = Some(status);
            }
        }
    }
}
