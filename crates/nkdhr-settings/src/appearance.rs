//! Appearance & Interaction presentation model for the nkdhr Settings application.
//!
//! The standalone Wayland and in-compositor hosts are intentionally separate
//! from this crate-level view model. Both hosts reconcile the same element
//! tree and provide their material capabilities at the frame boundary.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::BTreeMap,
    error::Error,
    fmt,
    rc::Rc,
    sync::Arc,
};

use nkdhr_render::{Color, Rect, Sampling, TextureError, TextureId, TextureStore};
use nkdhr_theme::{BuiltInTheme, PaletteData, ThemeBase, ThemeProfile};
use nkdhr_ui::{
    Align, Alignment, AnimationCtx, ArrangeCtx, Axis, Button, ButtonVariant, Constraints,
    CrossAxisAlignment, Element, Flex, GlassSurface, Insets, Invalidation, List, ListEntry,
    ListItem, MainAxisAlignment, MaterialCapabilities, MaterialTier, MeasureCtx, MotionFamily,
    Padding, PaintCtx, Reactive, ScalarMotion, Scroll, ScrollOffset, SemanticRole, Semantics,
    SemanticsCtx, Size, Slider, Text, TextInput, TextInputStatus, TextRole, Theme, ThemeReadSet,
    ThemeRuntime, Toggle, UiError, UpdateCtx, Widget,
};

use crate::{
    ThemeEditorError, ThemeEditorFeedback, ThemePersistenceRequest, ThemePersistenceTarget,
    ThemePersistenceToken, ThemeProfileEditor, WallpaperRegenerationOutcome,
    WallpaperRegenerationToken,
};

pub const DEFAULT_WINDOW_WIDTH: f32 = 1_160.0;
pub const DEFAULT_WINDOW_HEIGHT: f32 = 760.0;
pub const MINIMUM_WINDOW_WIDTH: f32 = 640.0;
pub const MINIMUM_WINDOW_HEIGHT: f32 = 480.0;
pub const WINDOW_OUTPUT_INSET: f32 = 48.0;
pub const HEADER_HEIGHT: f32 = 60.0;
pub const NAVIGATION_WIDTH: f32 = 216.0;
pub const COMPACT_NAVIGATION_WIDTH: f32 = 64.0;
pub const INSPECTOR_WIDTH: f32 = 288.0;
pub const LAYOUT_INSET: f32 = 16.0;
pub const LAYOUT_GAP: f32 = 16.0;
pub const STACKED_ROW_BREAKPOINT: f32 = 560.0;
pub const CONTENT_IDEAL_MAX_WIDTH: f32 = 720.0;

const STATUS_HEIGHT: f32 = 48.0;
const WIDE_BREAKPOINT: f32 = 1_120.0;
const NAVIGATION_BREAKPOINT: f32 = 820.0;
const SINGLE_COLUMN_BREAKPOINT: f32 = 680.0;
const CONTENT_HEIGHT: f32 = 1_248.0;
const STACKED_CONTENT_HEIGHT: f32 = 1_560.0;

/// Responsive frame selected from the owner-approved Settings breakpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsLayoutMode {
    ThreeColumn,
    NavigationAndContent,
    CompactNavigation,
    SingleColumn,
}

