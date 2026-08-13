//! Shared retained UI toolkit for nkdhr.

mod action;
mod animation;
mod host;
mod input;
mod layout;
mod motion;
mod motion_curve;
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
    CompiledMotionCurve, MotionCurveAnalysis, MotionCurveCompileError, split_motion_curve,
};
pub use nkdhr_theme::{
    MAX_MOTION_CURVE_ABSOLUTE_PROGRESS, MAX_MOTION_CURVE_ANCHORS, MIN_MOTION_CURVE_ANCHORS,
    MIN_MOTION_CURVE_TIME_GAP, MOTION_CURVE_AUTO_ALGORITHM_VERSION, MOTION_CURVE_SCHEMA_VERSION,
    MotionAnchorData, MotionCurveData, MotionCurveDataError, MotionTangentsData, MotionVectorData,
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
