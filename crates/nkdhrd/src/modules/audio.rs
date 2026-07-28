use std::sync::Mutex;

use nkdhr_ipc::{AUDIO_OBJECT_PATH, AudioStatus};
use zbus::blocking::Connection;
use zbus::message::Header;

use crate::backends::pipewire_client::{PipeWireHandle, SharedAudioState};
use crate::backends::polkit;

pub struct Audio {
    system: Connection,
    pipewire: PipeWireHandle,
}

impl Audio {
    pub fn new(system: Connection, pipewire: PipeWireHandle) -> Self {
        Self { system, pipewire }
    }
}

#[zbus::interface(name = "org.nkdhr.Audio1")]
impl Audio {
    fn get_status(&self) -> AudioStatus {
        read_status(&self.pipewire.state)
    }

    async fn set_volume(
        &self,
        percent: u8,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        polkit::check_authorization(
            &self.system,
            connection,
            &header,
            "org.nkdhr.policy.audio-set-volume",
        )
        .await?;
        self.pipewire.set_sink_volume(percent);
        Ok(())
    }

    async fn set_mute(
        &self,
        muted: bool,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        polkit::check_authorization(
            &self.system,
            connection,
            &header,
            "org.nkdhr.policy.audio-set-mute",
        )
        .await?;
        self.pipewire.set_sink_muted(muted);
        Ok(())
    }

    /// Fired whenever [`attach_watcher`] observes a real change to the
    /// status [`get_status`](Self::get_status) would now return.
    #[zbus(signal)]
    async fn changed(
        signal_emitter: &zbus::object_server::SignalEmitter<'_>,
        status: AudioStatus,
    ) -> zbus::Result<()>;
}

fn read_status(state: &SharedAudioState) -> AudioStatus {
    let state = state.lock().expect("audio state mutex poisoned");
    AudioStatus {
        sink_name: state.sink_name.clone().into(),
        sink_volume_percent: state.sink_volume_percent.into(),
        sink_muted: state.sink_muted.unwrap_or(false),
        source_name: state.source_name.clone().into(),
        source_volume_percent: state.source_volume_percent.into(),
        source_muted: state.source_muted.unwrap_or(false),
    }
}

/// Attaches the CTRL-4 watcher to the already-running PipeWire worker
/// thread, rather than spawning a new one: `nkdhrd`'s PipeWire connection
/// is already event-driven end to end (see `backends::pipewire_client`), so
/// `Audio1.Changed` piggybacks on its existing `reconcile()` step instead of
/// polling PipeWire a second time from a separate thread.
///
/// Must be called after `session`'s `Audio1` object is registered (i.e.
/// after the daemon's connection is fully built), since it looks that
/// registration up via the object server.
pub fn attach_watcher(pipewire: PipeWireHandle, session: Connection) {
    let iface = match session
        .object_server()
        .interface::<_, Audio>(AUDIO_OBJECT_PATH)
    {
        Ok(iface) => iface,
        Err(err) => {
            eprintln!("nkdhrd: audio watcher failed to attach: {err}");
            return;
        }
    };

    let state = pipewire.state.clone();
    let last = Mutex::new(None);
    pipewire.on_change(move || {
        let status = read_status(&state);
        let mut last = last
            .lock()
            .expect("audio watcher last-status mutex poisoned");
        if last.as_ref() != Some(&status) {
            // Logged rather than propagated: this runs inside the PipeWire
            // worker's own event callbacks, and a transient D-Bus send
            // failure shouldn't take down audio tracking entirely.
            if let Err(err) = zbus::block_on(Audio::changed(iface.signal_emitter(), status.clone()))
            {
                eprintln!("nkdhrd: failed to emit Audio1.Changed: {err}");
            }
            *last = Some(status);
        }
    });
}