impl SettingsLayoutMode {
    pub const fn for_width(width: f32) -> Self {
        if width >= WIDE_BREAKPOINT {
            Self::ThreeColumn
        } else if width >= NAVIGATION_BREAKPOINT {
            Self::NavigationAndContent
        } else if width >= SINGLE_COLUMN_BREAKPOINT {
            Self::CompactNavigation
        } else {
            Self::SingleColumn
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingsLayoutSpec {
    pub mode: SettingsLayoutMode,
    pub navigation_width: f32,
    pub content_width: f32,
    pub inspector_is_drawer: bool,
    pub rows_are_stacked: bool,
    pub focus_workspace: bool,
}

impl SettingsLayoutSpec {
    pub fn resolve(width: f32, professional_mode: bool) -> Self {
        Self::resolve_internal(width, professional_mode, false)
    }

    fn resolve_focus_workspace(width: f32, professional_mode: bool) -> Self {
        Self::resolve_internal(width, professional_mode, true)
    }

    fn resolve_internal(width: f32, professional_mode: bool, focus_workspace: bool) -> Self {
        let width = if width.is_finite() {
            width.max(0.0)
        } else {
            0.0
        };
        let mode = SettingsLayoutMode::for_width(width);
        let inner = (width - LAYOUT_INSET * 2.0).max(0.0);
        let navigation_width = match mode {
            SettingsLayoutMode::ThreeColumn | SettingsLayoutMode::NavigationAndContent
                if focus_workspace =>
            {
                COMPACT_NAVIGATION_WIDTH.min(inner)
            }
            SettingsLayoutMode::ThreeColumn | SettingsLayoutMode::NavigationAndContent => {
                NAVIGATION_WIDTH.min(inner)
            }
            SettingsLayoutMode::CompactNavigation => COMPACT_NAVIGATION_WIDTH.min(inner),
            SettingsLayoutMode::SingleColumn => 0.0,
        };
        let fixed_inspector_leaves_graph =
            inner - navigation_width - LAYOUT_GAP - INSPECTOR_WIDTH - LAYOUT_GAP >= 640.0;
        let inspector_is_drawer = mode != SettingsLayoutMode::ThreeColumn
            || (focus_workspace && !fixed_inspector_leaves_graph);
        let persistent_inspector = if professional_mode && !inspector_is_drawer {
            INSPECTOR_WIDTH.min(inner)
        } else {
            0.0
        };
        let gaps = if navigation_width > 0.0 {
            LAYOUT_GAP
        } else {
            0.0
        } + if persistent_inspector > 0.0 {
            LAYOUT_GAP
        } else {
            0.0
        };
        let available_content = inner - navigation_width - persistent_inspector - gaps;
        let content_width = if focus_workspace {
            available_content.max(0.0)
        } else {
            available_content.clamp(0.0, CONTENT_IDEAL_MAX_WIDTH)
        };
        Self {
            mode,
            navigation_width,
            content_width,
            inspector_is_drawer,
            rows_are_stacked: content_width < STACKED_ROW_BREAKPOINT,
            focus_workspace,
        }
    }
}

/// Centered default size requested by either Settings host.
pub fn recommended_window_size(output: Size) -> Size {
    let available = Size::new(
        (output.width - WINDOW_OUTPUT_INSET).max(0.0),
        (output.height - WINDOW_OUTPUT_INSET).max(0.0),
    );
    Size::new(
        DEFAULT_WINDOW_WIDTH
            .min(available.width)
            .max(MINIMUM_WINDOW_WIDTH.min(available.width)),
        DEFAULT_WINDOW_HEIGHT
            .min(available.height)
            .max(MINIMUM_WINDOW_HEIGHT.min(available.height)),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsScope {
    Global,
    Workspace,
    Display,
}

impl SettingsScope {
    fn label(self) -> &'static str {
        match self {
            Self::Global => "全局",
            Self::Workspace => "当前 workspace",
            Self::Display => "当前显示器",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Global => Self::Workspace,
            Self::Workspace => Self::Display,
            Self::Display => Self::Global,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPage {
    Appearance,
    MotionEditor,
    Wallpaper,
    EdgeComponents,
    Windows,
    Input,
    Notifications,
    Gaming,
    Plugins,
    Accessibility,
}

impl SettingsPage {
    const fn identity(self) -> u64 {
        match self {
            Self::Appearance => 1,
            Self::MotionEditor => 10,
            Self::Wallpaper => 2,
            Self::EdgeComponents => 3,
            Self::Windows => 4,
            Self::Input => 5,
            Self::Notifications => 6,
            Self::Gaming => 7,
            Self::Plugins => 8,
            Self::Accessibility => 9,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "外观与配色",
            Self::MotionEditor => "动画工作室",
            Self::Wallpaper => "壁纸与画布",
            Self::EdgeComponents => "边缘组件",
            Self::Windows => "窗口与 workspace",
            Self::Input => "输入设备",
            Self::Notifications => "通知与隐私",
            Self::Gaming => "游戏模式",
            Self::Plugins => "插件",
            Self::Accessibility => "无障碍",
        }
    }
}

/// Stable texture handles for Settings-owned visual assets. Each host loads
/// them into the same store later used by its [`nkdhr_ui::UiRoot`].
#[derive(Debug, Clone)]
pub struct SettingsAssets {
    navigation: [TextureId; 9],
}

impl SettingsAssets {
    pub fn load(textures: &mut TextureStore) -> Result<Self, TextureError> {
        Ok(Self {
            navigation: [
                insert_icon_mask(textures, include_bytes!("../assets/icons/palette.alpha8"))?,
                insert_icon_mask(textures, include_bytes!("../assets/icons/image.alpha8"))?,
                insert_icon_mask(textures, include_bytes!("../assets/icons/panel-top.alpha8"))?,
                insert_icon_mask(
                    textures,
                    include_bytes!("../assets/icons/panels-top-left.alpha8"),
                )?,
                insert_icon_mask(
                    textures,
                    include_bytes!("../assets/icons/mouse-pointer-2.alpha8"),
                )?,
                insert_icon_mask(textures, include_bytes!("../assets/icons/bell.alpha8"))?,
                insert_icon_mask(textures, include_bytes!("../assets/icons/gamepad-2.alpha8"))?,
                insert_icon_mask(textures, include_bytes!("../assets/icons/plug.alpha8"))?,
                insert_icon_mask(
                    textures,
                    include_bytes!("../assets/icons/accessibility.alpha8"),
                )?,
            ],
        })
    }

    fn navigation(&self, page: SettingsPage) -> TextureId {
        self.navigation[page.identity() as usize - 1]
    }
}

fn insert_icon_mask(
    textures: &mut TextureStore,
    mask: &'static [u8; 96 * 96],
) -> Result<TextureId, TextureError> {
    textures.insert_mask(96, 96, Arc::<[u8]>::from(mask.as_slice()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    TokyoNight,
    Nord,
    Wallpaper,
    Custom,
}

impl ColorScheme {
    fn label(self) -> &'static str {
        match self {
            Self::TokyoNight => "Tokyo Night",
            Self::Nord => "Nord",
            Self::Wallpaper => "壁纸生成",
            Self::Custom => "自定义",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionPreference {
    Standard,
    Reduced,
    Expressive,
    Off,
}

impl MotionPreference {
    fn label(self) -> &'static str {
        match self {
            Self::Standard => "标准",
            Self::Reduced => "减少动画",
            Self::Expressive => "表现力增强",
            Self::Off => "关闭",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Standard => Self::Reduced,
            Self::Reduced => Self::Expressive,
            Self::Expressive => Self::Off,
            Self::Off => Self::Standard,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentDensity {
    Compact,
    Standard,
    Relaxed,
}

/// Stable identity shared by local controls and a future configuration host.
/// UI-4 will map these presentation identities onto atomic config mutations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppearanceSetting {
    Scheme,
    WallpaperAdaptive,
    BackgroundBlur,
    ContentOpacity,
    OpacityOverride,
    Motion,
    MotionSpeed,
    FontFamily,
    Density,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFeedbackKind {
    Informational,
    Pending,
    Success,
    Error,
}

/// Generation-ordered handle for one downstream apply request. Completing an
/// older token after a newer request is deliberately ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsApplyToken {
    generation: u64,
    setting: AppearanceSetting,
}

impl ComponentDensity {
    fn label(self) -> &'static str {
        match self {
            Self::Compact => "紧凑",
            Self::Standard => "标准",
            Self::Relaxed => "宽松",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceSnapshot {
    pub professional_mode: bool,
    pub search: String,
    pub scope: SettingsScope,
    pub page: SettingsPage,
    pub scheme: ColorScheme,
    pub wallpaper_adaptive: bool,
    pub background_blur: bool,
    pub content_opacity: f32,
    pub motion: MotionPreference,
    pub motion_speed: f32,
    pub font_family: String,
    pub density: ComponentDensity,
    pub has_local_opacity_override: bool,
    pub feedback: SettingsFeedbackKind,
    pub feedback_setting: Option<AppearanceSetting>,
    pub pending_settings: Vec<AppearanceSetting>,
    pub status: String,
}

#[derive(Debug, Clone)]
enum UndoAction {
    Scheme {
        scheme: ColorScheme,
        profile: ThemeProfile,
    },
    WallpaperAdaptive(bool),
    BackgroundBlur(bool),
    ContentOpacity(f32),
    Motion(MotionPreference),
    MotionSpeed(f32),
    FontFamily(String),
    Density(ComponentDensity),
    OpacityOverride(bool),
}

struct AppearanceState {
    theme_profiles: ThemeProfileEditor,
    professional_mode: Reactive<bool>,
    search: Reactive<String>,
    scope: Reactive<SettingsScope>,
    page: Reactive<SettingsPage>,
    scheme: Reactive<ColorScheme>,
    wallpaper_adaptive: Reactive<bool>,
    background_blur: Reactive<bool>,
    content_opacity: Reactive<f32>,
    motion: Reactive<MotionPreference>,
    motion_speed: Reactive<f32>,
    font_family: Reactive<String>,
    font_status: Reactive<TextInputStatus>,
    density: Reactive<ComponentDensity>,
    opacity_override: Reactive<bool>,
    status: Reactive<String>,
    feedback: Reactive<SettingsFeedbackKind>,
    feedback_setting: Reactive<Option<AppearanceSetting>>,
    content_scroll: Reactive<ScrollOffset>,
    navigation_scroll: Reactive<ScrollOffset>,
    navigation_selection: Reactive<Option<u64>>,
    mobile_navigation_open: Reactive<bool>,
    composition_revision: Reactive<u64>,
    motion_editor: crate::motion_editor_view::MotionEditorSession,
    next_apply_generation: Cell<u64>,
    pending_apply: RefCell<BTreeMap<AppearanceSetting, SettingsApplyToken>>,
    undo: RefCell<Option<UndoAction>>,
    opacity_tracker: RefCell<f32>,
    speed_tracker: RefCell<f32>,
    font_tracker: RefCell<String>,
}

/// Long-lived Settings presentation state shared by both future hosts.
#[derive(Clone)]
pub struct AppearanceSettings {
    state: Rc<AppearanceState>,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl AppearanceSettings {
    pub fn new() -> Self {
        Self::with_theme_profiles(ThemeProfileEditor::default())
    }

    pub fn with_theme_profiles(theme_profiles: ThemeProfileEditor) -> Self {
        let scheme = scheme_for_profile(&theme_profiles.snapshot().committed_profile);
        let composition_revision = Reactive::new(1);
        let motion_editor =
            crate::motion_editor_view::MotionEditorSession::new(composition_revision.clone());
        Self {
            state: Rc::new(AppearanceState {
                theme_profiles,
                professional_mode: Reactive::new(false),
                search: Reactive::new(String::new()),
                scope: Reactive::new(SettingsScope::Global),
                page: Reactive::new(SettingsPage::Appearance),
                scheme: Reactive::new(scheme),
                wallpaper_adaptive: Reactive::new(true),
                background_blur: Reactive::new(true),
                content_opacity: Reactive::new(86.0),
                motion: Reactive::new(MotionPreference::Standard),
                motion_speed: Reactive::new(100.0),
                font_family: Reactive::new("Maple Mono NF CN".to_owned()),
                font_status: Reactive::new(TextInputStatus::Idle),
                density: Reactive::new(ComponentDensity::Standard),
                opacity_override: Reactive::new(false),
                status: Reactive::new("所有修改都会实时预览".to_owned()),
                feedback: Reactive::new(SettingsFeedbackKind::Success),
                feedback_setting: Reactive::new(None),
                content_scroll: Reactive::new(ScrollOffset::ZERO),
                navigation_scroll: Reactive::new(ScrollOffset::ZERO),
                navigation_selection: Reactive::new(Some(SettingsPage::Appearance.identity())),
                mobile_navigation_open: Reactive::new(false),
                composition_revision,
                motion_editor,
                next_apply_generation: Cell::new(1),
                pending_apply: RefCell::new(BTreeMap::new()),
                undo: RefCell::new(None),
                opacity_tracker: RefCell::new(86.0),
                speed_tracker: RefCell::new(100.0),
                font_tracker: RefCell::new("Maple Mono NF CN".to_owned()),
            }),
        }
    }

    /// Runtime a host attaches to its [`nkdhr_ui::UiRoot`]. Profile previews
    /// are published here immediately; persistence remains an async host job.
    pub fn theme_runtime(&self) -> ThemeRuntime {
        self.state.theme_profiles.runtime()
    }

    pub fn theme_profiles(&self) -> ThemeProfileEditor {
        self.state.theme_profiles.clone()
    }

    pub fn preview_theme_profile(&self, profile: ThemeProfile) -> Result<(), ThemeEditorError> {
        self.state.theme_profiles.preview(profile.clone())?;
        self.state.scheme.set(scheme_for_profile(&profile));
        self.sync_theme_editor_feedback();
        Ok(())
    }

    pub fn preview_theme_profile_json(&self, text: &str) -> Result<(), ThemeEditorError> {
        let profile = ThemeProfile::from_json(text)?;
        self.preview_theme_profile(profile)
    }

    pub fn cancel_theme_preview(&self) -> Result<bool, ThemeEditorError> {
        let cancelled = self.state.theme_profiles.cancel_preview()?;
        if cancelled {
            let snapshot = self.state.theme_profiles.snapshot();
            self.state
                .scheme
                .set(scheme_for_profile(&snapshot.committed_profile));
            self.sync_theme_editor_feedback();
        }
        Ok(cancelled)
    }

    pub fn begin_theme_profile_commit(&self) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let request = self.state.theme_profiles.begin_profile_commit()?;
        self.sync_theme_editor_feedback();
        Ok(request)
    }

    pub fn begin_save_current_theme_profile(
        &self,
    ) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let request = self.state.theme_profiles.begin_save_current()?;
        self.sync_theme_editor_feedback();
        Ok(request)
    }

    pub fn begin_copy_saved_theme_profile(
        &self,
        source_id: &str,
        new_id: impl Into<String>,
        new_name: impl Into<String>,
    ) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let request = self
            .state
            .theme_profiles
            .begin_copy_saved(source_id, new_id, new_name)?;
        self.sync_theme_editor_feedback();
        Ok(request)
    }

    pub fn begin_import_theme_profile(
        &self,
        text: &str,
    ) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let request = self.state.theme_profiles.begin_import_profile(text)?;
        self.sync_theme_editor_feedback();
        Ok(request)
    }

    pub fn begin_import_theme_library(
        &self,
        text: &str,
    ) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let request = self.state.theme_profiles.begin_import_library(text)?;
        self.sync_theme_editor_feedback();
        Ok(request)
    }

    pub fn export_current_theme_profile(&self) -> Result<String, ThemeEditorError> {
        self.state.theme_profiles.export_current_profile()
    }

    pub fn export_saved_theme_profile(&self, id: &str) -> Result<String, ThemeEditorError> {
        self.state.theme_profiles.export_saved_profile(id)
    }

    pub fn export_theme_library(&self) -> Result<String, ThemeEditorError> {
        self.state.theme_profiles.export_library()
    }

    pub fn complete_theme_persistence(
        &self,
        token: ThemePersistenceToken,
        result: Result<String, String>,
    ) -> bool {
        let completed = self
            .state
            .theme_profiles
            .complete_persistence(token, result);
        if completed {
            self.sync_theme_editor_feedback();
        }
        completed
    }

    pub fn begin_wallpaper_palette_regeneration(
        &self,
        wallpaper_id: impl Into<String>,
    ) -> Result<WallpaperRegenerationToken, ThemeEditorError> {
        let token = self
            .state
            .theme_profiles
            .begin_wallpaper_regeneration(wallpaper_id)?;
        self.sync_theme_editor_feedback();
        Ok(token)
    }

    pub fn complete_wallpaper_palette_regeneration(
        &self,
        token: WallpaperRegenerationToken,
        result: Result<PaletteData, String>,
    ) -> Result<WallpaperRegenerationOutcome, ThemeEditorError> {
        let result = self
            .state
            .theme_profiles
            .complete_wallpaper_regeneration(token, result);
        let snapshot = self.state.theme_profiles.snapshot();
        let visible = snapshot
            .preview_profile
            .as_ref()
            .unwrap_or(&snapshot.committed_profile);
        self.state.scheme.set(scheme_for_profile(visible));
        self.sync_theme_editor_feedback();
        result
    }

    pub fn accept_external_theme_profile_json(&self, text: &str) -> Result<(), ThemeEditorError> {
        self.state
            .theme_profiles
            .accept_external_profile_json(text)?;
        let snapshot = self.state.theme_profiles.snapshot();
        if snapshot.preview_profile.is_none() {
            self.state
                .scheme
                .set(scheme_for_profile(&snapshot.committed_profile));
        }
        self.sync_theme_editor_feedback();
        Ok(())
    }

    pub fn accept_external_theme_library_json(&self, text: &str) -> Result<(), ThemeEditorError> {
        self.state
            .theme_profiles
            .accept_external_library_json(text)?;
        self.sync_theme_editor_feedback();
        Ok(())
    }

    /// Structural changes increment this value. A host reconciles the view
    /// when it changes; value-only controls remain reactive without rebuilding.
    pub fn composition_revision(&self) -> Reactive<u64> {
        self.state.composition_revision.clone()
    }

    pub fn professional_mode(&self) -> bool {
        self.state.professional_mode.get()
    }

    pub fn set_professional_mode(&self, enabled: bool) {
        let changed = self.state.professional_mode.set_if_changed(enabled);
        let closed_editor = !enabled && self.state.page.get() == SettingsPage::MotionEditor;
        if closed_editor {
            self.state.page.set(SettingsPage::Appearance);
            self.state
                .navigation_selection
                .set(Some(SettingsPage::Appearance.identity()));
        }
        if changed || closed_editor {
            self.request_reconcile();
        }
    }

    /// Enter the UI-7E professional motion workspace without changing any
    /// authored value. The global professional switch remains the authority.
    pub fn open_motion_editor(&self) {
        self.state.professional_mode.set(true);
        self.state.page.set(SettingsPage::MotionEditor);
        self.state
            .navigation_selection
            .set(Some(SettingsPage::Appearance.identity()));
        self.state
            .status
            .set("动画曲线已在本地预览；尚未保存".to_owned());
        self.state.feedback.set(SettingsFeedbackKind::Success);
        self.state.feedback_setting.set(None);
        self.request_reconcile();
    }

    pub fn motion_editor_snapshot(&self) -> nkdhr_ui::MotionCurveEditorSnapshot {
        self.state.motion_editor.snapshot()
    }

    pub fn search_query(&self) -> String {
        self.state.search.get()
    }

    pub fn set_search_query(&self, query: impl Into<String>) {
        if self.state.search.set_if_changed(query.into()) {
            self.request_reconcile();
        }
    }

    pub fn set_motion_preference(&self, preference: MotionPreference) {
        if self.state.motion.set_if_changed(preference) {
            self.request_reconcile();
        }
    }

    pub fn snapshot(&self) -> AppearanceSnapshot {
        let mut pending_settings: Vec<_> =
            self.state.pending_apply.borrow().keys().copied().collect();
        let theme = self.state.theme_profiles.snapshot();
        if (theme
            .pending
            .contains(&ThemePersistenceTarget::ActiveProfile)
            || theme.wallpaper_regeneration_pending)
            && !pending_settings.contains(&AppearanceSetting::Scheme)
        {
            pending_settings.push(AppearanceSetting::Scheme);
            pending_settings.sort_unstable();
        }
        AppearanceSnapshot {
            professional_mode: self.state.professional_mode.get(),
            search: self.state.search.get(),
            scope: self.state.scope.get(),
            page: self.state.page.get(),
            scheme: self.state.scheme.get(),
            wallpaper_adaptive: self.state.wallpaper_adaptive.get(),
            background_blur: self.state.background_blur.get(),
            content_opacity: self.state.content_opacity.get(),
            motion: self.state.motion.get(),
            motion_speed: self.state.motion_speed.get(),
            font_family: self.state.font_family.get(),
            density: self.state.density.get(),
            has_local_opacity_override: self.state.opacity_override.get(),
            feedback: self.state.feedback.get(),
            feedback_setting: self.state.feedback_setting.get(),
            pending_settings,
            status: self.state.status.get(),
        }
    }

    /// Mark one live preview as awaiting confirmation from its downstream
    /// service. This is intentionally host-independent: a Settings host owns
    /// transport, while this model owns generation ordering and visible state.
    pub fn begin_apply(
        &self,
        setting: AppearanceSetting,
        status: impl Into<String>,
    ) -> SettingsApplyToken {
        let generation = self.state.next_apply_generation.get();
        self.state
            .next_apply_generation
            .set(generation.wrapping_add(1).max(1));
        let token = SettingsApplyToken {
            generation,
            setting,
        };
        self.state.pending_apply.borrow_mut().insert(setting, token);
        self.state.feedback.set(SettingsFeedbackKind::Pending);
        self.state.feedback_setting.set(Some(setting));
        self.state.status.set(status.into());
        if setting == AppearanceSetting::FontFamily {
            self.state.font_status.set(TextInputStatus::Pending);
        }
        self.request_reconcile();
        token
    }

    /// Resolve only the latest request. `Ok` and `Err` carry the durable
    /// product-facing message supplied by the downstream service.
    pub fn complete_apply(
        &self,
        token: SettingsApplyToken,
        result: Result<String, String>,
    ) -> bool {
        let is_latest = self
            .state
            .pending_apply
            .borrow()
            .get(&token.setting)
            .is_some_and(|pending| *pending == token);
        if !is_latest {
            return false;
        }
        self.state.pending_apply.borrow_mut().remove(&token.setting);
        match result {
            Ok(status) => {
                self.state.feedback.set(SettingsFeedbackKind::Success);
                self.state.status.set(status);
                if token.setting == AppearanceSetting::FontFamily {
                    self.state.font_status.set(TextInputStatus::Valid);
                }
            }
            Err(status) => {
                self.state.feedback.set(SettingsFeedbackKind::Error);
                self.state.status.set(status.clone());
                if token.setting == AppearanceSetting::FontFamily {
                    self.state
                        .font_status
                        .set(TextInputStatus::BackendError(status));
                }
            }
        }
        self.request_reconcile();
        true
    }

    fn is_pending(&self, setting: AppearanceSetting) -> bool {
        self.state.pending_apply.borrow().contains_key(&setting)
    }

    pub fn element(
        &self,
        viewport: Size,
        theme: Arc<Theme>,
        assets: &SettingsAssets,
        capabilities: MaterialCapabilities,
    ) -> Result<Element, SettingsViewError> {
        if !viewport.is_valid() {
            return Err(SettingsViewError::InvalidViewport);
        }
        let motion_editor = self.state.page.get() == SettingsPage::MotionEditor;
        let mut resolved_theme = (*theme).clone();
        resolved_theme.content_surface.opacity =
            (self.state.content_opacity.get() / 100.0).clamp(0.0, 1.0);
        resolved_theme.density = match self.state.density.get() {
            ComponentDensity::Compact => nkdhr_ui::Density::Compact,
            ComponentDensity::Standard => nkdhr_ui::Density::Standard,
            ComponentDensity::Relaxed => nkdhr_ui::Density::Relaxed,
        };
        resolved_theme.motion.mode = match self.state.motion.get() {
            MotionPreference::Standard => nkdhr_ui::MotionMode::Standard,
            MotionPreference::Reduced => nkdhr_ui::MotionMode::Reduced,
            MotionPreference::Expressive => nkdhr_ui::MotionMode::Expressive,
            MotionPreference::Off => nkdhr_ui::MotionMode::Off,
        };
        resolved_theme.motion = resolved_theme
            .motion
            .with_speed_multiplier(self.state.motion_speed.get() / 100.0)?;
        let font_family = self.state.font_family.get();
        if !font_family.trim().is_empty() {
            resolved_theme
                .typography
                .families
                .ui
                .retain(|family| family != &font_family);
            resolved_theme.typography.families.ui.insert(0, font_family);
        }
        let theme = Arc::new(resolved_theme);
        let surface_capabilities = MaterialCapabilities {
            backdrop_blur: capabilities.backdrop_blur && self.state.background_blur.get(),
            ..capabilities
        };
        let professional_mode = self.state.professional_mode.get();
        let spec = if motion_editor {
            SettingsLayoutSpec::resolve_focus_workspace(viewport.width, professional_mode)
        } else {
            SettingsLayoutSpec::resolve(viewport.width, professional_mode)
        };
        let nested = nested_capabilities(capabilities);
        let header = self.header(Arc::clone(&theme), nested, spec.mode);
        let navigation = self.navigation(Arc::clone(&theme), nested, spec, assets)?;
        let navigation = if motion_editor {
            crate::motion_editor_view::navigation_shell(Arc::clone(&theme), nested, navigation)
        } else {
            navigation
        };
        let content = self.content(Arc::clone(&theme), nested, spec)?;
        let inspector = if motion_editor {
            crate::motion_editor_view::inspector(
                Arc::clone(&theme),
                nested,
                spec.inspector_is_drawer,
                self.state.motion_editor.clone(),
            )?
        } else {
            self.inspector(Arc::clone(&theme), nested, spec.inspector_is_drawer)
        };
        let body = Element::new(Padding {
            insets: Insets::all(LAYOUT_INSET),
        })
        .child(
            Element::new(SettingsBodyLayout {
                spec,
                professional_mode,
                mobile_navigation_open: self.state.mobile_navigation_open.get(),
                theme: Arc::clone(&theme),
            })
            .child(navigation)
            .child(content)
            .child(inspector),
        );
        let status = self.status_bar(Arc::clone(&theme), nested);
        let shell = Element::new(SettingsShellLayout {
            divider: alpha(theme.palette.edge, if motion_editor { 0.045 } else { 0.12 }),
        })
        .child(header)
        .child(body)
        .child(status);

        Ok(Element::new(
            GlassSurface::new(theme, MaterialTier::ContentSurface)
                .capabilities(surface_capabilities)
                .radius(28.0),
        )
        .child(shell))
    }

    fn header(
        &self,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
        mode: SettingsLayoutMode,
    ) -> Element {
        let brand = if mode == SettingsLayoutMode::SingleColumn {
            Element::new(
                GlassSurface::new(Arc::clone(&theme), MaterialTier::CompactNode)
                    .capabilities(capabilities)
                    .radius(theme.radii.control)
                    .padding(Insets::symmetric(12.0, 6.0)),
            )
            .child(text(
                "N",
                TextRole::Label,
                theme.palette.text_primary,
                &theme,
            ))
        } else {
            Element::new(Flex {
                axis: Axis::Horizontal,
                gap: 10.0,
                main_alignment: MainAxisAlignment::Start,
                cross_alignment: CrossAxisAlignment::Center,
            })
            .child(
                Element::new(
                    GlassSurface::new(Arc::clone(&theme), MaterialTier::CompactNode)
                        .capabilities(capabilities)
                        .radius(theme.radii.control)
                        .padding(Insets::symmetric(12.0, 6.0)),
                )
                .child(text(
                    "N",
                    TextRole::Label,
                    theme.palette.text_primary,
                    &theme,
                )),
            )
            .child(
                Element::new(Flex {
                    axis: Axis::Vertical,
                    gap: 0.0,
                    main_alignment: MainAxisAlignment::Center,
                    cross_alignment: CrossAxisAlignment::Start,
                })
                .child(text(
                    "设置",
                    TextRole::Label,
                    theme.palette.text_primary,
                    &theme,
                ))
                .child(text(
                    "nkdhr",
                    TextRole::Caption,
                    theme.palette.text_muted,
                    &theme,
                )),
            )
        };

        let model = self.clone();
        let scope = self.state.scope.get();
        let scope_button = Element::new(
            Button::new(scope.label(), Arc::clone(&theme))
                .variant(ButtonVariant::Quiet)
                .capabilities(capabilities)
                .on_activate(move || {
                    let next = model.state.scope.get().next();
                    model.state.scope.set(next);
                    model.set_status(format!("设置作用范围已切换为{}", next.label()));
                    model.request_reconcile();
                }),
        );

        let model = self.clone();
        let mut search = Element::new(
            TextInput::new(
                "搜索设置、命令或组件",
                self.state.search.clone(),
                Arc::clone(&theme),
            )
            .capabilities(capabilities)
            .on_change(move |_| model.request_reconcile()),
        );
        if self.state.search.get().is_empty() {
            search = search.child(text(
                "搜索设置、命令或组件",
                TextRole::Body,
                theme.palette.text_muted,
                &theme,
            ));
        }
        let search = search.flex(1.0);

        let pro = Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 8.0,
            main_alignment: MainAxisAlignment::End,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .child(text(
            "专业模式",
            TextRole::BodySmall,
            theme.palette.text_secondary,
            &theme,
        ))
        .child(Element::new(
            Toggle::new(
                "专业模式",
                self.state.professional_mode.clone(),
                Arc::clone(&theme),
            )
            .capabilities(capabilities)
            .on_change({
                let model = self.clone();
                move |enabled| {
                    model.set_professional_mode(enabled);
                    model.set_status(if enabled {
                        "专业控制已展开，现有配置保持不变"
                    } else {
                        "专业控制已隐藏，现有配置保持不变"
                    });
                }
            }),
        ));

        let mut row = Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 12.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .child(brand)
        .child(scope_button)
        .child(search);
        if mode == SettingsLayoutMode::SingleColumn {
            let model = self.clone();
            row = row.child(Element::new(
                Button::new("页面", Arc::clone(&theme))
                    .variant(ButtonVariant::Quiet)
                    .capabilities(capabilities)
                    .on_activate(move || {
                        let next = !model.state.mobile_navigation_open.get();
                        model.state.mobile_navigation_open.set(next);
                        model.request_reconcile();
                    }),
            ));
        }
        row = row.child(pro);
        Element::new(Padding {
            insets: Insets::symmetric(16.0, 8.0),
        })
        .child(row)
    }

    fn navigation(
        &self,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
        spec: SettingsLayoutSpec,
        assets: &SettingsAssets,
    ) -> Result<Element, SettingsViewError> {
        let compact = spec.navigation_width <= COMPACT_NAVIGATION_WIDTH;
        let groups = [
            (
                "个性化",
                &[
                    SettingsPage::Appearance,
                    SettingsPage::Wallpaper,
                    SettingsPage::EdgeComponents,
                ][..],
            ),
            (
                "工作方式",
                &[
                    SettingsPage::Windows,
                    SettingsPage::Input,
                    SettingsPage::Notifications,
                    SettingsPage::Gaming,
                ][..],
            ),
            (
                "系统",
                &[SettingsPage::Plugins, SettingsPage::Accessibility][..],
            ),
        ];
        let selection = self.state.navigation_selection.clone();
        let mut entries = Vec::new();
        let mut children = Vec::new();
        for (group_index, (group, pages)) in groups.into_iter().enumerate() {
            if !compact {
                let identity = 100 + group_index as u64;
                entries.push(ListEntry::new(identity, group).enabled(false));
                children.push(
                    Element::new(Padding {
                        insets: Insets::new(12.0, 12.0, 12.0, 4.0),
                    })
                    .child(text(
                        group,
                        TextRole::Caption,
                        theme.palette.text_muted,
                        &theme,
                    ))
                    .keyed(identity),
                );
            }
            for page in pages {
                entries.push(ListEntry::new(page.identity(), page.label()));
                let model = self.clone();
                let target = *page;
                let icon = Element::new(Icon {
                    texture: assets.navigation(*page),
                    size: 18.0,
                    color: theme.palette.text_primary,
                });
                let content = if compact {
                    Element::new(CompactNavigationSlot).child(icon)
                } else {
                    Element::new(Flex {
                        axis: Axis::Horizontal,
                        gap: 8.0,
                        main_alignment: MainAxisAlignment::Center,
                        cross_alignment: CrossAxisAlignment::Center,
                    })
                    .child(icon)
                    .child(text(
                        page.label(),
                        TextRole::Body,
                        theme.palette.text_primary,
                        &theme,
                    ))
                };
                children.push(
                    Element::new(
                        ListItem::new(
                            page.identity(),
                            page.label(),
                            selection.clone(),
                            Arc::clone(&theme),
                        )
                        .on_activate(move || {
                            model.state.page.set(target);
                            model
                                .state
                                .navigation_selection
                                .set(Some(target.identity()));
                            model.state.mobile_navigation_open.set(false);
                            model.set_status(format!("已打开{}", target.label()));
                            model.request_reconcile();
                        }),
                    )
                    .child(content)
                    .keyed(page.identity()),
                );
            }
        }
        let compact_content_height = entries.len() as f32 * theme.density_metrics().row_height;
        let content_size = Size::new(
            if compact {
                COMPACT_NAVIGATION_WIDTH
            } else {
                NAVIGATION_WIDTH
            },
            if compact {
                compact_content_height
            } else {
                620.0
            },
        );
        let list = List::from_entries("设置分类", selection, entries, Arc::clone(&theme))?
            .material_tier(MaterialTier::Ghost)
            .panel_surface(self.state.page.get() != SettingsPage::MotionEditor)
            .square_selection_node(compact)
            .capabilities(capabilities);
        Ok(Element::new(
            Scroll::new(
                "设置分类",
                content_size,
                self.state.navigation_scroll.clone(),
                Arc::clone(&theme),
            )?
            .horizontal(false)
            .vertical(true),
        )
        .child(Element::new(list).children(children)))
    }

    fn content(
        &self,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
        spec: SettingsLayoutSpec,
    ) -> Result<Element, SettingsViewError> {
        let page = self.state.page.get();
        let body = if page == SettingsPage::Appearance {
            self.appearance_page(Arc::clone(&theme), capabilities, spec.rows_are_stacked)?
        } else if page == SettingsPage::MotionEditor {
            return crate::motion_editor_view::workspace(
                theme,
                capabilities,
                self.state.motion_editor.clone(),
            )
            .map_err(Into::into);
        } else {
            self.placeholder_page(page, Arc::clone(&theme))
        };
        let content_height = if spec.rows_are_stacked {
            STACKED_CONTENT_HEIGHT
        } else {
            CONTENT_HEIGHT
        };
        Ok(Element::new(
            Scroll::new(
                "设置内容",
                Size::new(spec.content_width.max(1.0), content_height),
                self.state.content_scroll.clone(),
                theme,
            )?
            .horizontal(false)
            .vertical(true),
        )
        .child(body))
    }

    fn appearance_page(
        &self,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
        stacked: bool,
    ) -> Result<Element, SettingsViewError> {
        let query = self.state.search.get().trim().to_lowercase();
        let scheme = self.state.scheme.get();
        let mut sections = Vec::new();
        if matches_query(&query, "主题 配色 壁纸 预设 tokyo nord custom") {
            sections.push(self.color_section(Arc::clone(&theme), capabilities, stacked));
        }
        if matches_query(&query, "材质 模糊 透明度 blur opacity fluid") {
            sections.push(self.material_section(Arc::clone(&theme), capabilities, stacked)?);
        }
        if matches_query(&query, "动画 motion curve speed reduced 弹性") {
            sections.push(self.motion_section(Arc::clone(&theme), capabilities, stacked)?);
        }
        if matches_query(&query, "字体 font density 密度 compact relaxed maple noto") {
            sections.push(self.typography_section(Arc::clone(&theme), capabilities, stacked));
        }

        let heading = Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 16.0,
            main_alignment: MainAxisAlignment::SpaceBetween,
            cross_alignment: CrossAxisAlignment::Start,
        })
        .child(
            Element::new(Flex {
                axis: Axis::Vertical,
                gap: 4.0,
                main_alignment: MainAxisAlignment::Start,
                cross_alignment: CrossAxisAlignment::Start,
            })
            .child(text(
                "外观与交互",
                TextRole::Page,
                theme.palette.text_primary,
                &theme,
            ))
            .child(text(
                "统一调整配色、材质、密度和动画反馈。",
                TextRole::Body,
                theme.palette.text_secondary,
                &theme,
            ))
            .flex(1.0),
        )
        .child(text(
            format!("来源：{}", scheme.label()),
            TextRole::Caption,
            theme.palette.text_muted,
            &theme,
        ));

        let mut column = Element::new(Flex {
            axis: Axis::Vertical,
            gap: 24.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Stretch,
        })
        .child(heading);
        if sections.is_empty() {
            column = column.child(
                Element::new(Align {
                    horizontal: Alignment::Center,
                    vertical: Alignment::Start,
                })
                .child(text(
                    "没有匹配的设置",
                    TextRole::Body,
                    theme.palette.text_muted,
                    &theme,
                )),
            );
        } else {
            column = column.children(sections);
        }
        Ok(Element::new(Padding {
            insets: Insets::new(4.0, 4.0, 8.0, 32.0),
        })
        .child(column))
    }

    fn color_section(
        &self,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
        stacked: bool,
    ) -> Element {
        let selected = self.state.scheme.get();
        let theme_grid = Element::new(Flex {
            axis: Axis::Vertical,
            gap: 8.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Stretch,
        })
        .children([
            self.choice_row(
                &[ColorScheme::TokyoNight, ColorScheme::Nord],
                selected,
                Arc::clone(&theme),
                capabilities,
            ),
            self.choice_row(
                &[ColorScheme::Wallpaper, ColorScheme::Custom],
                selected,
                Arc::clone(&theme),
                capabilities,
            ),
        ]);
        let rows = vec![
            setting_row(
                "基础方案",
                "更改后立即预览，不覆盖你的局部调整。",
                theme_grid,
                &theme,
                stacked,
            ),
            setting_row(
                "壁纸自适应",
                "在不破坏主题身份的前提下补偿局部可读性。",
                Element::new(
                    Toggle::new(
                        "壁纸自适应",
                        self.state.wallpaper_adaptive.clone(),
                        Arc::clone(&theme),
                    )
                    .pending(self.is_pending(AppearanceSetting::WallpaperAdaptive))
                    .capabilities(capabilities)
                    .on_change({
                        let model = self.clone();
                        move |next| {
                            model.record(
                                UndoAction::WallpaperAdaptive(!next),
                                if next {
                                    "壁纸自适应已开启"
                                } else {
                                    "壁纸自适应已关闭"
                                },
                            );
                        }
                    }),
                ),
                &theme,
                stacked,
            ),
        ];
        section(
            "配色方案",
            "从内置方案开始，或让壁纸生成完整语义色。",
            rows,
            theme,
            capabilities,
        )
    }

    fn material_section(
        &self,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
        stacked: bool,
    ) -> Result<Element, SettingsViewError> {
        let opacity = Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 10.0,
            main_alignment: MainAxisAlignment::End,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .child(Element::new(
            Slider::new(
                "内容表面透明度",
                self.state.content_opacity.clone(),
                60.0,
                98.0,
                Arc::clone(&theme),
            )?
            .step(1.0)?
            .ideal_width(150.0)?
            .pending(self.is_pending(AppearanceSetting::ContentOpacity))
            .capabilities(capabilities)
            .on_change({
                let model = self.clone();
                move |next| {
                    let previous =
                        std::mem::replace(&mut *model.state.opacity_tracker.borrow_mut(), next);
                    model.record(
                        UndoAction::ContentOpacity(previous),
                        format!("内容表面透明度已预览为{next:.0}%"),
                    );
                }
            }),
        ))
        .child(text(
            format!("{:.0}%", self.state.content_opacity.get()),
            TextRole::Label,
            theme.palette.text_primary,
            &theme,
        ));
        let override_active = self.state.opacity_override.get();
        let model = self.clone();
        let rows = vec![
            setting_row(
                "背景模糊",
                "性能模式关闭时会自动提高实色覆盖。",
                Element::new(
                    Toggle::new(
                        "背景模糊",
                        self.state.background_blur.clone(),
                        Arc::clone(&theme),
                    )
                    .pending(self.is_pending(AppearanceSetting::BackgroundBlur))
                    .capabilities(capabilities)
                    .on_change({
                        let model = self.clone();
                        move |next| {
                            model.record(
                                UndoAction::BackgroundBlur(!next),
                                if next {
                                    "背景模糊已开启"
                                } else {
                                    "背景模糊已关闭"
                                },
                            );
                        }
                    }),
                ),
                &theme,
                stacked,
            ),
            setting_row(
                "内容表面透明度",
                "当前作用于设置窗口和系统应用。",
                opacity,
                &theme,
                stacked,
            ),
            setting_row(
                "继承状态",
                if override_active {
                    "当前层已有覆盖，可以恢复为主题继承值。"
                } else {
                    "这个值来自当前主题，可以在此层创建覆盖。"
                },
                Element::new(
                    Button::new(
                        if override_active {
                            "恢复继承"
                        } else {
                            "创建覆盖"
                        },
                        Arc::clone(&theme),
                    )
                    .pending(self.is_pending(AppearanceSetting::OpacityOverride))
                    .capabilities(capabilities)
                    .on_activate(move || {
                        let previous = model.state.opacity_override.get();
                        model.state.opacity_override.set(!previous);
                        model.record(
                            UndoAction::OpacityOverride(previous),
                            if previous {
                                "已恢复主题继承"
                            } else {
                                "已创建当前层覆盖"
                            },
                        );
                        model.request_reconcile();
                    }),
                ),
                &theme,
                stacked,
            ),
        ];
        Ok(section(
            "材质",
            "紧凑节点保持轻盈，内容表面优先保证阅读。",
            rows,
            theme,
            capabilities,
        ))
    }

    fn motion_section(
        &self,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
        stacked: bool,
    ) -> Result<Element, SettingsViewError> {
        let motion = self.state.motion.get();
        let model = self.clone();
        let speed = Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 10.0,
            main_alignment: MainAxisAlignment::End,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .child(Element::new(
            Slider::new(
                "全局动画速度",
                self.state.motion_speed.clone(),
                50.0,
                150.0,
                Arc::clone(&theme),
            )?
            .step(1.0)?
            .ideal_width(150.0)?
            .pending(self.is_pending(AppearanceSetting::MotionSpeed))
            .capabilities(capabilities)
            .on_change({
                let model = self.clone();
                move |next| {
                    let previous =
                        std::mem::replace(&mut *model.state.speed_tracker.borrow_mut(), next);
                    model.record(
                        UndoAction::MotionSpeed(previous),
                        format!("全局动画速度已预览为{next:.0}%"),
                    );
                }
            }),
        ))
        .child(text(
            format!("{:.0}%", self.state.motion_speed.get()),
            TextRole::Label,
            theme.palette.text_primary,
            &theme,
        ));
        let rows = vec![
            setting_row(
                "动画模式",
                "无障碍减少动画始终拥有最高优先级。",
                Element::new(
                    Button::new(motion.label(), Arc::clone(&theme))
                        .pending(self.is_pending(AppearanceSetting::Motion))
                        .capabilities(capabilities)
                        .on_activate(move || {
                            let previous = model.state.motion.get();
                            let next = previous.next();
                            model.state.motion.set(next);
                            model.record(
                                UndoAction::Motion(previous),
                                format!("动画模式已切换为{}", next.label()),
                            );
                            model.request_reconcile();
                        }),
                ),
                &theme,
                stacked,
            ),
            setting_row(
                "全局速度",
                "组件仍可在专业模式中单独覆盖。",
                speed,
                &theme,
                stacked,
            ),
            setting_row(
                "专业曲线",
                "编辑时间、进度、越界与流体包络。",
                Element::new(
                    Button::new("打开编辑器", Arc::clone(&theme))
                        .variant(ButtonVariant::Primary)
                        .capabilities(capabilities)
                        .enabled(self.state.professional_mode.get())
                        .on_activate({
                            let model = self.clone();
                            move || model.open_motion_editor()
                        }),
                ),
                &theme,
                stacked,
            ),
        ];
        Ok(section(
            "动画",
            "标准控件克制响应，空间节点保留流体表现力。",
            rows,
            theme,
            capabilities,
        ))
    }

    fn typography_section(
        &self,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
        stacked: bool,
    ) -> Element {
        let font = Element::new(
            TextInput::new(
                "系统 UI 字体",
                self.state.font_family.clone(),
                Arc::clone(&theme),
            )
            .status(self.state.font_status.clone())
            .capabilities(capabilities)
            .on_change({
                let model = self.clone();
                move |next| {
                    let previous = std::mem::replace(
                        &mut *model.state.font_tracker.borrow_mut(),
                        next.to_owned(),
                    );
                    model.record(
                        UndoAction::FontFamily(previous),
                        format!("正在预览字体“{next}”"),
                    );
                }
            }),
        );
        let selected = self.state.density.get();
        let density = Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 6.0,
            main_alignment: MainAxisAlignment::End,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .children(
            [
                ComponentDensity::Compact,
                ComponentDensity::Standard,
                ComponentDensity::Relaxed,
            ]
            .into_iter()
            .map(|density| {
                let model = self.clone();
                Element::new(
                    Button::new(density.label(), Arc::clone(&theme))
                        .variant(if density == selected {
                            ButtonVariant::Selected
                        } else {
                            ButtonVariant::Quiet
                        })
                        .pending(self.is_pending(AppearanceSetting::Density))
                        .capabilities(capabilities)
                        .on_activate(move || {
                            let previous = model.state.density.get();
                            model.state.density.set(density);
                            model.record(
                                UndoAction::Density(previous),
                                format!("组件密度已预览为{}", density.label()),
                            );
                            model.request_reconcile();
                        }),
                )
            }),
        );
        let model = self.clone();
        let rows = vec![
            setting_row(
                "系统 UI 字体",
                "Noto Sans CJK 会在缺失字形时自动后备。",
                font,
                &theme,
                stacked,
            ),
            setting_row(
                "组件密度",
                "不会缩小触摸目标或裁切放大文字。",
                density,
                &theme,
                stacked,
            ),
            setting_row_colored(
                "无效字体覆盖",
                "找不到字族“Example Sans”，继续使用有效配置。",
                Element::new(
                    Button::new("修正", Arc::clone(&theme))
                        .capabilities(capabilities)
                        .on_activate(move || model.set_status("已移除无效字体覆盖")),
                ),
                &theme,
                theme.palette.error,
                stacked,
            ),
        ];
        section(
            "排版与密度",
            "字体缩放与组件密度彼此独立。",
            rows,
            theme,
            capabilities,
        )
    }

    fn choice_row(
        &self,
        choices: &[ColorScheme],
        selected: ColorScheme,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
    ) -> Element {
        Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 8.0,
            main_alignment: MainAxisAlignment::End,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .children(choices.iter().copied().map(|choice| {
            let model = self.clone();
            Element::new(
                Button::new(choice.label(), Arc::clone(&theme))
                    .variant(if choice == selected {
                        ButtonVariant::Selected
                    } else {
                        ButtonVariant::Quiet
                    })
                    .pending(
                        self.is_pending(AppearanceSetting::Scheme)
                            || self
                                .state
                                .theme_profiles
                                .snapshot()
                                .pending
                                .contains(&ThemePersistenceTarget::ActiveProfile)
                            || self
                                .state
                                .theme_profiles
                                .snapshot()
                                .wallpaper_regeneration_pending,
                    )
                    .capabilities(capabilities)
                    .on_activate(move || {
                        let previous = model.state.scheme.get();
                        let editor = model.state.theme_profiles.snapshot();
                        let previous_profile =
                            editor.preview_profile.unwrap_or(editor.committed_profile);
                        if let Some(profile) = profile_for_scheme(choice) {
                            if let Err(error) = model.preview_theme_profile(profile) {
                                model.state.feedback.set(SettingsFeedbackKind::Error);
                                model
                                    .state
                                    .feedback_setting
                                    .set(Some(AppearanceSetting::Scheme));
                                model.state.status.set(format!("主题预览失败：{error}"));
                                model.request_reconcile();
                                return;
                            }
                        } else {
                            model.state.scheme.set(choice);
                        }
                        model.state.undo.replace(Some(UndoAction::Scheme {
                            scheme: previous,
                            profile: previous_profile,
                        }));
                        model.set_status(format!("配色方案已预览为{}", choice.label()));
                        model.request_reconcile();
                    }),
            )
        }))
    }

    fn placeholder_page(&self, page: SettingsPage, theme: Arc<Theme>) -> Element {
        Element::new(Padding {
            insets: Insets::new(4.0, 4.0, 8.0, 32.0),
        })
        .child(
            Element::new(Flex {
                axis: Axis::Vertical,
                gap: 8.0,
                main_alignment: MainAxisAlignment::Start,
                cross_alignment: CrossAxisAlignment::Start,
            })
            .child(text(
                page.label(),
                TextRole::Page,
                theme.palette.text_primary,
                &theme,
            ))
            .child(text(
                "页面框架已接入；具体设计将在对应阶段与你逐项确认。",
                TextRole::Body,
                theme.palette.text_secondary,
                &theme,
            )),
        )
    }

    fn inspector(
        &self,
        theme: Arc<Theme>,
        capabilities: MaterialCapabilities,
        drawer: bool,
    ) -> Element {
        let opacity = self.state.content_opacity.get();
        let source = self.state.scheme.get().label();
        let scope = self.state.scope.get().label();
        let model = self.clone();
        let details = [
            ("有效值", format!("{opacity:.0}%")),
            ("来源", source.to_owned()),
            ("作用范围", scope.to_owned()),
            ("合法范围", "60–98%".to_owned()),
        ]
        .into_iter()
        .map(|(label, value)| {
            Element::new(Flex {
                axis: Axis::Horizontal,
                gap: 8.0,
                main_alignment: MainAxisAlignment::SpaceBetween,
                cross_alignment: CrossAxisAlignment::Center,
            })
            .child(text(
                label,
                TextRole::BodySmall,
                theme.palette.text_muted,
                &theme,
            ))
            .child(text(
                value,
                TextRole::Label,
                theme.palette.text_primary,
                &theme,
            ))
        });
        let panel = Element::new(
            GlassSurface::new(
                Arc::clone(&theme),
                if drawer {
                    MaterialTier::ContentSurface
                } else {
                    MaterialTier::ExpandedPanel
                },
            )
            .capabilities(capabilities)
            .radius(theme.radii.group)
            .padding(Insets::all(16.0)),
        )
        .child(
            Element::new(Flex {
                axis: Axis::Vertical,
                gap: 16.0,
                main_alignment: MainAxisAlignment::Start,
                cross_alignment: CrossAxisAlignment::Stretch,
            })
            .child(text(
                "内容表面透明度",
                TextRole::Section,
                theme.palette.text_primary,
                &theme,
            ))
            .child(text(
                "最终值由当前主题继承，可在全局层创建覆盖。",
                TextRole::BodySmall,
                theme.palette.text_secondary,
                &theme,
            ))
            .child(
                Element::new(Flex {
                    axis: Axis::Vertical,
                    gap: 10.0,
                    main_alignment: MainAxisAlignment::Start,
                    cross_alignment: CrossAxisAlignment::Stretch,
                })
                .children(details),
            )
            .child(
                Element::new(
                    GlassSurface::new(Arc::clone(&theme), MaterialTier::Ghost)
                        .capabilities(capabilities)
                        .padding(Insets::all(10.0)),
                )
                .child(text(
                    format!("/appearance material content opacity {opacity:.0}"),
                    TextRole::Mono,
                    theme.palette.text_secondary,
                    &theme,
                )),
            )
            .child(Element::new(
                Button::new("重置为继承值", Arc::clone(&theme))
                    .capabilities(capabilities)
                    .on_activate(move || {
                        let previous = model.state.opacity_override.get();
                        model.state.opacity_override.set(false);
                        model.record(UndoAction::OpacityOverride(previous), "已重置为主题继承值");
                        model.request_reconcile();
                    }),
            )),
        );
        if drawer {
            Element::new(InputBarrier).child(panel)
        } else {
            panel
        }
    }

    fn status_bar(&self, theme: Arc<Theme>, capabilities: MaterialCapabilities) -> Element {
        let feedback = self.state.feedback.get();
        let has_undo =
            self.state.undo.borrow().is_some() && self.state.pending_apply.borrow().is_empty();
        let (glyph, glyph_color) = match feedback {
            SettingsFeedbackKind::Informational => ("•", theme.palette.text_muted),
            SettingsFeedbackKind::Pending => ("…", theme.palette.accent_secondary),
            SettingsFeedbackKind::Success => ("✓", theme.palette.success),
            SettingsFeedbackKind::Error => ("!", theme.palette.error),
        };
        let model = self.clone();
        Element::new(Padding {
            insets: Insets::symmetric(16.0, 6.0),
        })
        .child(
            Element::new(Flex {
                axis: Axis::Horizontal,
                gap: 12.0,
                main_alignment: MainAxisAlignment::Start,
                cross_alignment: CrossAxisAlignment::Center,
            })
            .child(text(glyph, TextRole::Label, glyph_color, &theme))
            .child(
                Element::new(Text::bound(
                    self.state.status.clone(),
                    theme.text_style(TextRole::BodySmall),
                    theme.palette.text_secondary,
                ))
                .flex(1.0),
            )
            .child(Element::new(
                Button::new("撤销", theme)
                    .variant(ButtonVariant::Quiet)
                    .capabilities(capabilities)
                    .enabled(has_undo)
                    .on_activate(move || model.undo()),
            )),
        )
    }

    fn record(&self, undo: UndoAction, status: impl Into<String>) {
        self.state.undo.replace(Some(undo));
        if self.state.pending_apply.borrow().is_empty() {
            self.state.status.set(status.into());
            self.state.feedback.set(SettingsFeedbackKind::Success);
            self.state.feedback_setting.set(None);
        }
        self.request_reconcile();
    }

    fn sync_theme_editor_feedback(&self) {
        let snapshot = self.state.theme_profiles.snapshot();
        let feedback = match snapshot.feedback {
            ThemeEditorFeedback::Idle | ThemeEditorFeedback::Previewing => {
                SettingsFeedbackKind::Informational
            }
            ThemeEditorFeedback::Pending => SettingsFeedbackKind::Pending,
            ThemeEditorFeedback::Success => SettingsFeedbackKind::Success,
            ThemeEditorFeedback::Error | ThemeEditorFeedback::Conflict => {
                SettingsFeedbackKind::Error
            }
        };
        let setting = snapshot
            .pending
            .contains(&ThemePersistenceTarget::ActiveProfile)
            .then_some(AppearanceSetting::Scheme)
            .or_else(|| {
                snapshot
                    .wallpaper_regeneration_pending
                    .then_some(AppearanceSetting::Scheme)
            })
            .or_else(|| {
                matches!(
                    snapshot.feedback,
                    ThemeEditorFeedback::Previewing
                        | ThemeEditorFeedback::Error
                        | ThemeEditorFeedback::Conflict
                )
                .then_some(AppearanceSetting::Scheme)
            });
        self.state.feedback.set(feedback);
        self.state.feedback_setting.set(setting);
        self.state.status.set(snapshot.status);
        self.request_reconcile();
    }

    fn set_status(&self, status: impl Into<String>) {
        if self.state.pending_apply.borrow().is_empty() {
            self.state.status.set(status.into());
            self.state.feedback.set(SettingsFeedbackKind::Informational);
            self.state.feedback_setting.set(None);
        }
    }

    fn request_reconcile(&self) {
        self.state
            .composition_revision
            .update(|revision| *revision = revision.wrapping_add(1).max(1));
    }

    fn undo(&self) {
        let Some(action) = self.state.undo.take() else {
            return;
        };
        match action {
            UndoAction::Scheme { scheme, profile } => {
                self.state.scheme.set(scheme);
                if let Err(error) = self.state.theme_profiles.preview(profile) {
                    self.state.feedback.set(SettingsFeedbackKind::Error);
                    self.state
                        .feedback_setting
                        .set(Some(AppearanceSetting::Scheme));
                    self.state.status.set(format!("主题撤销失败：{error}"));
                    self.request_reconcile();
                    return;
                }
            }
            UndoAction::WallpaperAdaptive(value) => self.state.wallpaper_adaptive.set(value),
            UndoAction::BackgroundBlur(value) => self.state.background_blur.set(value),
            UndoAction::ContentOpacity(value) => {
                self.state.content_opacity.set(value);
                self.state.opacity_tracker.replace(value);
            }
            UndoAction::Motion(value) => self.state.motion.set(value),
            UndoAction::MotionSpeed(value) => {
                self.state.motion_speed.set(value);
                self.state.speed_tracker.replace(value);
            }
            UndoAction::FontFamily(value) => {
                self.state.font_family.set(value.clone());
                self.state.font_tracker.replace(value);
            }
            UndoAction::Density(value) => self.state.density.set(value),
            UndoAction::OpacityOverride(value) => self.state.opacity_override.set(value),
        }
        self.state.status.set("已撤销上一项修改".to_owned());
        self.state.feedback.set(SettingsFeedbackKind::Success);
        self.state.feedback_setting.set(None);
        self.request_reconcile();
    }
}

fn profile_for_scheme(scheme: ColorScheme) -> Option<ThemeProfile> {
    match scheme {
        ColorScheme::TokyoNight => Some(ThemeProfile::default()),
        ColorScheme::Nord => Some(ThemeProfile {
            id: "nord".into(),
            name: "Nord".into(),
            base: ThemeBase::BuiltIn {
                preset: BuiltInTheme::Nord,
            },
            ..ThemeProfile::default()
        }),
        ColorScheme::Wallpaper | ColorScheme::Custom => None,
    }
}

fn scheme_for_profile(profile: &ThemeProfile) -> ColorScheme {
    let has_overrides = profile
        .overrides
        .as_object()
        .is_none_or(|overrides| !overrides.is_empty());
    match &profile.base {
        ThemeBase::BuiltIn {
            preset: BuiltInTheme::TokyoNight,
        } if profile.id == "tokyo-night" && !has_overrides => ColorScheme::TokyoNight,
        ThemeBase::BuiltIn {
            preset: BuiltInTheme::Nord,
        } if profile.id == "nord" && !has_overrides => ColorScheme::Nord,
        ThemeBase::BuiltIn { .. } => ColorScheme::Custom,
        ThemeBase::Wallpaper { .. } => ColorScheme::Wallpaper,
    }
}

fn section(
    title: &str,
    description: &str,
    rows: Vec<Element>,
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
) -> Element {
    let mut grouped = Element::new(Flex {
        axis: Axis::Vertical,
        gap: 0.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Stretch,
    });
    let row_count = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        grouped = grouped.child(row);
        if index + 1 < row_count {
            grouped = grouped.child(Element::new(Divider {
                color: alpha(theme.palette.edge, 0.10),
                inset: 16.0,
            }));
        }
    }
    Element::new(Flex {
        axis: Axis::Vertical,
        gap: 12.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Stretch,
    })
    .child(
        Element::new(Flex {
            axis: Axis::Vertical,
            gap: 2.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Start,
        })
        .child(text(
            title,
            TextRole::Section,
            theme.palette.text_primary,
            &theme,
        ))
        .child(text(
            description,
            TextRole::BodySmall,
            theme.palette.text_secondary,
            &theme,
        )),
    )
    .child(
        Element::new(
            GlassSurface::new(theme, MaterialTier::Ghost)
                .capabilities(capabilities)
                .radius(18.0),
        )
        .child(grouped),
    )
}

fn setting_row(
    label: &str,
    description: &str,
    control: Element,
    theme: &Theme,
    stacked: bool,
) -> Element {
    setting_row_colored(
        label,
        description,
        control,
        theme,
        theme.palette.text_secondary,
        stacked,
    )
}

fn setting_row_colored(
    label: &str,
    description: &str,
    control: Element,
    theme: &Theme,
    description_color: Color,
    stacked: bool,
) -> Element {
    let copy = Element::new(Flex {
        axis: Axis::Vertical,
        gap: 2.0,
        main_alignment: MainAxisAlignment::Start,
        cross_alignment: CrossAxisAlignment::Start,
    })
    .child(text(
        label,
        TextRole::Label,
        theme.palette.text_primary,
        theme,
    ))
    .child(text(
        description,
        TextRole::BodySmall,
        description_color,
        theme,
    ));
    let row = if stacked {
        Element::new(Flex {
            axis: Axis::Vertical,
            gap: 10.0,
            main_alignment: MainAxisAlignment::Start,
            cross_alignment: CrossAxisAlignment::Stretch,
        })
        .child(copy)
        .child(control)
    } else {
        Element::new(Flex {
            axis: Axis::Horizontal,
            gap: 16.0,
            main_alignment: MainAxisAlignment::SpaceBetween,
            cross_alignment: CrossAxisAlignment::Center,
        })
        .child(copy.flex(1.0))
        .child(control)
    };
    Element::new(Padding {
        insets: Insets::symmetric(16.0, 12.0),
    })
    .child(row)
}

fn text(content: impl Into<String>, role: TextRole, color: Color, theme: &Theme) -> Element {
    Element::new(Text::new(content, theme.text_style(role), color))
}

fn matches_query(query: &str, searchable: &str) -> bool {
    query.is_empty()
        || query
            .split_whitespace()
            .all(|word| searchable.to_lowercase().contains(word))
}

fn nested_capabilities(capabilities: MaterialCapabilities) -> MaterialCapabilities {
    MaterialCapabilities {
        backdrop_blur: false,
        reduced_transparency: capabilities.reduced_transparency,
        high_contrast: capabilities.high_contrast,
    }
}

fn alpha(color: Color, alpha: f32) -> Color {
    let [red, green, blue, _] = color.components();
    Color::new(red, green, blue, alpha).expect("theme colors and static alpha are valid")
}

#[derive(Debug)]
pub enum SettingsViewError {
    InvalidViewport,
    Scroll(nkdhr_ui::ScrollError),
    List(nkdhr_ui::ListError),
    Slider(nkdhr_ui::SliderError),
    Motion(nkdhr_ui::MotionError),
}

impl fmt::Display for SettingsViewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidViewport => formatter.write_str("Settings viewport must be finite"),
            Self::Scroll(error) => error.fmt(formatter),
            Self::List(error) => error.fmt(formatter),
            Self::Slider(error) => error.fmt(formatter),
            Self::Motion(error) => error.fmt(formatter),
        }
    }
}

impl Error for SettingsViewError {}

impl From<nkdhr_ui::ScrollError> for SettingsViewError {
    fn from(value: nkdhr_ui::ScrollError) -> Self {
        Self::Scroll(value)
    }
}

impl From<nkdhr_ui::ListError> for SettingsViewError {
    fn from(value: nkdhr_ui::ListError) -> Self {
        Self::List(value)
    }
}

impl From<nkdhr_ui::SliderError> for SettingsViewError {
    fn from(value: nkdhr_ui::SliderError) -> Self {
        Self::Slider(value)
    }
}

impl From<nkdhr_ui::MotionError> for SettingsViewError {
    fn from(value: nkdhr_ui::MotionError) -> Self {
        Self::Motion(value)
    }
}

#[derive(Debug, Clone, Copy)]
struct SettingsShellLayout {
    divider: Color,
}

impl Widget for SettingsShellLayout {
    fn update(&self, previous: &dyn std::any::Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        ctx.invalidate(if previous.divider == self.divider {
            Invalidation::LAYOUT | Invalidation::SEMANTICS
        } else {
            Invalidation::ALL
        });
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() != 3 {
            return Err(UiError::ChildCountMismatch {
                expected: 3,
                actual: ctx.child_count(),
            });
        }
        let size = constraints.max();
        let body_height = (size.height - HEADER_HEIGHT - STATUS_HEIGHT).max(0.0);
        ctx.measure_child(0, Constraints::tight(Size::new(size.width, HEADER_HEIGHT))?)?;
        ctx.measure_child(1, Constraints::tight(Size::new(size.width, body_height))?)?;
        ctx.measure_child(2, Constraints::tight(Size::new(size.width, STATUS_HEIGHT))?)?;
        Ok(constraints.constrain(size))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        let body_height = (rect.height - HEADER_HEIGHT - STATUS_HEIGHT).max(0.0);
        ctx.arrange_child(0, Rect::new(rect.x, rect.y, rect.width, HEADER_HEIGHT))?;
        ctx.arrange_child(
            1,
            Rect::new(rect.x, rect.y + HEADER_HEIGHT, rect.width, body_height),
        )?;
        ctx.arrange_child(
            2,
            Rect::new(
                rect.x,
                rect.bottom() - STATUS_HEIGHT,
                rect.width,
                STATUS_HEIGHT,
            ),
        )
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        ctx.builder().rect(
            Rect::new(rect.x, rect.y + HEADER_HEIGHT, rect.width, 1.0),
            self.divider,
        )?;
        ctx.builder().rect(
            Rect::new(rect.x, rect.bottom() - STATUS_HEIGHT, rect.width, 1.0),
            self.divider,
        )?;
        ctx.paint_children()
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx<'_>) -> Semantics {
        Semantics {
            role: SemanticRole::Group,
            label: Some("nkdhr 设置".to_owned()),
            ..Semantics::default()
        }
    }
}

#[derive(Debug, Clone)]
struct SettingsBodyLayout {
    spec: SettingsLayoutSpec,
    professional_mode: bool,
    mobile_navigation_open: bool,
    theme: Arc<Theme>,
}

impl SettingsBodyLayout {
    fn child_rects(&self, rect: Rect, inspector_openness: f32) -> [Rect; 3] {
        let inspector_openness = inspector_openness.clamp(0.0, 1.0);
        let nav = match self.spec.mode {
            SettingsLayoutMode::SingleColumn if self.mobile_navigation_open => rect,
            SettingsLayoutMode::SingleColumn => Rect::new(rect.x, rect.y, 0.0, 0.0),
            _ => Rect::new(rect.x, rect.y, self.spec.navigation_width, rect.height),
        };
        let mut content = match self.spec.mode {
            SettingsLayoutMode::SingleColumn if self.mobile_navigation_open => {
                Rect::new(rect.x, rect.y, 0.0, 0.0)
            }
            SettingsLayoutMode::SingleColumn => rect,
            _ => Rect::new(
                rect.x + self.spec.navigation_width + LAYOUT_GAP,
                rect.y,
                self.spec.content_width,
                rect.height,
            ),
        };
        if self.spec.mode == SettingsLayoutMode::ThreeColumn {
            let first_gap = if self.spec.navigation_width > 0.0 {
                LAYOUT_GAP
            } else {
                0.0
            };
            let inspector_occupancy = (INSPECTOR_WIDTH + LAYOUT_GAP) * inspector_openness;
            let available =
                (rect.width - self.spec.navigation_width - first_gap - inspector_occupancy)
                    .max(0.0);
            content.width = if self.spec.focus_workspace {
                available
            } else {
                available.min(CONTENT_IDEAL_MAX_WIDTH)
            };
        }
        let inspector = if self.spec.inspector_is_drawer {
            let width = INSPECTOR_WIDTH.min(rect.width);
            Rect::new(
                rect.right() - width * inspector_openness,
                rect.y,
                width,
                rect.height,
            )
        } else {
            let width = INSPECTOR_WIDTH.min(rect.width);
            let target_x = content.right() + LAYOUT_GAP;
            Rect::new(
                rect.right() + (target_x - rect.right()) * inspector_openness,
                rect.y,
                width,
                rect.height,
            )
        };
        [nav, content, inspector]
    }

    fn target_openness(&self) -> f32 {
        if self.professional_mode { 1.0 } else { 0.0 }
    }

    fn transition_family(&self, opening: bool) -> MotionFamily {
        match (self.spec.inspector_is_drawer, opening) {
            (true, true) => MotionFamily::DrawerEnter,
            (true, false) => MotionFamily::DrawerExit,
            (false, true) => MotionFamily::PanelEnter,
            (false, false) => MotionFamily::PanelExit,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SettingsBodyLayoutState {
    inspector_openness: ScalarMotion,
}

impl Widget for SettingsBodyLayout {
    fn theme_reads(&self) -> ThemeReadSet {
        ThemeReadSet::from_paths([
            "motion.mode",
            "motion.speed_multiplier",
            "motion.settle",
            "motion.exit",
            "motion.durations.panel_enter",
            "motion.durations.panel_exit",
            "motion.durations.drawer_enter",
            "motion.durations.drawer_exit",
        ])
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn create_state(&self) -> Box<dyn Any> {
        Box::new(SettingsBodyLayoutState {
            inspector_openness: ScalarMotion::settled(self.target_openness()),
        })
    }

    fn update(&self, previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        let target = self.target_openness();
        let now = ctx.now();
        let state = ctx
            .state_mut::<SettingsBodyLayoutState>()
            .expect("SettingsBodyLayout owns SettingsBodyLayoutState");
        if !self.theme.motion.spatial_motion_enabled() {
            state.inspector_openness.settle(target);
        } else if previous.professional_mode != self.professional_mode {
            state.inspector_openness.retarget(
                now,
                target,
                self.theme
                    .motion
                    .spec(self.transition_family(self.professional_mode)),
            );
        }
        let active = state.inspector_openness.is_active(now);
        ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
        if active {
            ctx.request_animation_frame();
        }
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() != 3 {
            return Err(UiError::ChildCountMismatch {
                expected: 3,
                actual: ctx.child_count(),
            });
        }
        let size = constraints.max();
        let openness = ctx
            .state_mut::<SettingsBodyLayoutState>()?
            .inspector_openness
            .value(ctx.now());
        let rects = self.child_rects(Rect::new(0.0, 0.0, size.width, size.height), openness);
        for (index, rect) in rects.into_iter().enumerate() {
            ctx.measure_child(
                index,
                Constraints::tight(Size::new(rect.width, rect.height))?,
            )?;
        }
        Ok(constraints.constrain(size))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        let openness = ctx
            .state_mut::<SettingsBodyLayoutState>()?
            .inspector_openness
            .value(ctx.now());
        for (index, child) in self.child_rects(rect, openness).into_iter().enumerate() {
            ctx.arrange_child(index, child)?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let now = ctx.now();
        let (openness, active) = {
            let state = ctx.state_mut::<SettingsBodyLayoutState>()?;
            (
                state.inspector_openness.value(now).clamp(0.0, 1.0),
                state.inspector_openness.is_active(now),
            )
        };
        if active {
            ctx.request_animation_frame();
        }
        match self.spec.mode {
            SettingsLayoutMode::SingleColumn if self.mobile_navigation_open => {
                ctx.paint_child(0)?
            }
            SettingsLayoutMode::SingleColumn => ctx.paint_child(1)?,
            _ => {
                ctx.paint_child(0)?;
                ctx.paint_child(1)?;
            }
        }
        if openness >= 1.0 - f32::EPSILON {
            ctx.paint_child(2)?;
        } else if openness > f32::EPSILON {
            ctx.paint_child_clipped(2, ctx.rect())?;
        }
        Ok(())
    }

    fn animation(&self, ctx: &mut AnimationCtx<'_>) {
        let now = ctx.now();
        let active = ctx
            .state_mut::<SettingsBodyLayoutState>()
            .is_ok_and(|state| state.inspector_openness.is_active(now));
        if active {
            ctx.invalidate(Invalidation::LAYOUT | Invalidation::PAINT);
            ctx.request_animation_frame();
        }
    }
}

/// Decorative alpha-mask icon. Stable geometry and semantic labels remain on
/// the owning Button, so glyph appearance never changes the focus contract.
#[derive(Debug, Clone, Copy)]
struct Icon {
    texture: TextureId,
    size: f32,
    color: Color,
}

impl Widget for Icon {
    fn update(&self, previous: &dyn std::any::Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        ctx.invalidate(if previous.size == self.size {
            Invalidation::PAINT
        } else {
            Invalidation::LAYOUT
        });
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() != 0 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 0,
                actual: ctx.child_count(),
            });
        }
        Ok(constraints.constrain(Size::new(self.size, self.size)))
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        ctx.builder().tinted_texture(
            rect,
            self.texture,
            None,
            self.color,
            1.0,
            Sampling::Linear,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct Divider {
    color: Color,
    inset: f32,
}

/// Prevents pointer-through on an overlaid drawer while leaving its painted
/// descendants as the topmost interactive targets.
#[derive(Debug, Clone, Copy)]
struct InputBarrier;

/// Makes a compact navigation glyph consume the row's available content slot
/// before centering its fixed-size mask. `ListItem` keeps its standard 16 px
/// label inset for keyboard and pointer consistency, so centering the raw icon
/// directly would place it eight pixels left of the 64 px rail center.
#[derive(Debug, Clone, Copy)]
struct CompactNavigationSlot;

impl Widget for CompactNavigationSlot {
    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() != 1 {
            return Err(UiError::ChildCountMismatch {
                expected: 1,
                actual: ctx.child_count(),
            });
        }
        let child = ctx.measure_child(0, Constraints::new(Size::ZERO, constraints.max())?)?;
        Ok(constraints.constrain(Size::new(constraints.max().width, child.height)))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        let child = ctx.child_size(0)?;
        ctx.arrange_child(
            0,
            Rect::new(
                rect.x + (rect.width - child.width).max(0.0) * 0.5,
                rect.y + (rect.height - child.height).max(0.0) * 0.5,
                child.width.min(rect.width),
                child.height.min(rect.height),
            ),
        )
    }
}

impl Widget for InputBarrier {
    fn accepts_pointer(&self) -> bool {
        true
    }
}

impl Widget for Divider {
    fn measure(
        &self,
        _ctx: &mut MeasureCtx<'_>,
        constraints: Constraints,
    ) -> Result<Size, UiError> {
        Ok(constraints.constrain(Size::new(constraints.max().width, 1.0)))
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let rect = ctx.rect();
        ctx.builder().rect(
            Rect::new(
                rect.x + self.inset,
                rect.y,
                (rect.width - self.inset * 2.0).max(0.0),
                rect.height.min(1.0),
            ),
            self.color,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_approved_breakpoints_are_exact() {
        assert_eq!(
            SettingsLayoutMode::for_width(1_120.0),
            SettingsLayoutMode::ThreeColumn
        );
        assert_eq!(
            SettingsLayoutMode::for_width(1_119.0),
            SettingsLayoutMode::NavigationAndContent
        );
        assert_eq!(
            SettingsLayoutMode::for_width(820.0),
            SettingsLayoutMode::NavigationAndContent
        );
        assert_eq!(
            SettingsLayoutMode::for_width(819.0),
            SettingsLayoutMode::CompactNavigation
        );
        assert_eq!(
            SettingsLayoutMode::for_width(680.0),
            SettingsLayoutMode::CompactNavigation
        );
        assert_eq!(
            SettingsLayoutMode::for_width(679.0),
            SettingsLayoutMode::SingleColumn
        );
    }

    #[test]
    fn default_window_size_respects_output_inset_and_minimum() {
        assert_eq!(
            recommended_window_size(Size::new(1_920.0, 1_080.0)),
            Size::new(1_160.0, 760.0)
        );
        assert_eq!(
            recommended_window_size(Size::new(1_000.0, 700.0)),
            Size::new(952.0, 652.0)
        );
        assert_eq!(
            recommended_window_size(Size::new(600.0, 440.0)),
            Size::new(552.0, 392.0)
        );
    }

    #[test]
    fn professional_inspector_changes_only_the_wide_content_allocation() {
        let ordinary = SettingsLayoutSpec::resolve(1_160.0, false);
        let professional = SettingsLayoutSpec::resolve(1_160.0, true);
        assert_eq!(ordinary.mode, SettingsLayoutMode::ThreeColumn);
        assert!(!ordinary.inspector_is_drawer);
        assert_eq!(ordinary.content_width, 720.0);
        assert_eq!(professional.content_width, 592.0);
        assert!(!professional.rows_are_stacked);

        let medium = SettingsLayoutSpec::resolve(1_000.0, true);
        assert!(medium.inspector_is_drawer);
        assert_eq!(medium.content_width, 720.0);
    }

    #[test]
    fn motion_focus_workspace_preserves_the_graph_before_the_inspector() {
        let wide = SettingsLayoutSpec::resolve_focus_workspace(1_160.0, true);
        assert_eq!(wide.navigation_width, 64.0);
        assert_eq!(wide.content_width, 744.0);
        assert!(!wide.inspector_is_drawer);

        let medium = SettingsLayoutSpec::resolve_focus_workspace(1_000.0, true);
        assert_eq!(medium.navigation_width, 64.0);
        assert_eq!(medium.content_width, 888.0);
        assert!(medium.inspector_is_drawer);

        let compact = SettingsLayoutSpec::resolve_focus_workspace(760.0, true);
        assert_eq!(compact.navigation_width, 64.0);
        assert_eq!(compact.content_width, 648.0);
        assert!(compact.inspector_is_drawer);

        let minimum = SettingsLayoutSpec::resolve_focus_workspace(640.0, true);
        assert_eq!(minimum.navigation_width, 0.0);
        assert_eq!(minimum.content_width, 608.0);
        assert!(minimum.inspector_is_drawer);
    }

    #[test]
    fn navigation_assets_are_real_coverage_masks_not_placeholder_blocks() {
        let mut textures = TextureStore::new();
        let assets = SettingsAssets::load(&mut textures).unwrap();
        for texture in assets.navigation {
            let pixels = textures.get(texture).unwrap().pixels();
            assert!(pixels.contains(&0));
            assert!(pixels.iter().any(|coverage| *coverage > 0));
        }
    }

    #[test]
    fn appearance_bridge_previews_and_commits_the_same_atomic_profile() {
        let model = AppearanceSettings::new();
        let nord = profile_for_scheme(ColorScheme::Nord).unwrap();
        model.preview_theme_profile(nord.clone()).unwrap();
        assert_eq!(model.snapshot().scheme, ColorScheme::Nord);
        assert_eq!(
            model.theme_runtime().snapshot().resolved().profile,
            nord.clone()
        );

        let request = model.begin_theme_profile_commit().unwrap();
        assert_eq!(request.key(), crate::ACTIVE_THEME_PROFILE_KEY);
        assert!(
            model
                .snapshot()
                .pending_settings
                .contains(&AppearanceSetting::Scheme)
        );
        assert!(model.complete_theme_persistence(request.token(), Ok("主题配置已保存".into())));
        assert_eq!(model.snapshot().feedback, SettingsFeedbackKind::Success);
        assert_eq!(model.theme_profiles().snapshot().committed_profile, nord);
    }

    #[test]
    fn built_in_based_overrides_keep_their_custom_identity() {
        let custom = ThemeProfile {
            id: "my-tokyo".into(),
            name: "My Tokyo".into(),
            overrides: serde_json::json!({"palette": {"accent": "#010203ff"}}),
            ..ThemeProfile::default()
        };
        assert_eq!(scheme_for_profile(&custom), ColorScheme::Custom);
        let model = AppearanceSettings::new();
        model.preview_theme_profile(custom.clone()).unwrap();
        assert_eq!(model.snapshot().scheme, ColorScheme::Custom);
        assert_eq!(model.theme_runtime().snapshot().resolved().profile, custom);
    }

    #[test]
    fn appearance_bridge_reports_wallpaper_generation_and_persistence_as_one_pending_setting() {
        let profile = ThemeProfile {
            id: "live-wallpaper".into(),
            name: "Live Wallpaper".into(),
            base: ThemeBase::Wallpaper {
                live: true,
                wallpaper_id: "old".into(),
                frozen_palette: Box::new(PaletteData::tokyo_night()),
            },
            ..ThemeProfile::default()
        };
        let editor = ThemeProfileEditor::new(profile, Default::default()).unwrap();
        let model = AppearanceSettings::with_theme_profiles(editor);
        let token = model
            .begin_wallpaper_palette_regeneration("wallpaper:new")
            .unwrap();
        assert_eq!(model.snapshot().feedback, SettingsFeedbackKind::Pending);
        assert!(
            model
                .snapshot()
                .pending_settings
                .contains(&AppearanceSetting::Scheme)
        );

        let outcome = model
            .complete_wallpaper_palette_regeneration(token, Ok(PaletteData::nord()))
            .unwrap();
        let WallpaperRegenerationOutcome::PersistenceRequired(request) = outcome else {
            panic!("clean regeneration produces a persistence request")
        };
        assert_eq!(model.snapshot().scheme, ColorScheme::Wallpaper);
        assert!(model.complete_theme_persistence(request.token(), Ok("壁纸配色已保存".into())));
        assert_eq!(model.snapshot().feedback, SettingsFeedbackKind::Success);
    }
}
