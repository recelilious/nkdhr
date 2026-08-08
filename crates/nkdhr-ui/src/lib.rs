//! Shared retained UI toolkit for nkdhr.

mod animation;
mod input;
mod layout;
mod reactive;
mod semantics;
pub mod text;
mod tree;

pub use animation::{Clock, ManualClock, SystemClock, Timeline, lerp};
pub use input::{Key, Modifiers, PointerButton, UiEvent};
pub use layout::{
    Align, Alignment, Axis, Clip, Constraints, CrossAxisAlignment, Flex, Insets, MainAxisAlignment,
    Padding, Size, Stack,
};
pub use reactive::Reactive;
pub(crate) use reactive::RootReactivity;
pub use semantics::{SemanticNode, SemanticRole, Semantics};
pub use tree::{
    ArrangeCtx, DispatchResult, Element, EventCtx, Invalidation, MeasureCtx, PaintCtx,
    SemanticsCtx, UiError, UiResult, UiRoot, UpdateCtx, Widget, WidgetId, WidgetKey,
};
