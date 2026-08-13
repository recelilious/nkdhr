use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use nkdhr_theme::{
    MAX_MOTION_CURVE_ABSOLUTE_PROGRESS, MAX_MOTION_CURVE_ANCHORS, MIN_MOTION_CURVE_TIME_GAP,
    MotionAnchorData, MotionCurveData, MotionTangentsData, MotionVectorData,
};
use serde::{Deserialize, Serialize};

use crate::{
    CompiledMotionCurve, MotionCurveCompileError, resolve_motion_curve_handles, split_motion_curve,
};

const MAX_CONSUMERS: usize = 256;
const MAX_HISTORY: usize = 512;
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;
const MAX_DURATION: Duration = Duration::from_secs(60);
const MIN_VIEW_SPAN: f64 = 1.0e-4;
const CLIPBOARD_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionCurveConsumerDomain {
    Spatial,
    Shape,
    Opacity,
    Color,
    BoundedScalar,
}

impl MotionCurveConsumerDomain {
    fn allows_overshoot(self) -> bool {
        matches!(self, Self::Spatial | Self::Shape)
    }

    fn allows_reverse(self) -> bool {
        matches!(self, Self::Spatial | Self::Shape)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionCurveConsumer {
    id: Arc<str>,
    domain: MotionCurveConsumerDomain,
}

impl MotionCurveConsumer {
    pub fn new(
        id: impl Into<Arc<str>>,
        domain: MotionCurveConsumerDomain,
    ) -> Result<Self, MotionCurveEditorError> {
        let id = id.into();
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
        {
            return Err(MotionCurveEditorError::InvalidConsumerId);
        }
        Ok(Self { id, domain })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn domain(&self) -> MotionCurveConsumerDomain {
        self.domain
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionCurveConsumerSet {
    consumers: Arc<[MotionCurveConsumer]>,
    allow_overshoot: bool,
    allow_reverse: bool,
}

impl MotionCurveConsumerSet {
    pub fn new(mut consumers: Vec<MotionCurveConsumer>) -> Result<Self, MotionCurveEditorError> {
        if consumers.len() > MAX_CONSUMERS {
            return Err(MotionCurveEditorError::TooManyConsumers);
        }
        consumers.sort_by(|left, right| left.id.cmp(&right.id));
        if consumers.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(MotionCurveEditorError::DuplicateConsumer);
        }
        let allow_overshoot = !consumers.is_empty()
            && consumers
                .iter()
                .all(|consumer| consumer.domain.allows_overshoot());
        let allow_reverse = !consumers.is_empty()
            && consumers
                .iter()
                .all(|consumer| consumer.domain.allows_reverse());
        Ok(Self {
            consumers: consumers.into(),
            allow_overshoot,
            allow_reverse,
        })
    }

    pub fn conservative() -> Self {
        Self {
            consumers: Arc::new([]),
            allow_overshoot: false,
            allow_reverse: false,
        }
    }

    pub fn consumers(&self) -> &[MotionCurveConsumer] {
        &self.consumers
    }

    pub fn allows_overshoot(&self) -> bool {
        self.allow_overshoot
    }

    pub fn allows_reverse(&self) -> bool {
        self.allow_reverse
    }

    fn compile(
        &self,
        curve: &MotionCurveData,
    ) -> Result<CompiledMotionCurve, MotionCurveEditorError> {
        if curve.allow_overshoot && !self.allow_overshoot {
            return Err(MotionCurveEditorError::OvershootUnsupported);
        }
        if curve.allow_reverse && !self.allow_reverse {
            return Err(MotionCurveEditorError::ReverseUnsupported);
        }
        CompiledMotionCurve::compile(curve).map_err(Into::into)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionEditorSnap {
    pub time_step: Option<f64>,
    pub progress_step: Option<f64>,
}

impl MotionEditorSnap {
    pub const NONE: Self = Self {
        time_step: None,
        progress_step: None,
    };

    pub fn validate(self) -> Result<(), MotionCurveEditorError> {
        if self.time_step.is_some_and(|value| {
            !value.is_finite() || !(MIN_MOTION_CURVE_TIME_GAP..=1.0).contains(&value)
        }) || self.progress_step.is_some_and(|value| {
            !value.is_finite() || value <= 0.0 || value > MAX_MOTION_CURVE_ABSOLUTE_PROGRESS
        }) {
            return Err(MotionCurveEditorError::InvalidSnap);
        }
        Ok(())
    }
}

impl Default for MotionEditorSnap {
    fn default() -> Self {
        Self {
            time_step: Some(0.05),
            progress_step: Some(0.05),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionCurveEditorConfig {
    pub history_limit: usize,
    pub snap: MotionEditorSnap,
    pub keyboard_time_step: f64,
    pub keyboard_progress_step: f64,
    pub coarse_multiplier: f64,
}

impl Default for MotionCurveEditorConfig {
    fn default() -> Self {
        Self {
            history_limit: 128,
            snap: MotionEditorSnap::default(),
            keyboard_time_step: 0.01,
            keyboard_progress_step: 0.01,
            coarse_multiplier: 10.0,
        }
    }
}

impl MotionCurveEditorConfig {
    fn validate(self) -> Result<(), MotionCurveEditorError> {
        if !(1..=MAX_HISTORY).contains(&self.history_limit)
            || !self.keyboard_time_step.is_finite()
            || self.keyboard_time_step <= 0.0
            || self.keyboard_time_step > 1.0
            || !self.keyboard_progress_step.is_finite()
            || self.keyboard_progress_step <= 0.0
            || self.keyboard_progress_step > MAX_MOTION_CURVE_ABSOLUTE_PROGRESS
            || !self.coarse_multiplier.is_finite()
            || self.coarse_multiplier < 1.0
            || self.coarse_multiplier > 100.0
        {
            return Err(MotionCurveEditorError::InvalidConfig);
        }
        self.snap.validate()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MotionGraphPoint {
    pub time: f64,
    pub progress: f64,
}

impl MotionGraphPoint {
    pub const fn new(time: f64, progress: f64) -> Self {
        Self { time, progress }
    }

    fn is_finite(self) -> bool {
        self.time.is_finite() && self.progress.is_finite()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionGraphViewport {
    time_start: f64,
    time_end: f64,
    progress_start: f64,
    progress_end: f64,
}

impl Default for MotionGraphViewport {
    fn default() -> Self {
        Self {
            time_start: 0.0,
            time_end: 1.0,
            progress_start: 0.0,
            progress_end: 1.0,
        }
    }
}

impl MotionGraphViewport {
    pub fn time_start(self) -> f64 {
        self.time_start
    }

    pub fn time_end(self) -> f64 {
        self.time_end
    }

    pub fn progress_start(self) -> f64 {
        self.progress_start
    }

    pub fn progress_end(self) -> f64 {
        self.progress_end
    }

    pub fn pan(
        &mut self,
        delta: MotionGraphPoint,
        allow_overshoot: bool,
    ) -> Result<bool, MotionCurveEditorError> {
        if !delta.is_finite() {
            return Err(MotionCurveEditorError::InvalidCoordinate);
        }
        let previous = *self;
        self.constrain(allow_overshoot);
        let time_span = self.time_end - self.time_start;
        self.time_start = (self.time_start + delta.time).clamp(0.0, 1.0 - time_span);
        self.time_end = self.time_start + time_span;
        let (minimum, maximum) = progress_limits(allow_overshoot);
        let progress_span = self.progress_end - self.progress_start;
        self.progress_start =
            (self.progress_start + delta.progress).clamp(minimum, maximum - progress_span);
        self.progress_end = self.progress_start + progress_span;
        Ok(*self != previous)
    }

    pub fn zoom(
        &mut self,
        anchor: MotionGraphPoint,
        time_factor: f64,
        progress_factor: f64,
        allow_overshoot: bool,
    ) -> Result<bool, MotionCurveEditorError> {
        if !anchor.is_finite()
            || !time_factor.is_finite()
            || time_factor <= 0.0
            || !progress_factor.is_finite()
            || progress_factor <= 0.0
        {
            return Err(MotionCurveEditorError::InvalidViewport);
        }
        let previous = *self;
        self.constrain(allow_overshoot);
        let time_ratio =
            ((anchor.time - self.time_start) / (self.time_end - self.time_start)).clamp(0.0, 1.0);
        let progress_ratio = ((anchor.progress - self.progress_start)
            / (self.progress_end - self.progress_start))
            .clamp(0.0, 1.0);
        let time_span = ((self.time_end - self.time_start) / time_factor).clamp(MIN_VIEW_SPAN, 1.0);
        let (minimum, maximum) = progress_limits(allow_overshoot);
        let progress_span = ((self.progress_end - self.progress_start) / progress_factor)
            .clamp(MIN_VIEW_SPAN, maximum - minimum);
        self.time_start = (anchor.time - time_span * time_ratio).clamp(0.0, 1.0 - time_span);
        self.time_end = self.time_start + time_span;
        self.progress_start = (anchor.progress - progress_span * progress_ratio)
            .clamp(minimum, maximum - progress_span);
        self.progress_end = self.progress_start + progress_span;
        Ok(*self != previous)
    }

    fn fit_curve(&mut self, curve: &CompiledMotionCurve, allow_overshoot: bool) {
        self.time_start = 0.0;
        self.time_end = 1.0;
        if allow_overshoot {
            let analysis = curve.analysis();
            let minimum = analysis.minimum_progress.min(0.0);
            let maximum = analysis.maximum_progress.max(1.0);
            let padding = ((maximum - minimum) * 0.05).max(0.05);
            self.progress_start = (minimum - padding).max(-MAX_MOTION_CURVE_ABSOLUTE_PROGRESS);
            self.progress_end = (maximum + padding).min(MAX_MOTION_CURVE_ABSOLUTE_PROGRESS);
        } else {
            self.progress_start = 0.0;
            self.progress_end = 1.0;
        }
    }

    fn constrain(&mut self, allow_overshoot: bool) {
        let time_span = (self.time_end - self.time_start).clamp(MIN_VIEW_SPAN, 1.0);
        let time_center = (self.time_start + self.time_end) * 0.5;
        self.time_start = (time_center - time_span * 0.5).clamp(0.0, 1.0 - time_span);
        self.time_end = self.time_start + time_span;

        let (minimum, maximum) = progress_limits(allow_overshoot);
        let progress_span =
            (self.progress_end - self.progress_start).clamp(MIN_VIEW_SPAN, maximum - minimum);
        let progress_center = (self.progress_start + self.progress_end) * 0.5;
        self.progress_start =
            (progress_center - progress_span * 0.5).clamp(minimum, maximum - progress_span);
        self.progress_end = self.progress_start + progress_span;
    }
}

fn progress_limits(allow_overshoot: bool) -> (f64, f64) {
    if allow_overshoot {
        (
            -MAX_MOTION_CURVE_ABSOLUTE_PROGRESS,
            MAX_MOTION_CURVE_ABSOLUTE_PROGRESS,
        )
    } else {
        (0.0, 1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionEditorTimeAxis {
    Normalized,
    RealTime,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionEditorAxis {
    pub mode: MotionEditorTimeAxis,
    pub duration: Duration,
}

impl MotionEditorAxis {
    pub fn display_time(self, normalized_time: f64) -> f64 {
        match self.mode {
            MotionEditorTimeAxis::Normalized => normalized_time,
            MotionEditorTimeAxis::RealTime => normalized_time * self.duration.as_secs_f64(),
        }
    }

    pub fn normalized_time(self, displayed_time: f64) -> Result<f64, MotionCurveEditorError> {
        if !displayed_time.is_finite() {
            return Err(MotionCurveEditorError::InvalidCoordinate);
        }
        Ok(match self.mode {
            MotionEditorTimeAxis::Normalized => displayed_time,
            MotionEditorTimeAxis::RealTime => displayed_time / self.duration.as_secs_f64(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionCurveSource {
    Inherited,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionTangentMode {
    Automatic,
    Continuous,
    Broken,
    Corner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionEditorTangentSide {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MotionEditorEditId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MotionEditorPlayback {
    Paused,
    Playing { started: Duration, origin: f64 },
}

#[derive(Debug, Clone)]
struct DocumentState {
    curve_override: Option<MotionCurveData>,
    duration_override: Option<Duration>,
}

impl PartialEq for DocumentState {
    fn eq(&self, other: &Self) -> bool {
        self.curve_override == other.curve_override
            && self.duration_override == other.duration_override
    }
}

#[derive(Debug, Clone)]
struct ActiveTransaction {
    id: MotionEditorEditId,
    document: DocumentState,
    selection: BTreeSet<usize>,
    primary_selection: Option<usize>,
    playhead: f64,
    viewport: MotionGraphViewport,
    playback: MotionEditorPlayback,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MotionEditorTransactionOutcome {
    pub document_changed: bool,
    pub transient_changed: bool,
}

#[derive(Debug, Clone)]
pub struct MotionCurveEditorSnapshot {
    pub curve: MotionCurveData,
    pub curve_source: MotionCurveSource,
    pub inherited_curve: MotionCurveData,
    pub duration: Duration,
    pub duration_source: MotionCurveSource,
    pub inherited_duration: Duration,
    pub selection: BTreeSet<usize>,
    pub primary_selection: Option<usize>,
    pub playhead: f64,
    pub viewport: MotionGraphViewport,
    pub time_axis: MotionEditorTimeAxis,
    pub playback: MotionEditorPlayback,
    pub looping: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub document_generation: u64,
}

#[derive(Debug, Clone)]
pub struct MotionEditorPreview {
    pub generation: u64,
    pub curve: MotionCurveData,
    pub compiled: CompiledMotionCurve,
    pub inherited_curve: MotionCurveData,
    pub inherited_compiled: CompiledMotionCurve,
    pub duration: Duration,
    pub playhead: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionAnchorClipboardData {
    version: u32,
    time_origin: f64,
    anchors: Vec<MotionAnchorData>,
}

impl MotionAnchorClipboardData {
    pub fn to_json(&self) -> Result<String, MotionCurveEditorError> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|error| MotionCurveEditorError::ClipboardSyntax(error.to_string()))
    }

    pub fn from_json(text: &str) -> Result<Self, MotionCurveEditorError> {
        if text.len() > MAX_CLIPBOARD_BYTES {
            return Err(MotionCurveEditorError::ClipboardTooLarge);
        }
        let data: Self = serde_json::from_str(text)
            .map_err(|error| MotionCurveEditorError::ClipboardSyntax(error.to_string()))?;
        data.validate()?;
        Ok(data)
    }

    pub fn anchors(&self) -> &[MotionAnchorData] {
        &self.anchors
    }

    pub fn time_origin(&self) -> f64 {
        self.time_origin
    }

    fn validate(&self) -> Result<(), MotionCurveEditorError> {
        if self.version != CLIPBOARD_VERSION
            || self.anchors.is_empty()
            || self.anchors.len() > MAX_MOTION_CURVE_ANCHORS - 2
            || !self.time_origin.is_finite()
            || self.anchors.iter().any(|anchor| {
                !anchor.time.is_finite()
                    || !anchor.progress.is_finite()
                    || !(0.0..=1.0).contains(&anchor.time)
                    || anchor.progress.abs() > MAX_MOTION_CURVE_ABSOLUTE_PROGRESS
            })
            || self
                .anchors
                .windows(2)
                .any(|pair| pair[1].time - pair[0].time < MIN_MOTION_CURVE_TIME_GAP)
        {
            return Err(MotionCurveEditorError::InvalidClipboard);
        }
        Ok(())
    }
}

pub struct MotionCurveEditor {
    inherited_curve: MotionCurveData,
    inherited_compiled: CompiledMotionCurve,
    inherited_duration: Duration,
    document: DocumentState,
    compiled: CompiledMotionCurve,
    consumers: MotionCurveConsumerSet,
    config: MotionCurveEditorConfig,
    selection: BTreeSet<usize>,
    primary_selection: Option<usize>,
    undo: VecDeque<DocumentState>,
    redo: Vec<DocumentState>,
    transaction: Option<ActiveTransaction>,
    viewport: MotionGraphViewport,
    time_axis: MotionEditorTimeAxis,
    playhead: f64,
    playback: MotionEditorPlayback,
    looping: bool,
    document_generation: u64,
    preview_generation: u64,
    delivered_preview_generation: u64,
}

impl fmt::Debug for MotionCurveEditor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MotionCurveEditor")
            .field("document_generation", &self.document_generation)
            .field("preview_generation", &self.preview_generation)
            .field("curve_source", &self.curve_source())
            .field("duration_source", &self.duration_source())
            .field("selection", &self.selection)
            .field(
                "transaction",
                &self.transaction.as_ref().map(|value| value.id),
            )
            .finish()
    }
}

impl MotionCurveEditor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inherited_curve: MotionCurveData,
        curve_override: Option<MotionCurveData>,
        inherited_duration: Duration,
        duration_override: Option<Duration>,
        consumers: MotionCurveConsumerSet,
        config: MotionCurveEditorConfig,
    ) -> Result<Self, MotionCurveEditorError> {
        config.validate()?;
        validate_duration(inherited_duration)?;
        if let Some(duration) = duration_override {
            validate_duration(duration)?;
        }
        let inherited_compiled = consumers.compile(&inherited_curve)?;
        let compiled = consumers.compile(curve_override.as_ref().unwrap_or(&inherited_curve))?;
        Ok(Self {
            inherited_curve,
            inherited_compiled,
            inherited_duration,
            document: DocumentState {
                curve_override,
                duration_override,
            },
            compiled,
            consumers,
            config,
            selection: BTreeSet::new(),
            primary_selection: None,
            undo: VecDeque::new(),
            redo: Vec::new(),
            transaction: None,
            viewport: MotionGraphViewport::default(),
            time_axis: MotionEditorTimeAxis::Normalized,
            playhead: 0.0,
            playback: MotionEditorPlayback::Paused,
            looping: true,
            document_generation: 1,
            preview_generation: 1,
            delivered_preview_generation: 0,
        })
    }

    pub fn snapshot(&self) -> MotionCurveEditorSnapshot {
        MotionCurveEditorSnapshot {
            curve: self.effective_curve().clone(),
            curve_source: self.curve_source(),
            inherited_curve: self.inherited_curve.clone(),
            duration: self.effective_duration(),
            duration_source: self.duration_source(),
            inherited_duration: self.inherited_duration,
            selection: self.selection.clone(),
            primary_selection: self.primary_selection,
            playhead: self.playhead,
            viewport: self.viewport,
            time_axis: self.time_axis,
            playback: self.playback,
            looping: self.looping,
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
            document_generation: self.document_generation,
        }
    }

    pub fn effective_curve(&self) -> &MotionCurveData {
        self.document
            .curve_override
            .as_ref()
            .unwrap_or(&self.inherited_curve)
    }

    pub fn compiled(&self) -> &CompiledMotionCurve {
        &self.compiled
    }

    pub fn effective_duration(&self) -> Duration {
        self.document
            .duration_override
            .unwrap_or(self.inherited_duration)
    }

    pub fn curve_source(&self) -> MotionCurveSource {
        if self.document.curve_override.is_some() {
            MotionCurveSource::Explicit
        } else {
            MotionCurveSource::Inherited
        }
    }

    pub fn duration_source(&self) -> MotionCurveSource {
        if self.document.duration_override.is_some() {
            MotionCurveSource::Explicit
        } else {
            MotionCurveSource::Inherited
        }
    }

    pub fn consumers(&self) -> &MotionCurveConsumerSet {
        &self.consumers
    }

    pub fn config(&self) -> MotionCurveEditorConfig {
        self.config
    }

    pub fn document_generation(&self) -> u64 {
        self.document_generation
    }

    pub fn preview_generation(&self) -> u64 {
        self.preview_generation
    }

    pub fn preview_pending(&self) -> bool {
        self.delivered_preview_generation != self.preview_generation
    }

    pub fn primary_selection(&self) -> Option<usize> {
        self.primary_selection
    }

    pub fn anchor_point(&self, index: usize) -> Result<MotionGraphPoint, MotionCurveEditorError> {
        self.effective_curve()
            .anchors
            .get(index)
            .map(|anchor| MotionGraphPoint::new(anchor.time, anchor.progress))
            .ok_or(MotionCurveEditorError::MissingAnchor(index))
    }

    pub fn axis(&self) -> MotionEditorAxis {
        MotionEditorAxis {
            mode: self.time_axis,
            duration: self.effective_duration(),
        }
    }

    pub fn set_time_axis(&mut self, axis: MotionEditorTimeAxis) -> bool {
        if self.time_axis == axis {
            false
        } else {
            self.time_axis = axis;
            true
        }
    }

    pub fn viewport(&self) -> MotionGraphViewport {
        self.viewport
    }

    pub fn fit_viewport(&mut self) {
        self.viewport
            .fit_curve(&self.compiled, self.effective_curve().allow_overshoot);
    }

    /// Restore the canonical one-to-one normalized graph view. Overshoot may
    /// remain authored in the curve, but the viewport returns to `0..=1` on
    /// both axes until the user fits or zooms again.
    pub fn reset_viewport(&mut self) -> bool {
        let previous = self.viewport;
        self.viewport = MotionGraphViewport::default();
        self.viewport
            .constrain(self.effective_curve().allow_overshoot);
        self.viewport != previous
    }

    pub fn pan_viewport(
        &mut self,
        delta: MotionGraphPoint,
    ) -> Result<bool, MotionCurveEditorError> {
        self.viewport
            .pan(delta, self.effective_curve().allow_overshoot)
    }

    pub fn zoom_viewport(
        &mut self,
        anchor: MotionGraphPoint,
        time_factor: f64,
        progress_factor: f64,
    ) -> Result<bool, MotionCurveEditorError> {
        self.viewport.zoom(
            anchor,
            time_factor,
            progress_factor,
            self.effective_curve().allow_overshoot,
        )
    }

    pub fn select_anchor(
        &mut self,
        index: usize,
        extend: bool,
        toggle: bool,
    ) -> Result<bool, MotionCurveEditorError> {
        if index >= self.effective_curve().anchors.len() {
            return Err(MotionCurveEditorError::MissingAnchor(index));
        }
        let previous = self.selection.clone();
        if !extend && !toggle {
            self.selection.clear();
        }
        if toggle && self.selection.contains(&index) {
            self.selection.remove(&index);
            if self.primary_selection == Some(index) {
                self.primary_selection = self.selection.iter().next_back().copied();
            }
        } else {
            self.selection.insert(index);
            self.primary_selection = Some(index);
        }
        Ok(previous != self.selection)
    }

    pub fn select_all_editable(&mut self) -> bool {
        let count = self.effective_curve().anchors.len();
        let next = (1..count.saturating_sub(1)).collect::<BTreeSet<_>>();
        let changed = self.selection != next;
        self.selection = next;
        self.primary_selection = self.selection.iter().next_back().copied();
        changed
    }

    pub fn clear_selection(&mut self) -> bool {
        let changed = !self.selection.is_empty();
        self.selection.clear();
        self.primary_selection = None;
        changed
    }

    pub fn begin_transaction(
        &mut self,
        id: MotionEditorEditId,
    ) -> Result<(), MotionCurveEditorError> {
        if self.transaction.is_some() {
            return Err(MotionCurveEditorError::InteractionBusy);
        }
        self.transaction = Some(ActiveTransaction {
            id,
            document: self.document.clone(),
            selection: self.selection.clone(),
            primary_selection: self.primary_selection,
            playhead: self.playhead,
            viewport: self.viewport,
            playback: self.playback,
        });
        Ok(())
    }

    pub fn commit_transaction(
        &mut self,
        id: MotionEditorEditId,
    ) -> Result<MotionEditorTransactionOutcome, MotionCurveEditorError> {
        let transaction = self.take_transaction(id)?;
        let document_changed = transaction.document != self.document;
        if document_changed {
            self.push_undo(transaction.document);
        }
        Ok(MotionEditorTransactionOutcome {
            document_changed,
            transient_changed: transaction.selection != self.selection
                || transaction.primary_selection != self.primary_selection
                || transaction.playhead != self.playhead
                || transaction.viewport != self.viewport
                || transaction.playback != self.playback,
        })
    }

    pub fn cancel_transaction(
        &mut self,
        id: MotionEditorEditId,
    ) -> Result<MotionEditorTransactionOutcome, MotionCurveEditorError> {
        let transaction = self.take_transaction(id)?;
        let document_changed = transaction.document != self.document;
        let transient_changed = transaction.selection != self.selection
            || transaction.primary_selection != self.primary_selection
            || transaction.playhead != self.playhead
            || transaction.viewport != self.viewport
            || transaction.playback != self.playback;
        if document_changed {
            self.restore_document(transaction.document)?;
        }
        self.selection = transaction.selection;
        self.primary_selection = transaction.primary_selection;
        if transaction.playhead != self.playhead {
            self.playhead = transaction.playhead;
            self.mark_preview();
        }
        self.viewport = transaction.viewport;
        self.playback = transaction.playback;
        Ok(MotionEditorTransactionOutcome {
            document_changed,
            transient_changed,
        })
    }

    pub fn insert_exact(&mut self, time: f64) -> Result<usize, MotionCurveEditorError> {
        let index = self
            .effective_curve()
            .anchors
            .partition_point(|anchor| anchor.time < time);
        let curve = split_motion_curve(self.effective_curve(), time)?;
        self.apply_curve(curve)?;
        self.selection.clear();
        self.selection.insert(index);
        self.primary_selection = Some(index);
        Ok(index)
    }

    pub fn set_anchor_numeric(
        &mut self,
        index: usize,
        point: MotionGraphPoint,
    ) -> Result<bool, MotionCurveEditorError> {
        if !point.is_finite() {
            return Err(MotionCurveEditorError::InvalidCoordinate);
        }
        let anchor_count = self.effective_curve().anchors.len();
        if index >= anchor_count {
            return Err(MotionCurveEditorError::MissingAnchor(index));
        }
        if index == 0 || index == anchor_count - 1 {
            return Err(MotionCurveEditorError::FixedEndpoint);
        }
        let mut curve = self.effective_curve().clone();
        let anchor = curve
            .anchors
            .get_mut(index)
            .ok_or(MotionCurveEditorError::MissingAnchor(index))?;
        anchor.time = point.time;
        anchor.progress = point.progress;
        self.apply_curve(curve)
    }

    pub fn move_selected_to(
        &mut self,
        primary: usize,
        point: MotionGraphPoint,
        snapping: bool,
    ) -> Result<bool, MotionCurveEditorError> {
        if !point.is_finite() {
            return Err(MotionCurveEditorError::InvalidCoordinate);
        }
        let curve = self.effective_curve().clone();
        if primary >= curve.anchors.len() {
            return Err(MotionCurveEditorError::MissingAnchor(primary));
        }
        if primary == 0 || primary == curve.anchors.len() - 1 {
            return Err(MotionCurveEditorError::FixedEndpoint);
        }
        if !self.selection.contains(&primary) {
            self.selection.clear();
            self.selection.insert(primary);
            self.primary_selection = Some(primary);
        }
        let anchor = curve
            .anchors
            .get(primary)
            .ok_or(MotionCurveEditorError::MissingAnchor(primary))?;
        let target_time = if snapping {
            snap(point.time, self.config.snap.time_step)
        } else {
            point.time
        };
        let target_progress = if snapping {
            snap(point.progress, self.config.snap.progress_step)
        } else {
            point.progress
        };
        let mut delta_time = target_time - anchor.time;
        let mut delta_progress = target_progress - anchor.progress;
        let movable = self
            .selection
            .iter()
            .copied()
            .filter(|index| *index > 0 && index + 1 < curve.anchors.len())
            .collect::<BTreeSet<_>>();
        if movable.is_empty() {
            return Err(MotionCurveEditorError::FixedEndpoint);
        }
        let resolved = resolve_motion_curve_handles(&curve)?;
        let mut minimum_delta: f64 = -1.0;
        let mut maximum_delta: f64 = 1.0;
        for index in &movable {
            let (incoming, outgoing) = broken_vectors(&resolved.anchors[*index].tangents)
                .ok_or(MotionCurveEditorError::HandleUnavailable)?;
            if !movable.contains(&(index - 1)) {
                minimum_delta = minimum_delta.max(
                    curve.anchors[index - 1].time + MIN_MOTION_CURVE_TIME_GAP
                        - curve.anchors[*index].time,
                );
                let (_, previous_outgoing) = broken_vectors(&resolved.anchors[index - 1].tangents)
                    .ok_or(MotionCurveEditorError::HandleUnavailable)?;
                minimum_delta = minimum_delta.max(
                    resolved.anchors[index - 1].time + previous_outgoing.time
                        - resolved.anchors[*index].time
                        - incoming.time,
                );
            }
            if !movable.contains(&(index + 1)) {
                maximum_delta = maximum_delta.min(
                    curve.anchors[index + 1].time
                        - MIN_MOTION_CURVE_TIME_GAP
                        - curve.anchors[*index].time,
                );
                let (next_incoming, _) = broken_vectors(&resolved.anchors[index + 1].tangents)
                    .ok_or(MotionCurveEditorError::HandleUnavailable)?;
                maximum_delta = maximum_delta.min(
                    resolved.anchors[index + 1].time + next_incoming.time
                        - resolved.anchors[*index].time
                        - outgoing.time,
                );
            }
        }
        delta_time = delta_time.clamp(minimum_delta, maximum_delta);
        let (minimum_progress, maximum_progress) =
            progress_limits(curve.allow_overshoot && self.consumers.allows_overshoot());
        let mut minimum_progress_delta = movable.iter().fold(f64::NEG_INFINITY, |value, index| {
            value.max(minimum_progress - curve.anchors[*index].progress)
        });
        let mut maximum_progress_delta = movable.iter().fold(f64::INFINITY, |value, index| {
            value.min(maximum_progress - curve.anchors[*index].progress)
        });
        if !curve.allow_reverse {
            for index in &movable {
                if !movable.contains(&(index - 1)) {
                    minimum_progress_delta = minimum_progress_delta
                        .max(curve.anchors[index - 1].progress - curve.anchors[*index].progress);
                    let (_, previous_outgoing) =
                        broken_vectors(&resolved.anchors[index - 1].tangents)
                            .ok_or(MotionCurveEditorError::HandleUnavailable)?;
                    let (incoming, _) = broken_vectors(&resolved.anchors[*index].tangents)
                        .ok_or(MotionCurveEditorError::HandleUnavailable)?;
                    minimum_progress_delta = minimum_progress_delta.max(
                        resolved.anchors[index - 1].progress + previous_outgoing.progress
                            - resolved.anchors[*index].progress
                            - incoming.progress,
                    );
                }
                if !movable.contains(&(index + 1)) {
                    maximum_progress_delta = maximum_progress_delta
                        .min(curve.anchors[index + 1].progress - curve.anchors[*index].progress);
                    let (_, outgoing) = broken_vectors(&resolved.anchors[*index].tangents)
                        .ok_or(MotionCurveEditorError::HandleUnavailable)?;
                    let (next_incoming, _) = broken_vectors(&resolved.anchors[index + 1].tangents)
                        .ok_or(MotionCurveEditorError::HandleUnavailable)?;
                    maximum_progress_delta = maximum_progress_delta.min(
                        resolved.anchors[index + 1].progress + next_incoming.progress
                            - resolved.anchors[*index].progress
                            - outgoing.progress,
                    );
                }
            }
        }
        delta_progress = delta_progress.clamp(minimum_progress_delta, maximum_progress_delta);
        self.move_selected_by(MotionGraphPoint::new(delta_time, delta_progress))
    }

    pub fn move_selected_by(
        &mut self,
        delta: MotionGraphPoint,
    ) -> Result<bool, MotionCurveEditorError> {
        if !delta.is_finite() {
            return Err(MotionCurveEditorError::InvalidCoordinate);
        }
        let mut curve = self.effective_curve().clone();
        if self.selection.is_empty() {
            return Err(MotionCurveEditorError::EmptySelection);
        }
        for index in &self.selection {
            if *index == 0 || index + 1 == curve.anchors.len() {
                continue;
            }
            curve.anchors[*index].time += delta.time;
            curve.anchors[*index].progress += delta.progress;
        }
        self.apply_curve(curve)
    }

    pub fn delete_selection(&mut self) -> Result<bool, MotionCurveEditorError> {
        let mut curve = self.effective_curve().clone();
        let removable = self
            .selection
            .iter()
            .copied()
            .filter(|index| *index > 0 && index + 1 < curve.anchors.len())
            .collect::<Vec<_>>();
        if removable.is_empty() {
            return Ok(false);
        }
        for index in removable.iter().rev() {
            curve.anchors.remove(*index);
        }
        let changed = self.apply_curve(curve)?;
        self.clear_selection();
        Ok(changed)
    }

    pub fn set_tangent_mode(
        &mut self,
        index: usize,
        mode: MotionTangentMode,
    ) -> Result<bool, MotionCurveEditorError> {
        if index >= self.effective_curve().anchors.len() {
            return Err(MotionCurveEditorError::MissingAnchor(index));
        }
        let resolved = resolve_motion_curve_handles(self.effective_curve())?;
        let (incoming, outgoing) = broken_vectors(&resolved.anchors[index].tangents)
            .ok_or(MotionCurveEditorError::HandleUnavailable)?;
        let tangents = match mode {
            MotionTangentMode::Automatic => MotionTangentsData::Automatic,
            MotionTangentMode::Corner => MotionTangentsData::Corner,
            MotionTangentMode::Broken => MotionTangentsData::Broken { incoming, outgoing },
            MotionTangentMode::Continuous => continuous_from_handles(incoming, outgoing),
        };
        let mut curve = self.effective_curve().clone();
        curve.anchors[index].tangents = tangents;
        self.apply_curve(curve)
    }

    pub fn set_handle_numeric(
        &mut self,
        index: usize,
        side: MotionEditorTangentSide,
        point: MotionGraphPoint,
    ) -> Result<bool, MotionCurveEditorError> {
        if !point.is_finite() {
            return Err(MotionCurveEditorError::InvalidCoordinate);
        }
        let mut curve = self.effective_curve().clone();
        let anchor_count = curve.anchors.len();
        let anchor = curve
            .anchors
            .get_mut(index)
            .ok_or(MotionCurveEditorError::MissingAnchor(index))?;
        if (index == 0 && side == MotionEditorTangentSide::Incoming)
            || (index + 1 == anchor_count && side == MotionEditorTangentSide::Outgoing)
        {
            return Err(MotionCurveEditorError::HandleUnavailable);
        }
        let vector =
            MotionVectorData::new(point.time - anchor.time, point.progress - anchor.progress);
        set_tangent_vector(&mut anchor.tangents, side, vector)?;
        self.apply_curve(curve)
    }

    pub fn set_permissions(
        &mut self,
        allow_overshoot: bool,
        allow_reverse: bool,
    ) -> Result<bool, MotionCurveEditorError> {
        let mut curve = self.effective_curve().clone();
        curve.allow_overshoot = allow_overshoot;
        curve.allow_reverse = allow_reverse;
        self.apply_curve(curve)
    }

    pub fn set_duration(&mut self, duration: Duration) -> Result<bool, MotionCurveEditorError> {
        validate_duration(duration)?;
        if self.document.duration_override == Some(duration) {
            return Ok(false);
        }
        self.record_before_atomic();
        self.document.duration_override = Some(duration);
        self.document_changed();
        Ok(true)
    }

    pub fn reset_curve(&mut self) -> Result<bool, MotionCurveEditorError> {
        if self.document.curve_override.is_none() {
            return Ok(false);
        }
        self.record_before_atomic();
        self.document.curve_override = None;
        self.compiled = self.inherited_compiled.clone();
        self.viewport
            .constrain(self.inherited_curve.allow_overshoot);
        self.reconcile_selection();
        self.document_changed();
        Ok(true)
    }

    pub fn reset_duration(&mut self) -> bool {
        if self.document.duration_override.is_none() {
            return false;
        }
        self.record_before_atomic();
        self.document.duration_override = None;
        self.document_changed();
        true
    }

    pub fn replace_inherited(
        &mut self,
        curve: MotionCurveData,
        duration: Duration,
    ) -> Result<bool, MotionCurveEditorError> {
        if self.transaction.is_some() {
            return Err(MotionCurveEditorError::InteractionBusy);
        }
        validate_duration(duration)?;
        let compiled = self.consumers.compile(&curve)?;
        if let Some(override_curve) = &self.document.curve_override {
            self.consumers.compile(override_curve)?;
        }
        let changed = self.inherited_curve != curve || self.inherited_duration != duration;
        if !changed {
            return Ok(false);
        }
        self.inherited_curve = curve;
        self.inherited_compiled = compiled;
        self.inherited_duration = duration;
        if self.document.curve_override.is_none() {
            self.compiled = self.inherited_compiled.clone();
            self.viewport
                .constrain(self.inherited_curve.allow_overshoot);
        }
        self.undo.clear();
        self.redo.clear();
        self.reconcile_selection();
        self.document_changed();
        Ok(true)
    }

    /// Rebinds the editor to a new set of property consumers. This is host
    /// context, not authored document state, so it does not create an undo
    /// entry. History is cleared because an older state may be invalid under
    /// the new capability intersection.
    pub fn replace_consumers(
        &mut self,
        consumers: MotionCurveConsumerSet,
    ) -> Result<bool, MotionCurveEditorError> {
        if self.transaction.is_some() {
            return Err(MotionCurveEditorError::InteractionBusy);
        }
        if self.consumers == consumers {
            return Ok(false);
        }
        let inherited_compiled = consumers.compile(&self.inherited_curve)?;
        let compiled = consumers.compile(self.effective_curve())?;
        self.consumers = consumers;
        self.inherited_compiled = inherited_compiled;
        self.compiled = compiled;
        self.undo.clear();
        self.redo.clear();
        self.mark_preview();
        Ok(true)
    }

    pub fn copy_selection(&self) -> Result<MotionAnchorClipboardData, MotionCurveEditorError> {
        let resolved = resolve_motion_curve_handles(self.effective_curve())?;
        let anchors = self
            .selection
            .iter()
            .copied()
            .filter(|index| *index > 0 && index + 1 < self.effective_curve().anchors.len())
            .map(|index| resolved.anchors[index].clone())
            .collect::<Vec<_>>();
        let time_origin = anchors
            .first()
            .ok_or(MotionCurveEditorError::EmptySelection)?
            .time;
        let data = MotionAnchorClipboardData {
            version: CLIPBOARD_VERSION,
            time_origin,
            anchors,
        };
        data.validate()?;
        Ok(data)
    }

    pub fn paste_at(
        &mut self,
        clipboard: &MotionAnchorClipboardData,
        time: f64,
        progress_delta: f64,
    ) -> Result<bool, MotionCurveEditorError> {
        clipboard.validate()?;
        if !time.is_finite() || !progress_delta.is_finite() {
            return Err(MotionCurveEditorError::InvalidCoordinate);
        }
        let mut pasted = clipboard.anchors.clone();
        for anchor in &mut pasted {
            anchor.time += time - clipboard.time_origin;
            anchor.progress += progress_delta;
        }
        let mut curve = self.effective_curve().clone();
        if curve.anchors.len() + pasted.len() > MAX_MOTION_CURVE_ANCHORS {
            return Err(MotionCurveEditorError::TooManyAnchors);
        }
        curve.anchors.extend(pasted.iter().cloned());
        curve
            .anchors
            .sort_by(|left, right| left.time.total_cmp(&right.time));
        if matches!(
            self.consumers.compile(&curve),
            Err(MotionCurveEditorError::Compile(
                MotionCurveCompileError::NonMonotonicHandles { .. }
                    | MotionCurveCompileError::InvalidContinuousDirection { .. }
                    | MotionCurveCompileError::OvershootNotAllowed { .. }
                    | MotionCurveCompileError::ReverseNotAllowed
            ))
        ) {
            curve = resolve_motion_curve_handles(self.effective_curve())?;
            curve.anchors.extend(pasted.iter().cloned());
            curve
                .anchors
                .sort_by(|left, right| left.time.total_cmp(&right.time));
            constrain_handle_order(&mut curve);
        }
        let changed = self.apply_curve(curve)?;
        self.selection.clear();
        for pasted_anchor in pasted {
            if let Some(index) = self.effective_curve().anchors.iter().position(|anchor| {
                anchor.time == pasted_anchor.time && anchor.progress == pasted_anchor.progress
            }) {
                self.selection.insert(index);
                self.primary_selection = Some(index);
            }
        }
        Ok(changed)
    }

    pub fn undo(&mut self) -> Result<bool, MotionCurveEditorError> {
        if self.transaction.is_some() {
            return Err(MotionCurveEditorError::InteractionBusy);
        }
        let Some(previous) = self.undo.pop_back() else {
            return Ok(false);
        };
        self.redo.push(self.document.clone());
        self.restore_document(previous)?;
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool, MotionCurveEditorError> {
        if self.transaction.is_some() {
            return Err(MotionCurveEditorError::InteractionBusy);
        }
        let Some(next) = self.redo.pop() else {
            return Ok(false);
        };
        self.push_undo_without_clearing_redo(self.document.clone());
        self.restore_document(next)?;
        Ok(true)
    }

    pub fn playhead(&self) -> f64 {
        self.playhead
    }

    pub fn scrub_playhead(&mut self, time: f64) -> Result<bool, MotionCurveEditorError> {
        if !time.is_finite() {
            return Err(MotionCurveEditorError::InvalidCoordinate);
        }
        let time = time.clamp(0.0, 1.0);
        let changed = self.playhead != time || self.playback != MotionEditorPlayback::Paused;
        self.playhead = time;
        self.playback = MotionEditorPlayback::Paused;
        if changed {
            self.mark_preview();
        }
        Ok(changed)
    }

    pub fn playback(&self) -> MotionEditorPlayback {
        self.playback
    }

    pub fn set_looping(&mut self, looping: bool) -> bool {
        let changed = self.looping != looping;
        self.looping = looping;
        changed
    }

    pub fn toggle_playback(&mut self, now: Duration) -> bool {
        match self.playback {
            MotionEditorPlayback::Paused => {
                let origin = if self.playhead >= 1.0 {
                    self.playhead = 0.0;
                    self.mark_preview();
                    0.0
                } else {
                    self.playhead
                };
                self.playback = MotionEditorPlayback::Playing {
                    started: now,
                    origin,
                };
            }
            MotionEditorPlayback::Playing { .. } => {
                self.playback = MotionEditorPlayback::Paused;
            }
        }
        true
    }

    pub fn advance_playback(&mut self, now: Duration) -> bool {
        let MotionEditorPlayback::Playing { started, origin } = self.playback else {
            return false;
        };
        let elapsed = now.saturating_sub(started).as_secs_f64();
        let raw = origin + elapsed / self.effective_duration().as_secs_f64();
        let next = if self.looping {
            raw.rem_euclid(1.0)
        } else {
            raw.min(1.0)
        };
        let changed = self.playhead != next;
        self.playhead = next;
        if !self.looping && raw >= 1.0 {
            self.playback = MotionEditorPlayback::Paused;
        }
        if changed {
            self.mark_preview();
        }
        changed
    }

    pub fn take_preview(&mut self) -> Option<MotionEditorPreview> {
        if self.delivered_preview_generation == self.preview_generation {
            return None;
        }
        self.delivered_preview_generation = self.preview_generation;
        Some(MotionEditorPreview {
            generation: self.preview_generation,
            curve: self.effective_curve().clone(),
            compiled: self.compiled.clone(),
            inherited_curve: self.inherited_curve.clone(),
            inherited_compiled: self.inherited_compiled.clone(),
            duration: self.effective_duration(),
            playhead: self.playhead,
        })
    }

    fn apply_curve(&mut self, curve: MotionCurveData) -> Result<bool, MotionCurveEditorError> {
        let compiled = self.consumers.compile(&curve)?;
        if self.document.curve_override.as_ref() == Some(&curve) {
            return Ok(false);
        }
        self.record_before_atomic();
        self.document.curve_override = Some(curve);
        self.compiled = compiled;
        self.viewport
            .constrain(self.effective_curve().allow_overshoot);
        self.reconcile_selection();
        self.document_changed();
        Ok(true)
    }

    fn record_before_atomic(&mut self) {
        if self.transaction.is_none() {
            self.push_undo(self.document.clone());
        }
    }

    fn push_undo(&mut self, state: DocumentState) {
        self.push_undo_without_clearing_redo(state);
        self.redo.clear();
    }

    fn push_undo_without_clearing_redo(&mut self, state: DocumentState) {
        self.undo.push_back(state);
        while self.undo.len() > self.config.history_limit {
            self.undo.pop_front();
        }
    }

    fn restore_document(&mut self, document: DocumentState) -> Result<(), MotionCurveEditorError> {
        self.compiled = self.consumers.compile(
            document
                .curve_override
                .as_ref()
                .unwrap_or(&self.inherited_curve),
        )?;
        self.document = document;
        self.viewport
            .constrain(self.effective_curve().allow_overshoot);
        self.reconcile_selection();
        self.document_changed();
        Ok(())
    }

    fn document_changed(&mut self) {
        self.document_generation = self.document_generation.wrapping_add(1).max(1);
        self.mark_preview();
    }

    fn mark_preview(&mut self) {
        self.preview_generation = self.preview_generation.wrapping_add(1).max(1);
    }

    fn reconcile_selection(&mut self) {
        let count = self.effective_curve().anchors.len();
        self.selection.retain(|index| *index < count);
        if self.primary_selection.is_some_and(|index| index >= count) {
            self.primary_selection = self.selection.iter().next_back().copied();
        }
    }

    fn take_transaction(
        &mut self,
        id: MotionEditorEditId,
    ) -> Result<ActiveTransaction, MotionCurveEditorError> {
        match self.transaction.take() {
            Some(transaction) if transaction.id == id => Ok(transaction),
            Some(transaction) => {
                self.transaction = Some(transaction);
                Err(MotionCurveEditorError::WrongInteraction)
            }
            None => Err(MotionCurveEditorError::NoInteraction),
        }
    }
}

fn validate_duration(duration: Duration) -> Result<(), MotionCurveEditorError> {
    if duration.is_zero() || duration > MAX_DURATION {
        Err(MotionCurveEditorError::InvalidDuration)
    } else {
        Ok(())
    }
}

fn snap(value: f64, step: Option<f64>) -> f64 {
    step.map_or(value, |step| (value / step).round() * step)
}

fn broken_vectors(tangents: &MotionTangentsData) -> Option<(MotionVectorData, MotionVectorData)> {
    match tangents {
        MotionTangentsData::Broken { incoming, outgoing } => Some((*incoming, *outgoing)),
        _ => None,
    }
}

fn continuous_from_handles(
    incoming: MotionVectorData,
    outgoing: MotionVectorData,
) -> MotionTangentsData {
    let incoming_length = incoming.time.hypot(incoming.progress);
    let outgoing_length = outgoing.time.hypot(outgoing.progress);
    let candidate = if outgoing_length > f64::EPSILON {
        MotionVectorData::new(
            outgoing.time / outgoing_length,
            outgoing.progress / outgoing_length,
        )
    } else if incoming_length > f64::EPSILON {
        MotionVectorData::new(
            -incoming.time / incoming_length,
            -incoming.progress / incoming_length,
        )
    } else {
        MotionVectorData::new(1.0, 0.0)
    };
    let direction = if candidate.time > 0.0 {
        candidate
    } else {
        MotionVectorData::new(1.0, 0.0)
    };
    MotionTangentsData::Continuous {
        direction,
        incoming_length,
        outgoing_length,
    }
}

fn set_tangent_vector(
    tangents: &mut MotionTangentsData,
    side: MotionEditorTangentSide,
    vector: MotionVectorData,
) -> Result<(), MotionCurveEditorError> {
    match tangents {
        MotionTangentsData::Broken { incoming, outgoing } => {
            match side {
                MotionEditorTangentSide::Incoming => *incoming = vector,
                MotionEditorTangentSide::Outgoing => *outgoing = vector,
            }
            Ok(())
        }
        MotionTangentsData::Continuous {
            direction,
            incoming_length,
            outgoing_length,
        } => {
            let length = vector.time.hypot(vector.progress);
            if length <= f64::EPSILON {
                match side {
                    MotionEditorTangentSide::Incoming => *incoming_length = 0.0,
                    MotionEditorTangentSide::Outgoing => *outgoing_length = 0.0,
                }
                return Ok(());
            }
            let sign = match side {
                MotionEditorTangentSide::Incoming => -1.0,
                MotionEditorTangentSide::Outgoing => 1.0,
            };
            let next_direction =
                MotionVectorData::new(vector.time * sign / length, vector.progress * sign / length);
            if next_direction.time <= 0.0 {
                return Err(MotionCurveEditorError::InvalidHandleDirection);
            }
            *direction = next_direction;
            match side {
                MotionEditorTangentSide::Incoming => *incoming_length = length,
                MotionEditorTangentSide::Outgoing => *outgoing_length = length,
            }
            Ok(())
        }
        MotionTangentsData::Automatic | MotionTangentsData::Corner => {
            Err(MotionCurveEditorError::HandleUnavailable)
        }
    }
}

fn constrain_handle_order(curve: &mut MotionCurveData) {
    let allow_reverse = curve.allow_reverse;
    for index in 0..curve.anchors.len().saturating_sub(1) {
        let (left, right) = curve.anchors.split_at_mut(index + 1);
        let left = &mut left[index];
        let right = &mut right[0];
        let MotionTangentsData::Broken { outgoing, .. } = &mut left.tangents else {
            continue;
        };
        let MotionTangentsData::Broken { incoming, .. } = &mut right.tangents else {
            continue;
        };
        clamp_vector_time(outgoing, outgoing.time.max(0.0));
        clamp_vector_time(incoming, incoming.time.min(0.0));
        let gap = right.time - left.time;
        let occupied = outgoing.time - incoming.time;
        if occupied > gap && occupied > f64::EPSILON {
            let scale = gap / occupied;
            outgoing.time *= scale;
            outgoing.progress *= scale;
            incoming.time *= scale;
            incoming.progress *= scale;
        }
        if !allow_reverse {
            let minimum = left.progress.min(right.progress);
            let maximum = left.progress.max(right.progress);
            outgoing.progress =
                (left.progress + outgoing.progress).clamp(minimum, maximum) - left.progress;
            incoming.progress =
                (right.progress + incoming.progress).clamp(minimum, maximum) - right.progress;
        }
    }
}

fn clamp_vector_time(vector: &mut MotionVectorData, time: f64) {
    if vector.time.abs() > f64::EPSILON {
        let scale = time / vector.time;
        vector.time = time;
        vector.progress *= scale;
    } else {
        vector.time = time;
        if time == 0.0 {
            vector.progress = 0.0;
        }
    }
}

#[derive(Debug)]
pub enum MotionCurveEditorError {
    Compile(MotionCurveCompileError),
    InvalidConsumerId,
    TooManyConsumers,
    DuplicateConsumer,
    OvershootUnsupported,
    ReverseUnsupported,
    InvalidSnap,
    InvalidConfig,
    InvalidDuration,
    InvalidCoordinate,
    InvalidViewport,
    MissingAnchor(usize),
    FixedEndpoint,
    EmptySelection,
    HandleUnavailable,
    InvalidHandleDirection,
    TooManyAnchors,
    InteractionBusy,
    WrongInteraction,
    NoInteraction,
    ClipboardTooLarge,
    ClipboardSyntax(String),
    InvalidClipboard,
}

impl fmt::Display for MotionCurveEditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(error) => error.fmt(formatter),
            Self::InvalidConsumerId => formatter.write_str("invalid motion consumer identifier"),
            Self::TooManyConsumers => formatter.write_str("too many motion property consumers"),
            Self::DuplicateConsumer => formatter.write_str("duplicate motion property consumer"),
            Self::OvershootUnsupported => {
                formatter.write_str("one or more property consumers forbid overshoot")
            }
            Self::ReverseUnsupported => {
                formatter.write_str("one or more property consumers forbid reverse progress")
            }
            Self::InvalidSnap => formatter.write_str("invalid motion editor snapping interval"),
            Self::InvalidConfig => formatter.write_str("invalid motion editor configuration"),
            Self::InvalidDuration => {
                formatter.write_str("motion duration must be within 1 ns..=60 s")
            }
            Self::InvalidCoordinate => formatter.write_str("invalid graph coordinate"),
            Self::InvalidViewport => formatter.write_str("invalid graph viewport operation"),
            Self::MissingAnchor(index) => write!(formatter, "missing motion anchor {index}"),
            Self::FixedEndpoint => formatter.write_str("motion curve endpoints are fixed"),
            Self::EmptySelection => formatter.write_str("no editable motion anchors are selected"),
            Self::HandleUnavailable => {
                formatter.write_str("the selected tangent mode has no direct handle")
            }
            Self::InvalidHandleDirection => {
                formatter.write_str("motion handle points to the wrong side of its anchor")
            }
            Self::TooManyAnchors => formatter.write_str("motion curve anchor limit exceeded"),
            Self::InteractionBusy => formatter.write_str("another editor interaction is active"),
            Self::WrongInteraction => formatter.write_str("editor interaction identity mismatch"),
            Self::NoInteraction => formatter.write_str("no editor interaction is active"),
            Self::ClipboardTooLarge => formatter.write_str("motion anchor clipboard is too large"),
            Self::ClipboardSyntax(error) => write!(formatter, "invalid motion clipboard: {error}"),
            Self::InvalidClipboard => formatter.write_str("invalid motion anchor clipboard data"),
        }
    }
}

impl std::error::Error for MotionCurveEditorError {}

impl From<MotionCurveCompileError> for MotionCurveEditorError {
    fn from(value: MotionCurveCompileError) -> Self {
        Self::Compile(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn consumer_set(domains: &[MotionCurveConsumerDomain]) -> MotionCurveConsumerSet {
        MotionCurveConsumerSet::new(
            domains
                .iter()
                .enumerate()
                .map(|(index, domain)| {
                    MotionCurveConsumer::new(format!("consumer-{index}"), *domain).unwrap()
                })
                .collect(),
        )
        .unwrap()
    }

    fn editor_with(
        inherited_curve: MotionCurveData,
        curve_override: Option<MotionCurveData>,
        config: MotionCurveEditorConfig,
    ) -> MotionCurveEditor {
        MotionCurveEditor::new(
            inherited_curve,
            curve_override,
            Duration::from_millis(400),
            None,
            consumer_set(&[MotionCurveConsumerDomain::Spatial]),
            config,
        )
        .unwrap()
    }

    fn three_anchor_curve(time: f64) -> MotionCurveData {
        split_motion_curve(&MotionCurveData::linear(), time).unwrap()
    }

    fn assert_same_shape(left: &CompiledMotionCurve, right: &CompiledMotionCurve) {
        for step in 0..=2_000 {
            let time = f64::from(step) / 2_000.0;
            assert!((left.sample(time) - right.sample(time)).abs() < 2.0e-9);
        }
    }

    #[test]
    fn exact_insert_creates_an_override_without_changing_shape_and_reset_restores_parent() {
        let inherited = MotionCurveData::from_legacy_cubic([0.2, 0.0, 0.8, 1.0]).unwrap();
        let before = CompiledMotionCurve::compile(&inherited).unwrap();
        let mut editor = editor_with(inherited.clone(), None, MotionCurveEditorConfig::default());

        assert_eq!(editor.curve_source(), MotionCurveSource::Inherited);
        assert_eq!(editor.insert_exact(0.417).unwrap(), 1);
        assert_eq!(editor.curve_source(), MotionCurveSource::Explicit);
        assert_eq!(editor.effective_curve().anchors.len(), 3);
        assert_same_shape(&before, editor.compiled());

        assert!(editor.reset_curve().unwrap());
        assert_eq!(editor.curve_source(), MotionCurveSource::Inherited);
        assert_eq!(editor.effective_curve(), &inherited);
        assert_same_shape(&before, editor.compiled());
    }

    #[test]
    fn inherited_duration_is_independent_and_editing_it_creates_only_an_override() {
        let mut editor = editor_with(
            MotionCurveData::linear(),
            None,
            MotionCurveEditorConfig::default(),
        );
        let curve = editor.effective_curve().clone();
        assert!(editor.set_duration(Duration::from_millis(750)).unwrap());
        assert_eq!(editor.duration_source(), MotionCurveSource::Explicit);
        assert_eq!(editor.effective_duration(), Duration::from_millis(750));
        assert_eq!(editor.effective_curve(), &curve);
        assert!(editor.reset_duration());
        assert_eq!(editor.duration_source(), MotionCurveSource::Inherited);
        assert_eq!(editor.effective_duration(), Duration::from_millis(400));
    }

    #[test]
    fn capability_intersection_rejects_unsafe_flags_without_mutation() {
        assert!(MotionCurveConsumer::new("Bad Id", MotionCurveConsumerDomain::Spatial).is_err());
        let duplicate =
            MotionCurveConsumer::new("same", MotionCurveConsumerDomain::Spatial).unwrap();
        assert!(matches!(
            MotionCurveConsumerSet::new(vec![duplicate.clone(), duplicate]),
            Err(MotionCurveEditorError::DuplicateConsumer)
        ));
        let consumers = consumer_set(&[
            MotionCurveConsumerDomain::Spatial,
            MotionCurveConsumerDomain::Opacity,
        ]);
        assert!(!consumers.allows_overshoot());
        assert!(!consumers.allows_reverse());
        let mut editor = MotionCurveEditor::new(
            MotionCurveData::linear(),
            None,
            Duration::from_millis(400),
            None,
            consumers,
            MotionCurveEditorConfig::default(),
        )
        .unwrap();
        let before = editor.snapshot();
        let preview_generation = editor.preview_generation();
        assert!(matches!(
            editor.set_permissions(true, false),
            Err(MotionCurveEditorError::OvershootUnsupported)
        ));
        assert_eq!(editor.effective_curve(), &before.curve);
        assert_eq!(editor.document_generation(), before.document_generation);
        assert_eq!(editor.preview_generation(), preview_generation);
    }

    #[test]
    fn replacing_consumers_is_atomic_and_clears_incompatible_history() {
        let mut editor = editor_with(
            MotionCurveData::linear(),
            None,
            MotionCurveEditorConfig::default(),
        );
        assert!(editor.set_permissions(true, true).unwrap());
        let before = editor.effective_curve().clone();
        let opacity = consumer_set(&[MotionCurveConsumerDomain::Opacity]);
        assert!(matches!(
            editor.replace_consumers(opacity),
            Err(MotionCurveEditorError::OvershootUnsupported)
        ));
        assert_eq!(editor.effective_curve(), &before);
        assert!(editor.consumers().allows_overshoot());

        assert!(editor.set_permissions(false, false).unwrap());
        assert!(editor.snapshot().can_undo);
        assert!(
            editor
                .replace_consumers(consumer_set(&[MotionCurveConsumerDomain::Color]))
                .unwrap()
        );
        assert!(!editor.snapshot().can_undo);
        assert!(!editor.consumers().allows_reverse());
    }

    #[test]
    fn invalid_numeric_edit_preserves_document_history_and_preview_generation() {
        let curve = three_anchor_curve(0.5);
        let mut editor = editor_with(
            curve.clone(),
            Some(curve),
            MotionCurveEditorConfig::default(),
        );
        let before = editor.snapshot();
        let preview_generation = editor.preview_generation();
        assert!(
            editor
                .set_anchor_numeric(1, MotionGraphPoint::new(1.0, 0.5))
                .is_err()
        );
        assert_eq!(editor.effective_curve(), &before.curve);
        assert_eq!(editor.document_generation(), before.document_generation);
        assert_eq!(editor.preview_generation(), preview_generation);
        assert_eq!(editor.snapshot().can_undo, before.can_undo);
        assert!(matches!(
            editor.set_anchor_numeric(usize::MAX, MotionGraphPoint::new(0.5, 0.5)),
            Err(MotionCurveEditorError::MissingAnchor(usize::MAX))
        ));
    }

    #[test]
    fn selected_endpoints_remain_fixed_and_do_not_remove_neighbor_constraints() {
        let curve = three_anchor_curve(0.5);
        let mut editor = editor_with(
            curve.clone(),
            Some(curve),
            MotionCurveEditorConfig::default(),
        );
        editor.select_anchor(1, false, false).unwrap();
        editor.select_anchor(0, true, false).unwrap();
        assert!(
            editor
                .move_selected_to(1, MotionGraphPoint::new(-10.0, -10.0), false)
                .unwrap()
        );
        assert_eq!(editor.effective_curve().anchors[0].time, 0.0);
        assert_eq!(editor.effective_curve().anchors[0].progress, 0.0);
        assert!(editor.effective_curve().anchors[1].time > 0.0);
        assert!(editor.effective_curve().anchors[1].progress >= 0.0);
        CompiledMotionCurve::compile(editor.effective_curve()).unwrap();
    }

    #[test]
    fn history_is_bounded_and_one_transaction_produces_one_undo_step() {
        let config = MotionCurveEditorConfig {
            history_limit: 2,
            ..MotionCurveEditorConfig::default()
        };
        let mut editor = editor_with(MotionCurveData::linear(), None, config);
        editor.insert_exact(0.2).unwrap();
        editor.insert_exact(0.4).unwrap();
        editor.insert_exact(0.6).unwrap();
        assert_eq!(editor.effective_curve().anchors.len(), 5);
        assert!(editor.undo().unwrap());
        assert_eq!(editor.effective_curve().anchors.len(), 4);
        assert!(editor.undo().unwrap());
        assert_eq!(editor.effective_curve().anchors.len(), 3);
        assert!(!editor.undo().unwrap());

        let curve = three_anchor_curve(0.5);
        let mut editor = editor_with(
            curve.clone(),
            Some(curve.clone()),
            MotionCurveEditorConfig::default(),
        );
        editor.select_anchor(1, false, false).unwrap();
        let id = MotionEditorEditId(7);
        editor.begin_transaction(id).unwrap();
        editor
            .move_selected_to(1, MotionGraphPoint::new(0.55, 0.55), false)
            .unwrap();
        editor
            .move_selected_to(1, MotionGraphPoint::new(0.6, 0.6), false)
            .unwrap();
        assert!(editor.commit_transaction(id).unwrap().document_changed);
        assert!(editor.undo().unwrap());
        assert_eq!(editor.effective_curve(), &curve);
        assert!(!editor.undo().unwrap());
    }

    #[test]
    fn cancellation_restores_document_selection_playhead_and_viewport() {
        let curve = three_anchor_curve(0.5);
        let mut editor = editor_with(
            curve.clone(),
            Some(curve),
            MotionCurveEditorConfig::default(),
        );
        editor.select_anchor(1, false, false).unwrap();
        editor.scrub_playhead(0.25).unwrap();
        let before = editor.snapshot();
        let id = MotionEditorEditId(8);
        editor.begin_transaction(id).unwrap();
        editor
            .move_selected_to(1, MotionGraphPoint::new(0.6, 0.6), false)
            .unwrap();
        editor.scrub_playhead(0.9).unwrap();
        editor
            .zoom_viewport(MotionGraphPoint::new(0.5, 0.5), 2.0, 2.0)
            .unwrap();
        let outcome = editor.cancel_transaction(id).unwrap();
        assert!(outcome.document_changed);
        assert!(outcome.transient_changed);
        let after = editor.snapshot();
        assert_eq!(after.curve, before.curve);
        assert_eq!(after.selection, before.selection);
        assert_eq!(after.primary_selection, before.primary_selection);
        assert_eq!(after.playhead, before.playhead);
        assert_eq!(after.viewport, before.viewport);
    }

    #[test]
    fn tangent_modes_and_numeric_handles_keep_fixed_endpoint_contracts() {
        let curve = three_anchor_curve(0.5);
        let mut editor = editor_with(
            curve.clone(),
            Some(curve),
            MotionCurveEditorConfig::default(),
        );
        assert!(
            editor
                .set_tangent_mode(1, MotionTangentMode::Continuous)
                .unwrap()
        );
        assert!(
            editor
                .set_handle_numeric(
                    1,
                    MotionEditorTangentSide::Outgoing,
                    MotionGraphPoint::new(0.7, 0.8),
                )
                .unwrap()
        );
        assert!(matches!(
            editor.set_handle_numeric(
                1,
                MotionEditorTangentSide::Incoming,
                MotionGraphPoint::new(0.6, 0.6),
            ),
            Err(MotionCurveEditorError::InvalidHandleDirection)
        ));
        assert!(matches!(
            editor.set_anchor_numeric(0, MotionGraphPoint::new(0.1, 0.1)),
            Err(MotionCurveEditorError::FixedEndpoint)
        ));
        assert_eq!(editor.effective_curve().anchors.first().unwrap().time, 0.0);
        assert_eq!(editor.effective_curve().anchors.last().unwrap().time, 1.0);
    }

    #[test]
    fn clipboard_round_trip_is_bounded_and_collision_failure_is_atomic() {
        let curve = three_anchor_curve(0.25);
        let mut editor = editor_with(
            curve.clone(),
            Some(curve.clone()),
            MotionCurveEditorConfig::default(),
        );
        editor.select_anchor(1, false, false).unwrap();
        let encoded = editor.copy_selection().unwrap().to_json().unwrap();
        let clipboard = MotionAnchorClipboardData::from_json(&encoded).unwrap();
        assert_eq!(clipboard.anchors().len(), 1);
        assert!(editor.paste_at(&clipboard, 0.75, 0.0).unwrap());
        assert_eq!(editor.effective_curve().anchors.len(), 4);

        let before = editor.snapshot();
        assert!(editor.paste_at(&clipboard, 0.75, 0.0).is_err());
        assert_eq!(editor.effective_curve(), &before.curve);
        assert_eq!(editor.document_generation(), before.document_generation);
        assert!(matches!(
            MotionAnchorClipboardData::from_json(&"x".repeat(MAX_CLIPBOARD_BYTES + 1)),
            Err(MotionCurveEditorError::ClipboardTooLarge)
        ));
        assert!(
            MotionAnchorClipboardData::from_json(
                r#"{"version":1,"time_origin":0.2,"anchors":[],"extra":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn time_axis_viewport_playback_and_preview_are_host_clock_driven() {
        let mut editor = editor_with(
            MotionCurveData::linear(),
            None,
            MotionCurveEditorConfig::default(),
        );
        editor.set_duration(Duration::from_secs(2)).unwrap();
        editor.set_time_axis(MotionEditorTimeAxis::RealTime);
        assert_eq!(editor.axis().display_time(0.25), 0.5);
        assert_eq!(editor.axis().normalized_time(1.5).unwrap(), 0.75);

        editor
            .zoom_viewport(MotionGraphPoint::new(0.5, 0.5), 2.0, 2.0)
            .unwrap();
        editor
            .pan_viewport(MotionGraphPoint::new(20.0, 20.0))
            .unwrap();
        assert_eq!(editor.viewport().time_end(), 1.0);
        assert_eq!(editor.viewport().progress_end(), 1.0);

        editor.set_permissions(true, true).unwrap();
        editor.fit_viewport();
        assert!(editor.viewport().progress_end() - editor.viewport().progress_start() > 1.0);
        assert!(editor.reset_viewport());
        assert_eq!(editor.viewport(), MotionGraphViewport::default());
        editor.set_permissions(false, false).unwrap();
        editor
            .pan_viewport(MotionGraphPoint::new(0.0, 0.1))
            .unwrap();
        assert_eq!(editor.viewport().progress_start(), 0.0);
        assert_eq!(editor.viewport().progress_end(), 1.0);

        let _ = editor.take_preview().unwrap();
        editor.scrub_playhead(0.25).unwrap();
        editor.toggle_playback(Duration::from_secs(10));
        editor.advance_playback(Duration::from_secs(11));
        assert_eq!(editor.playhead(), 0.75);
        editor.advance_playback(Duration::from_millis(12_500));
        assert_eq!(editor.playhead(), 0.5);
        let preview = editor.take_preview().unwrap();
        assert_eq!(preview.playhead, 0.5);
        assert!(editor.take_preview().is_none());
    }

    #[test]
    fn deterministic_edit_sequence_always_leaves_a_compilable_last_good_curve() {
        let curve = three_anchor_curve(0.5);
        let mut editor = editor_with(
            curve.clone(),
            Some(curve),
            MotionCurveEditorConfig::default(),
        );
        let mut state = 0x5eed_u64;
        for _ in 0..250 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let time = 0.1 + (state as f64 / u64::MAX as f64) * 0.8;
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let progress = state as f64 / u64::MAX as f64;
            let before = editor.effective_curve().clone();
            let generation = editor.document_generation();
            let result = editor.set_anchor_numeric(1, MotionGraphPoint::new(time, progress));
            if result.is_err() {
                assert_eq!(editor.effective_curve(), &before);
                assert_eq!(editor.document_generation(), generation);
            }
            CompiledMotionCurve::compile(editor.effective_curve()).unwrap();
            assert_eq!(editor.effective_curve().anchors.first().unwrap().time, 0.0);
            assert_eq!(editor.effective_curve().anchors.last().unwrap().time, 1.0);
        }
    }
}
