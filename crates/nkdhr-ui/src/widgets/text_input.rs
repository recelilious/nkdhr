use std::{any::Any, fmt, ops::Range, rc::Rc, sync::Arc};

use nkdhr_render::{CornerRadii, Rect};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::with_alpha;
use crate::{
    ArrangeCtx, Constraints, EventCtx, Invalidation, Key, MaterialCapabilities, MaterialTier,
    MeasureCtx, MotionFamily, PaintCtx, PointerButton, Reactive, ScalarMotion, SemanticRole,
    Semantics, SemanticsCtx, Size, Theme, UiError, UiEvent, UpdateCtx, Widget,
};

use super::surface::{SurfaceState, paint_surface};

type TextCallback = Rc<dyn Fn(&str)>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TextInputStatus {
    #[default]
    Idle,
    Pending,
    Valid,
    Invalid(String),
}

pub struct TextInput {
    label: String,
    value: Reactive<String>,
    status: Reactive<TextInputStatus>,
    theme: Arc<Theme>,
    capabilities: MaterialCapabilities,
    enabled: bool,
    read_only: bool,
    password: bool,
    multiline: bool,
    minimum_lines: usize,
    on_change: Option<TextCallback>,
    on_submit: Option<TextCallback>,
}

impl TextInput {
    pub fn new(label: impl Into<String>, value: Reactive<String>, theme: Arc<Theme>) -> Self {
        Self {
            label: label.into(),
            value,
            status: Reactive::new(TextInputStatus::Idle),
            theme,
            capabilities: MaterialCapabilities::default(),
            enabled: true,
            read_only: false,
            password: false,
            multiline: false,
            minimum_lines: 1,
            on_change: None,
            on_submit: None,
        }
    }

