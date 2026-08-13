use std::{any::Any, fmt, ops::Range, rc::Rc, sync::Arc, time::Duration};

use nkdhr_render::{CornerRadii, Point, Rect};
use unicode_segmentation::UnicodeSegmentation;

use crate::text::{TextLayout, TextWrap};
use crate::theme::with_alpha;
use crate::{
    AnimationCtx, ArrangeCtx, Constraints, EventCtx, Invalidation, Key, MaterialCapabilities,
    MaterialTier, MeasureCtx, MotionFamily, PaintCtx, PointerButton, Reactive, ScalarMotion,
    SemanticRole, Semantics, SemanticsCtx, Size, Theme, ThemeReadSet, UiError, UiEvent, UpdateCtx,
    Widget,
};

use super::surface::{SurfaceState, paint_fluid_well, surface_theme_reads};

type TextCallback = Rc<dyn Fn(&str)>;
type FormatterCallback = Rc<dyn Fn(TextInputEdit) -> TextInputEdit>;
type ValidationCallback = Rc<dyn Fn(TextInputValidationRequest)>;
type DisplayPreedit<'a> = (Range<usize>, &'a str, Option<(usize, usize)>);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TextInputStatus {
    #[default]
    Idle,
    Pending,
    Valid,
    Invalid(String),
    BackendError(String),
}

/// Logical selection expressed as UTF-8 byte boundaries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextInputSelection {
    pub anchor: usize,
    pub caret: usize,
}

impl TextInputSelection {
    pub const fn new(anchor: usize, caret: usize) -> Self {
        Self { anchor, caret }
    }

    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }
}

/// Formatter input/output. Returning the selection makes caret preservation an
/// explicit part of formatting rather than an unreliable string-diff guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInputEdit {
    pub value: String,
    pub selection: TextInputSelection,
}

