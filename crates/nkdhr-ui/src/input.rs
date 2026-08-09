//! Backend-neutral UI input values.

use nkdhr_render::Point;

use crate::tree::WidgetId;

/// Pointer button identity without a libinput or Wayland dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
    Other(u16),
}

/// Keyboard modifiers normalized by the host adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub logo: bool,
}

/// Lifecycle of a continuous touch or precision-scroll gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrollPhase {
    Begin,
    Update,
    End,
    Cancel,
}

/// Logical key identity used by the toolkit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
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
    PageUp,
    PageDown,
    Backspace,
    Delete,
    Character(String),
    Named(String),
}

/// Host clipboard work emitted by one input dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardRequest {
    /// Read plain text and return it to the originating widget with
    /// `UiEvent::ClipboardText`.
    ReadText {
        target: WidgetId,
    },
    WriteText(String),
}

/// One input transaction delivered at a frame boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    PointerMoved {
        position: Point,
    },
    PointerDown {
        position: Point,
        button: PointerButton,
        modifiers: Modifiers,
        /// Host-normalized click sequence count, beginning at one.
        click_count: u8,
    },
    PointerUp {
        position: Point,
        button: PointerButton,
        modifiers: Modifiers,
        /// Same click sequence count as the corresponding press.
        click_count: u8,
    },
    PointerScroll {
        position: Point,
        delta_x: f32,
        delta_y: f32,
        modifiers: Modifiers,
    },
    ScrollGesture {
        position: Point,
        delta_x: f32,
        delta_y: f32,
        phase: ScrollPhase,
        modifiers: Modifiers,
    },
    PointerCancel,
    PointerLeft,
    KeyDown {
        key: Key,
        modifiers: Modifiers,
        repeat: bool,
    },
    KeyUp {
        key: Key,
        modifiers: Modifiers,
    },
    TextInput(String),
    ImePreedit {
        text: String,
        selection: Option<(usize, usize)>,
    },
    ImeCommit(String),
    /// Plain text returned by the host for an earlier clipboard read.
    ClipboardText {
        target: WidgetId,
        text: String,
    },
    FocusChanged(bool),
    HoverChanged(bool),
}

impl UiEvent {
    pub fn pointer_position(&self) -> Option<Point> {
        match self {
            Self::PointerMoved { position }
            | Self::PointerDown { position, .. }
            | Self::PointerUp { position, .. }
            | Self::PointerScroll { position, .. }
            | Self::ScrollGesture { position, .. } => Some(*position),
            _ => None,
        }
    }

    pub fn is_pointer(&self) -> bool {
        matches!(
            self,
            Self::PointerMoved { .. }
                | Self::PointerDown { .. }
                | Self::PointerUp { .. }
                | Self::PointerScroll { .. }
                | Self::ScrollGesture { .. }
                | Self::PointerCancel
                | Self::PointerLeft
        )
    }

    pub fn is_keyboard(&self) -> bool {
        matches!(
            self,
            Self::KeyDown { .. }
                | Self::KeyUp { .. }
                | Self::TextInput(_)
                | Self::ImePreedit { .. }
                | Self::ImeCommit(_)
                | Self::ClipboardText { .. }
        )
    }
}
