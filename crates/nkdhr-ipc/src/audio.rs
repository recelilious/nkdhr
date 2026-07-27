use serde::{Deserialize, Serialize};
use zbus::zvariant::{Optional, Type};

/// Object path at which `nkdhrd` serves `org.nkdhr.Audio1`.
pub const AUDIO_OBJECT_PATH: &str = "/org/nkdhr/Audio1";

/// Snapshot returned by [`AudioProxy::get_status`], describing PipeWire's
/// default sink (playback) and default source (capture).
///
/// `*_name` and `*_volume_percent` use [`Optional`] because they are
/// unknown until `nkdhrd`'s PipeWire worker thread has resolved which node
/// is the current default sink/source and read its first `Props` update —
/// briefly after startup, or if no such device exists at all (no
/// microphone, say), there is nothing to report yet. `*_muted` fields are
/// plain `bool` rather than `Optional<bool>`: zvariant's `Optional` encodes
/// absence as the type's default value, and `bool`'s default (`false`) is
/// indistinguishable from a real "not muted" — encoding `Optional<bool>`
/// panics by design. `false` doubles as nkdhr's "not known to be muted"
/// sentinel here.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AudioStatus {
    pub sink_name: Optional<String>,
    pub sink_volume_percent: Optional<u8>,
    pub sink_muted: bool,
    pub source_name: Optional<String>,
    pub source_volume_percent: Optional<u8>,
    pub source_muted: bool,
}

/// Client-side contract for `org.nkdhr.Audio1`. See `daemon.rs` for why the
/// interface/service/path strings are duplicated as literals here.
#[zbus::proxy(
    interface = "org.nkdhr.Audio1",
    default_service = "org.nkdhr.Daemon1",
    default_path = "/org/nkdhr/Audio1"
)]
pub trait Audio {
    async fn get_status(&self) -> zbus::Result<AudioStatus>;
    async fn set_volume(&self, percent: u8) -> zbus::Result<()>;
    async fn set_mute(&self, muted: bool) -> zbus::Result<()>;
}
