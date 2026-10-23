use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

/// Object path at which `nkdhrd` serves `org.nkdhr.Config1`.
pub const CONFIG_OBJECT_PATH: &str = "/org/nkdhr/Config1";

/// Client-side contract for `org.nkdhr.Config1`, the CTRL-5 config store.
///
/// Keys are dotted paths: the namespace, then one segment per nested table
/// down to the value (e.g. `outputs.eDP-1.scale`). `nkdhr-ipc` carries this
/// wire-level contract, not the schemas: which namespaces exist and what
/// fields each one has is defined by the namespace schemas `nkdhrd`
/// registers with its config store (see `nkdhrd`'s `backends::config_store`
/// and `namespaces`). A namespace's value types live in this crate only
/// when a client has to decode them too, as with [`crate::OutputPlacements`].
#[zbus::proxy(
    interface = "org.nkdhr.Config1",
    default_service = "org.nkdhr.Daemon1",
    default_path = "/org/nkdhr/Config1"
)]
pub trait Config {
    /// Reads one leaf value by its dotted key.
    async fn get(&self, key: &str) -> zbus::Result<OwnedValue>;

    /// Sets one leaf value by its dotted key. Rejected (leaving the
    /// previous value active) if the key is unknown, the namespace has no
    /// registered schema, or the resulting namespace fails validation.
    async fn set(&self, key: &str, value: Value<'_>) -> zbus::Result<()>;

    /// Reads every leaf value under `prefix` (a namespace name, a deeper
    /// dotted path within one, or `""` for every registered namespace),
    /// keyed by its full dotted path.
    async fn get_all(&self, prefix: &str) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// Fired whenever a leaf value actually changes — via `set`, or via a
    /// re-validated external edit to the backing TOML file. Never on a
    /// timer. `nkdhrctl config watch <prefix>` is a thin loop over this
    /// signal, filtering to keys under `prefix`.
    #[zbus(signal)]
    fn changed(&self, key: String, value: OwnedValue) -> zbus::Result<()>;
}
