//! Portable, non-executable multi-segment motion-curve data.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const MOTION_CURVE_SCHEMA_VERSION: u32 = 1;
pub const MOTION_CURVE_AUTO_ALGORITHM_VERSION: u16 = 1;
pub const MIN_MOTION_CURVE_ANCHORS: usize = 2;
pub const MAX_MOTION_CURVE_ANCHORS: usize = 64;
pub const MIN_MOTION_CURVE_TIME_GAP: f64 = 1.0e-6;
pub const MAX_MOTION_CURVE_ABSOLUTE_PROGRESS: f64 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionVectorData {
    pub time: f64,
    pub progress: f64,
}

impl MotionVectorData {
    pub const ZERO: Self = Self {
        time: 0.0,
        progress: 0.0,
    };

    pub const fn new(time: f64, progress: f64) -> Self {
        Self { time, progress }
    }

    fn is_finite(self) -> bool {
        self.time.is_finite() && self.progress.is_finite()
    }
}

/// Tangent representation preserves the four owner-approved editing modes.
/// Continuous tangents store one direction plus independent side lengths so
/// collinearity is data, not a floating-point convention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum MotionTangentsData {
    Automatic,
    Continuous {
        direction: MotionVectorData,
        incoming_length: f64,
        outgoing_length: f64,
    },
    Broken {
        incoming: MotionVectorData,
        outgoing: MotionVectorData,
    },
    Corner,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionAnchorData {
    pub time: f64,
    pub progress: f64,
    pub tangents: MotionTangentsData,
}

impl MotionAnchorData {
    pub const fn new(time: f64, progress: f64, tangents: MotionTangentsData) -> Self {
        Self {
            time,
            progress,
            tangents,
        }
    }
}

/// One atomic inherited curve value. Duration is intentionally stored outside
/// this type so the same normalized shape can be reused at different speeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionCurveData {
    pub version: u32,
    pub auto_algorithm: u16,
    #[serde(default)]
    pub allow_overshoot: bool,
    #[serde(default)]
    pub allow_reverse: bool,
    pub anchors: Vec<MotionAnchorData>,
}

impl MotionCurveData {
    pub fn linear() -> Self {
        Self {
            version: MOTION_CURVE_SCHEMA_VERSION,
            auto_algorithm: MOTION_CURVE_AUTO_ALGORITHM_VERSION,
            allow_overshoot: false,
            allow_reverse: false,
            anchors: vec![
                MotionAnchorData::new(
                    0.0,
                    0.0,
                    MotionTangentsData::Broken {
                        incoming: MotionVectorData::ZERO,
                        outgoing: MotionVectorData::new(1.0 / 3.0, 1.0 / 3.0),
                    },
                ),
                MotionAnchorData::new(
                    1.0,
                    1.0,
                    MotionTangentsData::Broken {
                        incoming: MotionVectorData::new(-1.0 / 3.0, -1.0 / 3.0),
                        outgoing: MotionVectorData::ZERO,
                    },
                ),
            ],
        }
    }

