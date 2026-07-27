use std::thread;
use std::time::Instant;

use nkdhr_ipc::{BUS_NAME, DaemonStatus, OBJECT_PATH};
use zbus::blocking::connection::Builder;
use zbus::fdo::RequestNameFlags;

struct Daemon {
    start: Instant,
}

#[zbus::interface(name = "org.nkdhr.Daemon1")]
impl Daemon {
    fn ping(&self) -> String {
        "pong".to_owned()
    }

    fn get_status(&self) -> DaemonStatus {
        DaemonStatus {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            uptime_secs: self.start.elapsed().as_secs(),
            modules: Vec::new(),
        }
    }

    fn get_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_owned()
    }
}

fn run() -> zbus::Result<()> {
    let daemon = Daemon {
        start: Instant::now(),
    };
    let connection = Builder::session()?.serve_at(OBJECT_PATH, daemon)?.build()?;

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
