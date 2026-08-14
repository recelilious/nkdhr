//! Reusable Settings application surface shared by both UI-5 hosts.

use std::{
    fmt,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use nkdhr_ipc::ConfigProxyBlocking;
use nkdhr_render::{DisplayList, TextureStore};
use nkdhr_theme::{MotionPresetLibraryData, ThemeProfile};
use nkdhr_ui::text::{TextConfig, TextResources};
use nkdhr_ui::{
    DispatchResult, MaterialCapabilities, Reactive, Size, ThemeRuntime, UiEvent, UiHost, UiResult,
    UiRoot, UiSurface, WidgetId,
};

use zbus::{
    blocking::Connection,
    zvariant::{OwnedValue, Value},
};

use crate::{
    ACTIVE_THEME_PROFILE_KEY, AppearanceSettings, MOTION_PRESET_LIBRARY_KEY,
    MotionPresetLibraryEditor, MotionPresetPersistenceRequest, MotionPresetPersistenceToken,
    SettingsAssets, ThemePersistenceRequest, ThemePersistenceToken, ThemeProfileEditor,
};

/// The complete retained Settings application, without any window-system or
/// compositor-scene ownership. This is the identity UI-5 exercises in both
/// hosts; adapters only supply size, scale, normalized input and presentation.
pub struct AppearanceSurface {
    model: AppearanceSettings,
    assets: SettingsAssets,
    theme_runtime: ThemeRuntime,
    composition_revision: Reactive<u64>,
    seen_composition_revision: u64,
    seen_theme_generation: u64,
    viewport: Size,
    capabilities: MaterialCapabilities,
    persistence: SettingsPersistenceWorker,
    host: UiHost,
}

impl AppearanceSurface {
    pub fn new(
        viewport: Size,
        output_scale: f32,
        capabilities: MaterialCapabilities,
    ) -> Result<Self, AppearanceHostError> {
        let text = TextResources::from_config(TextConfig::default(), output_scale)
            .map_err(AppearanceHostError::new)?;
        Self::with_text_resources(viewport, output_scale, capabilities, text)
    }

    pub fn with_text_resources(
        viewport: Size,
        output_scale: f32,
        capabilities: MaterialCapabilities,
        mut text: TextResources,
    ) -> Result<Self, AppearanceHostError> {
        let (theme_profiles, motion_presets) = load_settings_editors();
        let model = AppearanceSettings::with_editors(theme_profiles, motion_presets);
        let assets = SettingsAssets::load(text.textures_mut()).map_err(AppearanceHostError::new)?;
        let theme_runtime = model.theme_runtime();
        let snapshot = theme_runtime.snapshot();
        let element = model
            .element(viewport, snapshot.theme(), &assets, capabilities)
            .map_err(AppearanceHostError::new)?;
        let root = UiRoot::with_text(element, text).map_err(AppearanceHostError::new)?;
        let host = UiHost::new(root, viewport, output_scale).map_err(AppearanceHostError::new)?;
        let composition_revision = model.composition_revision();
        Ok(Self {
            model,
            assets,
            theme_runtime,
            seen_composition_revision: composition_revision.get(),
            seen_theme_generation: snapshot.generation(),
            composition_revision,
            viewport,
            capabilities,
            persistence: SettingsPersistenceWorker::default(),
            host,
        })
    }

    pub fn model(&self) -> &AppearanceSettings {
        &self.model
    }

    fn reconcile_if_needed(&mut self, viewport: Size) -> UiResult<()> {
        let revision = self.composition_revision.get();
        let theme = self.theme_runtime.snapshot();
        if self.viewport == viewport
            && self.seen_composition_revision == revision
            && self.seen_theme_generation == theme.generation()
        {
            return Ok(());
        }
        let element = self
            .model
            .element(viewport, theme.theme(), &self.assets, self.capabilities)
            .map_err(|error| nkdhr_ui::UiError::Text(error.to_string()))?;
        self.host.reconcile(element)?;
        self.viewport = viewport;
        self.seen_composition_revision = revision;
        self.seen_theme_generation = theme.generation();
        Ok(())
    }

    fn flush_persistence(&mut self) {
        if let Some(request) = self.model.take_motion_editor_persistence_request() {
            let token = request.token();
            if let Err(error) = self.persistence.submit_theme(request) {
                self.model
                    .complete_theme_persistence(token, Err(format!("动画设置保存失败：{error}")));
            }
        }
        if let Some(request) = self.model.take_motion_preset_persistence_request() {
            let token = request.token();
            if let Err(error) = self.persistence.submit_motion_preset(request) {
                self.model.complete_motion_preset_persistence(
                    token,
                    Err(format!("动画预设资料库保存失败：{error}")),
                );
            }
        }
        while let Some(completion) = self.persistence.try_completion() {
            match completion {
                SettingsPersistenceCompletion::Theme { token, result } => {
                    self.model.complete_theme_persistence(token, result);
                }
                SettingsPersistenceCompletion::MotionPreset { token, result } => {
                    self.model.complete_motion_preset_persistence(token, result);
                }
            }
        }
    }
}

impl UiSurface for AppearanceSurface {
    fn render(&mut self, logical_size: Size, output_scale: f32) -> UiResult<()> {
        self.flush_persistence();
        self.reconcile_if_needed(logical_size)?;
        self.host.resize(logical_size, output_scale)?;
        self.host.render().map(|_| ())
    }

    fn display_list(&self) -> &DisplayList {
        self.host.display_list()
    }

    fn textures(&self) -> &TextureStore {
        self.host.textures()
    }

    fn commit(&self) -> u64 {
        self.host.commit()
    }

    fn dispatch(&mut self, event: &UiEvent) -> UiResult<DispatchResult> {
        let result = self.host.dispatch(event)?;
        self.flush_persistence();
        Ok(result)
    }

    fn pointer_capture(&self) -> Option<WidgetId> {
        self.host.pointer_capture()
    }

    fn keyboard_focus(&self) -> Option<WidgetId> {
        self.host.keyboard_focus()
    }

    fn frame_requested(&mut self) -> bool {
        self.flush_persistence();
        self.composition_revision.get() != self.seen_composition_revision
            || self.theme_runtime.snapshot().generation() != self.seen_theme_generation
            || self.host.frame_requested()
    }
}

#[derive(Default)]
struct SettingsPersistenceWorker {
    running: Option<RunningPersistenceWorker>,
}

struct RunningPersistenceWorker {
    requests: Sender<SettingsPersistenceRequest>,
    completions: Receiver<SettingsPersistenceCompletion>,
}

enum SettingsPersistenceRequest {
    Theme(ThemePersistenceRequest),
    MotionPreset(MotionPresetPersistenceRequest),
}

enum SettingsPersistenceCompletion {
    Theme {
        token: ThemePersistenceToken,
        result: Result<String, String>,
    },
    MotionPreset {
        token: MotionPresetPersistenceToken,
        result: Result<String, String>,
    },
}

impl SettingsPersistenceWorker {
    fn submit_theme(&mut self, request: ThemePersistenceRequest) -> Result<(), String> {
        self.submit(SettingsPersistenceRequest::Theme(request))
    }

    fn submit_motion_preset(
        &mut self,
        request: MotionPresetPersistenceRequest,
    ) -> Result<(), String> {
        self.submit(SettingsPersistenceRequest::MotionPreset(request))
    }

    fn submit(&mut self, request: SettingsPersistenceRequest) -> Result<(), String> {
        if self.running.is_none() {
            self.running = Some(spawn_persistence_worker()?);
        }
        let running = self.running.as_ref().expect("worker was just started");
        running
            .requests
            .send(request)
            .map_err(|_| "后台持久化工作线程已停止".to_owned())
    }

    fn try_completion(&mut self) -> Option<SettingsPersistenceCompletion> {
        let running = self.running.as_ref()?;
        match running.completions.try_recv() {
            Ok(completion) => Some(completion),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.running = None;
                None
            }
        }
    }
}