    /// Lossless geometry migration from the Phase-3 CSS-compatible cubic.
    /// Legacy curves constrained only time coordinates, so both professional
    /// permissions remain enabled until UI-7B resolves consumer capabilities.
    pub fn from_legacy_cubic(control: [f32; 4]) -> Result<Self, MotionCurveDataError> {
        let [x1, y1, x2, y2] = control.map(f64::from);
        if ![x1, y1, x2, y2].into_iter().all(f64::is_finite)
            || !(0.0..=1.0).contains(&x1)
            || !(0.0..=1.0).contains(&x2)
        {
            return Err(MotionCurveDataError::InvalidLegacyCubic);
        }
        let anchors = if x1 <= x2 {
            vec![
                MotionAnchorData::new(
                    0.0,
                    0.0,
                    MotionTangentsData::Broken {
                        incoming: MotionVectorData::ZERO,
                        outgoing: MotionVectorData::new(x1, y1),
                    },
                ),
                MotionAnchorData::new(
                    1.0,
                    1.0,
                    MotionTangentsData::Broken {
                        incoming: MotionVectorData::new(x2 - 1.0, y2 - 1.0),
                        outgoing: MotionVectorData::ZERO,
                    },
                ),
            ]
        } else {
            // CSS accepts both x controls anywhere in 0..=1, while UI-7's
            // directly editable segments require an ordered control polygon.
            // One exact half split always orders both resulting time polygons.
            let first = MotionVectorData::new(0.0, 0.0);
            let control_one = MotionVectorData::new(x1, y1);
            let control_two = MotionVectorData::new(x2, y2);
            let last = MotionVectorData::new(1.0, 1.0);
            let midpoint = |left: MotionVectorData, right: MotionVectorData| {
                MotionVectorData::new(
                    (left.time + right.time) * 0.5,
                    (left.progress + right.progress) * 0.5,
                )
            };
            let first_control = midpoint(first, control_one);
            let middle_control = midpoint(control_one, control_two);
            let last_control = midpoint(control_two, last);
            let left_control = midpoint(first_control, middle_control);
            let right_control = midpoint(middle_control, last_control);
            let split = midpoint(left_control, right_control);
            let subtract = |point: MotionVectorData, origin: MotionVectorData| {
                MotionVectorData::new(point.time - origin.time, point.progress - origin.progress)
            };
            vec![
                MotionAnchorData::new(
                    0.0,
                    0.0,
                    MotionTangentsData::Broken {
                        incoming: MotionVectorData::ZERO,
                        outgoing: first_control,
                    },
                ),
                MotionAnchorData::new(
                    split.time,
                    split.progress,
                    MotionTangentsData::Broken {
                        incoming: subtract(left_control, split),
                        outgoing: subtract(right_control, split),
                    },
                ),
                MotionAnchorData::new(
                    1.0,
                    1.0,
                    MotionTangentsData::Broken {
                        incoming: subtract(last_control, last),
                        outgoing: MotionVectorData::ZERO,
                    },
                ),
            ]
        };
        let curve = Self {
            version: MOTION_CURVE_SCHEMA_VERSION,
            auto_algorithm: MOTION_CURVE_AUTO_ALGORITHM_VERSION,
            allow_overshoot: true,
            allow_reverse: true,
            anchors,
        };
        curve.validate()?;
        Ok(curve)
    }

    pub fn validate(&self) -> Result<(), MotionCurveDataError> {
        if self.version != MOTION_CURVE_SCHEMA_VERSION {
            return Err(MotionCurveDataError::UnsupportedVersion(self.version));
        }
        if self.auto_algorithm != MOTION_CURVE_AUTO_ALGORITHM_VERSION {
            return Err(MotionCurveDataError::UnsupportedAutoAlgorithm(
                self.auto_algorithm,
            ));
        }
        if !(MIN_MOTION_CURVE_ANCHORS..=MAX_MOTION_CURVE_ANCHORS).contains(&self.anchors.len()) {
            return Err(MotionCurveDataError::AnchorCount(self.anchors.len()));
        }
        let first = &self.anchors[0];
        let last = &self.anchors[self.anchors.len() - 1];
        if first.time != 0.0 || first.progress != 0.0 || last.time != 1.0 || last.progress != 1.0 {
            return Err(MotionCurveDataError::FixedEndpoints);
        }
        for (index, anchor) in self.anchors.iter().enumerate() {
            if !anchor.time.is_finite()
                || !anchor.progress.is_finite()
                || !(0.0..=1.0).contains(&anchor.time)
                || anchor.progress.abs() > MAX_MOTION_CURVE_ABSOLUTE_PROGRESS
            {
                return Err(MotionCurveDataError::InvalidAnchor(index));
            }
            if index > 0 && anchor.time - self.anchors[index - 1].time < MIN_MOTION_CURVE_TIME_GAP {
                return Err(MotionCurveDataError::TimeOrder(index));
            }
            validate_tangents(index, &anchor.tangents)?;
        }
        Ok(())
    }
}

