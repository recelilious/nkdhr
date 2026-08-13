//! Deterministic compilation and evaluation for professional motion curves.

use std::fmt;
use std::sync::Arc;

use nkdhr_theme::{
    MAX_MOTION_CURVE_ABSOLUTE_PROGRESS, MIN_MOTION_CURVE_TIME_GAP, MotionAnchorData,
    MotionCurveData, MotionCurveDataError, MotionTangentsData, MotionVectorData,
};

const INVERSION_ITERATIONS: usize = 32;
const ANALYSIS_EPSILON: f64 = 1.0e-10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionCurveAnalysis {
    pub minimum_progress: f64,
    pub maximum_progress: f64,
    pub has_overshoot: bool,
    pub has_reverse: bool,
}

#[derive(Debug, Clone)]
pub struct CompiledMotionCurve {
    inner: Arc<CompiledMotionCurveInner>,
}

#[derive(Debug)]
struct CompiledMotionCurveInner {
    source: MotionCurveData,
    segments: Box<[CompiledSegment]>,
    analysis: MotionCurveAnalysis,
    fingerprint: u64,
}

#[derive(Debug, Clone, Copy)]
struct Point {
    time: f64,
    progress: f64,
}

impl Point {
    fn from_anchor(anchor: &MotionAnchorData) -> Self {
        Self {
            time: anchor.time,
            progress: anchor.progress,
        }
    }

    fn offset(self, vector: MotionVectorData) -> Self {
        Self {
            time: self.time + vector.time,
            progress: self.progress + vector.progress,
        }
    }

    fn vector_to(self, other: Self) -> MotionVectorData {
        MotionVectorData::new(other.time - self.time, other.progress - self.progress)
    }

