use std::fmt;
use std::time::Duration;

use super::{
    MotionAnchorClipboardData, MotionCurveEditor, MotionCurveEditorError, MotionEditorEditId,
    MotionEditorTangentSide, MotionGraphPoint,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MotionEditorModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub logo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionEditorKey {
    Tab,
    Enter,
    Space,
    Escape,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    Backspace,
    Delete,
    Character(char),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MotionEditorDevice {
    Mouse,
    Pen {
        pressure: f64,
        barrel: bool,
        eraser: bool,
    },
    Touch {
        contacts: u8,
    },
    PrecisionTouchpad {
        contacts: u8,
    },
}

impl MotionEditorDevice {
    fn validate(self) -> Result<(), MotionEditorInputError> {
        match self {
            Self::Pen { pressure, .. }
                if !pressure.is_finite() || !(0.0..=1.0).contains(&pressure) =>
            {
                Err(MotionEditorInputError::InvalidDevice)
            }
            Self::Touch { contacts } | Self::PrecisionTouchpad { contacts }
                if contacts == 0 || contacts > 2 =>
            {
                Err(MotionEditorInputError::UnsupportedContactCount)
            }
            _ => Ok(()),
        }
    }

    fn supports_direct(self) -> bool {
        matches!(
            self,
            Self::Mouse | Self::Pen { .. } | Self::Touch { contacts: 1 }
        )
    }

    fn supports_viewport(self) -> bool {
        matches!(
            self,
            Self::Mouse
                | Self::Pen { barrel: true, .. }
                | Self::Touch { contacts: 2 }
                | Self::PrecisionTouchpad { contacts: 2 }
        )
    }

    fn is_eraser(self) -> bool {
        matches!(self, Self::Pen { eraser: true, .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionEditorGesturePhase {
    Begin,
    Update,
    End,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionEditorTarget {
    Anchor(usize),
    IncomingHandle(usize),
    OutgoingHandle(usize),
    Curve,
    Playhead,
    Graph,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionEditorDirectInput {
    pub id: MotionEditorEditId,
    pub phase: MotionEditorGesturePhase,
    pub device: MotionEditorDevice,
    pub target: MotionEditorTarget,
    pub position: MotionGraphPoint,
    pub modifiers: MotionEditorModifiers,
    pub activation_count: u8,
    pub snapping: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionEditorViewportInput {
    pub id: MotionEditorEditId,
    pub phase: MotionEditorGesturePhase,
    pub device: MotionEditorDevice,
    /// Anchor in normalized curve coordinates.
    pub anchor: MotionGraphPoint,
    /// Incremental viewport translation since the previous update.
    pub translation: MotionGraphPoint,
    /// Incremental scale; `1` preserves the current span.
    pub time_scale: f64,
    pub progress_scale: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MotionEditorInput {
    Direct(MotionEditorDirectInput),
    Viewport(MotionEditorViewportInput),
    Key {
        key: MotionEditorKey,
        modifiers: MotionEditorModifiers,
        repeat: bool,
        now: Duration,
    },
    PasteText(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionEditorClipboardAction {
    WriteText(String),
    ReadText,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use]
pub struct MotionEditorInputOutcome {
    pub document_changed: bool,
    pub transient_changed: bool,
    pub preview_pending: bool,
    pub clipboard: Option<MotionEditorClipboardAction>,
}

#[derive(Debug, Clone, Copy)]
enum ActiveInputKind {
    Direct {
        target: MotionEditorTarget,
        last_position: MotionGraphPoint,
        snapping: bool,
    },
    Viewport,
}

#[derive(Debug, Clone, Copy)]
struct ActiveInput {
    id: MotionEditorEditId,
    device: MotionEditorDevice,
    kind: ActiveInputKind,
}

/// One owner for editor gesture identity. Input is already targeted to the
/// graph; this controller never registers compositor-global touch gestures.
#[derive(Debug, Default)]
pub struct MotionEditorInputController {
    active: Option<ActiveInput>,
}

impl MotionEditorInputController {
    pub fn handle(
        &mut self,
        editor: &mut MotionCurveEditor,
        input: MotionEditorInput,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        let before_document = editor.document_generation();
        let mut outcome = match input {
            MotionEditorInput::Direct(input) => self.handle_direct(editor, input)?,
            MotionEditorInput::Viewport(input) => self.handle_viewport(editor, input)?,
            MotionEditorInput::Key {
                key,
                modifiers,
                repeat,
                now,
            } => self.handle_key(editor, key, modifiers, repeat, now)?,
            MotionEditorInput::PasteText(text) => {
                if self.active.is_some() {
                    return Err(MotionEditorInputError::InteractionBusy);
                }
                let clipboard = MotionAnchorClipboardData::from_json(&text)?;
                let changed = editor.paste_at(&clipboard, editor.playhead(), 0.0)?;
                MotionEditorInputOutcome {
                    document_changed: changed,
                    transient_changed: changed,
                    ..MotionEditorInputOutcome::default()
                }
            }
        };
        outcome.document_changed |= editor.document_generation() != before_document;
        outcome.preview_pending = editor.preview_pending();
        Ok(outcome)
    }

    pub fn cancel_active(
        &mut self,
        editor: &mut MotionCurveEditor,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        let Some(active) = self.active.take() else {
            return Ok(MotionEditorInputOutcome::default());
        };
        let outcome = editor.cancel_transaction(active.id)?;
        Ok(MotionEditorInputOutcome {
            document_changed: outcome.document_changed,
            transient_changed: outcome.transient_changed,
            preview_pending: editor.preview_pending(),
            clipboard: None,
        })
    }

    fn handle_direct(
        &mut self,
        editor: &mut MotionCurveEditor,
        input: MotionEditorDirectInput,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        input.device.validate()?;
        if !input.position.time.is_finite() || !input.position.progress.is_finite() {
            return Err(MotionEditorInputError::InvalidCoordinate);
        }
        match input.phase {
            MotionEditorGesturePhase::Begin => self.begin_direct(editor, input),
            MotionEditorGesturePhase::Update => self.update_direct(editor, input),
            MotionEditorGesturePhase::End => self.end_direct(editor, input),
            MotionEditorGesturePhase::Cancel => self.cancel_for_id(editor, input.id),
        }
    }

    fn begin_direct(
        &mut self,
        editor: &mut MotionCurveEditor,
        input: MotionEditorDirectInput,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        if self.active.is_some() {
            return Err(MotionEditorInputError::InteractionBusy);
        }
        if !input.device.supports_direct() {
            return Err(MotionEditorInputError::DeviceCannotDirectEdit);
        }
        editor.begin_transaction(input.id)?;
        let result = self.begin_direct_started(editor, input);
        if result.is_err() {
            self.active = None;
            let _ = editor.cancel_transaction(input.id);
        }
        result
    }

    fn begin_direct_started(
        &mut self,
        editor: &mut MotionCurveEditor,
        input: MotionEditorDirectInput,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        let mut transient_changed = false;
        if let MotionEditorTarget::Anchor(index)
        | MotionEditorTarget::IncomingHandle(index)
        | MotionEditorTarget::OutgoingHandle(index) = input.target
        {
            transient_changed |=
                editor.select_anchor(index, input.modifiers.shift, input.modifiers.control)?;
        }
        if input.device.is_eraser() {
            if !matches!(input.target, MotionEditorTarget::Anchor(_)) {
                return Err(MotionEditorInputError::EraserRequiresAnchor);
            }
            let changed = editor.delete_selection()?;
            let committed = editor.commit_transaction(input.id)?;
            return Ok(MotionEditorInputOutcome {
                document_changed: changed || committed.document_changed,
                transient_changed: true,
                ..MotionEditorInputOutcome::default()
            });
        }
        if input.target == MotionEditorTarget::Curve && input.activation_count >= 2 {
            let _ = editor.insert_exact(input.position.time)?;
            let committed = editor.commit_transaction(input.id)?;
            return Ok(MotionEditorInputOutcome {
                document_changed: committed.document_changed,
                transient_changed: true,
                ..MotionEditorInputOutcome::default()
            });
        }
        match input.target {
            MotionEditorTarget::Playhead | MotionEditorTarget::Curve => {
                transient_changed |= editor.scrub_playhead(input.position.time)?;
            }
            MotionEditorTarget::Graph
            | MotionEditorTarget::Anchor(_)
            | MotionEditorTarget::IncomingHandle(_)
            | MotionEditorTarget::OutgoingHandle(_) => {}
        }
        self.active = Some(ActiveInput {
            id: input.id,
            device: input.device,
            kind: ActiveInputKind::Direct {
                target: input.target,
                last_position: input.position,
                snapping: input.snapping,
            },
        });
        Ok(MotionEditorInputOutcome {
            transient_changed,
            ..MotionEditorInputOutcome::default()
        })
    }

    fn update_direct(
        &mut self,
        editor: &mut MotionCurveEditor,
        input: MotionEditorDirectInput,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        let active = self.active.ok_or(MotionEditorInputError::NoInteraction)?;
        if active.id != input.id || active.device != input.device {
            return Err(MotionEditorInputError::WrongInteraction);
        }
        let ActiveInputKind::Direct {
            target,
            last_position,
            snapping,
        } = active.kind
        else {
            return Err(MotionEditorInputError::WrongInteraction);
        };
        let changed = match target {
            MotionEditorTarget::Anchor(index) => {
                editor.move_selected_to(index, input.position, snapping)?
            }
            MotionEditorTarget::IncomingHandle(index) => editor.set_handle_numeric(
                index,
                MotionEditorTangentSide::Incoming,
                input.position,
            )?,
            MotionEditorTarget::OutgoingHandle(index) => editor.set_handle_numeric(
                index,
                MotionEditorTangentSide::Outgoing,
                input.position,
            )?,
            MotionEditorTarget::Playhead | MotionEditorTarget::Curve => {
                editor.scrub_playhead(input.position.time)?
            }
            MotionEditorTarget::Graph => editor.pan_viewport(MotionGraphPoint::new(
                last_position.time - input.position.time,
                last_position.progress - input.position.progress,
            ))?,
        };
        self.active = Some(ActiveInput {
            kind: ActiveInputKind::Direct {
                target,
                last_position: input.position,
                snapping,
            },
            ..active
        });
        Ok(MotionEditorInputOutcome {
            transient_changed: changed,
            ..MotionEditorInputOutcome::default()
        })
    }

    fn end_direct(
        &mut self,
        editor: &mut MotionCurveEditor,
        input: MotionEditorDirectInput,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        if let Err(error) = self.update_direct(editor, input) {
            let _ = self.cancel_for_id(editor, input.id);
            return Err(error);
        }
        self.active = None;
        let committed = editor.commit_transaction(input.id)?;
        Ok(MotionEditorInputOutcome {
            document_changed: committed.document_changed,
            transient_changed: committed.transient_changed,
            ..MotionEditorInputOutcome::default()
        })
    }

    fn handle_viewport(
        &mut self,
        editor: &mut MotionCurveEditor,
        input: MotionEditorViewportInput,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        input.device.validate()?;
        if !input.anchor.time.is_finite()
            || !input.anchor.progress.is_finite()
            || !input.translation.time.is_finite()
            || !input.translation.progress.is_finite()
            || !input.time_scale.is_finite()
            || input.time_scale <= 0.0
            || !input.progress_scale.is_finite()
            || input.progress_scale <= 0.0
        {
            return Err(MotionEditorInputError::InvalidViewport);
        }
        match input.phase {
            MotionEditorGesturePhase::Begin => {
                if self.active.is_some() {
                    return Err(MotionEditorInputError::InteractionBusy);
                }
                if !input.device.supports_viewport() {
                    return Err(MotionEditorInputError::DeviceCannotControlViewport);
                }
                editor.begin_transaction(input.id)?;
                self.active = Some(ActiveInput {
                    id: input.id,
                    device: input.device,
                    kind: ActiveInputKind::Viewport,
                });
                Ok(MotionEditorInputOutcome::default())
            }
            MotionEditorGesturePhase::Update => {
                let changed = self.update_viewport(editor, input)?;
                Ok(MotionEditorInputOutcome {
                    transient_changed: changed,
                    ..MotionEditorInputOutcome::default()
                })
            }
            MotionEditorGesturePhase::End => {
                let changed = self.update_viewport(editor, input)?;
                self.active = None;
                let committed = editor.commit_transaction(input.id)?;
                Ok(MotionEditorInputOutcome {
                    transient_changed: changed || committed.transient_changed,
                    ..MotionEditorInputOutcome::default()
                })
            }
            MotionEditorGesturePhase::Cancel => self.cancel_for_id(editor, input.id),
        }
    }

    fn handle_key(
        &mut self,
        editor: &mut MotionCurveEditor,
        key: MotionEditorKey,
        modifiers: MotionEditorModifiers,
        _repeat: bool,
        now: Duration,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        if self.active.is_some() && key != MotionEditorKey::Escape {
            return Err(MotionEditorInputError::InteractionBusy);
        }
        if key == MotionEditorKey::Escape {
            if self.active.is_some() {
                return self.cancel_active(editor);
            }
            return Ok(MotionEditorInputOutcome {
                transient_changed: editor.clear_selection(),
                ..MotionEditorInputOutcome::default()
            });
        }

        let character = match key {
            MotionEditorKey::Character(value) => Some(value.to_ascii_lowercase()),
            _ => None,
        };
        if modifiers.control || modifiers.logo {
            let mut outcome = MotionEditorInputOutcome::default();
            match character {
                Some('a') => outcome.transient_changed = editor.select_all_editable(),
                Some('c') => {
                    outcome.clipboard = Some(MotionEditorClipboardAction::WriteText(
                        editor.copy_selection()?.to_json()?,
                    ));
                }
                Some('v') => outcome.clipboard = Some(MotionEditorClipboardAction::ReadText),
                Some('z') if modifiers.shift => outcome.document_changed = editor.redo()?,
                Some('z') => outcome.document_changed = editor.undo()?,
                Some('y') => outcome.document_changed = editor.redo()?,
                _ => return Ok(outcome),
            }
            return Ok(outcome);
        }

        let mut outcome = MotionEditorInputOutcome::default();
        match key {
            MotionEditorKey::Delete | MotionEditorKey::Backspace => {
                outcome.document_changed = editor.delete_selection()?;
                outcome.transient_changed = outcome.document_changed;
            }
            MotionEditorKey::Space => outcome.transient_changed = editor.toggle_playback(now),
            MotionEditorKey::Home => outcome.transient_changed = editor.scrub_playhead(0.0)?,
            MotionEditorKey::End => outcome.transient_changed = editor.scrub_playhead(1.0)?,
            MotionEditorKey::Tab => {
                outcome.transient_changed = cycle_selection(editor, modifiers.shift)?
            }
            MotionEditorKey::ArrowLeft | MotionEditorKey::Character('h' | 'H') => {
                outcome.document_changed = nudge(editor, -1.0, 0.0, modifiers.shift)?;
            }
            MotionEditorKey::ArrowRight | MotionEditorKey::Character('l' | 'L') => {
                outcome.document_changed = nudge(editor, 1.0, 0.0, modifiers.shift)?;
            }
            MotionEditorKey::ArrowUp | MotionEditorKey::Character('k' | 'K') => {
                outcome.document_changed = nudge(editor, 0.0, 1.0, modifiers.shift)?;
            }
            MotionEditorKey::ArrowDown | MotionEditorKey::Character('j' | 'J') => {
                outcome.document_changed = nudge(editor, 0.0, -1.0, modifiers.shift)?;
            }
            MotionEditorKey::Enter | MotionEditorKey::Character(_) | MotionEditorKey::Escape => {}
        }
        outcome.transient_changed |= outcome.document_changed;
        Ok(outcome)
    }

    fn update_viewport(
        &self,
        editor: &mut MotionCurveEditor,
        input: MotionEditorViewportInput,
    ) -> Result<bool, MotionEditorInputError> {
        let active = self.active.ok_or(MotionEditorInputError::NoInteraction)?;
        if active.id != input.id
            || active.device != input.device
            || !matches!(active.kind, ActiveInputKind::Viewport)
        {
            return Err(MotionEditorInputError::WrongInteraction);
        }
        let panned = editor.pan_viewport(input.translation)?;
        let zoomed = editor.zoom_viewport(input.anchor, input.time_scale, input.progress_scale)?;
        Ok(panned || zoomed)
    }

    fn cancel_for_id(
        &mut self,
        editor: &mut MotionCurveEditor,
        id: MotionEditorEditId,
    ) -> Result<MotionEditorInputOutcome, MotionEditorInputError> {
        let active = self.active.ok_or(MotionEditorInputError::NoInteraction)?;
        if active.id != id {
            return Err(MotionEditorInputError::WrongInteraction);
        }
        self.active = None;
        let cancelled = editor.cancel_transaction(id)?;
        Ok(MotionEditorInputOutcome {
            document_changed: cancelled.document_changed,
            transient_changed: cancelled.transient_changed,
            ..MotionEditorInputOutcome::default()
        })
    }
}

fn nudge(
    editor: &mut MotionCurveEditor,
    time_direction: f64,
    progress_direction: f64,
    coarse: bool,
) -> Result<bool, MotionEditorInputError> {
    let primary = editor
        .primary_selection()
        .ok_or(MotionEditorInputError::EmptySelection)?;
    let point = editor.anchor_point(primary)?;
    let config = editor.config();
    let multiplier = if coarse {
        config.coarse_multiplier
    } else {
        1.0
    };
    editor
        .move_selected_to(
            primary,
            MotionGraphPoint::new(
                point.time + time_direction * config.keyboard_time_step * multiplier,
                point.progress + progress_direction * config.keyboard_progress_step * multiplier,
            ),
            false,
        )
        .map_err(Into::into)
}

fn cycle_selection(
    editor: &mut MotionCurveEditor,
    backwards: bool,
) -> Result<bool, MotionEditorInputError> {
    let count = editor.effective_curve().anchors.len();
    let current = editor.primary_selection();
    let next = match (current, backwards) {
        (Some(0), true) | (None, true) => count - 1,
        (Some(index), true) => index - 1,
        (Some(index), false) if index + 1 < count => index + 1,
        _ => 0,
    };
    editor.select_anchor(next, false, false).map_err(Into::into)
}

#[derive(Debug)]
pub enum MotionEditorInputError {
    Editor(MotionCurveEditorError),
    InvalidDevice,
    UnsupportedContactCount,
    DeviceCannotDirectEdit,
    DeviceCannotControlViewport,
    EraserRequiresAnchor,
    InvalidCoordinate,
    InvalidViewport,
    InteractionBusy,
    WrongInteraction,
    NoInteraction,
    EmptySelection,
}

impl fmt::Display for MotionEditorInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Editor(error) => error.fmt(formatter),
            Self::InvalidDevice => formatter.write_str("invalid motion editor input device data"),
            Self::UnsupportedContactCount => {
                formatter.write_str("motion editor accepts only one or two targeted contacts")
            }
            Self::DeviceCannotDirectEdit => {
                formatter.write_str("input device cannot directly edit the graph")
            }
            Self::DeviceCannotControlViewport => {
                formatter.write_str("input device cannot control the graph viewport")
            }
            Self::EraserRequiresAnchor => formatter.write_str("pen eraser requires an anchor"),
            Self::InvalidCoordinate => formatter.write_str("invalid editor input coordinate"),
            Self::InvalidViewport => formatter.write_str("invalid editor viewport input"),
            Self::InteractionBusy => formatter.write_str("another editor input is active"),
            Self::WrongInteraction => formatter.write_str("editor input identity mismatch"),
            Self::NoInteraction => formatter.write_str("no editor input is active"),
            Self::EmptySelection => formatter.write_str("no anchor is selected"),
        }
    }
}

impl std::error::Error for MotionEditorInputError {}

impl From<MotionCurveEditorError> for MotionEditorInputError {
    fn from(value: MotionCurveEditorError) -> Self {
        match value {
            MotionCurveEditorError::InteractionBusy => Self::InteractionBusy,
            MotionCurveEditorError::WrongInteraction => Self::WrongInteraction,
            MotionCurveEditorError::NoInteraction => Self::NoInteraction,
            other => Self::Editor(other),
        }
    }
}

#[cfg(test)]
mod tests {
    // State assertions intentionally verify several successful outcomes after
    // the controller has consumed them.
    #![allow(unused_must_use)]

    use super::*;
    use crate::{
        CompiledMotionCurve, MotionCurveConsumer, MotionCurveConsumerDomain,
        MotionCurveConsumerSet, MotionCurveEditorConfig, MotionCurveSource, MotionEditorTimeAxis,
        split_motion_curve,
    };
    use nkdhr_theme::MotionCurveData;

    fn editor(curve: MotionCurveData, explicit: bool) -> MotionCurveEditor {
        let consumers = MotionCurveConsumerSet::new(vec![
            MotionCurveConsumer::new("test", MotionCurveConsumerDomain::Spatial).unwrap(),
        ])
        .unwrap();
        MotionCurveEditor::new(
            curve.clone(),
            explicit.then_some(curve),
            Duration::from_millis(500),
            None,
            consumers,
            MotionCurveEditorConfig::default(),
        )
        .unwrap()
    }

    fn three_anchor_editor() -> MotionCurveEditor {
        editor(
            split_motion_curve(&MotionCurveData::linear(), 0.5).unwrap(),
            true,
        )
    }

    fn direct(
        id: u64,
        phase: MotionEditorGesturePhase,
        device: MotionEditorDevice,
        target: MotionEditorTarget,
        position: MotionGraphPoint,
    ) -> MotionEditorInput {
        MotionEditorInput::Direct(MotionEditorDirectInput {
            id: MotionEditorEditId(id),
            phase,
            device,
            target,
            position,
            modifiers: MotionEditorModifiers::default(),
            activation_count: 1,
            snapping: false,
        })
    }

    fn key(key: MotionEditorKey, modifiers: MotionEditorModifiers) -> MotionEditorInput {
        MotionEditorInput::Key {
            key,
            modifiers,
            repeat: false,
            now: Duration::from_secs(10),
        }
    }

    fn assert_same_shape(left: &CompiledMotionCurve, right: &CompiledMotionCurve) {
        for step in 0..=1_000 {
            let time = f64::from(step) / 1_000.0;
            assert!((left.sample(time) - right.sample(time)).abs() < 2.0e-9);
        }
    }

    #[test]
    fn double_activation_inserts_exactly_and_does_not_leave_an_active_gesture() {
        let mut editor = editor(MotionCurveData::linear(), false);
        let before = editor.compiled().clone();
        let mut controller = MotionEditorInputController::default();
        let mut event = match direct(
            1,
            MotionEditorGesturePhase::Begin,
            MotionEditorDevice::Mouse,
            MotionEditorTarget::Curve,
            MotionGraphPoint::new(0.417, 0.3),
        ) {
            MotionEditorInput::Direct(event) => event,
            _ => unreachable!(),
        };
        event.activation_count = 2;
        let outcome = controller
            .handle(&mut editor, MotionEditorInput::Direct(event))
            .unwrap();
        assert!(outcome.document_changed);
        assert_eq!(editor.curve_source(), MotionCurveSource::Explicit);
        assert_eq!(editor.effective_curve().anchors.len(), 3);
        assert_same_shape(&before, editor.compiled());
        assert!(controller.active.is_none());
    }

    #[test]
    fn failed_begin_rolls_back_ownership_and_a_later_gesture_can_start() {
        let mut editor = three_anchor_editor();
        let before = editor.snapshot();
        let mut controller = MotionEditorInputController::default();
        assert!(
            controller
                .handle(
                    &mut editor,
                    direct(
                        2,
                        MotionEditorGesturePhase::Begin,
                        MotionEditorDevice::Mouse,
                        MotionEditorTarget::Anchor(99),
                        MotionGraphPoint::new(0.5, 0.5),
                    ),
                )
                .is_err()
        );
        assert!(controller.active.is_none());
        assert_eq!(editor.effective_curve(), &before.curve);
        controller
            .handle(
                &mut editor,
                direct(
                    3,
                    MotionEditorGesturePhase::Begin,
                    MotionEditorDevice::Mouse,
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.5, 0.5),
                ),
            )
            .unwrap();
        controller.cancel_active(&mut editor).unwrap();
    }

    #[test]
    fn drag_is_one_undo_step_wrong_identity_is_rejected_and_cancel_is_exact() {
        let mut editor = three_anchor_editor();
        let original = editor.effective_curve().clone();
        let mut controller = MotionEditorInputController::default();
        controller
            .handle(
                &mut editor,
                direct(
                    4,
                    MotionEditorGesturePhase::Begin,
                    MotionEditorDevice::Mouse,
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.5, 0.5),
                ),
            )
            .unwrap();
        assert!(matches!(
            controller.handle(
                &mut editor,
                direct(
                    5,
                    MotionEditorGesturePhase::Update,
                    MotionEditorDevice::Mouse,
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.55, 0.55),
                ),
            ),
            Err(MotionEditorInputError::WrongInteraction)
        ));
        for point in [
            MotionGraphPoint::new(0.54, 0.54),
            MotionGraphPoint::new(0.58, 0.58),
        ] {
            controller
                .handle(
                    &mut editor,
                    direct(
                        4,
                        MotionEditorGesturePhase::Update,
                        MotionEditorDevice::Mouse,
                        MotionEditorTarget::Anchor(1),
                        point,
                    ),
                )
                .unwrap();
        }
        controller
            .handle(
                &mut editor,
                direct(
                    4,
                    MotionEditorGesturePhase::End,
                    MotionEditorDevice::Mouse,
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.6, 0.6),
                ),
            )
            .unwrap();
        assert_ne!(editor.effective_curve(), &original);
        assert!(editor.undo().unwrap());
        assert_eq!(editor.effective_curve(), &original);
        assert!(!editor.undo().unwrap());

        controller
            .handle(
                &mut editor,
                direct(
                    6,
                    MotionEditorGesturePhase::Begin,
                    MotionEditorDevice::Mouse,
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.5, 0.5),
                ),
            )
            .unwrap();
        controller
            .handle(
                &mut editor,
                direct(
                    6,
                    MotionEditorGesturePhase::Update,
                    MotionEditorDevice::Mouse,
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.58, 0.4),
                ),
            )
            .unwrap();
        controller
            .handle(
                &mut editor,
                direct(
                    6,
                    MotionEditorGesturePhase::Cancel,
                    MotionEditorDevice::Mouse,
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.58, 0.4),
                ),
            )
            .unwrap();
        assert_eq!(editor.effective_curve(), &original);
    }

    #[test]
    fn targeted_touch_pen_and_touchpad_capabilities_are_kept_separate() {
        let mut editor = three_anchor_editor();
        let mut controller = MotionEditorInputController::default();
        controller
            .handle(
                &mut editor,
                direct(
                    7,
                    MotionEditorGesturePhase::Begin,
                    MotionEditorDevice::Touch { contacts: 1 },
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.5, 0.5),
                ),
            )
            .unwrap();
        controller.cancel_active(&mut editor).unwrap();
        assert!(matches!(
            controller.handle(
                &mut editor,
                direct(
                    8,
                    MotionEditorGesturePhase::Begin,
                    MotionEditorDevice::Touch { contacts: 2 },
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.5, 0.5),
                ),
            ),
            Err(MotionEditorInputError::DeviceCannotDirectEdit)
        ));
        assert!(matches!(
            controller.handle(
                &mut editor,
                direct(
                    9,
                    MotionEditorGesturePhase::Begin,
                    MotionEditorDevice::PrecisionTouchpad { contacts: 1 },
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.5, 0.5),
                ),
            ),
            Err(MotionEditorInputError::DeviceCannotDirectEdit)
        ));
        assert!(matches!(
            controller.handle(
                &mut editor,
                direct(
                    10,
                    MotionEditorGesturePhase::Begin,
                    MotionEditorDevice::Touch { contacts: 3 },
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.5, 0.5),
                ),
            ),
            Err(MotionEditorInputError::UnsupportedContactCount)
        ));

        let viewport = |phase| {
            MotionEditorInput::Viewport(MotionEditorViewportInput {
                id: MotionEditorEditId(11),
                phase,
                device: MotionEditorDevice::PrecisionTouchpad { contacts: 2 },
                anchor: MotionGraphPoint::new(0.5, 0.5),
                translation: MotionGraphPoint::default(),
                time_scale: 2.0,
                progress_scale: 2.0,
            })
        };
        controller
            .handle(&mut editor, viewport(MotionEditorGesturePhase::Begin))
            .unwrap();
        controller
            .handle(&mut editor, viewport(MotionEditorGesturePhase::Update))
            .unwrap();
        controller
            .handle(&mut editor, viewport(MotionEditorGesturePhase::End))
            .unwrap();
        assert!(
            (editor.viewport().time_end() - editor.viewport().time_start() - 0.25).abs() < 1e-12
        );
    }

    #[test]
    fn pen_eraser_deletes_only_an_anchor_and_failure_does_not_stick() {
        let mut editor = three_anchor_editor();
        let mut controller = MotionEditorInputController::default();
        let eraser = MotionEditorDevice::Pen {
            pressure: 0.5,
            barrel: false,
            eraser: true,
        };
        assert!(matches!(
            controller.handle(
                &mut editor,
                direct(
                    12,
                    MotionEditorGesturePhase::Begin,
                    eraser,
                    MotionEditorTarget::Graph,
                    MotionGraphPoint::new(0.5, 0.5),
                ),
            ),
            Err(MotionEditorInputError::EraserRequiresAnchor)
        ));
        assert!(controller.active.is_none());
        let outcome = controller
            .handle(
                &mut editor,
                direct(
                    13,
                    MotionEditorGesturePhase::Begin,
                    eraser,
                    MotionEditorTarget::Anchor(1),
                    MotionGraphPoint::new(0.5, 0.5),
                ),
            )
            .unwrap();
        assert!(outcome.document_changed);
        assert_eq!(editor.effective_curve().anchors.len(), 2);
    }

    #[test]
    fn keyboard_uses_standard_vim_directions_and_clipboard_protocol() {
        let mut editor = three_anchor_editor();
        let mut controller = MotionEditorInputController::default();
        editor.select_anchor(1, false, false).unwrap();
        let before = editor.anchor_point(1).unwrap();
        controller
            .handle(
                &mut editor,
                key(
                    MotionEditorKey::Character('h'),
                    MotionEditorModifiers::default(),
                ),
            )
            .unwrap();
        let after_h = editor.anchor_point(1).unwrap();
        assert!(after_h.time < before.time);
        controller
            .handle(
                &mut editor,
                key(
                    MotionEditorKey::Character('j'),
                    MotionEditorModifiers::default(),
                ),
            )
            .unwrap();
        assert!(editor.anchor_point(1).unwrap().progress < after_h.progress);
        let before_coarse = editor.anchor_point(1).unwrap().progress;
        controller
            .handle(
                &mut editor,
                key(
                    MotionEditorKey::ArrowUp,
                    MotionEditorModifiers {
                        shift: true,
                        ..MotionEditorModifiers::default()
                    },
                ),
            )
            .unwrap();
        assert!((editor.anchor_point(1).unwrap().progress - before_coarse - 0.1).abs() < 1e-12);

        let copy = controller
            .handle(
                &mut editor,
                key(
                    MotionEditorKey::Character('c'),
                    MotionEditorModifiers {
                        control: true,
                        ..MotionEditorModifiers::default()
                    },
                ),
            )
            .unwrap();
        let MotionEditorClipboardAction::WriteText(text) = copy.clipboard.unwrap() else {
            panic!("copy must request a text write")
        };
        let paste_request = controller
            .handle(
                &mut editor,
                key(
                    MotionEditorKey::Character('v'),
                    MotionEditorModifiers {
                        control: true,
                        ..MotionEditorModifiers::default()
                    },
                ),
            )
            .unwrap();
        assert_eq!(
            paste_request.clipboard,
            Some(MotionEditorClipboardAction::ReadText)
        );
        editor.scrub_playhead(0.75).unwrap();
        assert!(
            controller
                .handle(&mut editor, MotionEditorInput::PasteText(text))
                .unwrap()
                .document_changed
        );
        assert_eq!(editor.effective_curve().anchors.len(), 4);

        let selected = editor.primary_selection().unwrap();
        controller
            .handle(
                &mut editor,
                key(MotionEditorKey::Tab, MotionEditorModifiers::default()),
            )
            .unwrap();
        assert_ne!(editor.primary_selection(), Some(selected));
        controller
            .handle(
                &mut editor,
                key(
                    MotionEditorKey::Character('z'),
                    MotionEditorModifiers {
                        control: true,
                        ..MotionEditorModifiers::default()
                    },
                ),
            )
            .unwrap();
        assert_eq!(editor.effective_curve().anchors.len(), 3);
    }

    #[test]
    fn playhead_and_axis_keys_are_transient_and_preview_coalesced() {
        let mut editor = three_anchor_editor();
        let _ = editor.take_preview();
        let mut controller = MotionEditorInputController::default();
        let end = controller
            .handle(
                &mut editor,
                key(MotionEditorKey::End, MotionEditorModifiers::default()),
            )
            .unwrap();
        assert!(end.transient_changed);
        assert!(end.preview_pending);
        controller
            .handle(
                &mut editor,
                key(MotionEditorKey::Home, MotionEditorModifiers::default()),
            )
            .unwrap();
        controller
            .handle(
                &mut editor,
                key(MotionEditorKey::Space, MotionEditorModifiers::default()),
            )
            .unwrap();
        assert!(editor.take_preview().is_some());
        assert!(editor.take_preview().is_none());
        assert!(editor.set_time_axis(MotionEditorTimeAxis::RealTime));
    }
}