fn validate_tangents(
    index: usize,
    tangents: &MotionTangentsData,
) -> Result<(), MotionCurveDataError> {
    let valid_component =
        |value: f64| value.is_finite() && value.abs() <= MAX_MOTION_CURVE_ABSOLUTE_PROGRESS * 2.0;
    match tangents {
        MotionTangentsData::Automatic | MotionTangentsData::Corner => Ok(()),
        MotionTangentsData::Continuous {
            direction,
            incoming_length,
            outgoing_length,
        } => {
            if !direction.is_finite()
                || direction.time <= 0.0
                || !valid_component(direction.time)
                || !valid_component(direction.progress)
                || !incoming_length.is_finite()
                || !outgoing_length.is_finite()
                || *incoming_length < 0.0
                || *outgoing_length < 0.0
                || *incoming_length > MAX_MOTION_CURVE_ABSOLUTE_PROGRESS * 2.0
                || *outgoing_length > MAX_MOTION_CURVE_ABSOLUTE_PROGRESS * 2.0
            {
                return Err(MotionCurveDataError::InvalidTangents(index));
            }
            Ok(())
        }
        MotionTangentsData::Broken { incoming, outgoing } => {
            if !incoming.is_finite()
                || !outgoing.is_finite()
                || incoming.time > 0.0
                || outgoing.time < 0.0
                || !valid_component(incoming.time)
                || !valid_component(incoming.progress)
                || !valid_component(outgoing.time)
                || !valid_component(outgoing.progress)
            {
                return Err(MotionCurveDataError::InvalidTangents(index));
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionCurveDataError {
    UnsupportedVersion(u32),
    UnsupportedAutoAlgorithm(u16),
    AnchorCount(usize),
    FixedEndpoints,
    InvalidAnchor(usize),
    TimeOrder(usize),
    InvalidTangents(usize),
    InvalidLegacyCubic,
}

impl fmt::Display for MotionCurveDataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported motion curve schema version {version}"
                )
            }
            Self::UnsupportedAutoAlgorithm(version) => {
                write!(
                    formatter,
                    "unsupported automatic tangent algorithm {version}"
                )
            }
            Self::AnchorCount(count) => write!(
                formatter,
                "motion curve must contain {MIN_MOTION_CURVE_ANCHORS}..={MAX_MOTION_CURVE_ANCHORS} anchors, found {count}"
            ),
            Self::FixedEndpoints => {
                formatter.write_str("motion curve endpoints must be exactly (0, 0) and (1, 1)")
            }
            Self::InvalidAnchor(index) => write!(formatter, "motion anchor {index} is invalid"),
            Self::TimeOrder(index) => write!(
                formatter,
                "motion anchor {index} is not strictly ordered in time"
            ),
            Self::InvalidTangents(index) => {
                write!(formatter, "motion anchor {index} has invalid tangents")
            }
            Self::InvalidLegacyCubic => formatter.write_str("legacy cubic curve is invalid"),
        }
    }
}

impl std::error::Error for MotionCurveDataError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_cubic_round_trips_portably() {
        let curve = MotionCurveData::from_legacy_cubic([0.2, 0.0, 0.0, 1.0]).unwrap();
        let json = serde_json::to_string(&curve).unwrap();
        let decoded: MotionCurveData = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.version, curve.version);
        assert_eq!(decoded.anchors.len(), curve.anchors.len());
        for (decoded, original) in decoded.anchors.iter().zip(&curve.anchors) {
            assert!((decoded.time - original.time).abs() <= f64::EPSILON);
            assert!((decoded.progress - original.progress).abs() <= f64::EPSILON);
        }
        decoded.validate().unwrap();
    }

    #[test]
    fn endpoints_and_time_order_are_structural_invariants() {
        let mut curve = MotionCurveData::linear();
        curve.anchors.insert(
            1,
            MotionAnchorData::new(0.0, 0.5, MotionTangentsData::Automatic),
        );
        assert_eq!(curve.validate(), Err(MotionCurveDataError::TimeOrder(1)));

        let mut curve = MotionCurveData::linear();
        curve.anchors[0].progress = 0.1;
        assert_eq!(curve.validate(), Err(MotionCurveDataError::FixedEndpoints));
    }

    #[test]
    fn malformed_tangents_are_rejected_without_normalization() {
        let mut curve = MotionCurveData::linear();
        curve.anchors[0].tangents = MotionTangentsData::Broken {
            incoming: MotionVectorData::ZERO,
            outgoing: MotionVectorData::new(-0.2, 0.1),
        };
        assert_eq!(
            curve.validate(),
            Err(MotionCurveDataError::InvalidTangents(0))
        );
    }
}