    fn lerp(self, other: Self, parameter: f64) -> Self {
        Self {
            time: self.time + (other.time - self.time) * parameter,
            progress: self.progress + (other.progress - self.progress) * parameter,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Polynomial {
    cubic: f64,
    quadratic: f64,
    linear: f64,
    constant: f64,
}

impl Polynomial {
    fn through(first: f64, control_one: f64, control_two: f64, last: f64) -> Self {
        Self {
            cubic: last - 3.0 * control_two + 3.0 * control_one - first,
            quadratic: 3.0 * (control_two - 2.0 * control_one + first),
            linear: 3.0 * (control_one - first),
            constant: first,
        }
    }

    fn sample(self, parameter: f64) -> f64 {
        ((self.cubic * parameter + self.quadratic) * parameter + self.linear) * parameter
            + self.constant
    }

    fn derivative(self, parameter: f64) -> f64 {
        (3.0 * self.cubic * parameter + 2.0 * self.quadratic) * parameter + self.linear
    }

    fn derivative_roots(self) -> Vec<f64> {
        quadratic_roots(3.0 * self.cubic, 2.0 * self.quadratic, self.linear)
            .into_iter()
            .filter(|root| *root > 0.0 && *root < 1.0)
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
struct CompiledSegment {
    first: Point,
    control_one: Point,
    control_two: Point,
    last: Point,
    time: Polynomial,
    progress: Polynomial,
}

impl CompiledSegment {
    fn new(first: Point, control_one: Point, control_two: Point, last: Point) -> Self {
        Self {
            first,
            control_one,
            control_two,
            last,
            time: Polynomial::through(first.time, control_one.time, control_two.time, last.time),
            progress: Polynomial::through(
                first.progress,
                control_one.progress,
                control_two.progress,
                last.progress,
            ),
        }
    }

    fn parameter_at_time(self, time: f64) -> f64 {
        if time <= self.first.time {
            return 0.0;
        }
        if time >= self.last.time {
            return 1.0;
        }
        let mut lower = 0.0;
        let mut upper = 1.0;
        for _ in 0..INVERSION_ITERATIONS {
            let parameter = (lower + upper) * 0.5;
            if self.time.sample(parameter) < time {
                lower = parameter;
            } else {
                upper = parameter;
            }
        }
        (lower + upper) * 0.5
    }
}

impl CompiledMotionCurve {
    pub fn compile(source: &MotionCurveData) -> Result<Self, MotionCurveCompileError> {
        source.validate().map_err(MotionCurveCompileError::Data)?;
        let handles = resolve_handles(source)?;
        let mut segments = Vec::with_capacity(source.anchors.len() - 1);
        for index in 0..source.anchors.len() - 1 {
            let first = Point::from_anchor(&source.anchors[index]);
            let last = Point::from_anchor(&source.anchors[index + 1]);
            let control_one = first.offset(handles[index].outgoing);
            let control_two = last.offset(handles[index + 1].incoming);
            if control_one.time < first.time
                || control_two.time < control_one.time
                || control_two.time > last.time
            {
                return Err(MotionCurveCompileError::NonMonotonicHandles { segment: index });
            }
            segments.push(CompiledSegment::new(first, control_one, control_two, last));
        }
        let analysis = analyze(&segments);
        if analysis.minimum_progress < -MAX_MOTION_CURVE_ABSOLUTE_PROGRESS
            || analysis.maximum_progress > MAX_MOTION_CURVE_ABSOLUTE_PROGRESS
        {
            return Err(MotionCurveCompileError::ProgressSafetyRange {
                minimum: analysis.minimum_progress,
                maximum: analysis.maximum_progress,
            });
        }
        if analysis.has_overshoot && !source.allow_overshoot {
            return Err(MotionCurveCompileError::OvershootNotAllowed {
                minimum: analysis.minimum_progress,
                maximum: analysis.maximum_progress,
            });
        }
        if analysis.has_reverse && !source.allow_reverse {
            return Err(MotionCurveCompileError::ReverseNotAllowed);
        }
        let fingerprint = fingerprint(source, &segments);
        Ok(Self {
            inner: Arc::new(CompiledMotionCurveInner {
                source: source.clone(),
                segments: segments.into_boxed_slice(),
                analysis,
                fingerprint,
            }),
        })
    }

    pub fn from_legacy_cubic(control: [f32; 4]) -> Result<Self, MotionCurveCompileError> {
        let source =
            MotionCurveData::from_legacy_cubic(control).map_err(MotionCurveCompileError::Data)?;
        Self::compile(&source)
    }

    pub fn source(&self) -> &MotionCurveData {
        &self.inner.source
    }

    pub fn analysis(&self) -> MotionCurveAnalysis {
        self.inner.analysis
    }

    pub fn fingerprint(&self) -> u64 {
        self.inner.fingerprint
    }

    /// Fixed-iteration, allocation-free and lock-free normalized sampling.
    pub fn sample(&self, time: f64) -> f64 {
        let time = time.clamp(0.0, 1.0);
        if time == 0.0 {
            return 0.0;
        }
        if time == 1.0 {
            return 1.0;
        }
        let segment = self.segment_at(time);
        segment.progress.sample(segment.parameter_at_time(time))
    }

    /// dy/dx at a normalized time. A vertical time tangent reports a signed
    /// infinity rather than introducing a frame-history dependency.
    pub fn velocity(&self, time: f64) -> f64 {
        let segment = self.segment_at(time.clamp(0.0, 1.0));
        let parameter = segment.parameter_at_time(time.clamp(0.0, 1.0));
        let dx = segment.time.derivative(parameter);
        let dy = segment.progress.derivative(parameter);
        if dx.abs() <= f64::EPSILON {
            if dy == 0.0 {
                0.0
            } else {
                dy.signum() * f64::INFINITY
            }
        } else {
            dy / dx
        }
    }

    fn segment_at(&self, time: f64) -> CompiledSegment {
        let mut lower = 0;
        let mut upper = self.inner.segments.len();
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            if self.inner.segments[middle].last.time < time {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        self.inner.segments[lower.min(self.inner.segments.len() - 1)]
    }
}

#[derive(Debug, Clone, Copy)]
struct ResolvedHandles {
    incoming: MotionVectorData,
    outgoing: MotionVectorData,
}

fn resolve_handles(
    source: &MotionCurveData,
) -> Result<Vec<ResolvedHandles>, MotionCurveCompileError> {
    let automatic_slopes = automatic_slopes(&source.anchors);
    source
        .anchors
        .iter()
        .enumerate()
        .map(|(index, anchor)| {
            let handles = match &anchor.tangents {
                MotionTangentsData::Automatic => {
                    let incoming_time = if index == 0 {
                        0.0
                    } else {
                        -(anchor.time - source.anchors[index - 1].time) / 3.0
                    };
                    let outgoing_time = if index + 1 == source.anchors.len() {
                        0.0
                    } else {
                        (source.anchors[index + 1].time - anchor.time) / 3.0
                    };
                    ResolvedHandles {
                        incoming: MotionVectorData::new(
                            incoming_time,
                            incoming_time * automatic_slopes[index],
                        ),
                        outgoing: MotionVectorData::new(
                            outgoing_time,
                            outgoing_time * automatic_slopes[index],
                        ),
                    }
                }
                MotionTangentsData::Continuous {
                    direction,
                    incoming_length,
                    outgoing_length,
                } => {
                    let length = direction.time.hypot(direction.progress);
                    if !length.is_finite() || length == 0.0 {
                        return Err(MotionCurveCompileError::InvalidContinuousDirection {
                            anchor: index,
                        });
                    }
                    let unit =
                        MotionVectorData::new(direction.time / length, direction.progress / length);
                    ResolvedHandles {
                        incoming: MotionVectorData::new(
                            -unit.time * incoming_length,
                            -unit.progress * incoming_length,
                        ),
                        outgoing: MotionVectorData::new(
                            unit.time * outgoing_length,
                            unit.progress * outgoing_length,
                        ),
                    }
                }
                MotionTangentsData::Broken { incoming, outgoing } => ResolvedHandles {
                    incoming: *incoming,
                    outgoing: *outgoing,
                },
                MotionTangentsData::Corner => ResolvedHandles {
                    incoming: MotionVectorData::ZERO,
                    outgoing: MotionVectorData::ZERO,
                },
            };
            Ok(handles)
        })
        .collect()
}

/// Version-one automatic tangents use the shape-preserving PCHIP derivative.
fn automatic_slopes(anchors: &[MotionAnchorData]) -> Vec<f64> {
    let segment_count = anchors.len() - 1;
    let widths = anchors
        .windows(2)
        .map(|pair| pair[1].time - pair[0].time)
        .collect::<Vec<_>>();
    let secants = anchors
        .windows(2)
        .zip(&widths)
        .map(|(pair, width)| (pair[1].progress - pair[0].progress) / width)
        .collect::<Vec<_>>();
    if segment_count == 1 {
        return vec![secants[0], secants[0]];
    }

    let mut slopes = vec![0.0; anchors.len()];
    slopes[0] = endpoint_slope(widths[0], widths[1], secants[0], secants[1]);
    slopes[segment_count] = endpoint_slope(
        widths[segment_count - 1],
        widths[segment_count - 2],
        secants[segment_count - 1],
        secants[segment_count - 2],
    );
    for index in 1..segment_count {
        let before = secants[index - 1];
        let after = secants[index];
        slopes[index] = if before == 0.0 || after == 0.0 || before.signum() != after.signum() {
            0.0
        } else {
            let before_width = widths[index - 1];
            let after_width = widths[index];
            let first_weight = 2.0 * after_width + before_width;
            let second_weight = after_width + 2.0 * before_width;
            (first_weight + second_weight) / (first_weight / before + second_weight / after)
        };
    }
    slopes
}

fn endpoint_slope(first_width: f64, second_width: f64, first: f64, second: f64) -> f64 {
    let mut slope = ((2.0 * first_width + second_width) * first - first_width * second)
        / (first_width + second_width);
    if slope.signum() != first.signum() {
        slope = 0.0;
    } else if first.signum() != second.signum() && slope.abs() > 3.0 * first.abs() {
        slope = 3.0 * first;
    }
    slope
}

fn analyze(segments: &[CompiledSegment]) -> MotionCurveAnalysis {
    let mut minimum = 0.0_f64;
    let mut maximum = 1.0_f64;
    let mut has_reverse = false;
    for segment in segments {
        let mut cuts = vec![0.0, 1.0];
        cuts.extend(segment.progress.derivative_roots());
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|left, right| (*left - *right).abs() <= f64::EPSILON);
        for parameter in &cuts {
            let progress = segment.progress.sample(*parameter);
            minimum = minimum.min(progress);
            maximum = maximum.max(progress);
        }
        for interval in cuts.windows(2) {
            let midpoint = (interval[0] + interval[1]) * 0.5;
            if segment.progress.derivative(midpoint) < -ANALYSIS_EPSILON
                && segment.progress.sample(midpoint) < 1.0 - ANALYSIS_EPSILON
            {
                has_reverse = true;
            }
        }
    }
    MotionCurveAnalysis {
        minimum_progress: minimum,
        maximum_progress: maximum,
        has_overshoot: minimum < -ANALYSIS_EPSILON || maximum > 1.0 + ANALYSIS_EPSILON,
        has_reverse,
    }
}

fn quadratic_roots(quadratic: f64, linear: f64, constant: f64) -> Vec<f64> {
    if quadratic.abs() <= f64::EPSILON {
        return if linear.abs() <= f64::EPSILON {
            Vec::new()
        } else {
            vec![-constant / linear]
        };
    }
    let discriminant = linear.mul_add(linear, -4.0 * quadratic * constant);
    if discriminant < 0.0 {
        return Vec::new();
    }
    let root = discriminant.sqrt();
    vec![
        (-linear - root) / (2.0 * quadratic),
        (-linear + root) / (2.0 * quadratic),
    ]
}

/// Exact De Casteljau split. The returned representation uses broken handles
/// so no automatic-mode recomputation can alter the original geometry.
pub fn split_motion_curve(
    source: &MotionCurveData,
    time: f64,
) -> Result<MotionCurveData, MotionCurveCompileError> {
    let compiled = CompiledMotionCurve::compile(source)?;
    if !time.is_finite() || time <= 0.0 || time >= 1.0 {
        return Err(MotionCurveCompileError::InvalidSplitTime);
    }
    if source
        .anchors
        .iter()
        .any(|anchor| (anchor.time - time).abs() < MIN_MOTION_CURVE_TIME_GAP)
    {
        return Err(MotionCurveCompileError::SplitTooCloseToAnchor);
    }
    let segment_index = compiled
        .inner
        .segments
        .iter()
        .position(|segment| time < segment.last.time)
        .ok_or(MotionCurveCompileError::InvalidSplitTime)?;
    let segment = compiled.inner.segments[segment_index];
    let parameter = segment.parameter_at_time(time);
    let first_control = segment.first.lerp(segment.control_one, parameter);
    let middle_control = segment.control_one.lerp(segment.control_two, parameter);
    let last_control = segment.control_two.lerp(segment.last, parameter);
    let left_control = first_control.lerp(middle_control, parameter);
    let right_control = middle_control.lerp(last_control, parameter);
    let split = left_control.lerp(right_control, parameter);

    let mut anchors = resolved_broken_anchors(&compiled);
    if split.time - anchors[segment_index].time < MIN_MOTION_CURVE_TIME_GAP
        || anchors[segment_index + 1].time - split.time < MIN_MOTION_CURVE_TIME_GAP
    {
        return Err(MotionCurveCompileError::SplitTooCloseToAnchor);
    }
    set_outgoing(
        &mut anchors[segment_index],
        segment.first.vector_to(first_control),
    );
    set_incoming(
        &mut anchors[segment_index + 1],
        segment.last.vector_to(last_control),
    );
    anchors.insert(
        segment_index + 1,
        MotionAnchorData::new(
            split.time,
            split.progress,
            MotionTangentsData::Broken {
                incoming: split.vector_to(left_control),
                outgoing: split.vector_to(right_control),
            },
        ),
    );
    let result = MotionCurveData {
        anchors,
        ..source.clone()
    };
    CompiledMotionCurve::compile(&result)?;
    Ok(result)
}

fn resolved_broken_anchors(compiled: &CompiledMotionCurve) -> Vec<MotionAnchorData> {
    compiled
        .source()
        .anchors
        .iter()
        .enumerate()
        .map(|(index, anchor)| {
            let point = Point::from_anchor(anchor);
            let incoming = if index == 0 {
                MotionVectorData::ZERO
            } else {
                point.vector_to(compiled.inner.segments[index - 1].control_two)
            };
            let outgoing = if index == compiled.inner.segments.len() {
                MotionVectorData::ZERO
            } else {
                point.vector_to(compiled.inner.segments[index].control_one)
            };
            MotionAnchorData::new(
                anchor.time,
                anchor.progress,
                MotionTangentsData::Broken { incoming, outgoing },
            )
        })
        .collect()
}

fn set_incoming(anchor: &mut MotionAnchorData, incoming: MotionVectorData) {
    let outgoing = match &anchor.tangents {
        MotionTangentsData::Broken { outgoing, .. } => *outgoing,
        _ => unreachable!("resolved anchors always use broken tangents"),
    };
    anchor.tangents = MotionTangentsData::Broken { incoming, outgoing };
}

fn set_outgoing(anchor: &mut MotionAnchorData, outgoing: MotionVectorData) {
    let incoming = match &anchor.tangents {
        MotionTangentsData::Broken { incoming, .. } => *incoming,
        _ => unreachable!("resolved anchors always use broken tangents"),
    };
    anchor.tangents = MotionTangentsData::Broken { incoming, outgoing };
}

fn fingerprint(source: &MotionCurveData, segments: &[CompiledSegment]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut feed = |value: u64| {
        hash ^= value;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    feed(u64::from(source.version));
    feed(u64::from(source.auto_algorithm));
    feed(u64::from(source.allow_overshoot));
    feed(u64::from(source.allow_reverse));
    for segment in segments {
        for value in [
            segment.first.time,
            segment.first.progress,
            segment.control_one.time,
            segment.control_one.progress,
            segment.control_two.time,
            segment.control_two.progress,
            segment.last.time,
            segment.last.progress,
        ] {
            feed(value.to_bits());
        }
    }
    hash
}

#[derive(Debug, Clone, PartialEq)]
pub enum MotionCurveCompileError {
    Data(MotionCurveDataError),
    InvalidContinuousDirection { anchor: usize },
    NonMonotonicHandles { segment: usize },
    ProgressSafetyRange { minimum: f64, maximum: f64 },
    OvershootNotAllowed { minimum: f64, maximum: f64 },
    ReverseNotAllowed,
    InvalidSplitTime,
    SplitTooCloseToAnchor,
}

impl fmt::Display for MotionCurveCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(error) => error.fmt(formatter),
            Self::InvalidContinuousDirection { anchor } => {
                write!(
                    formatter,
                    "motion anchor {anchor} has a zero continuous direction"
                )
            }
            Self::NonMonotonicHandles { segment } => {
                write!(formatter, "motion segment {segment} turns backward in time")
            }
            Self::ProgressSafetyRange { minimum, maximum } => write!(
                formatter,
                "motion curve progress range {minimum}..{maximum} exceeds the absolute safety bound"
            ),
            Self::OvershootNotAllowed { minimum, maximum } => write!(
                formatter,
                "motion curve overshoots to {minimum}..{maximum} without overshoot permission"
            ),
            Self::ReverseNotAllowed => {
                formatter.write_str("motion curve reverses progress without reverse permission")
            }
            Self::InvalidSplitTime => formatter.write_str("split time must be inside 0..1"),
            Self::SplitTooCloseToAnchor => {
                formatter.write_str("split time is too close to an existing anchor")
            }
        }
    }
}

impl std::error::Error for MotionCurveCompileError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn cubic(
        progress_one: f64,
        progress_two: f64,
        overshoot: bool,
        reverse: bool,
    ) -> MotionCurveData {
        let mut data = MotionCurveData::from_legacy_cubic([
            0.25,
            progress_one as f32,
            0.75,
            progress_two as f32,
        ])
        .unwrap();
        data.allow_overshoot = overshoot;
        data.allow_reverse = reverse;
        data
    }

    fn dense_equal(first: &CompiledMotionCurve, second: &CompiledMotionCurve, tolerance: f64) {
        for index in 0..=10_000 {
            let time = index as f64 / 10_000.0;
            assert!(
                (first.sample(time) - second.sample(time)).abs() <= tolerance,
                "curves differ at {time}: {} != {}",
                first.sample(time),
                second.sample(time)
            );
        }
    }

    #[test]
    fn legacy_defaults_migrate_without_changing_geometry() {
        for control in [
            [0.0, 0.0, 1.0, 1.0],
            [0.2, 0.0, 0.0, 1.0],
            [0.16, 1.0, 0.3, 1.0],
            [0.4, 0.0, 1.0, 1.0],
            [0.33, 1.0, 0.68, 1.0],
        ] {
            let migrated = CompiledMotionCurve::from_legacy_cubic(control).unwrap();
            assert_eq!(migrated.sample(0.0), 0.0);
            assert_eq!(migrated.sample(1.0), 1.0);
            for index in 0..=1_000 {
                let time = index as f64 / 1_000.0;
                let oracle = legacy_cubic_sample(control.map(f64::from), time);
                assert!((migrated.sample(time) - oracle).abs() < 1.0e-8);
            }
        }
    }

    #[test]
    fn de_casteljau_insertion_is_shape_preserving() {
        let source = cubic(0.05, 0.95, false, false);
        let before = CompiledMotionCurve::compile(&source).unwrap();
        let split = split_motion_curve(&source, 0.417).unwrap();
        assert_eq!(split.anchors.len(), 3);
        let after = CompiledMotionCurve::compile(&split).unwrap();
        dense_equal(&before, &after, 2.0e-9);
    }

    #[test]
    fn hidden_overshoot_and_reverse_are_analytically_rejected() {
        let overshoot = cubic(1.6, 1.6, false, false);
        assert!(matches!(
            CompiledMotionCurve::compile(&overshoot),
            Err(MotionCurveCompileError::OvershootNotAllowed { .. })
        ));

        let settling = cubic(1.6, 1.6, true, false);
        let analysis = CompiledMotionCurve::compile(&settling).unwrap().analysis();
        assert!(analysis.has_overshoot);
        assert!(!analysis.has_reverse);

        let reverse = cubic(1.0, -0.1, true, false);
        assert!(matches!(
            CompiledMotionCurve::compile(&reverse),
            Err(MotionCurveCompileError::ReverseNotAllowed)
        ));
    }

    #[test]
    fn automatic_tangents_are_deterministic_and_shape_preserving() {
        let data = MotionCurveData {
            version: nkdhr_theme::MOTION_CURVE_SCHEMA_VERSION,
            auto_algorithm: nkdhr_theme::MOTION_CURVE_AUTO_ALGORITHM_VERSION,
            allow_overshoot: false,
            allow_reverse: false,
            anchors: vec![
                MotionAnchorData::new(0.0, 0.0, MotionTangentsData::Automatic),
                MotionAnchorData::new(0.3, 0.15, MotionTangentsData::Automatic),
                MotionAnchorData::new(0.7, 0.9, MotionTangentsData::Automatic),
                MotionAnchorData::new(1.0, 1.0, MotionTangentsData::Automatic),
            ],
        };
        let first = CompiledMotionCurve::compile(&data).unwrap();
        let second = CompiledMotionCurve::compile(&data).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        dense_equal(&first, &second, 0.0);
        assert!(!first.analysis().has_overshoot);
        assert!(!first.analysis().has_reverse);
    }

    #[test]
    fn handle_time_that_turns_back_is_rejected() {
        let mut data = MotionCurveData::linear();
        data.anchors[0].tangents = MotionTangentsData::Broken {
            incoming: MotionVectorData::ZERO,
            outgoing: MotionVectorData::new(0.9, 0.2),
        };
        data.anchors[1].tangents = MotionTangentsData::Broken {
            incoming: MotionVectorData::new(-0.9, -0.2),
            outgoing: MotionVectorData::ZERO,
        };
        assert!(matches!(
            CompiledMotionCurve::compile(&data),
            Err(MotionCurveCompileError::NonMonotonicHandles { segment: 0 })
        ));
    }

    #[test]
    fn sampling_depends_only_on_absolute_time() {
        let curve = CompiledMotionCurve::compile(&cubic(0.1, 0.9, false, false)).unwrap();
        let irregular = [0.0, 0.013, 0.22, 0.221, 0.79, 1.0];
        for time in irregular {
            let direct = curve.sample(time);
            for _ in 0..100 {
                assert_eq!(curve.sample(time), direct);
            }
        }
    }

    #[test]
    fn analytic_extrema_enclose_dense_sampling() {
        let curve = CompiledMotionCurve::compile(&cubic(1.6, 1.6, true, false)).unwrap();
        let analysis = curve.analysis();
        let mut sampled_minimum = f64::INFINITY;
        let mut sampled_maximum = f64::NEG_INFINITY;
        for index in 0..=100_000 {
            let progress = curve.sample(index as f64 / 100_000.0);
            sampled_minimum = sampled_minimum.min(progress);
            sampled_maximum = sampled_maximum.max(progress);
        }
        assert!(analysis.minimum_progress <= sampled_minimum + 1.0e-10);
        assert!(analysis.maximum_progress >= sampled_maximum - 1.0e-10);
        assert!((analysis.maximum_progress - sampled_maximum).abs() < 1.0e-8);
    }

    #[test]
    fn maximum_legal_curve_is_finite_and_one_more_anchor_is_rejected() {
        let anchors = (0..nkdhr_theme::MAX_MOTION_CURVE_ANCHORS)
            .map(|index| {
                let value = index as f64 / (nkdhr_theme::MAX_MOTION_CURVE_ANCHORS - 1) as f64;
                MotionAnchorData::new(value, value, MotionTangentsData::Automatic)
            })
            .collect::<Vec<_>>();
        let data = MotionCurveData {
            version: nkdhr_theme::MOTION_CURVE_SCHEMA_VERSION,
            auto_algorithm: nkdhr_theme::MOTION_CURVE_AUTO_ALGORITHM_VERSION,
            allow_overshoot: false,
            allow_reverse: false,
            anchors,
        };
        let curve = CompiledMotionCurve::compile(&data).unwrap();
        for index in 0..=1_000 {
            assert!(curve.sample(index as f64 / 1_000.0).is_finite());
        }

        let mut too_many = data;
        let insertion = too_many.anchors.len() - 1;
        too_many.anchors.insert(
            insertion,
            MotionAnchorData::new(0.999_999, 0.999_999, MotionTangentsData::Automatic),
        );
        assert!(matches!(
            CompiledMotionCurve::compile(&too_many),
            Err(MotionCurveCompileError::Data(
                MotionCurveDataError::AnchorCount(_)
            ))
        ));
    }

    #[test]
    fn deterministic_generated_legal_curves_stay_finite_and_bounded() {
        let mut seed = 0x6e6b_6468_725f_7569_u64;
        for _ in 0..256 {
            let mut next = || {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (seed >> 11) as f64 / ((1_u64 << 53) - 1) as f64
            };
            let interior = 1 + (next() * 10.0) as usize;
            let time_weights = (0..=interior).map(|_| 0.1 + next()).collect::<Vec<_>>();
            let progress_weights = (0..=interior).map(|_| next()).collect::<Vec<_>>();
            let time_total = time_weights.iter().sum::<f64>();
            let progress_total = progress_weights.iter().sum::<f64>();
            let mut time = 0.0;
            let mut progress = 0.0;
            let mut anchors = vec![MotionAnchorData::new(
                0.0,
                0.0,
                MotionTangentsData::Automatic,
            )];
            for index in 0..interior {
                time += time_weights[index] / time_total;
                progress += progress_weights[index] / progress_total;
                anchors.push(MotionAnchorData::new(
                    time,
                    progress,
                    MotionTangentsData::Automatic,
                ));
            }
            anchors.push(MotionAnchorData::new(
                1.0,
                1.0,
                MotionTangentsData::Automatic,
            ));
            let data = MotionCurveData {
                version: nkdhr_theme::MOTION_CURVE_SCHEMA_VERSION,
                auto_algorithm: nkdhr_theme::MOTION_CURVE_AUTO_ALGORITHM_VERSION,
                allow_overshoot: false,
                allow_reverse: false,
                anchors,
            };
            let curve = CompiledMotionCurve::compile(&data).unwrap();
            assert!(!curve.analysis().has_overshoot);
            assert!(!curve.analysis().has_reverse);
            for index in 0..=100 {
                let progress = curve.sample(index as f64 / 100.0);
                assert!(progress.is_finite() && (-1.0e-10..=1.0 + 1.0e-10).contains(&progress));
            }
        }
    }

    #[test]
    fn public_cubic_migration_entry_uses_the_same_compiler() {
        let migrated = crate::CubicBezier::STANDARD.to_motion_curve_data().unwrap();
        let through_method = crate::CubicBezier::STANDARD.compile_motion_curve().unwrap();
        let directly = CompiledMotionCurve::compile(&migrated).unwrap();
        dense_equal(&through_method, &directly, 0.0);
    }

    fn legacy_cubic_sample(control: [f64; 4], time: f64) -> f64 {
        if time == 0.0 || time == 1.0 {
            return time;
        }
        let [x1, y1, x2, y2] = control;
        let x = Polynomial::through(0.0, x1, x2, 1.0);
        let y = Polynomial::through(0.0, y1, y2, 1.0);
        let mut lower = 0.0;
        let mut upper = 1.0;
        for _ in 0..INVERSION_ITERATIONS {
            let parameter = (lower + upper) * 0.5;
            if x.sample(parameter) < time {
                lower = parameter;
            } else {
                upper = parameter;
            }
        }
        y.sample((lower + upper) * 0.5)
    }
}
