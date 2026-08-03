mod backends;
mod daemon;
mod modules;

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use backends::config_store::{ConfigStore, NamespaceSchema};
use backends::pipewire_client;
use daemon::Daemon;
use modules::audio::Audio;
use modules::brightness::Brightness;
use modules::config::Config;
use modules::network::Network;
use modules::power::Power;
use modules::session::Session;
use nkdhr_ipc::{
    AUDIO_OBJECT_PATH, BRIGHTNESS_OBJECT_PATH, BUS_NAME, CONFIG_OBJECT_PATH, DAEMON_OBJECT_PATH,
    NETWORK_OBJECT_PATH, POWER_OBJECT_PATH, SESSION_OBJECT_PATH,
};
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::fdo::RequestNameFlags;

/// CTRL-5's namespace registry. Empty for now — see
/// `backends::config_store`'s module doc for why — and grown by later
/// phases (UI-4's `theme`, COMP-3's `canvas`, ...) each adding their own
/// entry here as they land.
static NAMESPACES: &[NamespaceSchema] = &[];

/// `$XDG_CONFIG_HOME/nkdhr`, falling back to `$HOME/.config/nkdhr` per the
/// XDG base directory spec.
fn config_dir() -> zbus::Result<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("nkdhr"));
    }
    let home = std::env::var("HOME")
        .map_err(|_| zbus::Error::Failure("neither XDG_CONFIG_HOME nor HOME is set".to_owned()))?;
    Ok(PathBuf::from(home).join(".config").join("nkdhr"))
}

fn run() -> zbus::Result<()> {
    let system = Connection::system()?;
    let mut modules = Vec::new();

    let pipewire = pipewire_client::spawn();
    let config_store = Arc::new(ConfigStore::open(config_dir()?, NAMESPACES)?);

    let mut builder = Builder::session()?
        .serve_at(SESSION_OBJECT_PATH, Session::new(system.clone()))?
        .serve_at(NETWORK_OBJECT_PATH, Network::new(system.clone()))?
        .serve_at(POWER_OBJECT_PATH, Power::new(system.clone()))?
        .serve_at(
            AUDIO_OBJECT_PATH,
            Audio::new(system.clone(), pipewire.clone()),
        )?
        .serve_at(CONFIG_OBJECT_PATH, Config::new(config_store.clone()))?;
    modules.push("Session".to_owned());
    modules.push("Network".to_owned());
    modules.push("Power".to_owned());
    modules.push("Audio".to_owned());
    modules.push("Config".to_owned());

    let mut brightness_device: Option<PathBuf> = None;
    match Brightness::new(system.clone()) {
        Ok(brightness) => {
            brightness_device = Some(brightness.device_path().to_owned());
            builder = builder.serve_at(BRIGHTNESS_OBJECT_PATH, brightness)?;
            modules.push("Brightness".to_owned());
        }
        Err(err) => eprintln!("nkdhrd: brightness module unavailable, skipping: {err}"),
    }

    let connection = builder
        .serve_at(DAEMON_OBJECT_PATH, Daemon::new(modules))?
        .build()?;

    // `Builder::name` requests the well-known name without `DoNotQueue`, so a
    // second instance would sit queued indefinitely instead of failing.
    // Request it manually with `DoNotQueue` so a second instance errors out.
    connection.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())?;

    // CTRL-4: each module's change watcher needs the final, fully-built
    // session connection (to emit its own `Changed` signal) and so can only
    // start after `build()` above, once every object is actually being
    // served. Backend reads still go over `system`.
    modules::power::spawn_watcher(system.clone(), connection.clone());
    modules::network::spawn_watcher(system.clone(), connection.clone());
    modules::session::spawn_watcher(system.clone(), connection.clone());
    if let Some(device) = brightness_device {
        modules::brightness::spawn_watcher(device, connection.clone());
    }
    modules::config::spawn_watcher(config_store, connection.clone());
    modules::audio::attach_watcher(pipewire, connection);

    loop {
        thread::park();
    }
}

fn main() {
    if let Err(err) = run() {
        match err {
            zbus::Error::NameTaken => {
                eprintln!("nkdhrd: another instance already owns {BUS_NAME} on the session bus");
            }
            other => eprintln!("nkdhrd: failed to start: {other}"),
        }
        std::process::exit(1);
    }
}
