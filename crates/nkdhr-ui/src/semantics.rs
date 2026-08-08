//! Accessibility-facing semantic tree values.

use nkdhr_render::Rect;

use crate::WidgetId;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SemanticRole {
    #[default]
    Generic,
    Group,
    Button,
    Toggle,
    Slider,
    List,
    ListItem,
    Text,
    TextInput,
    ScrollArea,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Semantics {
    pub role: SemanticRole,
    pub label: Option<String>,
    pub value: Option<String>,
    pub enabled: bool,
    pub focusable: bool,
}

impl Default for Semantics {
    fn default() -> Self {
        Self {
            role: SemanticRole::Generic,
            label: None,
            value: None,
            enabled: true,
            focusable: false,
        }
    }
}

/// Flattened semantic node in tree order.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticNode {
    pub id: WidgetId,
    pub parent: Option<WidgetId>,
    pub bounds: Rect,
    pub semantics: Semantics,
}
