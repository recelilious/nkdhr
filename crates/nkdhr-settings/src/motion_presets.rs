//! Host-independent persistence transactions for UI-7 motion preset snapshots.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use nkdhr_theme::{
    MOTION_STYLE_SCHEMA_VERSION, MotionPresetLibraryData, MotionPresetLibraryError,
    MotionStyleBaseData, MotionStylePresetData, MotionStyleProfileData,
};
use nkdhr_ui::{CompiledMotionStyle, MotionStyleCompileError};

use crate::{ThemeEditorFeedback, ThemeExternalOutcome};

pub const MOTION_PRESET_LIBRARY_KEY: &str = "theme.motion_library";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionPresetPersistenceToken {
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionPresetPersistenceRequest {
    token: MotionPresetPersistenceToken,
    value: String,
}

impl MotionPresetPersistenceRequest {
    pub const fn token(&self) -> MotionPresetPersistenceToken {
        self.token
    }

    pub const fn key(&self) -> &'static str {
        MOTION_PRESET_LIBRARY_KEY
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MotionPresetLibrarySnapshot {
    pub library: MotionPresetLibraryData,
    pub pending: bool,
    pub feedback: ThemeEditorFeedback,
    pub status: String,
    pub conflict: Option<String>,
}

struct PendingLibrary {
    token: MotionPresetPersistenceToken,
    candidate: MotionPresetLibraryData,
    value: String,
}

struct MotionPresetLibraryState {
    library: MotionPresetLibraryData,
    pending: Option<PendingLibrary>,
    next_generation: u64,
    feedback: ThemeEditorFeedback,
    status: String,
    conflict: Option<String>,
}

/// Cloneable transaction owner. Durable state changes only after the host
/// confirms the complete scalar CTRL-5 write.
#[derive(Clone)]
pub struct MotionPresetLibraryEditor {
    state: Rc<RefCell<MotionPresetLibraryState>>,
}

impl Default for MotionPresetLibraryEditor {
    fn default() -> Self {
        Self::new(MotionPresetLibraryData::default())
            .expect("the empty motion preset library is valid")
    }
}

impl MotionPresetLibraryEditor {
    pub fn new(library: MotionPresetLibraryData) -> Result<Self, MotionPresetEditorError> {
        validate_compiled_library(&library)?;
        Ok(Self {
            state: Rc::new(RefCell::new(MotionPresetLibraryState {
                library,
                pending: None,
                next_generation: 1,
                feedback: ThemeEditorFeedback::Idle,
                status: "动画预设资料库已同步".into(),
                conflict: None,
            })),
        })
    }

    pub fn snapshot(&self) -> MotionPresetLibrarySnapshot {
        let state = self.state.borrow();
        MotionPresetLibrarySnapshot {
            library: state.library.clone(),
            pending: state.pending.is_some(),
            feedback: state.feedback,
            status: state.status.clone(),
            conflict: state.conflict.clone(),
        }
    }

    pub fn begin_insert(
        &self,
        preset: MotionStylePresetData,
    ) -> Result<MotionPresetPersistenceRequest, MotionPresetEditorError> {
        validate_compiled_preset(&preset)?;
        let mut state = self.state.borrow_mut();
        let mut candidate = state.library.clone();
        candidate.insert(preset)?;
        begin_write(&mut state, candidate, "正在保存动画预设快照")
    }

    pub fn begin_import_preset(
        &self,
        text: &str,
    ) -> Result<MotionPresetPersistenceRequest, MotionPresetEditorError> {
        let preset =
            MotionStylePresetData::from_json(text).map_err(MotionStyleCompileError::Data)?;
        self.begin_insert(preset)
    }

    pub fn begin_import_library(
        &self,
        text: &str,
    ) -> Result<MotionPresetPersistenceRequest, MotionPresetEditorError> {
        let candidate = MotionPresetLibraryData::from_json(text)?;
        validate_compiled_library(&candidate)?;
        let mut state = self.state.borrow_mut();
        begin_write(&mut state, candidate, "正在导入动画预设资料库")
    }

    pub fn export_preset(
        &self,
        id: &str,
        revision: u32,
    ) -> Result<String, MotionPresetEditorError> {
        self.state
            .borrow()
            .library
            .export_preset_json(id, revision)
            .map_err(Into::into)
    }

    pub fn export_library(&self) -> Result<String, MotionPresetEditorError> {
        self.state
            .borrow()
            .library
            .to_json_pretty()
            .map_err(Into::into)
    }

    pub fn complete_persistence(
        &self,
        token: MotionPresetPersistenceToken,
        result: Result<String, String>,
    ) -> bool {
        let mut state = self.state.borrow_mut();
        if !state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.token == token)
        {
            return false;
        }
        let pending = state.pending.take().expect("latest pending exists");
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
        true
    }

    pub fn accept_external(
        &self,
        library: MotionPresetLibraryData,
    ) -> Result<ThemeExternalOutcome, MotionPresetEditorError> {
        validate_compiled_library(&library)?;
        let mut state = self.state.borrow_mut();
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.candidate == library)
        {
            state.pending = None;
            state.library = library;
            state.feedback = ThemeEditorFeedback::Success;
            state.status = "动画预设资料库已由 CTRL-5 确认".into();
            state.conflict = None;
            return Ok(ThemeExternalOutcome::ConfirmedPendingWrite);
        }
        let conflicted = state.pending.is_some();
        state.library = library;
        state.feedback = if conflicted {
            ThemeEditorFeedback::Conflict
        } else {
            ThemeEditorFeedback::Success
        };
        state.status = if conflicted {
            "外部动画预设资料库已变化；仍在途的本地写入被保留".into()
        } else {
            "已同步外部动画预设资料库".into()
        };
        state.conflict =
            conflicted.then(|| "theme.motion_library changed outside Motion Settings".into());
        Ok(if conflicted {
            ThemeExternalOutcome::PreservedLocalPreview
        } else {
            ThemeExternalOutcome::Adopted
        })
    }

    pub fn accept_external_json(
        &self,
        text: &str,
    ) -> Result<ThemeExternalOutcome, MotionPresetEditorError> {
        let library = MotionPresetLibraryData::from_json(text)?;
        validate_compiled_library(&library)?;
        {
            let mut state = self.state.borrow_mut();
            if state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.value == text)
            {
                state.pending = None;
                state.library = library;
                state.feedback = ThemeEditorFeedback::Success;
                state.status = "动画预设资料库已由 CTRL-5 确认".into();
                state.conflict = None;
                return Ok(ThemeExternalOutcome::ConfirmedPendingWrite);
            }
        }
        self.accept_external(library)
    }
}

