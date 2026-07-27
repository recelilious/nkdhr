use nkdhr_ipc::AudioStatus;
use zbus::blocking::Connection;
use zbus::message::Header;

use crate::backends::pipewire_client::PipeWireHandle;
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
        let state = self
            .pipewire
            .state
            .lock()
            .expect("audio state mutex poisoned");
        AudioStatus {
            sink_name: state.sink_name.clone().into(),
            sink_volume_percent: state.sink_volume_percent.into(),
            sink_muted: state.sink_muted.unwrap_or(false),
            source_name: state.source_name.clone().into(),
            source_volume_percent: state.source_volume_percent.into(),
            source_muted: state.source_muted.unwrap_or(false),
        }
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
}
