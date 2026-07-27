use std::time::Instant;

use nkdhr_ipc::DaemonStatus;

pub struct Daemon {
    start: Instant,
    modules: Vec<String>,
}

impl Daemon {
    pub fn new(modules: Vec<String>) -> Self {
        Self {
            start: Instant::now(),
            modules,
        }
    }
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
            modules: self.modules.clone(),
        }
    }

    fn get_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_owned()
    }
}
