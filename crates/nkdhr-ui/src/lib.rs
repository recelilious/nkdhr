//! Shared retained UI toolkit for nkdhr.

mod animation;
mod input;
mod layout;
mod motion;
mod reactive;
mod semantics;
pub mod text;
mod theme;
mod theme_runtime;
mod tree;
mod widgets;

pub use animation::{Clock, ManualClock, SystemClock, Timeline, lerp};
pub use input::{ClipboardRequest, Key, Modifiers, PointerButton, ScrollPhase, UiEvent};
pub use layout::{
    Align, Alignment, Axis, Clip, Constraints, CrossAxisAlignment, Flex, Insets, MainAxisAlignment,
    Padding, Size, Stack,
};
pub use motion::{
    CubicBezier, FluidTuning, FluidVariation, MotionDurations, MotionError, MotionFamily,
    MotionMode, MotionProfile, MotionSpec, ScalarMotion,
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
