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
            volume_percent: state.volume_percent.into(),
            muted: state.muted.unwrap_or(false),
        }
    }
}
