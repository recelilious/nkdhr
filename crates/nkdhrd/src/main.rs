mod backends;
mod daemon;
mod modules;

use std::thread;

use backends::pipewire_client;
use daemon::Daemon;
use modules::audio::Audio;
use modules::brightness::Brightness;
use modules::network::Network;
use modules::power::Power;
use modules::session::Session;
use nkdhr_ipc::{
    AUDIO_OBJECT_PATH, BRIGHTNESS_OBJECT_PATH, BUS_NAME, DAEMON_OBJECT_PATH, NETWORK_OBJECT_PATH,
    POWER_OBJECT_PATH, SESSION_OBJECT_PATH,
};
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::fdo::RequestNameFlags;

fn run() -> zbus::Result<()> {
    let system = Connection::system()?;
    let mut modules = Vec::new();

    let mut builder = Builder::session()?
        .serve_at(SESSION_OBJECT_PATH, Session::new(system.clone()))?
        .serve_at(NETWORK_OBJECT_PATH, Network::new(system.clone()))?
        .serve_at(POWER_OBJECT_PATH, Power::new(system.clone()))?
        .serve_at(AUDIO_OBJECT_PATH, Audio::new(pipewire_client::spawn()))?;
    modules.push("Session".to_owned());
    modules.push("Network".to_owned());
    modules.push("Power".to_owned());
    modules.push("Audio".to_owned());

    match Brightness::new() {
        Ok(brightness) => {
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