    pub fn status(mut self, status: Reactive<TextInputStatus>) -> Self {
        self.status = status;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn password(mut self, password: bool) -> Self {
        self.password = password;
        self
    }

    pub fn multiline(mut self, minimum_lines: usize) -> Result<Self, TextInputError> {
        if minimum_lines == 0 {
            return Err(TextInputError::InvalidLineCount);
        }
        self.multiline = true;
        self.minimum_lines = minimum_lines;
        Ok(self)
    }

    pub fn capabilities(mut self, capabilities: MaterialCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn on_change(mut self, callback: impl Fn(&str) + 'static) -> Self {
        self.on_change = Some(Rc::new(callback));
        self
    }

    pub fn on_submit(mut self, callback: impl Fn(&str) + 'static) -> Self {
        self.on_submit = Some(Rc::new(callback));
        self
    }

    fn editable(&self) -> bool {
        self.enabled && !self.read_only
    }

    fn publish(&self, value: String) {
        self.value.set(value.clone());
        if let Some(callback) = &self.on_change {
            callback(&value);
        }
    }
}

#[derive(Debug, Clone)]
struct TextInputState {
    focused: bool,
    focus_motion: ScalarMotion,
    anchor: usize,
    caret: usize,
    observed_value: String,
    preedit: Option<(String, Option<(usize, usize)>)>,
    pointer_pressed: bool,
}

impl Default for TextInputState {
    fn default() -> Self {
        Self {
            focused: false,
            focus_motion: ScalarMotion::settled(0.0),
            anchor: 0,
            caret: 0,
            observed_value: String::new(),
            preedit: None,
            pointer_pressed: false,
        }
    }
}

impl TextInputState {
    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    fn synchronize(&mut self, value: &str) {
        if self.observed_value == value {
            return;
        }
        self.anchor = floor_boundary(value, self.anchor.min(value.len()));
        self.caret = floor_boundary(value, self.caret.min(value.len()));
        if self.observed_value.is_empty() && self.anchor == 0 && self.caret == 0 {
            self.anchor = value.len();
            self.caret = value.len();
        }
        self.observed_value.clear();
        self.observed_value.push_str(value);
    }
}

impl Widget for TextInput {
    fn create_state(&self) -> Box<dyn Any> {
        Box::<TextInputState>::default()
    }

    fn update(&self, previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        if previous.multiline != self.multiline
            || previous.minimum_lines != self.minimum_lines
            || previous.theme.density != self.theme.density
        {
            ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
        } else {
            ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
        }
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        let horizontal = 12.0;
        let vertical = 8.0;
        let line_height = self
            .theme
            .typography
            .token(crate::TextRole::Body)
            .line_height;
        let minimum_height = if self.multiline {
            line_height * self.minimum_lines as f32 + vertical * 2.0
        } else {
            self.theme.density_metrics().control_height
        };
        let child = if ctx.child_count() == 1 {
            ctx.measure_child(
                0,
                constraints.deflate(crate::Insets::symmetric(horizontal, vertical))?,
            )?
        } else {
            Size::ZERO
        };
        Ok(constraints.constrain(Size::new(
            (child.width + horizontal * 2.0).max(120.0),
            (child.height + vertical * 2.0).max(minimum_height),
        )))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        if ctx.child_count() == 1 {
            let child = ctx.child_size(0)?;
            ctx.arrange_child(
                0,
                Rect::new(
                    rect.x + 12.0,
                    rect.y + (rect.height - child.height).max(0.0) * 0.5,
                    child.width.min((rect.width - 24.0).max(0.0)),
                    child.height.min((rect.height - 16.0).max(0.0)),
                ),
            )?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let value = ctx.watch(&self.value, Invalidation::PAINT | Invalidation::SEMANTICS);
        let status = ctx.watch(&self.status, Invalidation::PAINT | Invalidation::SEMANTICS);
        let now = ctx.now();
        let (focused, focus, active) = {
            let state = ctx.state_mut::<TextInputState>()?;
            state.synchronize(&value);
            (
                state.focused,
                state.focus_motion.value(now),
                state.focus_motion.is_active(now),
            )
        };
        if active {
            ctx.request_animation_frame();
        }
        let rect = ctx.rect();
        paint_surface(
            ctx.builder(),
            rect,
            CornerRadii::all(self.theme.radii.control),
            &self.theme,
            MaterialTier::CompactNode,
            self.capabilities,
            SurfaceState {
                focused,
                disabled: !self.enabled,
                ..SurfaceState::default()
            },
        )?;
        if focus > 0.0 && focused {
            let gather_width = (rect.width * 0.22).clamp(28.0, 72.0);
            ctx.builder().rounded_rect(
                Rect::new(rect.x, rect.bottom() - 2.0, gather_width, 2.0),
                CornerRadii::all(1.0),
                with_alpha(self.theme.palette.accent_secondary, 0.46 * focus),
            )?;
        }
        match status {
            TextInputStatus::Invalid(_) => ctx.builder().border(
                rect,
                CornerRadii::all(self.theme.radii.control),
                1.0,
                with_alpha(self.theme.palette.error, 0.88),
            )?,
            TextInputStatus::Valid => ctx.builder().border(
                rect,
                CornerRadii::all(self.theme.radii.control),
                1.0,
                with_alpha(self.theme.palette.success, 0.52),
            )?,
            TextInputStatus::Pending => ctx.builder().border(
                rect,
                CornerRadii::all(self.theme.radii.control),
                1.0,
                with_alpha(self.theme.palette.warning, 0.52),
            )?,
            TextInputStatus::Idle => {}
        }
        ctx.paint_children()
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        let mut value = self.value.get();
        {
            let state = ctx.state_mut::<TextInputState>()?;
            state.synchronize(&value);
        }
        match event {
            UiEvent::FocusChanged(focused) => {
                let now = ctx.now();
                let state = ctx.state_mut::<TextInputState>()?;
                state.focused = *focused;
                state.focus_motion.retarget(
                    now,
                    if *focused { 1.0 } else { 0.0 },
                    self.theme.motion.spec(MotionFamily::TextInputFocus),
                );
                if !focused {
                    state.preedit = None;
                }
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                ctx.request_animation_frame();
            }
            UiEvent::PointerDown {
                button: PointerButton::Primary,
                ..
            } if self.enabled => {
                let state = ctx.state_mut::<TextInputState>()?;
                state.pointer_pressed = true;
                // Exact glyph hit mapping belongs to the retained Text widget;
                // until it is connected, a shell click positions at the end.
                state.anchor = value.len();
                state.caret = value.len();
                ctx.request_focus();
                ctx.capture_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } => {
                if ctx.state_mut::<TextInputState>()?.pointer_pressed {
                    ctx.state_mut::<TextInputState>()?.pointer_pressed = false;
                    ctx.release_pointer();
                    ctx.set_handled();
                }
            }
            UiEvent::PointerCancel => {
                ctx.state_mut::<TextInputState>()?.pointer_pressed = false;
                ctx.release_pointer();
            }
            UiEvent::TextInput(text) if self.editable() => {
                let text = normalize_insert(text, self.multiline);
                replace_selection(ctx.state_mut::<TextInputState>()?, &mut value, &text);
                self.publish(value);
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::ImePreedit { text, selection } if self.editable() => {
                ctx.state_mut::<TextInputState>()?.preedit = Some((text.clone(), *selection));
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::ImeCommit(text) if self.editable() => {
                let text = normalize_insert(text, self.multiline);
                let state = ctx.state_mut::<TextInputState>()?;
                replace_selection(state, &mut value, &text);
                state.preedit = None;
                self.publish(value);
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::KeyDown {
                key,
                modifiers,
                repeat: _,
            } if self.enabled => {
                let editable = self.editable();
                let mut changed = false;
                let state = ctx.state_mut::<TextInputState>()?;
                match key {
                    Key::ArrowLeft => move_caret(state, &value, false, modifiers.shift),
                    Key::ArrowRight => move_caret(state, &value, true, modifiers.shift),
                    Key::Home => set_caret(state, 0, modifiers.shift),
                    Key::End => set_caret(state, value.len(), modifiers.shift),
                    Key::Backspace if editable => {
                        changed = delete_backward(state, &mut value);
                    }
                    Key::Delete if editable => {
                        changed = delete_forward(state, &mut value);
                    }
                    Key::Character(character)
                        if modifiers.control && character.eq_ignore_ascii_case("a") =>
                    {
                        state.anchor = 0;
                        state.caret = value.len();
                    }
                    Key::Enter if editable && self.multiline => {
                        replace_selection(state, &mut value, "\n");
                        changed = true;
                    }
                    Key::Enter if !self.multiline => {
                        if let Some(callback) = &self.on_submit {
                            callback(&value);
                        }
                    }
                    Key::Escape => state.preedit = None,
                    _ => return Ok(()),
                }
                if changed {
                    self.publish(value);
                }
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let value = ctx.watch(&self.value, Invalidation::SEMANTICS);
        let status = ctx.watch(&self.status, Invalidation::SEMANTICS);
        let semantic_value = if self.password {
            "•".repeat(value.graphemes(true).count())
        } else {
            value
        };
        let status = match status {
            TextInputStatus::Idle => None,
            TextInputStatus::Pending => Some("pending".to_owned()),
            TextInputStatus::Valid => Some("valid".to_owned()),
            TextInputStatus::Invalid(message) => Some(format!("invalid: {message}")),
        };
        Semantics {
            role: SemanticRole::TextInput,
            label: Some(self.label.clone()),
            value: Some(match status {
                Some(status) => format!("{semantic_value}; {status}"),
                None => semantic_value,
            }),
            enabled: self.enabled,
            focusable: self.enabled,
        }
    }

    fn focusable(&self) -> bool {
        self.enabled
    }

    fn accepts_pointer(&self) -> bool {
        self.enabled
    }
}

fn normalize_insert(text: &str, multiline: bool) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        text
    } else {
        text.chars()
            .map(|character| match character {
                '\r' | '\n' => ' ',
                other => other,
            })
            .collect()
    }
}

fn floor_boundary(value: &str, index: usize) -> usize {
    if index >= value.len() {
        return value.len();
    }
    value
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|boundary| *boundary <= index)
        .last()
        .unwrap_or(0)
}

fn previous_boundary(value: &str, index: usize) -> usize {
    value
        .grapheme_indices(true)
        .map(|(boundary, _)| boundary)
        .take_while(|boundary| *boundary < index)
        .last()
        .unwrap_or(0)
}

fn next_boundary(value: &str, index: usize) -> usize {
    value
        .grapheme_indices(true)
        .map(|(boundary, _)| boundary)
        .find(|boundary| *boundary > index)
        .unwrap_or(value.len())
}

fn replace_selection(state: &mut TextInputState, value: &mut String, replacement: &str) {
    let selection = state.selection();
    value.replace_range(selection.clone(), replacement);
    state.caret = selection.start + replacement.len();
    state.anchor = state.caret;
    state.observed_value.clear();
    state.observed_value.push_str(value);
}

fn set_caret(state: &mut TextInputState, caret: usize, extend: bool) {
    state.caret = caret;
    if !extend {
        state.anchor = caret;
    }
}

fn move_caret(state: &mut TextInputState, value: &str, forward: bool, extend: bool) {
    if !extend && state.anchor != state.caret {
        let selection = state.selection();
        set_caret(
            state,
            if forward {
                selection.end
            } else {
                selection.start
            },
            false,
        );
        return;
    }
    let next = if forward {
        next_boundary(value, state.caret)
    } else {
        previous_boundary(value, state.caret)
    };
    set_caret(state, next, extend);
}

fn delete_backward(state: &mut TextInputState, value: &mut String) -> bool {
    let mut selection = state.selection();
    if selection.is_empty() && state.caret > 0 {
        selection.start = previous_boundary(value, state.caret);
    }
    delete_range(state, value, selection)
}

fn delete_forward(state: &mut TextInputState, value: &mut String) -> bool {
    let mut selection = state.selection();
    if selection.is_empty() && state.caret < value.len() {
        selection.end = next_boundary(value, state.caret);
    }
    delete_range(state, value, selection)
}

fn delete_range(state: &mut TextInputState, value: &mut String, range: Range<usize>) -> bool {
    if range.is_empty() {
        return false;
    }
    value.replace_range(range.clone(), "");
    state.anchor = range.start;
    state.caret = range.start;
    state.observed_value.clear();
    state.observed_value.push_str(value);
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputError {
    InvalidLineCount,
}

impl fmt::Display for TextInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("multiline text input must reserve at least one line")
    }
}

impl std::error::Error for TextInputError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_deletion_never_splits_combining_sequences() {
        let mut value = "a\u{301}🙂".to_owned();
        let mut state = TextInputState {
            anchor: value.len(),
            caret: value.len(),
            observed_value: value.clone(),
            ..TextInputState::default()
        };
        assert!(delete_backward(&mut state, &mut value));
        assert_eq!(value, "a\u{301}");
        assert!(delete_backward(&mut state, &mut value));
        assert!(value.is_empty());
    }

    #[test]
    fn single_line_insert_normalizes_line_breaks() {
        assert_eq!(normalize_insert("a\r\nb\nc", false), "a b c");
        assert_eq!(normalize_insert("a\r\nb", true), "a\nb");
    }
}