impl TextInputEdit {
    pub fn new(value: impl Into<String>, selection: TextInputSelection) -> Self {
        Self {
            value: value.into(),
            selection,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputEnterBehavior {
    Submit,
    InsertLineBreak,
    SubmitUnlessShift,
    Ignore,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextInputTabBehavior {
    #[default]
    Navigate,
    InsertTab,
    Complete,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PasswordCopyPolicy {
    #[default]
    Deny,
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputValidationTrigger {
    OnChange { debounce: Duration },
    OnBlur,
    OnSubmit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInputValidationRequest {
    pub generation: u64,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextInputValidationOutcome {
    Valid,
    Invalid(String),
    BackendError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInputValidationResult {
    pub generation: u64,
    pub outcome: TextInputValidationOutcome,
}

impl TextInputValidationResult {
    pub const fn valid(generation: u64) -> Self {
        Self {
            generation,
            outcome: TextInputValidationOutcome::Valid,
        }
    }

    pub fn invalid(generation: u64, message: impl Into<String>) -> Self {
        Self {
            generation,
            outcome: TextInputValidationOutcome::Invalid(message.into()),
        }
    }

    pub fn backend_error(generation: u64, message: impl Into<String>) -> Self {
        Self {
            generation,
            outcome: TextInputValidationOutcome::BackendError(message.into()),
        }
    }
}

struct TextInputValidation {
    trigger: TextInputValidationTrigger,
    result: Reactive<Option<TextInputValidationResult>>,
    callback: ValidationCallback,
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
    password_revealed: Option<Reactive<bool>>,
    password_copy_policy: PasswordCopyPolicy,
    multiline: bool,
    minimum_lines: usize,
    enter_behavior: Option<TextInputEnterBehavior>,
    tab_behavior: TextInputTabBehavior,
    history_limit: usize,
    formatter: Option<FormatterCallback>,
    validation: Option<TextInputValidation>,
    on_change: Option<TextCallback>,
    on_submit: Option<TextCallback>,
    on_complete: Option<TextCallback>,
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
            password_revealed: None,
            password_copy_policy: PasswordCopyPolicy::Deny,
            multiline: false,
            minimum_lines: 1,
            enter_behavior: None,
            tab_behavior: TextInputTabBehavior::Navigate,
            history_limit: 100,
            formatter: None,
            validation: None,
            on_change: None,
            on_submit: None,
            on_complete: None,
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

    /// Live reveal state. Accessibility semantics remain redacted even while
    /// the user explicitly reveals the visible glyphs.
    pub fn password_reveal(mut self, revealed: Reactive<bool>) -> Self {
        self.password_revealed = Some(revealed);
        self
    }

    pub fn password_copy_policy(mut self, policy: PasswordCopyPolicy) -> Self {
        self.password_copy_policy = policy;
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

    pub fn enter_behavior(mut self, behavior: TextInputEnterBehavior) -> Self {
        self.enter_behavior = Some(behavior);
        self
    }

    pub fn tab_behavior(mut self, behavior: TextInputTabBehavior) -> Self {
        self.tab_behavior = behavior;
        self
    }

    pub fn history_limit(mut self, limit: usize) -> Result<Self, TextInputError> {
        if limit == 0 {
            return Err(TextInputError::InvalidHistoryLimit);
        }
        self.history_limit = limit;
        Ok(self)
    }

    pub fn formatter(
        mut self,
        formatter: impl Fn(TextInputEdit) -> TextInputEdit + 'static,
    ) -> Self {
        self.formatter = Some(Rc::new(formatter));
        self
    }

    pub fn validation(
        mut self,
        trigger: TextInputValidationTrigger,
        result: Reactive<Option<TextInputValidationResult>>,
        callback: impl Fn(TextInputValidationRequest) + 'static,
    ) -> Result<Self, TextInputError> {
        if matches!(
            trigger,
            TextInputValidationTrigger::OnChange { debounce } if debounce.is_zero()
        ) {
            return Err(TextInputError::InvalidValidationDebounce);
        }
        self.validation = Some(TextInputValidation {
            trigger,
            result,
            callback: Rc::new(callback),
        });
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

    pub fn on_complete(mut self, callback: impl Fn(&str) + 'static) -> Self {
        self.on_complete = Some(Rc::new(callback));
        self
    }

    fn editable(&self) -> bool {
        self.enabled && !self.read_only
    }

    fn effective_enter_behavior(&self) -> TextInputEnterBehavior {
        self.enter_behavior.unwrap_or(if self.multiline {
            TextInputEnterBehavior::InsertLineBreak
        } else {
            TextInputEnterBehavior::Submit
        })
    }

    fn may_copy(&self) -> bool {
        !self.password || self.password_copy_policy == PasswordCopyPolicy::Allow
    }

    fn publish(&self, value: String) {
        self.value.set(value.clone());
        if let Some(callback) = &self.on_change {
            callback(&value);
        }
    }
}

#[derive(Debug, Clone)]
struct EditSnapshot {
    value: String,
    selection: TextInputSelection,
}

#[derive(Debug, Clone, Copy)]
enum PointerGranularity {
    Character,
    Word,
    Line,
}

#[derive(Debug, Clone)]
struct PointerSelection {
    granularity: PointerGranularity,
    origin: Range<usize>,
}

#[derive(Debug, Clone)]
struct PendingValidation {
    deadline: Duration,
    request: TextInputValidationRequest,
}

#[derive(Debug, Clone)]
struct TextInputState {
    focused: bool,
    focus_motion: ScalarMotion,
    anchor: usize,
    caret: usize,
    preferred_x: Option<f32>,
    observed_value: String,
    preedit: Option<(String, Option<(usize, usize)>)>,
    pointer_selection: Option<PointerSelection>,
    layout: Option<Arc<TextLayout>>,
    display_boundaries: Vec<(usize, usize)>,
    composition_range: Option<Range<usize>>,
    composition_selection: Option<Range<usize>>,
    composition_caret: Option<usize>,
    text_origin: Point,
    undo: Vec<EditSnapshot>,
    redo: Vec<EditSnapshot>,
    validation_generation: u64,
    pending_validation: Option<PendingValidation>,
    validation_status: Option<TextInputStatus>,
    applied_result_generation: Option<u64>,
    valid_until: Option<Duration>,
}

impl Default for TextInputState {
    fn default() -> Self {
        Self {
            focused: false,
            focus_motion: ScalarMotion::settled(0.0),
            anchor: 0,
            caret: 0,
            preferred_x: None,
            observed_value: String::new(),
            preedit: None,
            pointer_selection: None,
            layout: None,
            display_boundaries: vec![(0, 0)],
            composition_range: None,
            composition_selection: None,
            composition_caret: None,
            text_origin: Point::new(0.0, 0.0),
            undo: Vec::new(),
            redo: Vec::new(),
            validation_generation: 0,
            pending_validation: None,
            validation_status: None,
            applied_result_generation: None,
            valid_until: None,
        }
    }
}

impl TextInputState {
    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    fn public_selection(&self) -> TextInputSelection {
        TextInputSelection::new(self.anchor, self.caret)
    }

    fn snapshot(&self, value: &str) -> EditSnapshot {
        EditSnapshot {
            value: value.to_owned(),
            selection: self.public_selection(),
        }
    }

    fn synchronize(&mut self, value: &str) {
        if self.observed_value == value {
            return;
        }
        let initially_empty = self.observed_value.is_empty() && self.anchor == 0 && self.caret == 0;
        self.anchor = floor_boundary(value, self.anchor.min(value.len()));
        self.caret = floor_boundary(value, self.caret.min(value.len()));
        if initially_empty {
            self.anchor = value.len();
            self.caret = value.len();
        }
        self.observed_value.clear();
        self.observed_value.push_str(value);
        self.preedit = None;
        self.preferred_x = None;
        self.layout = None;
        self.undo.clear();
        self.redo.clear();
        self.validation_generation = self.validation_generation.wrapping_add(1).max(1);
        self.pending_validation = None;
        self.validation_status = None;
        self.applied_result_generation = None;
        self.valid_until = None;
    }
}

impl Widget for TextInput {
    fn theme_reads(&self) -> ThemeReadSet {
        let mut reads = surface_theme_reads(MaterialTier::CompactNode);
        reads.extend([
            "density",
            "radii.control",
            "typography.ui_families",
            "typography.scale",
            "typography.body.font_size",
            "typography.body.line_height",
            "typography.body.weight",
            "palette.accent",
            "palette.accent_secondary",
            "palette.error",
            "palette.success",
            "palette.warning",
            "palette.text_primary",
            "palette.text_muted",
            "motion.mode",
            "motion.speed_multiplier",
            "motion.standard",
            "motion.durations.text_input_focus",
            "motion.durations.validation",
        ]);
        reads
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

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
            || previous.password_revealed.is_some() != self.password_revealed.is_some()
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
        let revealed = self
            .password_revealed
            .as_ref()
            .is_some_and(|revealed| ctx.watch(revealed, Invalidation::LAYOUT));
        let (selection, preedit) = {
            let state = ctx.state_mut::<TextInputState>()?;
            state.synchronize(&value);
            (state.selection(), state.preedit.clone())
        };
        let display = display_text(
            &value,
            self.password && !revealed,
            preedit.as_ref().map(|(text, selection_in_preedit)| {
                (selection, text.as_str(), *selection_in_preedit)
            }),
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
        state.composition_range = display.composition_range;
        state.composition_selection = display.composition_selection;
        state.composition_caret = display.composition_caret;
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
        let external_status =
            ctx.watch(&self.status, Invalidation::PAINT | Invalidation::SEMANTICS);
        let validation_result = self.validation.as_ref().and_then(|validation| {
            ctx.watch(
                &validation.result,
                Invalidation::PAINT | Invalidation::SEMANTICS,
            )
        });
        let now = ctx.now();
        let validation_duration = self.theme.motion.spec(MotionFamily::Validation).duration;
        let (
            focused,
            focus,
            active,
            layout,
            origin,
            caret,
            selection,
            boundaries,
            composition,
            composition_selection,
            composition_caret,
            status,
            validation_animation,
        ) = {
            let state = ctx.state_mut::<TextInputState>()?;
            state.synchronize(&value);
            let validation_animation = apply_validation_result(
                state,
                validation_result.as_ref(),
                now,
                validation_duration,
            );
            (
                state.focused,
                state.focus_motion.value(now),
                state.focus_motion.is_active(now),
                state.layout.clone(),
                state.text_origin,
                state.caret,
                state.selection(),
                state.display_boundaries.clone(),
                state.composition_range.clone(),
                state.composition_selection.clone(),
                state.composition_caret,
                state.validation_status.clone().unwrap_or(external_status),
                validation_animation,
            )
        };
        if active || validation_animation {
            ctx.request_animation_frame();
        }
        let rect = ctx.rect();
        paint_fluid_well(
            ctx.builder(),
            rect,
            CornerRadii::all(self.theme.radii.control),
            &self.theme,
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
            TextInputStatus::Invalid(_) | TextInputStatus::BackendError(_) => {
                ctx.builder().border(
                    rect,
                    CornerRadii::all(self.theme.radii.control),
                    1.0,
                    with_alpha(self.theme.palette.error, 0.88),
                )?
            }
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
            if composition.is_none() && !selection.is_empty() {
                let display = source_to_display(&boundaries, selection.start)
                    ..source_to_display(&boundaries, selection.end);
                for fragment in layout.selection_rects(display) {
                    if let Some(highlight) = Rect::new(
                        origin.x + fragment.rect.x,
                        origin.y + fragment.rect.y,
                        fragment.rect.width,
                        fragment.rect.height,
                    )
                    .intersect(clip)
                    {
                        ctx.builder().rounded_rect(
                            highlight,
                            CornerRadii::all(2.0),
                            with_alpha(self.theme.palette.accent, 0.28),
                        )?;
                    }
                }
            }
            if let Some(selection) = composition_selection {
                for fragment in layout.selection_rects(selection) {
                    if let Some(highlight) = Rect::new(
                        origin.x + fragment.rect.x,
                        origin.y + fragment.rect.y,
                        fragment.rect.width,
                        fragment.rect.height,
                    )
                    .intersect(clip)
                    {
                        ctx.builder().rounded_rect(
                            highlight,
                            CornerRadii::all(2.0),
                            with_alpha(self.theme.palette.accent_secondary, 0.22),
                        )?;
                    }
                }
            }
            if let Some(composition) = composition {
                for fragment in layout.selection_rects(composition) {
                    if let Some(underline) = Rect::new(
                        origin.x + fragment.rect.x,
                        origin.y + fragment.rect.bottom() - 1.5,
                        fragment.rect.width,
                        1.5,
                    )
                    .intersect(clip)
                    {
                        ctx.builder().rounded_rect(
                            underline,
                            CornerRadii::all(0.75),
                            self.theme.palette.accent_secondary,
                        )?;
                    }
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
                let caret = layout.caret(
                    composition_caret.unwrap_or_else(|| source_to_display(&boundaries, caret)),
                );
                if let Some(caret_rect) =
                    Rect::new(origin.x + caret.x, origin.y + caret.y, 1.5, caret.height)
                        .intersect(clip)
                {
                    ctx.builder().rounded_rect(
                        caret_rect,
                        CornerRadii::all(0.75),
                        self.theme.palette.accent_secondary,
                    )?;
                }
            }
        }
        ctx.paint_children()
    }

    fn animation(&self, ctx: &mut AnimationCtx<'_>) {
        let now = ctx.now();
        let mut validation_request = None;
        let mut keep_running = false;
        let mut changed = false;
        if let Ok(state) = ctx.state_mut::<TextInputState>() {
            if state
                .pending_validation
                .as_ref()
                .is_some_and(|pending| pending.deadline <= now)
            {
                let pending = state
                    .pending_validation
                    .take()
                    .expect("checked pending validation");
                state.validation_status = Some(TextInputStatus::Pending);
                validation_request = Some(pending.request);
                changed = true;
            } else if state.pending_validation.is_some() {
                keep_running = true;
            }
            if state.valid_until.is_some_and(|deadline| deadline <= now) {
                state.valid_until = None;
                state.validation_status = None;
                changed = true;
            } else if state.valid_until.is_some() {
                keep_running = true;
            }
        }
        if let (Some(validation), Some(request)) = (&self.validation, validation_request) {
            (validation.callback)(request);
        }
        if changed {
            ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
        }
        if keep_running {
            ctx.request_animation_frame();
        }
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
                let validation_request = {
                    let state = ctx.state_mut::<TextInputState>()?;
                    state.focused = *focused;
                    state.focus_motion.retarget(
                        now,
                        if *focused { 1.0 } else { 0.0 },
                        self.theme.motion.spec(MotionFamily::TextInputFocus),
                    );
                    state.pointer_selection = None;
                    let cleared_preedit = !focused && state.preedit.take().is_some();
                    let request = (!focused
                        && self.validation.as_ref().is_some_and(|validation| {
                            validation.trigger == TextInputValidationTrigger::OnBlur
                        }))
                    .then(|| issue_validation(state, &value));
                    (cleared_preedit, request)
                };
                if let (Some(validation), Some(request)) = (&self.validation, validation_request.1)
                {
                    (validation.callback)(request);
                }
                ctx.invalidate(if validation_request.0 {
                    Invalidation::LAYOUT | Invalidation::SEMANTICS
                } else {
                    Invalidation::PAINT | Invalidation::SEMANTICS
                });
                ctx.request_animation_frame();
            }
            UiEvent::PointerDown {
                button: PointerButton::Primary,
                position,
                modifiers,
                click_count,
            } if self.enabled => {
                let state = ctx.state_mut::<TextInputState>()?;
                let hit = hit_source_boundary(state, *position, value.len());
                let granularity = match click_count {
                    1 => PointerGranularity::Character,
                    2 => PointerGranularity::Word,
                    _ => PointerGranularity::Line,
                };
                let range = selection_unit(&value, hit, granularity);
                if modifiers.shift {
                    state.caret = if hit < state.anchor {
                        range.start
                    } else {
                        range.end
                    };
                    state.pointer_selection = Some(PointerSelection {
                        granularity,
                        origin: state.anchor..state.anchor,
                    });
                } else {
                    state.anchor = range.start;
                    state.caret = range.end;
                    state.pointer_selection = Some(PointerSelection {
                        granularity,
                        origin: range,
                    });
                }
                state.preferred_x = None;
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
                if ctx
                    .state_mut::<TextInputState>()?
                    .pointer_selection
                    .is_some()
                {
                    let state = ctx.state_mut::<TextInputState>()?;
                    update_pointer_selection(state, &value, *position);
                    state.pointer_selection = None;
                    ctx.release_pointer();
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::PointerMoved { position } => {
                let state = ctx.state_mut::<TextInputState>()?;
                if state.pointer_selection.is_some() {
                    update_pointer_selection(state, &value, *position);
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::PointerCancel => {
                ctx.state_mut::<TextInputState>()?.pointer_selection = None;
                ctx.release_pointer();
            }
            UiEvent::TextInput(text) if self.editable() => {
                let text = normalize_insert(text, self.multiline);
                let now = ctx.now();
                let (changed, validation_timer) = {
                    let state = ctx.state_mut::<TextInputState>()?;
                    commit_replacement(self, state, &mut value, &text, now)
                };
                if changed {
                    self.publish(value);
                }
                if validation_timer {
                    ctx.request_animation_frame();
                }
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
                let now = ctx.now();
                let (changed, validation_timer) = {
                    let state = ctx.state_mut::<TextInputState>()?;
                    commit_replacement(self, state, &mut value, &text, now)
                };
                if changed {
                    self.publish(value);
                }
                if validation_timer {
                    ctx.request_animation_frame();
                }
                ctx.set_handled();
                ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
            }
            UiEvent::ClipboardText { text, .. } if self.editable() => {
                let text = normalize_insert(text, self.multiline);
                let now = ctx.now();
                let (changed, validation_timer) = {
                    let state = ctx.state_mut::<TextInputState>()?;
                    commit_replacement(self, state, &mut value, &text, now)
                };
                if changed {
                    self.publish(value);
                }
                if validation_timer {
                    ctx.request_animation_frame();
                }
                ctx.set_handled();
                ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
            }
            UiEvent::KeyDown {
                key,
                modifiers,
                repeat: _,
            } if self.enabled => {
                let editable = self.editable();
                let now = ctx.now();
                let command = match key {
                    Key::Character(character) if modifiers.control => {
                        Some(character.to_ascii_lowercase())
                    }
                    _ => None,
                };

                if command.as_deref() == Some("c") {
                    let selection = ctx.state_mut::<TextInputState>()?.selection();
                    if self.may_copy() && !selection.is_empty() {
                        ctx.write_clipboard_text(value[selection].to_owned());
                    }
                    ctx.set_handled();
                    return Ok(());
                }
                if command.as_deref() == Some("v") && editable {
                    ctx.read_clipboard_text();
                    ctx.set_handled();
                    return Ok(());
                }
                if command.as_deref() == Some("x") {
                    let mut changed = false;
                    let mut validation_timer = false;
                    if editable && self.may_copy() {
                        let selection = ctx.state_mut::<TextInputState>()?.selection();
                        if !selection.is_empty() {
                            ctx.write_clipboard_text(value[selection.clone()].to_owned());
                            (changed, validation_timer) = {
                                let state = ctx.state_mut::<TextInputState>()?;
                                commit_replacement(self, state, &mut value, "", now)
                            };
                        }
                    }
                    if changed {
                        self.publish(value);
                        ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
                    }
                    if validation_timer {
                        ctx.request_animation_frame();
                    }
                    ctx.set_handled();
                    return Ok(());
                }
                if command.as_deref() == Some("a") {
                    let state = ctx.state_mut::<TextInputState>()?;
                    state.anchor = 0;
                    state.caret = value.len();
                    state.preferred_x = None;
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                    return Ok(());
                }
                if command.as_deref() == Some("z") || command.as_deref() == Some("y") {
                    let redo = command.as_deref() == Some("y")
                        || (command.as_deref() == Some("z") && modifiers.shift);
                    let (changed, validation_timer) = if editable && !self.password {
                        let state = ctx.state_mut::<TextInputState>()?;
                        restore_history(self, state, &mut value, redo, now)
                    } else {
                        (false, false)
                    };
                    if changed {
                        self.publish(value);
                        ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
                    }
                    if validation_timer {
                        ctx.request_animation_frame();
                    }
                    ctx.set_handled();
                    return Ok(());
                }

                let mut changed = false;
                let mut validation_timer = false;
                let mut layout_changed = false;
                let mut submit = false;
                let mut validation_request = None;
                {
                    let state = ctx.state_mut::<TextInputState>()?;
                    match key {
                        Key::ArrowLeft => {
                            move_horizontal(
                                state,
                                &value,
                                false,
                                modifiers.control,
                                modifiers.shift,
                            );
                        }
                        Key::ArrowRight => {
                            move_horizontal(
                                state,
                                &value,
                                true,
                                modifiers.control,
                                modifiers.shift,
                            );
                        }
                        Key::ArrowUp if self.multiline => {
                            move_vertical(state, false, modifiers.shift);
                        }
                        Key::ArrowDown if self.multiline => {
                            move_vertical(state, true, modifiers.shift);
                        }
                        Key::Home => {
                            move_home_end(state, &value, false, modifiers.control, modifiers.shift);
                        }
                        Key::End => {
                            move_home_end(state, &value, true, modifiers.control, modifiers.shift);
                        }
                        Key::Backspace if editable => {
                            let before = state.snapshot(&value);
                            changed = if modifiers.control {
                                delete_word_backward(state, &mut value)
                            } else {
                                delete_backward(state, &mut value)
                            };
                            if changed {
                                (changed, validation_timer) =
                                    finish_committed_edit(self, state, &mut value, before, now);
                            }
                        }
                        Key::Delete if editable => {
                            let before = state.snapshot(&value);
                            changed = if modifiers.control {
                                delete_word_forward(state, &mut value)
                            } else {
                                delete_forward(state, &mut value)
                            };
                            if changed {
                                (changed, validation_timer) =
                                    finish_committed_edit(self, state, &mut value, before, now);
                            }
                        }
                        Key::Enter => match self.effective_enter_behavior() {
                            TextInputEnterBehavior::Submit => submit = true,
                            TextInputEnterBehavior::InsertLineBreak if editable => {
                                (changed, validation_timer) =
                                    commit_replacement(self, state, &mut value, "\n", now);
                            }
                            TextInputEnterBehavior::SubmitUnlessShift
                                if modifiers.shift && editable =>
                            {
                                (changed, validation_timer) =
                                    commit_replacement(self, state, &mut value, "\n", now);
                            }
                            TextInputEnterBehavior::SubmitUnlessShift => submit = true,
                            TextInputEnterBehavior::Ignore
                            | TextInputEnterBehavior::InsertLineBreak => {}
                        },
                        Key::Tab => match self.tab_behavior {
                            TextInputTabBehavior::Navigate => return Ok(()),
                            TextInputTabBehavior::InsertTab if editable => {
                                (changed, validation_timer) =
                                    commit_replacement(self, state, &mut value, "\t", now);
                            }
                            TextInputTabBehavior::Complete => {
                                if let Some(callback) = &self.on_complete {
                                    callback(&value);
                                }
                            }
                            TextInputTabBehavior::InsertTab => {}
                        },
                        Key::Escape => layout_changed = state.preedit.take().is_some(),
                        _ => return Ok(()),
                    }
                    if submit
                        && self.validation.as_ref().is_some_and(|validation| {
                            validation.trigger == TextInputValidationTrigger::OnSubmit
                        })
                    {
                        validation_request = Some(issue_validation(state, &value));
                    }
                }
                if changed {
                    self.publish(value.clone());
                }
                if submit && let Some(callback) = &self.on_submit {
                    callback(&value);
                }
                if let (Some(validation), Some(request)) = (&self.validation, validation_request) {
                    (validation.callback)(request);
                }
                if validation_timer {
                    ctx.request_animation_frame();
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
        let external_status = ctx.watch(&self.status, Invalidation::SEMANTICS);
        let validation_result = self
            .validation
            .as_ref()
            .and_then(|validation| ctx.watch(&validation.result, Invalidation::SEMANTICS));
        let now = ctx.now();
        let validation_duration = self.theme.motion.spec(MotionFamily::Validation).duration;
        let (status, validation_animation) = if let Ok(state) = ctx.state_mut::<TextInputState>() {
            let active = apply_validation_result(
                state,
                validation_result.as_ref(),
                now,
                validation_duration,
            );
            (
                state.validation_status.clone().unwrap_or(external_status),
                active,
            )
        } else {
            (external_status, false)
        };
        if validation_animation {
            ctx.request_animation_frame();
        }
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
            TextInputStatus::BackendError(message) => {
                Some(format!("validation unavailable: {message}"))
            }
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
    composition_range: Option<Range<usize>>,
    composition_selection: Option<Range<usize>>,
    composition_caret: Option<usize>,
}

fn display_text(value: &str, password: bool, preedit: Option<DisplayPreedit<'_>>) -> DisplayText {
    let mut text = String::new();
    let mut boundaries = vec![(0, 0)];
    let mut composition_range = None;
    let mut composition_selection = None;
    let mut composition_caret = None;
    if let Some((selection, preedit, selection_in_preedit)) = preedit {
        append_source_display(
            &mut text,
            &mut boundaries,
            &value[..selection.start],
            0,
            password,
        );
        let composition_start = text.len();
        let mut preedit_boundaries = vec![(0, composition_start)];
        for (source_start, grapheme) in preedit.grapheme_indices(true) {
            if password {
                text.push('•');
            } else {
                text.push_str(grapheme);
            }
            boundaries.push((text.len(), selection.start));
            preedit_boundaries.push((source_start + grapheme.len(), text.len()));
        }
        composition_range = Some(composition_start..text.len());
        let (preedit_start, preedit_end) =
            selection_in_preedit.unwrap_or((preedit.len(), preedit.len()));
        let display_start = preedit_to_display(&preedit_boundaries, preedit_start);
        let display_end = preedit_to_display(&preedit_boundaries, preedit_end);
        composition_selection = (display_start != display_end)
            .then_some(display_start.min(display_end)..display_start.max(display_end));
        composition_caret = Some(display_end);
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
    DisplayText {
        text,
        boundaries,
        composition_range,
        composition_selection,
        composition_caret,
    }
}

fn preedit_to_display(boundaries: &[(usize, usize)], source: usize) -> usize {
    boundaries
        .iter()
        .rev()
        .find_map(|(boundary, display)| (*boundary <= source).then_some(*display))
        .unwrap_or(0)
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

fn update_pointer_selection(state: &mut TextInputState, value: &str, position: Point) {
    let Some(pointer) = state.pointer_selection.clone() else {
        return;
    };
    let hit = hit_source_boundary(state, position, value.len());
    let current = selection_unit(value, hit, pointer.granularity);
    if current.end <= pointer.origin.start {
        state.anchor = pointer.origin.end;
        state.caret = current.start;
    } else {
        state.anchor = pointer.origin.start;
        state.caret = current.end;
    }
}

fn selection_unit(value: &str, index: usize, granularity: PointerGranularity) -> Range<usize> {
    match granularity {
        PointerGranularity::Character => index..index,
        PointerGranularity::Word => word_range_at(value, index),
        PointerGranularity::Line => line_range_at(value, index),
    }
}

fn word_range_at(value: &str, index: usize) -> Range<usize> {
    let index = index.min(value.len());
    value
        .split_word_bound_indices()
        .find_map(|(start, segment)| {
            let end = start + segment.len();
            (index >= start && (index < end || index == value.len())).then_some(start..end)
        })
        .unwrap_or_else(|| {
            let start = previous_boundary(value, index);
            start..next_boundary(value, start)
        })
}

fn line_range_at(value: &str, index: usize) -> Range<usize> {
    let index = index.min(value.len());
    let start = value[..index]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let end = value[index..]
        .find('\n')
        .map_or(value.len(), |position| index + position + 1);
    start..end
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

fn previous_word_boundary(value: &str, index: usize) -> usize {
    value
        .unicode_word_indices()
        .map(|(start, _)| start)
        .take_while(|start| *start < index)
        .last()
        .unwrap_or_else(|| previous_boundary(value, index))
}

fn next_word_boundary(value: &str, index: usize) -> usize {
    for (start, word) in value.unicode_word_indices() {
        let end = start + word.len();
        if index < end {
            return end;
        }
    }
    next_boundary(value, index)
}

fn replace_selection(state: &mut TextInputState, value: &mut String, replacement: &str) {
    let selection = state.selection();
    value.replace_range(selection.clone(), replacement);
    state.caret = selection.start + replacement.len();
    state.anchor = state.caret;
}

fn normalize_selection(value: &str, selection: TextInputSelection) -> TextInputSelection {
    TextInputSelection::new(
        floor_boundary(value, selection.anchor.min(value.len())),
        floor_boundary(value, selection.caret.min(value.len())),
    )
}

fn commit_replacement(
    input: &TextInput,
    state: &mut TextInputState,
    value: &mut String,
    replacement: &str,
    now: Duration,
) -> (bool, bool) {
    let before = state.snapshot(value);
    replace_selection(state, value, replacement);
    finish_committed_edit(input, state, value, before, now)
}

fn finish_committed_edit(
    input: &TextInput,
    state: &mut TextInputState,
    value: &mut String,
    before: EditSnapshot,
    now: Duration,
) -> (bool, bool) {
    state.preedit = None;
    state.preferred_x = None;
    state.layout = None;
    if let Some(formatter) = &input.formatter {
        let formatted = formatter(TextInputEdit::new(value.clone(), state.public_selection()));
        *value = normalize_insert(&formatted.value, input.multiline);
        let selection = normalize_selection(value, formatted.selection);
        state.anchor = selection.anchor;
        state.caret = selection.caret;
    }
    let changed = *value != before.value;
    if !changed {
        state.observed_value.clear();
        state.observed_value.push_str(value);
        return (false, false);
    }
    if !input.password {
        state.undo.push(before);
        if state.undo.len() > input.history_limit {
            state.undo.remove(0);
        }
        state.redo.clear();
    }
    state.observed_value.clear();
    state.observed_value.push_str(value);
    let timer = schedule_validation_after_edit(input, state, value, now);
    (true, timer)
}

fn schedule_validation_after_edit(
    input: &TextInput,
    state: &mut TextInputState,
    value: &str,
    now: Duration,
) -> bool {
    let Some(validation) = &input.validation else {
        return false;
    };
    state.validation_generation = state.validation_generation.wrapping_add(1).max(1);
    state.pending_validation = None;
    state.validation_status = None;
    state.applied_result_generation = None;
    state.valid_until = None;
    if let TextInputValidationTrigger::OnChange { debounce } = validation.trigger {
        state.pending_validation = Some(PendingValidation {
            deadline: now.checked_add(debounce).unwrap_or(Duration::MAX),
            request: TextInputValidationRequest {
                generation: state.validation_generation,
                value: value.to_owned(),
            },
        });
        true
    } else {
        false
    }
}

fn issue_validation(state: &mut TextInputState, value: &str) -> TextInputValidationRequest {
    state.validation_generation = state.validation_generation.wrapping_add(1).max(1);
    state.pending_validation = None;
    state.validation_status = Some(TextInputStatus::Pending);
    state.applied_result_generation = None;
    state.valid_until = None;
    TextInputValidationRequest {
        generation: state.validation_generation,
        value: value.to_owned(),
    }
}

fn apply_validation_result(
    state: &mut TextInputState,
    result: Option<&TextInputValidationResult>,
    now: Duration,
    valid_duration: Duration,
) -> bool {
    let Some(result) = result else {
        return state.valid_until.is_some() || state.pending_validation.is_some();
    };
    if result.generation != state.validation_generation
        || state.applied_result_generation == Some(result.generation)
    {
        return state.valid_until.is_some() || state.pending_validation.is_some();
    }
    state.applied_result_generation = Some(result.generation);
    state.pending_validation = None;
    state.validation_status = Some(match &result.outcome {
        TextInputValidationOutcome::Valid => TextInputStatus::Valid,
        TextInputValidationOutcome::Invalid(message) => TextInputStatus::Invalid(message.clone()),
        TextInputValidationOutcome::BackendError(message) => {
            TextInputStatus::BackendError(message.clone())
        }
    });
    state.valid_until = matches!(result.outcome, TextInputValidationOutcome::Valid)
        .then(|| now.checked_add(valid_duration).unwrap_or(Duration::MAX));
    state.valid_until.is_some()
}

fn restore_history(
    input: &TextInput,
    state: &mut TextInputState,
    value: &mut String,
    redo: bool,
    now: Duration,
) -> (bool, bool) {
    let next = if redo {
        state.redo.pop()
    } else {
        state.undo.pop()
    };
    let Some(next) = next else {
        return (false, false);
    };
    let current = state.snapshot(value);
    if redo {
        state.undo.push(current);
    } else {
        state.redo.push(current);
    }
    *value = next.value;
    let selection = normalize_selection(value, next.selection);
    state.anchor = selection.anchor;
    state.caret = selection.caret;
    state.observed_value.clear();
    state.observed_value.push_str(value);
    state.preedit = None;
    state.preferred_x = None;
    state.layout = None;
    let timer = schedule_validation_after_edit(input, state, value, now);
    (true, timer)
}

fn set_caret(state: &mut TextInputState, caret: usize, extend: bool) {
    state.caret = caret;
    if !extend {
        state.anchor = caret;
    }
}

fn move_horizontal(
    state: &mut TextInputState,
    value: &str,
    forward: bool,
    by_word: bool,
    extend: bool,
) {
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
        state.preferred_x = None;
        return;
    }
    let next = if by_word {
        if forward {
            next_word_boundary(value, state.caret)
        } else {
            previous_word_boundary(value, state.caret)
        }
    } else if let Some(layout) = &state.layout {
        let display = source_to_display(&state.display_boundaries, state.caret);
        display_to_source(
            &state.display_boundaries,
            layout.visual_neighbor(display, forward),
        )
    } else if forward {
        next_boundary(value, state.caret)
    } else {
        previous_boundary(value, state.caret)
    };
    set_caret(state, next, extend);
    state.preferred_x = None;
}

fn move_vertical(state: &mut TextInputState, forward: bool, extend: bool) {
    let Some(layout) = &state.layout else {
        return;
    };
    let display = source_to_display(&state.display_boundaries, state.caret);
    let caret = layout.caret(display);
    let preferred_x = state.preferred_x.unwrap_or(caret.x);
    let target_line = if forward {
        (caret.line_index + 1).min(layout.line_count().saturating_sub(1))
    } else {
        caret.line_index.saturating_sub(1)
    };
    let target = layout.hit_line(target_line, preferred_x);
    let source = display_to_source(&state.display_boundaries, target.byte_index);
    set_caret(state, source, extend);
    state.preferred_x = Some(preferred_x);
}

fn move_home_end(state: &mut TextInputState, value: &str, end: bool, document: bool, extend: bool) {
    let caret = if document {
        if end { value.len() } else { 0 }
    } else if let Some(layout) = &state.layout {
        let display = source_to_display(&state.display_boundaries, state.caret);
        let line = layout.caret(display).line_index;
        display_to_source(
            &state.display_boundaries,
            layout
                .hit_line(
                    line,
                    if end {
                        f32::INFINITY
                    } else {
                        f32::NEG_INFINITY
                    },
                )
                .byte_index,
        )
    } else if end {
        line_range_at(value, state.caret).end
    } else {
        line_range_at(value, state.caret).start
    };
    set_caret(state, caret, extend);
    state.preferred_x = None;
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

fn delete_word_backward(state: &mut TextInputState, value: &mut String) -> bool {
    let mut selection = state.selection();
    if selection.is_empty() && state.caret > 0 {
        selection.start = previous_word_boundary(value, state.caret);
    }
    delete_range(state, value, selection)
}

fn delete_word_forward(state: &mut TextInputState, value: &mut String) -> bool {
    let mut selection = state.selection();
    if selection.is_empty() && state.caret < value.len() {
        selection.end = next_word_boundary(value, state.caret);
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
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputError {
    InvalidLineCount,
    InvalidHistoryLimit,
    InvalidValidationDebounce,
}

impl fmt::Display for TextInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLineCount => "multiline text input must reserve at least one line",
            Self::InvalidHistoryLimit => "text input history limit must be positive",
            Self::InvalidValidationDebounce => "on-change validation debounce must be positive",
        })
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
        let display = display_text("ab", false, Some((1..2, "你好", Some((0, 3)))));
        assert_eq!(display.text, "a你好");
        assert_eq!(display_to_source(&display.boundaries, "a你".len()), 1);
        assert_eq!(
            display_to_source(&display.boundaries, display.text.len()),
            2
        );
        assert_eq!(display.composition_range, Some(1.."a你好".len()));
        assert_eq!(display.composition_selection, Some(1.."a你".len()));
        assert_eq!(display.composition_caret, Some("a你".len()));

        let password = display_text("ab", true, Some((1..2, "你好", None)));
        assert_eq!(password.text, "•••");
        assert!(!password.text.contains('你'));
    }

    #[test]
    fn word_and_line_selection_follow_unicode_boundaries() {
        // UAX #29 intentionally treats adjacent ideographs as independent
        // word-boundary units without a locale dictionary.
        assert_eq!(word_range_at("hello 世界", 7), 6..9);
        assert_eq!(line_range_at("one\ntwo\nthree", 5), 4..8);
    }

    #[test]
    fn zero_history_and_debounce_descriptors_are_rejected() {
        let theme = Arc::new(Theme::default());
        assert_eq!(
            TextInput::new("History", Reactive::new(String::new()), Arc::clone(&theme))
                .history_limit(0)
                .err(),
            Some(TextInputError::InvalidHistoryLimit)
        );
        assert_eq!(
            TextInput::new("Validation", Reactive::new(String::new()), theme)
                .validation(
                    TextInputValidationTrigger::OnChange {
                        debounce: Duration::ZERO,
                    },
                    Reactive::new(None),
                    |_| {},
                )
                .err(),
            Some(TextInputError::InvalidValidationDebounce)
        );
    }
}
