//! Host-independent theme editing transactions for Appearance Settings.
//!
//! Preview publication is synchronous and local. Durable CTRL-5 writes are
//! returned as opaque requests so a Wayland or compositor host can execute
//! D-Bus work away from the UI thread and report the result later.

use std::{cell::RefCell, fmt, rc::Rc};

use nkdhr_theme::{
    PaletteData, ThemeLibraryError, ThemeProfile, ThemeProfileError, ThemeProfileLibrary,
    WallpaperPaletteError, regenerate_live_wallpaper_profile,
};
use nkdhr_ui::{ThemeRuntime, ThemeRuntimeError};

pub const ACTIVE_THEME_PROFILE_KEY: &str = "theme.profile";
pub const THEME_PROFILE_LIBRARY_KEY: &str = "theme.library";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThemePersistenceTarget {
    ActiveProfile,
    Library,
}

impl ThemePersistenceTarget {
    pub const fn key(self) -> &'static str {
        match self {
            Self::ActiveProfile => ACTIVE_THEME_PROFILE_KEY,
            Self::Library => THEME_PROFILE_LIBRARY_KEY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePersistenceToken {
    generation: u64,
    target: ThemePersistenceTarget,
}

impl ThemePersistenceToken {
    pub const fn target(self) -> ThemePersistenceTarget {
        self.target
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePersistenceRequest {
    token: ThemePersistenceToken,
    value: String,
}

impl ThemePersistenceRequest {
    pub const fn token(&self) -> ThemePersistenceToken {
        self.token
    }

    pub const fn key(&self) -> &'static str {
        self.token.target.key()
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeEditorFeedback {
    Idle,
    Previewing,
    Pending,
    Success,
    Error,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeExternalOutcome {
    Adopted,
    ConfirmedPendingWrite,
    PreservedLocalPreview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallpaperRegenerationToken {
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperRegenerationOutcome {
    IgnoredStale,
    Failed,
    PreviewUpdated,
    PersistenceRequired(ThemePersistenceRequest),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThemeEditorSnapshot {
    pub committed_profile: ThemeProfile,
    pub preview_profile: Option<ThemeProfile>,
    pub library: ThemeProfileLibrary,
    pub feedback: ThemeEditorFeedback,
    pub status: String,
    pub conflict: Option<String>,
    pub pending: Vec<ThemePersistenceTarget>,
    pub wallpaper_regeneration_pending: bool,
}

struct PendingProfile {
    token: ThemePersistenceToken,
    candidate: ThemeProfile,
}

struct PendingLibrary {
    token: ThemePersistenceToken,
    candidate: ThemeProfileLibrary,
}

struct PendingWallpaperRegeneration {
    token: WallpaperRegenerationToken,
    profile_id: String,
    wallpaper_id: String,
}

struct ThemeEditorState {
    runtime: ThemeRuntime,
    committed_profile: ThemeProfile,
    preview_profile: Option<ThemeProfile>,
    library: ThemeProfileLibrary,
    next_generation: u64,
    pending_profile: Option<PendingProfile>,
    pending_library: Option<PendingLibrary>,
    pending_wallpaper: Option<PendingWallpaperRegeneration>,
    feedback: ThemeEditorFeedback,
    status: String,
    conflict: Option<String>,
}

/// Cloneable Settings-side owner for one local preview session.
#[derive(Clone)]
pub struct ThemeProfileEditor {
    state: Rc<RefCell<ThemeEditorState>>,
}

impl Default for ThemeProfileEditor {
    fn default() -> Self {
        Self::new(ThemeProfile::default(), ThemeProfileLibrary::default())
            .expect("the default theme editing state is valid")
    }
}

impl ThemeProfileEditor {
    pub fn new(
        committed_profile: ThemeProfile,
        library: ThemeProfileLibrary,
    ) -> Result<Self, ThemeEditorError> {
        library.validate()?;
        let runtime = ThemeRuntime::new(committed_profile.clone())?;
        Ok(Self {
            state: Rc::new(RefCell::new(ThemeEditorState {
                runtime,
                committed_profile,
                preview_profile: None,
                library,
                next_generation: 1,
                pending_profile: None,
                pending_library: None,
                pending_wallpaper: None,
                feedback: ThemeEditorFeedback::Idle,
                status: "主题配置已同步".into(),
                conflict: None,
            })),
        })
    }

    pub fn runtime(&self) -> ThemeRuntime {
        self.state.borrow().runtime.clone()
    }

    pub fn snapshot(&self) -> ThemeEditorSnapshot {
        let state = self.state.borrow();
        let mut pending = Vec::new();
        if state.pending_profile.is_some() {
            pending.push(ThemePersistenceTarget::ActiveProfile);
        }
        if state.pending_library.is_some() {
            pending.push(ThemePersistenceTarget::Library);
        }
        ThemeEditorSnapshot {
            committed_profile: state.committed_profile.clone(),
            preview_profile: state.preview_profile.clone(),
            library: state.library.clone(),
            feedback: state.feedback,
            status: state.status.clone(),
            conflict: state.conflict.clone(),
            pending,
            wallpaper_regeneration_pending: state.pending_wallpaper.is_some(),
        }
    }

    /// Validate and atomically publish a local candidate without persistence.
    pub fn preview(&self, profile: ThemeProfile) -> Result<(), ThemeEditorError> {
        let mut state = self.state.borrow_mut();
        state.runtime.publish(profile.clone())?;
        if profile == state.committed_profile {
            state.preview_profile = None;
            state.feedback = ThemeEditorFeedback::Idle;
            state.status = "预览已回到当前已保存主题".into();
        } else {
            state.preview_profile = Some(profile);
            state.feedback = ThemeEditorFeedback::Previewing;
            state.status = "主题修改正在实时预览，尚未保存".into();
        }
        Ok(())
    }

    pub fn preview_json(&self, text: &str) -> Result<(), ThemeEditorError> {
        self.preview(ThemeProfile::from_json(text)?)
    }

    pub fn preview_saved(&self, id: &str) -> Result<(), ThemeEditorError> {
        let profile = self
            .state
            .borrow()
            .library
            .get(id)
            .cloned()
            .ok_or_else(|| ThemeLibraryError::MissingProfile(id.into()))?;
        self.preview(profile)
    }

    /// Revert a local preview. An already-issued active-profile write must be
    /// resolved first because its backend side effects can no longer be
    /// cancelled safely by this model.
    pub fn cancel_preview(&self) -> Result<bool, ThemeEditorError> {
        let mut state = self.state.borrow_mut();
        if state.pending_profile.is_some() {
            return Err(ThemeEditorError::PersistencePending(
                ThemePersistenceTarget::ActiveProfile,
            ));
        }
        if state.preview_profile.take().is_none() {
            return Ok(false);
        }
        let committed = state.committed_profile.clone();
        state.runtime.publish(committed)?;
        state.feedback = ThemeEditorFeedback::Success;
        state.status = "已取消主题预览并恢复已保存配置".into();
        state.conflict = None;
        Ok(true)
    }

    /// Create an active-profile CTRL-5 mutation for the current local preview.
    pub fn begin_profile_commit(&self) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let mut state = self.state.borrow_mut();
        let candidate = state
            .preview_profile
            .clone()
            .ok_or(ThemeEditorError::NoPreview)?;
        begin_profile_write(&mut state, candidate, "正在保存当前主题配置")
    }

    /// Start an asynchronous palette extraction for a new wallpaper identity.
    /// Only the latest token for the same editor may publish its result.
    pub fn begin_wallpaper_regeneration(
        &self,
        wallpaper_id: impl Into<String>,
    ) -> Result<WallpaperRegenerationToken, ThemeEditorError> {
        let wallpaper_id = wallpaper_id.into();
        if wallpaper_id.trim().is_empty()
            || wallpaper_id.len() > 1024
            || wallpaper_id.chars().any(char::is_control)
        {
            return Err(WallpaperPaletteError::InvalidWallpaperId.into());
        }
        let mut state = self.state.borrow_mut();
        let visible = state
            .preview_profile
            .as_ref()
            .unwrap_or(&state.committed_profile);
        match &visible.base {
            nkdhr_theme::ThemeBase::BuiltIn { .. } => {
                return Err(WallpaperPaletteError::NotWallpaperProfile.into());
            }
            nkdhr_theme::ThemeBase::Wallpaper { live: false, .. } => {
                return Err(WallpaperPaletteError::NotLiveLinked.into());
            }
            nkdhr_theme::ThemeBase::Wallpaper { live: true, .. } => {}
        }
        let profile_id = visible.id.clone();
        let generation = state.next_generation;
        state.next_generation = generation.wrapping_add(1).max(1);
        let token = WallpaperRegenerationToken { generation };
        state.pending_wallpaper = Some(PendingWallpaperRegeneration {
            token,
            profile_id,
            wallpaper_id,
        });
        state.feedback = ThemeEditorFeedback::Pending;
        state.status = "正在从新壁纸生成完整配色".into();
        Ok(token)
    }

    /// Publish a generated base palette. Clean profiles immediately return an
    /// atomic persistence request; an existing local preview is updated in
    /// place but remains explicitly unsaved so regeneration cannot commit the
    /// user's unrelated edits behind their back.
    pub fn complete_wallpaper_regeneration(
        &self,
        token: WallpaperRegenerationToken,
        result: Result<PaletteData, String>,
    ) -> Result<WallpaperRegenerationOutcome, ThemeEditorError> {
        let mut state = self.state.borrow_mut();
        let is_latest = state
            .pending_wallpaper
            .as_ref()
            .is_some_and(|pending| pending.token == token);
        if !is_latest {
            return Ok(WallpaperRegenerationOutcome::IgnoredStale);
        }
        let pending = state
            .pending_wallpaper
            .take()
            .expect("latest wallpaper regeneration exists");
        let palette = match result {
            Ok(palette) => palette,
            Err(status) => {
                state.feedback = ThemeEditorFeedback::Error;
                state.status = status;
                return Ok(WallpaperRegenerationOutcome::Failed);
            }
        };
        let visible = state
            .preview_profile
            .clone()
            .unwrap_or_else(|| state.committed_profile.clone());
        if visible.id != pending.profile_id
            || !matches!(
                &visible.base,
                nkdhr_theme::ThemeBase::Wallpaper { live: true, .. }
            )
        {
            state.feedback = if state.preview_profile.is_some() {
                ThemeEditorFeedback::Previewing
            } else {
                ThemeEditorFeedback::Idle
            };
            state.status = "壁纸已再次变化，较早的配色结果已忽略".into();
            return Ok(WallpaperRegenerationOutcome::IgnoredStale);
        }
        let had_local_work = state.preview_profile.is_some() || state.pending_profile.is_some();
        let candidate =
            match regenerate_live_wallpaper_profile(&visible, pending.wallpaper_id, palette) {
                Ok(candidate) => candidate,
                Err(error) => {
                    state.feedback = ThemeEditorFeedback::Error;
                    state.status = format!("壁纸配色无效：{error}");
                    return Err(error.into());
                }
            };
        if let Err(error) = state.runtime.publish(candidate.clone()) {
            state.feedback = ThemeEditorFeedback::Error;
            state.status = format!("壁纸配色无法发布：{error}");
            return Err(error.into());
        }
        state.preview_profile = Some(candidate.clone());
        if had_local_work {
            state.feedback = ThemeEditorFeedback::Previewing;
            state.status = "壁纸配色已更新；现有本地主题修改仍未保存".into();
            Ok(WallpaperRegenerationOutcome::PreviewUpdated)
        } else {
            let request = begin_profile_write(
                &mut state,
                candidate,
                "壁纸配色已生成，正在保存便携回退调色板",
            )?;
            Ok(WallpaperRegenerationOutcome::PersistenceRequired(request))
        }
    }

    /// Stage the displayed profile into the library and return one atomic
    /// `theme.library` write. The durable in-memory library changes only after
    /// the host confirms that write.
    pub fn begin_save_current(&self) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let mut state = self.state.borrow_mut();
        let current = state
            .preview_profile
            .clone()
            .unwrap_or_else(|| state.committed_profile.clone());
        let mut candidate = state.library.clone();
        candidate.save(current)?;
        begin_library_write(&mut state, candidate, "正在保存配色方案到资料库")
    }

    pub fn begin_copy_saved(
        &self,
        source_id: &str,
        new_id: impl Into<String>,
        new_name: impl Into<String>,
    ) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let mut state = self.state.borrow_mut();
        let mut candidate = state.library.clone();
        candidate.copy(source_id, new_id, new_name)?;
        begin_library_write(&mut state, candidate, "正在复制配色方案")
    }

    pub fn begin_import_profile(
        &self,
        text: &str,
    ) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let profile = ThemeProfile::from_json(text)?;
        profile.resolve()?;
        let mut state = self.state.borrow_mut();
        let mut candidate = state.library.clone();
        candidate.save(profile)?;
        begin_library_write(&mut state, candidate, "正在导入配色方案")
    }

    pub fn begin_import_library(
        &self,
        text: &str,
    ) -> Result<ThemePersistenceRequest, ThemeEditorError> {
        let candidate = ThemeProfileLibrary::from_json(text)?;
        let mut state = self.state.borrow_mut();
        begin_library_write(&mut state, candidate, "正在导入配色资料库")
    }

    pub fn export_current_profile(&self) -> Result<String, ThemeEditorError> {
        let state = self.state.borrow();
        state
            .preview_profile
            .as_ref()
            .unwrap_or(&state.committed_profile)
            .to_json_pretty()
            .map_err(ThemeEditorError::from)
    }

    pub fn export_saved_profile(&self, id: &str) -> Result<String, ThemeEditorError> {
        self.state
            .borrow()
            .library
            .export_profile_json(id)
            .map_err(ThemeEditorError::from)
    }

    pub fn export_library(&self) -> Result<String, ThemeEditorError> {
        self.state
            .borrow()
            .library
            .to_json_pretty()
            .map_err(ThemeEditorError::from)
    }

    /// Complete only the latest write for the target. A failed active-profile
    /// write deliberately keeps the preview visible and editable for retry.
    pub fn complete_persistence(
        &self,
        token: ThemePersistenceToken,
        result: Result<String, String>,
    ) -> bool {
        let mut state = self.state.borrow_mut();
        match token.target {
            ThemePersistenceTarget::ActiveProfile => {
                let is_latest = state
                    .pending_profile
                    .as_ref()
                    .is_some_and(|pending| pending.token == token);
                if !is_latest {
                    return false;
                }
                let pending = state.pending_profile.take().expect("latest pending exists");
                match result {
                    Ok(status) => {
                        state.committed_profile = pending.candidate.clone();
                        let newer_preview = state
                            .preview_profile
                            .as_ref()
                            .is_some_and(|preview| preview != &pending.candidate);
                        if !newer_preview {
                            state.preview_profile = None;
                        }
                        publish_visible(&mut state);
                        state.conflict = None;
                        if newer_preview {
                            state.feedback = ThemeEditorFeedback::Previewing;
                            state.status = format!("{status}；较新的本地预览仍未保存");
                        } else {
                            state.feedback = ThemeEditorFeedback::Success;
                            state.status = status;
                        }
                    }
                    Err(status) => {
                        state.feedback = ThemeEditorFeedback::Error;
                        state.status = status;
                    }
                }
            }
            ThemePersistenceTarget::Library => {
                let is_latest = state
                    .pending_library
                    .as_ref()
                    .is_some_and(|pending| pending.token == token);
                if !is_latest {
                    return false;
                }
                let pending = state.pending_library.take().expect("latest pending exists");
                match result {
                    Ok(status) => {
                        state.library = pending.candidate;
                        state.feedback = ThemeEditorFeedback::Success;
                        state.status = status;
                        state.conflict = None;
                    }
                    Err(status) => {
                        state.feedback = ThemeEditorFeedback::Error;
                        state.status = status;
                    }
                }
            }
        }
        true
    }

    /// Reconcile a changed `theme.profile` value received by the host.
    pub fn accept_external_profile(
        &self,
        profile: ThemeProfile,
    ) -> Result<ThemeExternalOutcome, ThemeEditorError> {
        profile.resolve()?;
        let mut state = self.state.borrow_mut();
        if state
            .pending_profile
            .as_ref()
            .is_some_and(|pending| pending.candidate == profile)
        {
            state.pending_profile = None;
            state.committed_profile = profile.clone();
            let newer_preview = state
                .preview_profile
                .as_ref()
                .is_some_and(|preview| preview != &profile);
            if !newer_preview {
                state.preview_profile = None;
            }
            publish_visible(&mut state);
            state.conflict = None;
            if newer_preview {
                state.feedback = ThemeEditorFeedback::Previewing;
                state.status = "CTRL-5 已确认较早的主题；较新的本地预览仍未保存".into();
            } else {
                state.feedback = ThemeEditorFeedback::Success;
                state.status = "主题配置已由 CTRL-5 确认".into();
            }
            return Ok(ThemeExternalOutcome::ConfirmedPendingWrite);
        }

        let write_in_flight = state.pending_profile.is_some();
        state.committed_profile = profile;
        if state.preview_profile.is_some() || write_in_flight {
            state.feedback = ThemeEditorFeedback::Conflict;
            state.status = if write_in_flight {
                "外部主题已变化；本地预览和仍在途的保存请求均被保留".into()
            } else {
                "外部主题已变化；本地预览被保留，请选择保存或取消".into()
            };
            state.conflict = Some("theme.profile changed outside Appearance Settings".into());
            Ok(ThemeExternalOutcome::PreservedLocalPreview)
        } else {
            publish_visible(&mut state);
            state.feedback = ThemeEditorFeedback::Success;
            state.status = "已同步外部主题配置".into();
            state.conflict = None;
            Ok(ThemeExternalOutcome::Adopted)
        }
    }

    pub fn accept_external_profile_json(
        &self,
        text: &str,
    ) -> Result<ThemeExternalOutcome, ThemeEditorError> {
        self.accept_external_profile(ThemeProfile::from_json(text)?)
    }

    pub fn accept_external_library(
        &self,
        library: ThemeProfileLibrary,
    ) -> Result<ThemeExternalOutcome, ThemeEditorError> {
        library.validate()?;
        let mut state = self.state.borrow_mut();
        if state
            .pending_library
            .as_ref()
            .is_some_and(|pending| pending.candidate == library)
        {
            state.pending_library = None;
            state.library = library;
            state.feedback = ThemeEditorFeedback::Success;
            state.status = "配色资料库已由 CTRL-5 确认".into();
            state.conflict = None;
            return Ok(ThemeExternalOutcome::ConfirmedPendingWrite);
        }
        let conflicted = state.pending_library.is_some();
        state.library = library;
        state.feedback = if conflicted {
            ThemeEditorFeedback::Conflict
        } else {
            ThemeEditorFeedback::Success
        };
        state.status = if conflicted {
            "外部配色资料库已变化；仍在途的本地资料库写入被保留".into()
        } else {
            "已同步外部配色资料库".into()
        };
        state.conflict =
            conflicted.then(|| "theme.library changed outside Appearance Settings".into());
        Ok(if conflicted {
            ThemeExternalOutcome::PreservedLocalPreview
        } else {
            ThemeExternalOutcome::Adopted
        })
    }

    pub fn accept_external_library_json(
        &self,
        text: &str,
    ) -> Result<ThemeExternalOutcome, ThemeEditorError> {
        self.accept_external_library(ThemeProfileLibrary::from_json(text)?)
    }
}

fn next_token(
    state: &mut ThemeEditorState,
    target: ThemePersistenceTarget,
) -> ThemePersistenceToken {
    let generation = state.next_generation;
    state.next_generation = generation.wrapping_add(1).max(1);
    ThemePersistenceToken { generation, target }
}

fn begin_profile_write(
    state: &mut ThemeEditorState,
    candidate: ThemeProfile,
    status: &str,
) -> Result<ThemePersistenceRequest, ThemeEditorError> {
    let value = candidate.to_json_pretty()?;
    let token = next_token(state, ThemePersistenceTarget::ActiveProfile);
    state.pending_profile = Some(PendingProfile { token, candidate });
    state.feedback = ThemeEditorFeedback::Pending;
    state.status = status.into();
    Ok(ThemePersistenceRequest { token, value })
}

fn begin_library_write(
    state: &mut ThemeEditorState,
    candidate: ThemeProfileLibrary,
    status: &str,
) -> Result<ThemePersistenceRequest, ThemeEditorError> {
    candidate.validate()?;
    let value = candidate.to_json()?;
    let token = next_token(state, ThemePersistenceTarget::Library);
    state.pending_library = Some(PendingLibrary { token, candidate });
    state.feedback = ThemeEditorFeedback::Pending;
    state.status = status.into();
    Ok(ThemePersistenceRequest { token, value })
}

fn publish_visible(state: &mut ThemeEditorState) {
    let visible = state
        .preview_profile
        .clone()
        .unwrap_or_else(|| state.committed_profile.clone());
    state
        .runtime
        .publish(visible)
        .expect("stored theme editor profiles were validated before insertion");
}

#[derive(Debug)]
pub enum ThemeEditorError {
    Profile(ThemeProfileError),
    Library(ThemeLibraryError),
    Runtime(ThemeRuntimeError),
    Wallpaper(WallpaperPaletteError),
    NoPreview,
    PersistencePending(ThemePersistenceTarget),
}

impl fmt::Display for ThemeEditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => error.fmt(formatter),
            Self::Library(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
            Self::Wallpaper(error) => error.fmt(formatter),
            Self::NoPreview => formatter.write_str("there is no unsaved theme preview"),
            Self::PersistencePending(target) => {
                write!(formatter, "a {} write is still pending", target.key())
            }
        }
    }
}

impl std::error::Error for ThemeEditorError {}

impl From<ThemeProfileError> for ThemeEditorError {
    fn from(value: ThemeProfileError) -> Self {
        Self::Profile(value)
    }
}

impl From<ThemeLibraryError> for ThemeEditorError {
    fn from(value: ThemeLibraryError) -> Self {
        Self::Library(value)
    }
}

impl From<ThemeRuntimeError> for ThemeEditorError {
    fn from(value: ThemeRuntimeError) -> Self {
        Self::Runtime(value)
    }
}

impl From<WallpaperPaletteError> for ThemeEditorError {
    fn from(value: WallpaperPaletteError) -> Self {
        Self::Wallpaper(value)
    }
}

#[cfg(test)]
mod tests {
    use nkdhr_theme::{BuiltInTheme, ThemeBase};
    use serde_json::json;

    use super::*;

    fn profile(id: &str, accent: &str) -> ThemeProfile {
        ThemeProfile {
            id: id.into(),
            name: id.into(),
            overrides: json!({"palette": {"accent": accent}}),
            ..ThemeProfile::default()
        }
    }

    fn wallpaper_profile() -> ThemeProfile {
        ThemeProfile {
            id: "live-wallpaper".into(),
            name: "Live Wallpaper".into(),
            base: ThemeBase::Wallpaper {
                live: true,
                wallpaper_id: "old".into(),
                frozen_palette: Box::new(PaletteData::tokyo_night()),
            },
            ..ThemeProfile::default()
        }
    }

    #[test]
    fn preview_and_cancel_publish_complete_atomic_snapshots() {
        let editor = ThemeProfileEditor::default();
        let initial = editor.runtime().snapshot();
        let preview = profile("preview", "#010203ff");
        editor.preview(preview.clone()).unwrap();
        let visible = editor.runtime().snapshot();
        assert!(visible.generation() > initial.generation());
        assert_eq!(visible.resolved().profile, preview);
        assert_eq!(editor.snapshot().feedback, ThemeEditorFeedback::Previewing);

        assert!(editor.cancel_preview().unwrap());
        assert_eq!(
            editor.runtime().snapshot().resolved().profile,
            ThemeProfile::default()
        );
        assert!(editor.snapshot().preview_profile.is_none());
    }

    #[test]
    fn failed_commit_keeps_preview_and_latest_token_wins() {
        let editor = ThemeProfileEditor::default();
        let preview = profile("preview", "#010203ff");
        editor.preview(preview.clone()).unwrap();
        let stale = editor.begin_profile_commit().unwrap();
        let latest = editor.begin_profile_commit().unwrap();
        assert!(!editor.complete_persistence(stale.token(), Ok("stale".into())));
        assert!(editor.complete_persistence(latest.token(), Err("backend failed".into())));
        let snapshot = editor.snapshot();
        assert_eq!(snapshot.preview_profile, Some(preview.clone()));
        assert_eq!(snapshot.feedback, ThemeEditorFeedback::Error);
        assert_eq!(editor.runtime().snapshot().resolved().profile, preview);
    }

    #[test]
    fn successful_older_commit_does_not_mark_a_newer_preview_as_saved() {
        let editor = ThemeProfileEditor::default();
        let first = profile("first", "#010203ff");
        editor.preview(first.clone()).unwrap();
        let request = editor.begin_profile_commit().unwrap();
        let newer = profile("newer", "#aabbccff");
        editor.preview(newer.clone()).unwrap();
        assert!(editor.complete_persistence(request.token(), Ok("较早主题已保存".into())));
        let snapshot = editor.snapshot();
        assert_eq!(snapshot.committed_profile, first);
        assert_eq!(snapshot.preview_profile, Some(newer.clone()));
        assert_eq!(snapshot.feedback, ThemeEditorFeedback::Previewing);
        assert!(snapshot.status.contains("较新的本地预览仍未保存"));
        assert_eq!(editor.runtime().snapshot().resolved().profile, newer);
    }

    #[test]
    fn external_change_preserves_local_work_and_confirmation_clears_it() {
        let editor = ThemeProfileEditor::default();
        let preview = profile("preview", "#010203ff");
        editor.preview(preview.clone()).unwrap();
        assert_eq!(
            editor
                .accept_external_profile(profile("external", "#aabbccff"))
                .unwrap(),
            ThemeExternalOutcome::PreservedLocalPreview
        );
        assert_eq!(editor.runtime().snapshot().resolved().profile, preview);
        assert_eq!(editor.snapshot().feedback, ThemeEditorFeedback::Conflict);

        let request = editor.begin_profile_commit().unwrap();
        let candidate = ThemeProfile::from_json(request.value()).unwrap();
        assert_eq!(
            editor.accept_external_profile(candidate.clone()).unwrap(),
            ThemeExternalOutcome::ConfirmedPendingWrite
        );
        assert!(editor.snapshot().preview_profile.is_none());
        assert_eq!(editor.snapshot().committed_profile, candidate);
    }

    #[test]
    fn divergent_external_change_keeps_the_in_flight_request_trackable() {
        let editor = ThemeProfileEditor::default();
        editor.preview(profile("local", "#010203ff")).unwrap();
        let request = editor.begin_profile_commit().unwrap();
        assert_eq!(
            editor
                .accept_external_profile(profile("external", "#aabbccff"))
                .unwrap(),
            ThemeExternalOutcome::PreservedLocalPreview
        );
        assert!(
            editor
                .snapshot()
                .pending
                .contains(&ThemePersistenceTarget::ActiveProfile)
        );
        assert!(editor.complete_persistence(request.token(), Ok("本地写入最终完成".into())));
        assert!(editor.snapshot().pending.is_empty());
    }

    #[test]
    fn library_operations_wait_for_host_confirmation_and_round_trip() {
        let editor = ThemeProfileEditor::default();
        editor.preview(profile("saved", "#010203ff")).unwrap();
        let save = editor.begin_save_current().unwrap();
        assert_eq!(save.key(), THEME_PROFILE_LIBRARY_KEY);
        assert!(editor.snapshot().library.get("saved").is_none());
        assert!(editor.complete_persistence(save.token(), Ok("saved".into())));
        assert!(editor.snapshot().library.get("saved").is_some());

        let copy = editor
            .begin_copy_saved("saved", "saved-copy", "Saved Copy")
            .unwrap();
        assert!(editor.complete_persistence(copy.token(), Ok("copied".into())));
        let exported = editor.export_saved_profile("saved-copy").unwrap();
        let imported = ThemeProfile::from_json(&exported).unwrap();
        assert_eq!(imported.name, "Saved Copy");
        ThemeProfileLibrary::from_json(&editor.export_library().unwrap()).unwrap();
    }

    #[test]
    fn invalid_import_never_changes_runtime_or_library() {
        let editor = ThemeProfileEditor::default();
        let generation = editor.runtime().snapshot().generation();
        let before = editor.snapshot().library;
        assert!(editor.preview_json("{not json").is_err());
        assert!(editor.begin_import_profile("{not json").is_err());
        assert_eq!(editor.runtime().snapshot().generation(), generation);
        assert_eq!(editor.snapshot().library, before);
    }

    #[test]
    fn built_in_base_can_be_previewed_without_overrides() {
        let editor = ThemeProfileEditor::default();
        let nord = ThemeProfile {
            id: "nord".into(),
            name: "Nord".into(),
            base: ThemeBase::BuiltIn {
                preset: BuiltInTheme::Nord,
            },
            ..ThemeProfile::default()
        };
        editor.preview(nord.clone()).unwrap();
        assert_eq!(editor.runtime().snapshot().resolved().profile, nord);
    }

    #[test]
    fn latest_clean_wallpaper_regeneration_previews_and_requests_persistence() {
        let editor =
            ThemeProfileEditor::new(wallpaper_profile(), ThemeProfileLibrary::default()).unwrap();
        let stale = editor
            .begin_wallpaper_regeneration("wallpaper:stale")
            .unwrap();
        let latest = editor
            .begin_wallpaper_regeneration("wallpaper:latest")
            .unwrap();
        assert_eq!(
            editor
                .complete_wallpaper_regeneration(stale, Ok(PaletteData::tokyo_night()))
                .unwrap(),
            WallpaperRegenerationOutcome::IgnoredStale
        );
        let outcome = editor
            .complete_wallpaper_regeneration(latest, Ok(PaletteData::nord()))
            .unwrap();
        let WallpaperRegenerationOutcome::PersistenceRequired(request) = outcome else {
            panic!("clean live regeneration must persist its frozen fallback")
        };
        assert_eq!(request.key(), ACTIVE_THEME_PROFILE_KEY);
        let candidate = ThemeProfile::from_json(request.value()).unwrap();
        let ThemeBase::Wallpaper {
            wallpaper_id,
            frozen_palette,
            ..
        } = &candidate.base
        else {
            panic!("candidate stays wallpaper-based")
        };
        assert_eq!(wallpaper_id, "wallpaper:latest");
        assert_eq!(frozen_palette.as_ref(), &PaletteData::nord());
        assert_eq!(editor.runtime().snapshot().resolved().profile, candidate);
    }

    #[test]
    fn regeneration_preserves_dirty_overrides_without_implicitly_saving_them() {
        let editor =
            ThemeProfileEditor::new(wallpaper_profile(), ThemeProfileLibrary::default()).unwrap();
        let mut dirty = wallpaper_profile();
        dirty.overrides = json!({"palette": {"accent": "#010203ff"}});
        editor.preview(dirty).unwrap();
        let token = editor
            .begin_wallpaper_regeneration("wallpaper:new")
            .unwrap();
        assert_eq!(
            editor
                .complete_wallpaper_regeneration(token, Ok(PaletteData::nord()))
                .unwrap(),
            WallpaperRegenerationOutcome::PreviewUpdated
        );
        let snapshot = editor.snapshot();
        assert!(snapshot.pending.is_empty());
        let preview = snapshot.preview_profile.unwrap();
        assert_eq!(
            preview.overrides,
            json!({"palette": {"accent": "#010203ff"}})
        );
        assert_eq!(preview.resolve().unwrap().data.palette.accent, "#010203ff");
        assert_eq!(
            preview.resolve().unwrap().data.palette.surface,
            PaletteData::nord().surface
        );
    }

    #[test]
    fn failed_wallpaper_extraction_keeps_the_last_visible_generation() {
        let editor =
            ThemeProfileEditor::new(wallpaper_profile(), ThemeProfileLibrary::default()).unwrap();
        let generation = editor.runtime().snapshot().generation();
        let token = editor
            .begin_wallpaper_regeneration("wallpaper:new")
            .unwrap();
        assert!(editor.snapshot().wallpaper_regeneration_pending);
        assert_eq!(
            editor
                .complete_wallpaper_regeneration(token, Err("图片解码失败".into()))
                .unwrap(),
            WallpaperRegenerationOutcome::Failed
        );
        assert_eq!(editor.runtime().snapshot().generation(), generation);
        assert_eq!(editor.snapshot().feedback, ThemeEditorFeedback::Error);
    }

    #[test]
    fn invalid_palette_or_profile_switch_cannot_publish_a_finished_job() {
        let editor =
            ThemeProfileEditor::new(wallpaper_profile(), ThemeProfileLibrary::default()).unwrap();
        let generation = editor.runtime().snapshot().generation();
        let invalid_token = editor
            .begin_wallpaper_regeneration("wallpaper:new")
            .unwrap();
        let mut invalid = PaletteData::nord();
        invalid.accent = "not-a-color".into();
        assert!(
            editor
                .complete_wallpaper_regeneration(invalid_token, Ok(invalid))
                .is_err()
        );
        assert_eq!(editor.runtime().snapshot().generation(), generation);
        assert_eq!(editor.snapshot().feedback, ThemeEditorFeedback::Error);

        let switched_token = editor
            .begin_wallpaper_regeneration("wallpaper:obsolete")
            .unwrap();
        editor.preview(ThemeProfile::default()).unwrap();
        assert_eq!(
            editor
                .complete_wallpaper_regeneration(switched_token, Ok(PaletteData::nord()))
                .unwrap(),
            WallpaperRegenerationOutcome::IgnoredStale
        );
        assert!(!editor.snapshot().wallpaper_regeneration_pending);
        assert_eq!(
            editor.runtime().snapshot().resolved().profile,
            ThemeProfile::default()
        );
    }

    #[test]
    fn newer_wallpaper_result_survives_confirmation_of_the_previous_palette() {
        let editor =
            ThemeProfileEditor::new(wallpaper_profile(), ThemeProfileLibrary::default()).unwrap();
        let first_token = editor
            .begin_wallpaper_regeneration("wallpaper:first")
            .unwrap();
        let first = editor
            .complete_wallpaper_regeneration(first_token, Ok(PaletteData::nord()))
            .unwrap();
        let WallpaperRegenerationOutcome::PersistenceRequired(first_request) = first else {
            panic!("first clean regeneration persists")
        };

        let newer_token = editor
            .begin_wallpaper_regeneration("wallpaper:newer")
            .unwrap();
        assert_eq!(
            editor
                .complete_wallpaper_regeneration(newer_token, Ok(PaletteData::tokyo_night()))
                .unwrap(),
            WallpaperRegenerationOutcome::PreviewUpdated
        );
        assert!(editor.complete_persistence(first_request.token(), Ok("旧调色板已保存".into())));
        let snapshot = editor.snapshot();
        assert_eq!(snapshot.feedback, ThemeEditorFeedback::Previewing);
        let preview = snapshot.preview_profile.unwrap();
        let ThemeBase::Wallpaper { wallpaper_id, .. } = preview.base else {
            panic!("preview remains wallpaper-based")
        };
        assert_eq!(wallpaper_id, "wallpaper:newer");
    }
}