fn spawn_persistence_worker() -> Result<RunningPersistenceWorker, String> {
    let (request_sender, request_receiver) = mpsc::channel::<SettingsPersistenceRequest>();
    let (completion_sender, completion_receiver) = mpsc::channel::<SettingsPersistenceCompletion>();
    thread::Builder::new()
        .name("nkdhr-settings-persistence".to_owned())
        .spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let completion = match request {
                    SettingsPersistenceRequest::Theme(request) => {
                        let token = request.token();
                        let result =
                            persist_config_value(request.key(), request.value(), "动画设置已保存");
                        SettingsPersistenceCompletion::Theme { token, result }
                    }
                    SettingsPersistenceRequest::MotionPreset(request) => {
                        let token = request.token();
                        let result = persist_config_value(
                            request.key(),
                            request.value(),
                            "动画预设资料库已保存",
                        );
                        SettingsPersistenceCompletion::MotionPreset { token, result }
                    }
                };
                if completion_sender.send(completion).is_err() {
                    break;
                }
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(RunningPersistenceWorker {
        requests: request_sender,
        completions: completion_receiver,
    })
}

fn persist_config_value(key: &str, value: &str, status: &str) -> Result<String, String> {
    let connection = Connection::session().map_err(|error| error.to_string())?;
    let config = ConfigProxyBlocking::new(&connection).map_err(|error| error.to_string())?;
    config
        .set(key, Value::new(value))
        .map_err(|error| error.to_string())?;
    Ok(status.to_owned())
}

