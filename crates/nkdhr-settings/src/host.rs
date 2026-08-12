//! Reusable Settings application surface shared by both UI-5 hosts.

use std::fmt;

use nkdhr_render::{DisplayList, TextureStore};
use nkdhr_ui::text::{TextConfig, TextResources};
use nkdhr_ui::{
    DispatchResult, MaterialCapabilities, Reactive, Size, ThemeRuntime, UiEvent, UiHost, UiResult,
    UiRoot, UiSurface, WidgetId,
};

use crate::{AppearanceSettings, SettingsAssets};

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
        let model = AppearanceSettings::new();
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
}

impl UiSurface for AppearanceSurface {
    fn render(&mut self, logical_size: Size, output_scale: f32) -> UiResult<()> {
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
        self.host.dispatch(event)
    }

    fn pointer_capture(&self) -> Option<WidgetId> {
        self.host.pointer_capture()
    }

    fn keyboard_focus(&self) -> Option<WidgetId> {
        self.host.keyboard_focus()
    }

    fn frame_requested(&mut self) -> bool {
        self.composition_revision.get() != self.seen_composition_revision
            || self.theme_runtime.snapshot().generation() != self.seen_theme_generation
            || self.host.frame_requested()
    }
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
