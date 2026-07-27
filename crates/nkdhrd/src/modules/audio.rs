use nkdhr_ipc::AudioStatus;

use crate::backends::pipewire_client::SharedAudioState;

pub struct Audio {
    state: SharedAudioState,
}

impl Audio {
    pub fn new(state: SharedAudioState) -> Self {
        Self { state }
    }
}

#[zbus::interface(name = "org.nkdhr.Audio1")]
impl Audio {
    fn get_status(&self) -> AudioStatus {
        let state = self.state.lock().expect("audio state mutex poisoned");
        AudioStatus {
            sink_name: state.sink_name.clone().into(),
            sink_volume_percent: state.sink_volume_percent.into(),
            sink_muted: state.sink_muted.unwrap_or(false),
            source_name: state.source_name.clone().into(),
            source_volume_percent: state.source_volume_percent.into(),
            source_muted: state.source_muted.unwrap_or(false),
        }
    }
}
