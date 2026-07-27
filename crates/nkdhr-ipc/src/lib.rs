//! Shared D-Bus contract for the nkdhr control plane: bus, path and
//! interface identifiers, wire types, and client proxies used by `nkdhrd`,
//! `nkdhrctl`, and any other process that talks to the daemon.

mod audio;
mod brightness;
mod daemon;
mod network;
mod power;
mod session;

pub use audio::{AUDIO_OBJECT_PATH, AudioProxy, AudioProxyBlocking, AudioStatus};
pub use brightness::{
    BRIGHTNESS_OBJECT_PATH, BrightnessProxy, BrightnessProxyBlocking, BrightnessStatus,
};
pub use daemon::{DAEMON_OBJECT_PATH, DaemonProxy, DaemonProxyBlocking, DaemonStatus};
pub use network::{NETWORK_OBJECT_PATH, NetworkProxy, NetworkProxyBlocking, NetworkStatus};
pub use power::{POWER_OBJECT_PATH, PowerProxy, PowerProxyBlocking, PowerStatus};
pub use session::{SESSION_OBJECT_PATH, SessionProxy, SessionProxyBlocking, SessionStatus};

/// Well-known bus name `nkdhrd` requests on the session bus. Every object in
/// the tree below is served through this one name.
pub const BUS_NAME: &str = "org.nkdhr.Daemon1";
