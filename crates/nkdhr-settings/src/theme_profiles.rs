//! Host-independent theme editing transactions for Appearance Settings.
//!
//! Preview publication is synchronous and local. Durable CTRL-5 writes are
//! returned as opaque requests so a Wayland or compositor host can execute
//! D-Bus work away from the UI thread and report the result later.

use std::{cell::RefCell, fmt, rc::Rc};

use nkdhr_theme::{ThemeLibraryError, ThemeProfile, ThemeProfileError, ThemeProfileLibrary};
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

#[derive(Debug, Clone, PartialEq)]
pub struct ThemeEditorSnapshot {
    pub committed_profile: ThemeProfile,
    pub preview_profile: Option<ThemeProfile>,
    pub library: ThemeProfileLibrary,
    pub feedback: ThemeEditorFeedback,
    pub status: String,
    pub conflict: Option<String>,
    pub pending: Vec<ThemePersistenceTarget>,
}

struct PendingProfile {
    token: ThemePersistenceToken,
    candidate: ThemeProfile,
}

struct PendingLibrary {
    token: ThemePersistenceToken,
    candidate: ThemeProfileLibrary,
}

struct ThemeEditorState {
    runtime: ThemeRuntime,
    committed_profile: ThemeProfile,
    preview_profile: Option<ThemeProfile>,
    library: ThemeProfileLibrary,
    next_generation: u64,
    pending_profile: Option<PendingProfile>,
    pending_library: Option<PendingLibrary>,
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
        let value = candidate.to_json_pretty()?;
        let token = next_token(&mut state, ThemePersistenceTarget::ActiveProfile);
        state.pending_profile = Some(PendingProfile { token, candidate });
        state.feedback = ThemeEditorFeedback::Pending;
        state.status = "正在保存当前主题配置".into();
        Ok(ThemePersistenceRequest { token, value })
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
                        if state.preview_profile.as_ref() == Some(&pending.candidate) {
                            state.preview_profile = None;
                        }
                        publish_visible(&mut state);
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
            if state.preview_profile.as_ref() == Some(&profile) {
                state.preview_profile = None;
            }
            publish_visible(&mut state);
            state.feedback = ThemeEditorFeedback::Success;
            state.status = "主题配置已由 CTRL-5 确认".into();
            state.conflict = None;
            return Ok(ThemeExternalOutcome::ConfirmedPendingWrite);
        }

        state.pending_profile = None;
        state.committed_profile = profile;
        if state.preview_profile.is_some() {
            state.feedback = ThemeEditorFeedback::Conflict;
            state.status = "外部主题已变化；本地预览被保留，请选择保存或取消".into();
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
        let conflicted = state.pending_library.take().is_some();
        state.library = library;
        state.feedback = if conflicted {
            ThemeEditorFeedback::Conflict
        } else {
            ThemeEditorFeedback::Success
        };
        state.status = if conflicted {
            "外部配色资料库已变化；未确认的本地资料库写入已丢弃".into()
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
    NoPreview,
    PersistencePending(ThemePersistenceTarget),
}

impl fmt::Display for ThemeEditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => error.fmt(formatter),
            Self::Library(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
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
}
