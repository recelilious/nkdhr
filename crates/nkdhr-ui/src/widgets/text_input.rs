use std::{any::Any, fmt, ops::Range, rc::Rc, sync::Arc};

use nkdhr_render::{CornerRadii, Point, Rect};
use unicode_segmentation::UnicodeSegmentation;

use crate::text::{TextLayout, TextWrap};
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
    layout: Option<Arc<TextLayout>>,
    display_boundaries: Vec<(usize, usize)>,
    text_origin: Point,
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
            layout: None,
            display_boundaries: vec![(0, 0)],
            text_origin: Point::new(0.0, 0.0),
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
            || previous.theme.typography != self.theme.typography
            || previous.password != self.password
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
        let value = ctx.watch(&self.value, Invalidation::LAYOUT | Invalidation::SEMANTICS);
        let (selection, preedit) = {
            let state = ctx.state_mut::<TextInputState>()?;
            state.synchronize(&value);
            (state.selection(), state.preedit.clone())
        };
        let display = display_text(
            &value,
            self.password,
            preedit.as_ref().map(|(text, _)| (selection, text.as_str())),
        );
        let mut style = self.theme.text_style(crate::TextRole::Body);
        style.wrap = if self.multiline {
            TextWrap::WordOrGlyph
        } else {
            TextWrap::None
        };
        let width = self
            .multiline
            .then_some((constraints.max().width - horizontal * 2.0).max(0.0));
        let layout = ctx.layout_text(&display.text, &style, width)?;
        let text_size = Size::new(layout.width(), layout.height());
        let state = ctx.state_mut::<TextInputState>()?;
        state.layout = Some(layout);
        state.display_boundaries = display.boundaries;
        let child = if ctx.child_count() == 1 {
            ctx.measure_child(
                0,
                constraints.deflate(crate::Insets::symmetric(horizontal, vertical))?,
            )?
        } else {
            Size::ZERO
        };
        Ok(constraints.constrain(Size::new(
            (child.width.max(text_size.width) + horizontal * 2.0).max(120.0),
            (child.height.max(text_size.height) + vertical * 2.0).max(minimum_height),
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
        let layout_height = ctx
            .state_mut::<TextInputState>()?
            .layout
            .as_ref()
            .map_or(0.0, |layout| layout.height());
        ctx.state_mut::<TextInputState>()?.text_origin = Point::new(
            rect.x + 12.0,
            if self.multiline {
                rect.y + 8.0
            } else {
                rect.y + (rect.height - layout_height).max(0.0) * 0.5
            },
        );
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let value = ctx.watch(&self.value, Invalidation::PAINT | Invalidation::SEMANTICS);
        let status = ctx.watch(&self.status, Invalidation::PAINT | Invalidation::SEMANTICS);
        let now = ctx.now();
        let (focused, focus, active, layout, origin, caret, selection, boundaries) = {
            let state = ctx.state_mut::<TextInputState>()?;
            state.synchronize(&value);
            (
                state.focused,
                state.focus_motion.value(now),
                state.focus_motion.is_active(now),
                state.layout.clone(),
                state.text_origin,
                state.caret,
                state.selection(),
                state.display_boundaries.clone(),
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
        if let Some(layout) = layout {
            let clip = rect.inset(8.0);
            if !selection.is_empty() {
                let start = layout.caret(source_to_display(&boundaries, selection.start));
                let end = layout.caret(source_to_display(&boundaries, selection.end));
                if start.line_index == end.line_index {
                    ctx.builder().rounded_rect(
                        Rect::new(
                            origin.x + start.x.min(end.x),
                            origin.y + start.y,
                            (start.x - end.x).abs().max(1.0),
                            start.height.max(end.height),
                        ),
                        CornerRadii::all(2.0),
                        with_alpha(self.theme.palette.accent, 0.28),
                    )?;
                }
            }
            ctx.draw_text(
                &layout,
                origin,
                if self.enabled {
                    self.theme.palette.text_primary
                } else {
                    self.theme.palette.text_muted
                },
                Some(clip),
            )?;
            if focused {
                let caret = layout.caret(source_to_display(&boundaries, caret));
                ctx.builder().rounded_rect(
                    Rect::new(origin.x + caret.x, origin.y + caret.y, 1.5, caret.height),
                    CornerRadii::all(0.75),
                    self.theme.palette.accent_secondary,
                )?;
            }
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
                let cleared_preedit = !focused && state.preedit.take().is_some();
                ctx.invalidate(if cleared_preedit {
                    Invalidation::LAYOUT | Invalidation::SEMANTICS
                } else {
                    Invalidation::PAINT | Invalidation::SEMANTICS
                });
                ctx.request_animation_frame();
            }
            UiEvent::PointerDown {
                button: PointerButton::Primary,
                position,
                ..
            } if self.enabled => {
                let state = ctx.state_mut::<TextInputState>()?;
                state.pointer_pressed = true;
                let caret = hit_source_boundary(state, *position, value.len());
                state.anchor = caret;
                state.caret = caret;
                ctx.request_focus();
                ctx.capture_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerUp {
                button: PointerButton::Primary,
                position,
                ..
            } => {
                if ctx.state_mut::<TextInputState>()?.pointer_pressed {
                    let state = ctx.state_mut::<TextInputState>()?;
                    state.caret = hit_source_boundary(state, *position, value.len());
                    state.pointer_pressed = false;
                    ctx.release_pointer();
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::PointerMoved { position } => {
                let state = ctx.state_mut::<TextInputState>()?;
                if state.pointer_pressed {
                    state.caret = hit_source_boundary(state, *position, value.len());
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::PointerCancel => {
                ctx.state_mut::<TextInputState>()?.pointer_pressed = false;
                ctx.release_pointer();
            }
            UiEvent::TextInput(text) if self.editable() => {
                let text = normalize_insert(text, self.multiline);
                let state = ctx.state_mut::<TextInputState>()?;
                replace_selection(state, &mut value, &text);
                state.preedit = None;
                self.publish(value);
                ctx.set_handled();
                ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
            }
            UiEvent::ImePreedit { text, selection } if self.editable() => {
                ctx.state_mut::<TextInputState>()?.preedit = Some((text.clone(), *selection));
                ctx.set_handled();
                ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
            }
            UiEvent::ImeCommit(text) if self.editable() => {
                let text = normalize_insert(text, self.multiline);
                let state = ctx.state_mut::<TextInputState>()?;
                replace_selection(state, &mut value, &text);
                state.preedit = None;
                self.publish(value);
                ctx.set_handled();
                ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
            }
            UiEvent::KeyDown {
                key,
                modifiers,
                repeat: _,
            } if self.enabled => {
                let editable = self.editable();
                let mut changed = false;
                let mut layout_changed = false;
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
                    Key::Escape => layout_changed = state.preedit.take().is_some(),
                    _ => return Ok(()),
                }
                if changed {
                    self.publish(value);
                }
                ctx.set_handled();
                ctx.invalidate(if changed || layout_changed {
                    Invalidation::LAYOUT | Invalidation::SEMANTICS
                } else {
                    Invalidation::PAINT | Invalidation::SEMANTICS
                });
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

struct DisplayText {
    text: String,
    /// `(display byte boundary, source byte boundary)` at every grapheme edge.
    boundaries: Vec<(usize, usize)>,
}

fn display_text(value: &str, password: bool, preedit: Option<(Range<usize>, &str)>) -> DisplayText {
    let mut text = String::new();
    let mut boundaries = vec![(0, 0)];
    if let Some((selection, preedit)) = preedit {
        append_source_display(
            &mut text,
            &mut boundaries,
            &value[..selection.start],
            0,
            password,
        );
        for grapheme in preedit.graphemes(true) {
            if password {
                text.push('•');
            } else {
                text.push_str(grapheme);
            }
            boundaries.push((text.len(), selection.start));
        }
        if let Some(last) = boundaries.last_mut() {
            last.1 = selection.end;
        }
        append_source_display(
            &mut text,
            &mut boundaries,
            &value[selection.end..],
            selection.end,
            password,
        );
    } else {
        append_source_display(&mut text, &mut boundaries, value, 0, password);
    }
    DisplayText { text, boundaries }
}

fn append_source_display(
    text: &mut String,
    boundaries: &mut Vec<(usize, usize)>,
    source: &str,
    source_offset: usize,
    password: bool,
) {
    for (source_start, grapheme) in source.grapheme_indices(true) {
        if password {
            text.push('•');
        } else {
            text.push_str(grapheme);
        }
        boundaries.push((text.len(), source_offset + source_start + grapheme.len()));
    }
}

fn source_to_display(boundaries: &[(usize, usize)], source: usize) -> usize {
    boundaries
        .iter()
        .rev()
        .find_map(|(display, boundary)| (*boundary <= source).then_some(*display))
        .unwrap_or(0)
}

fn display_to_source(boundaries: &[(usize, usize)], display: usize) -> usize {
    boundaries
        .iter()
        .rev()
        .find_map(|(boundary, source)| (*boundary <= display).then_some(*source))
        .unwrap_or(0)
}

fn hit_source_boundary(state: &TextInputState, position: Point, fallback: usize) -> usize {
    let Some(layout) = &state.layout else {
        return fallback;
    };
    let hit = layout.hit_test(Point::new(
        position.x - state.text_origin.x,
        position.y - state.text_origin.y,
    ));
    display_to_source(&state.display_boundaries, hit.byte_index)
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

    #[test]
    fn ime_preedit_replaces_the_selection_in_display_without_mutating_source() {
        let display = display_text("ab", false, Some((1..2, "你好")));
        assert_eq!(display.text, "a你好");
        assert_eq!(display_to_source(&display.boundaries, "a你".len()), 1);
        assert_eq!(
            display_to_source(&display.boundaries, display.text.len()),
            2
        );

        let password = display_text("ab", true, Some((1..2, "你好")));
        assert_eq!(password.text, "•••");
        assert!(!password.text.contains('你'));
    }
}