fn load_settings_editors() -> (ThemeProfileEditor, MotionPresetLibraryEditor) {
    let Ok(connection) = Connection::session() else {
        return Default::default();
    };
    let Ok(config) = ConfigProxyBlocking::new(&connection) else {
        return Default::default();
    };
    let theme_profiles = config
        .get(ACTIVE_THEME_PROFILE_KEY)
        .ok()
        .and_then(owned_string)
        .and_then(|text| ThemeProfile::from_json(&text).ok())
        .and_then(|profile| ThemeProfileEditor::new(profile, Default::default()).ok())
        .unwrap_or_default();
    let motion_presets = config
        .get(MOTION_PRESET_LIBRARY_KEY)
        .ok()
        .and_then(owned_string)
        .and_then(|text| MotionPresetLibraryData::from_json(&text).ok())
        .and_then(|library| MotionPresetLibraryEditor::new(library).ok())
        .unwrap_or_default();
    (theme_profiles, motion_presets)
}

fn owned_string(stored: OwnedValue) -> Option<String> {
    let Value::Str(text) = Value::from(stored) else {
        return None;
    };
    Some(text.as_str().to_owned())
}

#[derive(Debug)]
pub struct AppearanceHostError(String);

impl AppearanceHostError {
    fn new(error: impl fmt::Display) -> Self {
        Self(error.to_string())
    }
}

impl fmt::Display for AppearanceHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AppearanceHostError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities() -> MaterialCapabilities {
        MaterialCapabilities {
            backdrop_blur: false,
            reduced_transparency: false,
            high_contrast: false,
        }
    }

    #[test]
    fn identical_application_surfaces_record_identical_frames() {
        let size = Size::new(960.0, 640.0);
        let mut compositor = AppearanceSurface::new(size, 1.0, capabilities()).unwrap();
        let mut standalone = AppearanceSurface::new(size, 1.0, capabilities()).unwrap();

        compositor.render(size, 1.0).unwrap();
        standalone.render(size, 1.0).unwrap();

        assert_eq!(compositor.display_list(), standalone.display_list());
        assert_eq!(compositor.commit(), standalone.commit());
    }
}
