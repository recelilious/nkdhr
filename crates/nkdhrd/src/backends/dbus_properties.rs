//! Generic helper for watching `org.freedesktop.DBus.Properties.PropertiesChanged`
//! on an arbitrary object. Every CTRL-4 watcher (Power, Network, Session) is
//! built on this: react to the standard properties-changed signal, then
//! recompute and diff the module's whole status, rather than tracking
//! individual properties one by one.

use zbus::blocking::proxy::SignalIterator;
use zbus::blocking::{Connection, Proxy};

/// Returns a blocking iterator that yields once per `PropertiesChanged`
/// signal emitted by the object at `path` on `destination`. Iterating never
/// polls: [`zbus::blocking::Proxy::receive_signal`] parks the calling thread
/// on the connection's socket between signals.
///
/// The signal's own payload (which properties changed, to what) is not
/// decoded here — callers treat receipt as "something changed, recompute",
/// which is simpler and, for nkdhr's whole-status-per-module model, exactly
/// as precise as decoding the payload would be.
pub fn watch(
    connection: &Connection,
    destination: &str,
    path: &str,
) -> zbus::Result<SignalIterator<'static>> {
    let proxy = Proxy::new(
        connection,
        destination,
        path.to_owned(),
        "org.freedesktop.DBus.Properties",
    )?;
    proxy.receive_signal("PropertiesChanged")
}