fn begin_write(
    state: &mut MotionPresetLibraryState,
    candidate: MotionPresetLibraryData,
    status: &str,
) -> Result<MotionPresetPersistenceRequest, MotionPresetEditorError> {
    validate_compiled_library(&candidate)?;
    let value = candidate.to_json()?;
    let token = MotionPresetPersistenceToken {
        generation: state.next_generation,
    };
    state.next_generation = state.next_generation.wrapping_add(1).max(1);
    state.pending = Some(PendingLibrary {
        token,
        candidate,
        value: value.clone(),
    });
    state.feedback = ThemeEditorFeedback::Pending;
    state.status = status.into();
    Ok(MotionPresetPersistenceRequest { token, value })
}

fn validate_compiled_library(
    library: &MotionPresetLibraryData,
) -> Result<(), MotionPresetEditorError> {
    library.validate()?;
    for preset in &library.presets {
        validate_compiled_preset(preset)?;
    }
    Ok(())
}

fn validate_compiled_preset(preset: &MotionStylePresetData) -> Result<(), MotionPresetEditorError> {
    let profile = MotionStyleProfileData {
        schema_version: MOTION_STYLE_SCHEMA_VERSION,
        id: preset.id.clone(),
        name: preset.name.clone(),
        base: MotionStyleBaseData::Embedded {
            preset: Box::new(preset.clone()),
        },
        overrides: Default::default(),
    };
    CompiledMotionStyle::compile(profile)?;
    Ok(())
}

#[derive(Debug)]
pub enum MotionPresetEditorError {
    Library(MotionPresetLibraryError),
    Compile(MotionStyleCompileError),
}

impl fmt::Display for MotionPresetEditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Library(error) => error.fmt(formatter),
            Self::Compile(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MotionPresetEditorError {}

impl From<MotionPresetLibraryError> for MotionPresetEditorError {
    fn from(value: MotionPresetLibraryError) -> Self {
        Self::Library(value)
    }
}

impl From<MotionStyleCompileError> for MotionPresetEditorError {
    fn from(value: MotionStyleCompileError) -> Self {
        Self::Compile(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nkdhr_theme::{
        BALANCED_MOTION_STYLE_REVISION, BuiltInMotionStyle, MotionCurveData, MotionFamilyNodeData,
        MotionSemanticFamilyData, MotionTangentsData, MotionValuesData, MotionVectorData,
    };

    fn balanced() -> MotionStylePresetData {
        MotionStylePresetData::built_in(
            BuiltInMotionStyle::Balanced,
            BALANCED_MOTION_STYLE_REVISION,
        )
        .unwrap()
    }

    #[test]
    fn durable_library_changes_only_after_latest_confirmation() {
        let editor = MotionPresetLibraryEditor::default();
        let request = editor.begin_insert(balanced()).unwrap();
        assert_eq!(request.key(), MOTION_PRESET_LIBRARY_KEY);
        assert!(editor.snapshot().library.presets.is_empty());
        assert!(editor.complete_persistence(request.token(), Ok("saved".into())));
        assert_eq!(editor.snapshot().library.presets.len(), 1);
        assert!(!editor.complete_persistence(request.token(), Ok("stale".into())));
    }

    #[test]
    fn compile_invalid_import_never_creates_a_persistence_request() {
        let editor = MotionPresetLibraryEditor::default();
        let mut preset = balanced();
        let mut invalid = MotionCurveData::linear();
        invalid.anchors[0].tangents = MotionTangentsData::Broken {
            incoming: MotionVectorData::ZERO,
            outgoing: MotionVectorData::new(0.8, 0.2),
        };
        invalid.anchors[1].tangents = MotionTangentsData::Broken {
            incoming: MotionVectorData::new(-0.8, -0.2),
            outgoing: MotionVectorData::ZERO,
        };
        preset.style.families.insert(
            MotionSemanticFamilyData::Focus,
            MotionFamilyNodeData {
                values: MotionValuesData {
                    curve: Some(invalid),
                    duration_ms: None,
                    fluid: Default::default(),
                },
                components: Default::default(),
            },
        );
        assert!(matches!(
            editor.begin_insert(preset),
            Err(MotionPresetEditorError::Compile(_))
        ));
        let snapshot = editor.snapshot();
        assert!(!snapshot.pending);
        assert!(snapshot.library.presets.is_empty());
    }

    #[test]
    fn matching_external_value_confirms_pending_write() {
        let editor = MotionPresetLibraryEditor::default();
        let request = editor.begin_insert(balanced()).unwrap();
        assert_eq!(
            editor.accept_external_json(request.value()).unwrap(),
            ThemeExternalOutcome::ConfirmedPendingWrite
        );
        assert!(!editor.snapshot().pending);
        assert_eq!(editor.snapshot().library.presets.len(), 1);
    }
}
