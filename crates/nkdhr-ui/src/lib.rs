//! Shared retained UI toolkit for nkdhr.

mod action;
mod animation;
mod host;
mod input;
mod layout;
mod motion;
mod motion_curve;
mod motion_editor;
mod motion_runtime;
mod motion_style;
mod reactive;
mod semantics;
pub mod text;
mod theme;
mod theme_runtime;
mod tree;
mod widgets;

pub use action::{
    ActionArgument, ActionCatalog, ActionDescriptor, ActionDispatcher, ActionEnvironment,
    ActionFeedback, ActionFeedbackKind, ActionId, ActionInvocation, ActionKind, ActionPhase,
    ActionRegistry, ActionValue, ActionValueSchema, BindingAvailability, BindingContext,
    BindingDiagnostic, BindingDiagnosticCode, BindingDocument, BindingEntry, BindingPublication,
    BindingRuntime, BindingSeverity, BindingSnapshot, ButtonCode, CompiledBinding, CompiledTrigger,
    DeviceClass, DispatchError, GestureActivation, GestureDirection, GestureKind, GestureOrigin,
    InteractionId, KeyPhase, Modifier, ModifierSet, RuntimeTrigger, TerminalReason, Trigger,
    ValidatedActionInvocation, built_in_compositor_catalog, default_compositor_bindings,
};
pub use animation::{Clock, ManualClock, SystemClock, Timeline, lerp};
pub use host::{UiHost, UiHostFrame, UiSurface};
pub use input::{ClipboardRequest, Key, Modifiers, PointerButton, ScrollPhase, UiEvent};
pub use layout::{
    Align, Alignment, Axis, Clip, Constraints, CrossAxisAlignment, Flex, Insets, MainAxisAlignment,
    Padding, Size, Stack,
};
pub use motion::{
    CubicBezier, FluidTuning, FluidVariation, MotionDurations, MotionError, MotionFamily,
    MotionMode, MotionProfile, MotionSpec, ScalarMotion,
};
pub use motion_curve::{
    CompiledMotionCurve, MotionCurveAnalysis, MotionCurveCompileError,
    resolve_motion_curve_handles, split_motion_curve,
};
pub use motion_editor::{
    MotionAnchorClipboardData, MotionCurveConsumer, MotionCurveConsumerDomain,
    MotionCurveConsumerSet, MotionCurveEditor, MotionCurveEditorConfig, MotionCurveEditorError,
    MotionCurveEditorSnapshot, MotionCurveSource, MotionEditorAxis, MotionEditorClipboardAction,
    MotionEditorDevice, MotionEditorDirectInput, MotionEditorEditId, MotionEditorGesturePhase,
    MotionEditorInput, MotionEditorInputController, MotionEditorInputError,
    MotionEditorInputOutcome, MotionEditorKey, MotionEditorModifiers, MotionEditorPlayback,
    MotionEditorPreview, MotionEditorSnap, MotionEditorTangentSide, MotionEditorTarget,
    MotionEditorTimeAxis, MotionEditorTransactionOutcome, MotionEditorViewportInput,
    MotionGraphPoint, MotionGraphViewport, MotionTangentMode,
};
pub use motion_runtime::{
    FluidEnvelopeSample, FluidIdleSample, KineticAdvance, KineticMotion, KineticSample,
    MotionBeginOutcome, MotionExecutionSpec, MotionFeature, MotionPolicySource,
    MotionPropertyDomain, MotionRunId, MotionRuntimeError, MotionRuntimeProfile, MotionTerminal,
    MotionTerminalReason, ResolvedSemanticFluid, SelectionMassAdvance, SelectionMassEntry,
    SelectionMassMotion, SelectionMassSample, SemanticFluidParameters,
};
pub use motion_style::{CompiledMotionStyle, MotionStyleCompileError, ResolvedMotionStyle};
pub use nkdhr_theme::{
    BALANCED_MOTION_STYLE_REVISION, BuiltInMotionStyle, MAX_MOTION_CURVE_ABSOLUTE_PROGRESS,
    MAX_MOTION_CURVE_ANCHORS, MAX_MOTION_STYLE_NODES, MIN_MOTION_CURVE_ANCHORS,
    MIN_MOTION_CURVE_TIME_GAP, MOTION_CURVE_AUTO_ALGORITHM_VERSION, MOTION_CURVE_SCHEMA_VERSION,
    MOTION_PRESET_LIBRARY_SCHEMA_VERSION, MOTION_STYLE_SCHEMA_VERSION, MotionAnchorData,
    MotionComponentNodeData, MotionCurveData, MotionCurveDataError, MotionFamilyNodeData,
    MotionFluidOverridesData, MotionFluidProvenanceData, MotionPresetLibraryData,
    MotionPresetLibraryError, MotionScopeData, MotionScopeLevelData, MotionSemanticFamilyData,
    MotionStyleBaseData, MotionStyleError, MotionStylePresetData, MotionStyleProfileData,
    MotionStyleTreeData, MotionTangentsData, MotionValueOriginData, MotionValueProvenanceData,
    MotionValuesData, MotionVectorData, ResolvedMotionStyleData,
};
pub use reactive::Reactive;
pub(crate) use reactive::RootReactivity;
pub use semantics::{SemanticNode, SemanticRole, Semantics};
pub use theme::{
    Density, DensityMetrics, FontStacks, GlassMaterial, MaterialCapabilities, MaterialTier,
    Palette, Radii, ResolvedMaterial, ShadowToken, Spacing, TextRole, Theme, ThemeError, TypeToken,
    Typography,
};
pub use theme_runtime::{
    ThemePublication, ThemeReadSet, ThemeRuntime, ThemeRuntimeError, ThemeSnapshot, ThemeToken,
    tokens as theme_tokens,
};
pub use tree::{
    AnimationCtx, ArrangeCtx, DispatchResult, Element, EventCtx, Invalidation, MeasureCtx,
    PaintCtx, SemanticsCtx, UiError, UiResult, UiRoot, UpdateCtx, Widget, WidgetId, WidgetKey,
};
pub use widgets::{
    Button, ButtonVariant, GlassSurface, List, ListEntry, ListError, ListItem, ListItemBehavior,
    ListMultiSelection, ListReorder, ListSelection, ListTreeToggle, ListVirtualWindow,
    PasswordCopyPolicy, Scroll, ScrollAnchor, ScrollAxis, ScrollError, ScrollOffset, ScrollReveal,
    ScrollbarPolicy, Slider, SliderError, SurfaceState, Text, TextInput, TextInputEdit,
    TextInputEnterBehavior, TextInputError, TextInputSelection, TextInputStatus,
    TextInputTabBehavior, TextInputValidationOutcome, TextInputValidationRequest,
    TextInputValidationResult, TextInputValidationTrigger, Toggle,
};
