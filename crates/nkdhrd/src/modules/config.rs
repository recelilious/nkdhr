use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use inotify::{Inotify, WatchMask};
use nkdhr_ipc::CONFIG_OBJECT_PATH;
use zbus::blocking::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

use crate::backends::config_store::{self, ConfigError, ConfigStore};

pub struct Config {
    store: Arc<ConfigStore>,
}

impl Config {
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self { store }
    }
}

#[zbus::interface(name = "org.nkdhr.Config1")]
impl Config {
    fn get(&self, key: &str) -> zbus::fdo::Result<OwnedValue> {
        let value = self.store.get(key).map_err(to_fdo_error)?;
        to_owned_value(&value)
    }

    async fn set(
        &self,
        key: &str,
        value: Value<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let json = config_store::variant_to_json(&value).map_err(to_fdo_error)?;
        let stored = self.store.set(key, json).map_err(to_fdo_error)?;
        let owned = to_owned_value(&stored)?;
        emitter
            .changed(key.to_owned(), owned)
            .await
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
    }

    fn get_all(&self, prefix: &str) -> zbus::fdo::Result<HashMap<String, OwnedValue>> {
        self.store
            .get_all(prefix)
            .map_err(to_fdo_error)?
            .iter()
            .map(|(key, value)| Ok((key.clone(), to_owned_value(value)?)))
            .collect()
    }

    /// Fired whenever [`spawn_watcher`] observes a real, re-validated
    /// change to a namespace's backing file, and directly by [`set`] for
    /// its own writes.
    #[zbus(signal)]
    async fn changed(
        signal_emitter: &SignalEmitter<'_>,
        key: String,
        value: OwnedValue,
    ) -> zbus::Result<()>;
}

fn to_owned_value(value: &serde_json::Value) -> zbus::fdo::Result<OwnedValue> {
    let variant = config_store::json_to_variant(value).map_err(to_fdo_error)?;
    OwnedValue::try_from(variant)
        .map_err(|err| zbus::fdo::Error::Failed(format!("encoding config value: {err}")))
}

fn to_fdo_error(err: ConfigError) -> zbus::fdo::Error {
    zbus::fdo::Error::InvalidArgs(err.to_string())
}

/// Spawns the background thread that emits `Config1.Changed` whenever a
/// registered namespace's backing TOML file is edited externally (by hand,
/// or by `nkdhrd`'s own atomic write-then-rename in [`ConfigStore::set`],
/// though that path already emits directly from `set` — this watcher's
/// resulting reload is then a same-value no-op, not a duplicate signal).
/// Never polls: the thread blocks on `inotify` between events.
pub fn spawn_watcher(store: Arc<ConfigStore>, session: Connection) {
    thread::spawn(move || {
        if let Err(err) = watch(&store, &session) {
            eprintln!("nkdhrd: config watcher exited: {err}");
        }
    });
}

fn watch(store: &Arc<ConfigStore>, session: &Connection) -> std::io::Result<()> {
    let mut inotify = Inotify::init()?;
    inotify
        .watches()
        .add(store.dir(), WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO)?;

    let iface = session
        .object_server()
        .interface::<_, Config>(CONFIG_OBJECT_PATH)
        .map_err(std::io::Error::other)?;

    let mut buffer = [0; 4096];
    loop {
        // Blocks on the inotify file descriptor until the kernel reports at
        // least one event; never polls.
        for event in inotify.read_events_blocking(&mut buffer)? {
            let Some(name) = event.name.and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(namespace) = namespace_of(name) else {
                continue;
            };

            match store.reload(namespace) {
                Ok(changed) => {
                    for (key, value) in changed {
                        let owned = to_owned_value(&value).map_err(std::io::Error::other)?;
                        zbus::block_on(Config::changed(iface.signal_emitter(), key, owned))
                            .map_err(std::io::Error::other)?;
                    }
                }
                // Not one of ours (e.g. a stray file dropped into the
                // config directory by something else) — ignore silently.
                Err(ConfigError::UnknownNamespace(_)) => {}
                Err(err) => {
                    eprintln!("nkdhrd: rejected external edit to {name}: {err}");
                }
            }
        }
    }
}

/// `<namespace>.toml` -> `Some(namespace)`; anything else in the config
/// directory (the atomic-write `.toml.tmp` sibling, an unrelated file) ->
/// `None`.
fn namespace_of(file_name: &str) -> Option<&str> {
    file_name.strip_suffix(".toml")
}
