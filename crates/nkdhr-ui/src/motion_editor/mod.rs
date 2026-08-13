//! Style-neutral UI-7D motion-curve editor state and input semantics.

mod input;
mod model;

pub use input::{
    MotionEditorClipboardAction, MotionEditorDevice, MotionEditorDirectInput,
    MotionEditorGesturePhase, MotionEditorInput, MotionEditorInputController,
    MotionEditorInputError, MotionEditorInputOutcome, MotionEditorKey, MotionEditorModifiers,
    MotionEditorTarget, MotionEditorViewportInput,
};
pub use model::{
    MotionAnchorClipboardData, MotionCurveConsumer, MotionCurveConsumerDomain,
    MotionCurveConsumerSet, MotionCurveEditor, MotionCurveEditorConfig, MotionCurveEditorError,
    MotionCurveEditorSnapshot, MotionCurveSource, MotionEditorAxis, MotionEditorEditId,
    MotionEditorPlayback, MotionEditorPreview, MotionEditorSnap, MotionEditorTangentSide,
    MotionEditorTimeAxis, MotionEditorTransactionOutcome, MotionGraphPoint, MotionGraphViewport,
    MotionTangentMode,
};
