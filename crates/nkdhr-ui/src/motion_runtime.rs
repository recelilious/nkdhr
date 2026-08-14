//! Policy-governed motion execution, semantic fluid signals and interruption.

use std::collections::BTreeMap;
use std::f64::consts::TAU;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use nkdhr_theme::{
    MotionCurveData, MotionData, MotionFluidOverridesData, MotionFluidProvenanceData,
    MotionModeData, MotionScopeData,
};

use crate::{
    CompiledMotionCurve, CompiledMotionStyle, FluidTuning, MotionCurveCompileError, MotionMode,
    MotionStyleCompileError,
};

const REDUCED_DURATION_CEILING: Duration = Duration::from_millis(100);
const MAX_SEMANTIC_FLUID_MULTIPLIER: f64 = 4.0;
const MAX_SEMANTIC_FLUID_LENGTH: f64 = 64.0;
const MAX_PATH_LIVELINESS: f64 = 32.0;
const MAX_VARIATION: f64 = 0.5;

/// Whether a property changes geometry in screen space or only communicates a
/// state change without motion through space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionPropertyDomain {
    NonSpatial,
    Spatial,
}

/// Features which accessibility policy may remove independently of authored
/// style. Direct manipulation is never disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionFeature {
    DirectManipulation,
    SpatialPath,
    FluidTopology,
    Trail,
    Oscillation,
    ProceduralVariation,
    Inertia,
    IdleFluid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionPolicySource {
    AuthoredStyle,
    ReducedPolicy,
    OffPolicy,
}

/// Final execution contract. Consumers cannot recover a style duration or
/// curve which policy replaced.
#[derive(Debug, Clone)]
#[must_use]
pub struct MotionExecutionSpec {
    duration: Duration,
    curve: CompiledMotionCurve,
    mode: MotionMode,
    domain: MotionPropertyDomain,
    source: MotionPolicySource,
}

impl MotionExecutionSpec {
    pub fn is_immediate(&self) -> bool {
        self.duration.is_zero()
    }

    pub fn duration(&self) -> Duration {
        self.duration
    }

    pub fn curve(&self) -> &CompiledMotionCurve {
        &self.curve
    }

    pub fn mode(&self) -> MotionMode {
        self.mode
    }

    pub fn domain(&self) -> MotionPropertyDomain {
        self.domain
    }

    pub fn source(&self) -> MotionPolicySource {
        self.source
    }
}

/// Immutable UI-7C runtime snapshot. Motion Policy is evaluated after Motion
/// Style and therefore has final authority over every consumer.
#[derive(Debug, Clone)]
pub struct MotionRuntimeProfile {
    style: CompiledMotionStyle,
    mode: MotionMode,
    speed_multiplier: f64,
    reduced_duration: Duration,
    reduced_curve: CompiledMotionCurve,
    immediate_curve: CompiledMotionCurve,
    legacy_fluid_base: SemanticFluidParameters,
}

impl MotionRuntimeProfile {
    pub(crate) fn from_motion_data(
        data: &MotionData,
        legacy_fluid: FluidTuning,
        style: CompiledMotionStyle,
    ) -> Result<Self, MotionRuntimeError> {
        let speed_multiplier = f64::from(data.speed_multiplier);
        if !speed_multiplier.is_finite() || !(0.05..=20.0).contains(&speed_multiplier) {
            return Err(MotionRuntimeError::InvalidSpeedMultiplier);
        }
        let reduced_curve = CompiledMotionCurve::from_legacy_cubic([0.2, 0.0, 0.0, 1.0])
            .map_err(MotionRuntimeError::Curve)?;
        let immediate_curve = CompiledMotionCurve::compile(&MotionCurveData::linear())
            .map_err(MotionRuntimeError::Curve)?;
        let reduced_duration =
            Duration::from_millis(data.durations.reduced_transition).min(REDUCED_DURATION_CEILING);
        Ok(Self {
            style,
            mode: match data.mode {
                MotionModeData::Off => MotionMode::Off,
                MotionModeData::Reduced => MotionMode::Reduced,
                MotionModeData::Standard => MotionMode::Standard,
                MotionModeData::Expressive => MotionMode::Expressive,
            },
            speed_multiplier,
            reduced_duration,
            reduced_curve,
            immediate_curve,
            legacy_fluid_base: SemanticFluidParameters::from_legacy(legacy_fluid),
        })
    }

    pub fn mode(&self) -> MotionMode {
        self.mode
    }

    pub fn allows(&self, feature: MotionFeature) -> bool {
        if feature == MotionFeature::DirectManipulation {
            return true;
        }
        matches!(self.mode, MotionMode::Standard | MotionMode::Expressive)
    }

    pub fn resolve(
        &self,
        scope: &MotionScopeData,
        domain: MotionPropertyDomain,
    ) -> Result<MotionExecutionSpec, MotionRuntimeError> {
        match (self.mode, domain) {
            (MotionMode::Off, _) | (MotionMode::Reduced, MotionPropertyDomain::Spatial) => {
                Ok(MotionExecutionSpec {
                    duration: Duration::ZERO,
                    curve: self.immediate_curve.clone(),
                    mode: self.mode,
                    domain,
                    source: if self.mode == MotionMode::Off {
                        MotionPolicySource::OffPolicy
                    } else {
                        MotionPolicySource::ReducedPolicy
                    },
                })
            }
            (MotionMode::Reduced, MotionPropertyDomain::NonSpatial) => Ok(MotionExecutionSpec {
                duration: self.reduced_duration,
                curve: self.reduced_curve.clone(),
                mode: self.mode,
                domain,
                source: MotionPolicySource::ReducedPolicy,
            }),
            (MotionMode::Standard | MotionMode::Expressive, _) => {
                let resolved = self
                    .style
                    .resolve(scope)
                    .map_err(MotionRuntimeError::Style)?;
                Ok(MotionExecutionSpec {
                    duration: scale_duration(resolved.duration, self.speed_multiplier),
                    curve: resolved.curve,
                    mode: self.mode,
                    domain,
                    source: MotionPolicySource::AuthoredStyle,
                })
            }
        }
    }

    pub fn resolve_fluid(
        &self,
        scope: &MotionScopeData,
    ) -> Result<ResolvedSemanticFluid, MotionRuntimeError> {
        let resolved = self
            .style
            .resolve(scope)
            .map_err(MotionRuntimeError::Style)?;
        let mut parameters = self.legacy_fluid_base;
        parameters.overlay(resolved.fluid);
        parameters.validate()?;
        Ok(ResolvedSemanticFluid {
            parameters,
            provenance: resolved.fluid_provenance,
            mode: self.mode,
        })
    }
}

fn scale_duration(duration: Duration, speed_multiplier: f64) -> Duration {
    Duration::from_secs_f64(duration.as_secs_f64() / speed_multiplier)
}

#[derive(Debug)]
pub enum MotionRuntimeError {
    Style(MotionStyleCompileError),
    Curve(MotionCurveCompileError),
    InvalidSpeedMultiplier,
    InvalidFluidParameter(&'static str),
    InvalidSelectionMass,
}

impl fmt::Display for MotionRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Style(error) => error.fmt(formatter),
            Self::Curve(error) => error.fmt(formatter),
            Self::InvalidSpeedMultiplier => {
                formatter.write_str("motion speed multiplier must be within 0.05..=20")
            }
            Self::InvalidFluidParameter(field) => {
                write!(
                    formatter,
                    "invalid resolved semantic fluid parameter {field}"
                )
            }
            Self::InvalidSelectionMass => {
                formatter.write_str("selection mass must be finite and positive")
            }
        }
    }
}

impl std::error::Error for MotionRuntimeError {}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSemanticFluid {
    parameters: SemanticFluidParameters,
    provenance: MotionFluidProvenanceData,
    mode: MotionMode,
}

impl ResolvedSemanticFluid {
    pub fn parameters(&self) -> SemanticFluidParameters {
        self.parameters
    }

    pub fn provenance(&self) -> &MotionFluidProvenanceData {
        &self.provenance
    }

    pub fn sample(&self, progress: f64, seed: u64) -> FluidEnvelopeSample {
        self.parameters.sample(progress, seed, self.mode)
    }

    pub fn sample_idle(&self, absolute_time: Duration, seed: u64) -> FluidIdleSample {
        self.parameters.sample_idle(absolute_time, seed, self.mode)
    }
}

/// Named, bounded fluid controls. The bridge values preserve the existing
/// runtime; style overrides can replace each field independently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SemanticFluidParameters {
    pub viscosity: f64,
    pub surface_tension: f64,
    pub attraction: f64,
    pub neck: f64,
    pub trail: f64,
    pub path_liveliness: f64,
    pub oscillation: f64,
    pub damping: f64,
    pub variation: f64,
}

impl SemanticFluidParameters {
    pub fn from_legacy(legacy: FluidTuning) -> Self {
        Self {
            viscosity: 1.0,
            surface_tension: 1.0,
            attraction: 1.0,
            neck: f64::from(legacy.toggle_stretch),
            trail: f64::from(legacy.slider_trail),
            path_liveliness: f64::from(legacy.maximum_path_offset),
            // UI-7C must not introduce previously absent idle motion merely by
            // migrating an old profile. Components may opt in through style.
            oscillation: 0.0,
            damping: 1.0,
            variation: f64::from(
                legacy
                    .neck_variation
                    .max(legacy.trail_variation)
                    .max(legacy.phase_variation),
            ),
        }
    }

    pub fn validate(self) -> Result<(), MotionRuntimeError> {
        for (field, value, maximum) in [
            ("viscosity", self.viscosity, MAX_SEMANTIC_FLUID_MULTIPLIER),
            (
                "surface_tension",
                self.surface_tension,
                MAX_SEMANTIC_FLUID_MULTIPLIER,
            ),
            ("attraction", self.attraction, MAX_SEMANTIC_FLUID_MULTIPLIER),
            ("neck", self.neck, MAX_SEMANTIC_FLUID_LENGTH),
            ("trail", self.trail, MAX_SEMANTIC_FLUID_LENGTH),
            ("path_liveliness", self.path_liveliness, MAX_PATH_LIVELINESS),
            (
                "oscillation",
                self.oscillation,
                MAX_SEMANTIC_FLUID_MULTIPLIER,
            ),
            ("damping", self.damping, MAX_SEMANTIC_FLUID_MULTIPLIER),
            ("variation", self.variation, MAX_VARIATION),
        ] {
            if !value.is_finite() || value < 0.0 || value > maximum {
                return Err(MotionRuntimeError::InvalidFluidParameter(field));
            }
        }
        Ok(())
    }

    fn overlay(&mut self, values: MotionFluidOverridesData) {
        macro_rules! overlay_field {
            ($field:ident) => {
                if let Some(value) = values.$field {
                    self.$field = value;
                }
            };
        }
        overlay_field!(viscosity);
        overlay_field!(surface_tension);
        overlay_field!(attraction);
        overlay_field!(neck);
        overlay_field!(trail);
        overlay_field!(path_liveliness);
        overlay_field!(oscillation);
        overlay_field!(damping);
        overlay_field!(variation);
    }

    /// A deterministic transient signal. Variation changes only the path
    /// inside `(0, 1)`; endpoints and the caller-owned duration are invariant.
    fn sample(self, progress: f64, seed: u64, mode: MotionMode) -> FluidEnvelopeSample {
        if !matches!(mode, MotionMode::Standard | MotionMode::Expressive) {
            return FluidEnvelopeSample::ZERO;
        }
        let progress = progress.clamp(0.0, 1.0);
        if progress == 0.0 || progress == 1.0 {
            return FluidEnvelopeSample::ZERO;
        }
        let window = 4.0 * progress * (1.0 - progress);
        let viscous_window = window.powf(1.0 + self.viscosity * 0.25);
        let variation = 1.0 + deterministic_signed(seed, 0) * self.variation;
        let phase = deterministic_signed(seed, 1) * self.variation;
        let wave = self.oscillation * (TAU * (progress * (1.0 + self.oscillation) + phase)).sin()
            / (1.0 + self.damping);
        FluidEnvelopeSample {
            neck_extension: self.neck * viscous_window * variation * (0.5 + 0.5 * self.attraction),
            trail_extension: self.trail
                * window
                * (1.0 + deterministic_signed(seed, 2) * self.variation),
            path_offset: self.path_liveliness
                * window
                * ((TAU * progress).sin() + deterministic_signed(seed, 3) * self.variation),
            surface_displacement: self.surface_tension * window * wave,
        }
    }

    /// Absolute-time sampling keeps the water signal alive without accumulating
    /// frame error. Reduced and Off policy always return exact rest.
    fn sample_idle(self, absolute_time: Duration, seed: u64, mode: MotionMode) -> FluidIdleSample {
        if !matches!(mode, MotionMode::Standard | MotionMode::Expressive) || self.oscillation == 0.0
        {
            return FluidIdleSample::ZERO;
        }
        let time = absolute_time.as_secs_f64();
        let phase = deterministic_unit(seed, 4) * TAU;
        let frequency = 0.35 + self.oscillation * 0.65;
        let amplitude = self.surface_tension / (1.0 + self.damping);
        FluidIdleSample {
            surface_displacement: amplitude * (time * frequency * TAU + phase).sin(),
            lateral_displacement: self.path_liveliness
                * 0.125
                * (time * frequency * TAU * 0.73 + phase * 1.37).sin(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidEnvelopeSample {
    pub neck_extension: f64,
    pub trail_extension: f64,
    pub path_offset: f64,
    pub surface_displacement: f64,
}

impl FluidEnvelopeSample {
    pub const ZERO: Self = Self {
        neck_extension: 0.0,
        trail_extension: 0.0,
        path_offset: 0.0,
        surface_displacement: 0.0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidIdleSample {
    pub surface_displacement: f64,
    pub lateral_displacement: f64,
}

impl FluidIdleSample {
    pub const ZERO: Self = Self {
        surface_displacement: 0.0,
        lateral_displacement: 0.0,
    };
}

fn deterministic_unit(seed: u64, channel: u64) -> f64 {
    let mut value = seed.wrapping_add(channel.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    (value >> 11) as f64 / ((1_u64 << 53) - 1) as f64
}

fn deterministic_signed(seed: u64, channel: u64) -> f64 {
    deterministic_unit(seed, channel) * 2.0 - 1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MotionRunId(u64);

impl MotionRunId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionTerminalReason {
    Completed,
    Interrupted,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionTerminal {
    pub run_id: MotionRunId,
    pub reason: MotionTerminalReason,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KineticSample {
    pub value: f64,
    pub velocity: f64,
    pub target: f64,
    pub active_run: Option<MotionRunId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[must_use]
pub struct MotionBeginOutcome {
    pub began: Option<MotionRunId>,
    pub previous_terminal: Option<MotionTerminal>,
    pub immediate_terminal: Option<MotionTerminal>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[must_use]
pub struct KineticAdvance {
    pub sample: KineticSample,
    pub terminal: Option<MotionTerminal>,
}

#[derive(Debug, Clone)]
struct KineticSegment {
    run_id: MotionRunId,
    from: f64,
    target: f64,
    incoming_velocity: f64,
    started: Duration,
    spec: MotionExecutionSpec,
}

/// A latest-target scalar transition. Retargeting samples the currently
/// visible value and velocity and never queues an intermediate destination.
#[derive(Debug, Clone)]
pub struct KineticMotion {
    settled: f64,
    active: Option<KineticSegment>,
    next_run_id: u64,
}

impl KineticMotion {
    pub fn settled(value: f64) -> Self {
        Self {
            settled: finite_or_zero(value),
            active: None,
            next_run_id: 1,
        }
    }

    pub fn sample(&self, now: Duration) -> KineticSample {
        let Some(segment) = &self.active else {
            return KineticSample {
                value: self.settled,
                velocity: 0.0,
                target: self.settled,
                active_run: None,
            };
        };
        let (value, velocity, completed) = segment.sample(now);
        KineticSample {
            value,
            velocity,
            target: segment.target,
            active_run: (!completed).then_some(segment.run_id),
        }
    }

    pub fn retarget(
        &mut self,
        now: Duration,
        target: f64,
        spec: MotionExecutionSpec,
    ) -> MotionBeginOutcome {
        let target = finite_or_zero(target);
        let mut outcome = MotionBeginOutcome::default();
        let visible = if let Some(segment) = self.active.take() {
            let (value, velocity, completed) = segment.sample(now);
            if completed {
                self.settled = segment.target;
                outcome.previous_terminal = Some(MotionTerminal {
                    run_id: segment.run_id,
                    reason: MotionTerminalReason::Completed,
                });
                KineticSample {
                    value: segment.target,
                    velocity: 0.0,
                    target: segment.target,
                    active_run: None,
                }
            } else if segment.target == target {
                self.active = Some(segment);
                return outcome;
            } else {
                self.settled = value;
                outcome.previous_terminal = Some(MotionTerminal {
                    run_id: segment.run_id,
                    reason: MotionTerminalReason::Interrupted,
                });
                KineticSample {
                    value,
                    velocity,
                    target: segment.target,
                    active_run: None,
                }
            }
        } else {
            KineticSample {
                value: self.settled,
                velocity: 0.0,
                target: self.settled,
                active_run: None,
            }
        };
        if visible.value == target {
            self.settled = target;
            return outcome;
        }
        let run_id = self.allocate_run_id();
        outcome.began = Some(run_id);
        if spec.is_immediate() {
            self.settled = target;
            outcome.immediate_terminal = Some(MotionTerminal {
                run_id,
                reason: MotionTerminalReason::Completed,
            });
        } else {
            self.active = Some(KineticSegment {
                run_id,
                from: visible.value,
                target,
                incoming_velocity: finite_or_zero(visible.velocity),
                started: now,
                spec,
            });
        }
        outcome
    }

    pub fn advance(&mut self, now: Duration) -> KineticAdvance {
        let sample = self.sample(now);
        let terminal = self.active.as_ref().and_then(|segment| {
            (now.saturating_sub(segment.started) >= segment.spec.duration).then_some(
                MotionTerminal {
                    run_id: segment.run_id,
                    reason: MotionTerminalReason::Completed,
                },
            )
        });
        if terminal.is_some() {
            self.settled = self.active.as_ref().expect("active segment exists").target;
            self.active = None;
        }
        KineticAdvance {
            sample: if terminal.is_some() {
                self.sample(now)
            } else {
                sample
            },
            terminal,
        }
    }

    pub fn cancel(&mut self, now: Duration) -> Option<MotionTerminal> {
        let segment = self.active.take()?;
        let (value, _, completed) = segment.sample(now);
        self.settled = if completed { segment.target } else { value };
        Some(MotionTerminal {
            run_id: segment.run_id,
            reason: if completed {
                MotionTerminalReason::Completed
            } else {
                MotionTerminalReason::Cancelled
            },
        })
    }

    fn allocate_run_id(&mut self) -> MotionRunId {
        let id = MotionRunId(self.next_run_id.max(1));
        self.next_run_id = self.next_run_id.wrapping_add(1).max(1);
        id
    }
}

impl KineticSegment {
    fn sample(&self, now: Duration) -> (f64, f64, bool) {
        let elapsed = now.saturating_sub(self.started);
        if elapsed >= self.spec.duration {
            return (self.target, 0.0, true);
        }
        let seconds = self.spec.duration.as_secs_f64();
        let progress = (elapsed.as_secs_f64() / seconds).clamp(0.0, 1.0);
        let delta = self.target - self.from;
        let curve_progress = self.spec.curve.sample(progress);
        let start_curve_velocity = finite_or_zero(self.spec.curve.velocity(0.0) * delta / seconds);
        let end_curve_velocity = finite_or_zero(self.spec.curve.velocity(1.0) * delta / seconds);
        let start_correction = self.incoming_velocity - start_curve_velocity;
        let end_correction = -end_curve_velocity;
        let (start_basis, start_basis_velocity, end_basis, end_basis_velocity) =
            endpoint_correction_basis(progress);
        let value = self.from
            + delta * curve_progress
            + seconds * (start_correction * start_basis + end_correction * end_basis);
        let velocity = finite_or_zero(self.spec.curve.velocity(progress) * delta / seconds)
            + start_correction * start_basis_velocity
            + end_correction * end_basis_velocity;
        (finite_or_zero(value), finite_or_zero(velocity), false)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectionMassEntry {
    pub id: Arc<str>,
    pub mass: f64,
    pub velocity: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectionMassSample {
    pub entries: Vec<SelectionMassEntry>,
    pub total_mass: f64,
    pub target: Arc<str>,
    pub active_run: Option<MotionRunId>,
}

#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub struct SelectionMassAdvance {
    pub sample: SelectionMassSample,
    pub terminal: Option<MotionTerminal>,
}

#[derive(Debug, Clone)]
struct SelectionMassSegment {
    run_id: MotionRunId,
    ids: Vec<Arc<str>>,
    from: Vec<f64>,
    targets: Vec<f64>,
    incoming_velocities: Vec<f64>,
    target_id: Arc<str>,
    started: Duration,
    spec: MotionExecutionSpec,
}

/// A single conserved quantity distributed across topology nodes. New input
/// interrupts the current transfer, preserves its visible mass and tangent,
/// and redirects the whole quantity to only the latest target.
#[derive(Debug, Clone)]
pub struct SelectionMassMotion {
    total_mass: f64,
    settled_target: Arc<str>,
    settled: Vec<SelectionMassEntry>,
    active: Option<SelectionMassSegment>,
    next_run_id: u64,
}

impl SelectionMassMotion {
    pub fn new(target: impl Into<Arc<str>>, total_mass: f64) -> Result<Self, MotionRuntimeError> {
        if !total_mass.is_finite() || total_mass <= 0.0 {
            return Err(MotionRuntimeError::InvalidSelectionMass);
        }
        let target = target.into();
        Ok(Self {
            total_mass,
            settled_target: Arc::clone(&target),
            settled: vec![SelectionMassEntry {
                id: target,
                mass: total_mass,
                velocity: 0.0,
            }],
            active: None,
            next_run_id: 1,
        })
    }

    pub fn total_mass(&self) -> f64 {
        self.total_mass
    }

    /// Applies a policy-forced terminal state without manufacturing an
    /// animation run. Consumers use this when reduced/off motion becomes
    /// authoritative while a spatial transfer is already in flight.
    pub fn settle(&mut self, target: impl Into<Arc<str>>) {
        let target = target.into();
        self.settled_target = Arc::clone(&target);
        self.settled.clear();
        self.settled.push(SelectionMassEntry {
            id: target,
            mass: self.total_mass,
            velocity: 0.0,
        });
        self.active = None;
    }

    pub fn sample(&self, now: Duration) -> SelectionMassSample {
        let mut entries = Vec::new();
        let (target, active_run) = self.sample_into(now, &mut entries);
        SelectionMassSample {
            entries,
            total_mass: self.total_mass,
            target,
            active_run,
        }
    }

    /// Reuses caller storage for frame-loop sampling.
    pub fn sample_into(
        &self,
        now: Duration,
        entries: &mut Vec<SelectionMassEntry>,
    ) -> (Arc<str>, Option<MotionRunId>) {
        entries.clear();
        let Some(segment) = &self.active else {
            entries.extend(self.settled.iter().cloned());
            return (Arc::clone(&self.settled_target), None);
        };
        let completed = segment.sample_into(now, self.total_mass, entries);
        (
            Arc::clone(&segment.target_id),
            (!completed).then_some(segment.run_id),
        )
    }

    pub fn retarget(
        &mut self,
        now: Duration,
        target: impl Into<Arc<str>>,
        spec: MotionExecutionSpec,
    ) -> MotionBeginOutcome {
        let target = target.into();
        let mut outcome = MotionBeginOutcome::default();
        let mut visible = Vec::new();
        if let Some(segment) = self.active.take() {
            let completed = segment.sample_into(now, self.total_mass, &mut visible);
            if completed {
                self.settled_target = Arc::clone(&segment.target_id);
                self.settled.clear();
                self.settled.push(SelectionMassEntry {
                    id: Arc::clone(&segment.target_id),
                    mass: self.total_mass,
                    velocity: 0.0,
                });
                outcome.previous_terminal = Some(MotionTerminal {
                    run_id: segment.run_id,
                    reason: MotionTerminalReason::Completed,
                });
            } else if segment.target_id == target {
                self.active = Some(segment);
                return outcome;
            } else {
                outcome.previous_terminal = Some(MotionTerminal {
                    run_id: segment.run_id,
                    reason: MotionTerminalReason::Interrupted,
                });
            }
        } else {
            visible.extend(self.settled.iter().cloned());
        }

        if self.settled_target == target
            && visible.len() == 1
            && visible[0].id == target
            && self.active.is_none()
        {
            return outcome;
        }
        let run_id = self.allocate_run_id();
        outcome.began = Some(run_id);
        if spec.is_immediate() {
            self.settled_target = Arc::clone(&target);
            self.settled.clear();
            self.settled.push(SelectionMassEntry {
                id: target,
                mass: self.total_mass,
                velocity: 0.0,
            });
            outcome.immediate_terminal = Some(MotionTerminal {
                run_id,
                reason: MotionTerminalReason::Completed,
            });
            return outcome;
        }

        let mut components = BTreeMap::<Arc<str>, (f64, f64)>::new();
        for entry in visible {
            components.insert(entry.id, (entry.mass, entry.velocity));
        }
        components.entry(Arc::clone(&target)).or_insert((0.0, 0.0));
        let ids = components.keys().cloned().collect::<Vec<_>>();
        let from = ids.iter().map(|id| components[id].0).collect::<Vec<_>>();
        let incoming_velocities = ids.iter().map(|id| components[id].1).collect::<Vec<_>>();
        let targets = ids
            .iter()
            .map(|id| if id == &target { self.total_mass } else { 0.0 })
            .collect::<Vec<_>>();
        self.active = Some(SelectionMassSegment {
            run_id,
            ids,
            from,
            targets,
            incoming_velocities,
            target_id: target,
            started: now,
            spec,
        });
        outcome
    }

    pub fn advance(&mut self, now: Duration) -> SelectionMassAdvance {
        let sample = self.sample(now);
        let terminal = self.active.as_ref().and_then(|segment| {
            (now.saturating_sub(segment.started) >= segment.spec.duration).then_some(
                MotionTerminal {
                    run_id: segment.run_id,
                    reason: MotionTerminalReason::Completed,
                },
            )
        });
        if terminal.is_some() {
            self.settled_target = Arc::clone(
                &self
                    .active
                    .as_ref()
                    .expect("active selection segment exists")
                    .target_id,
            );
            self.settled.clear();
            self.settled.push(SelectionMassEntry {
                id: Arc::clone(&self.settled_target),
                mass: self.total_mass,
                velocity: 0.0,
            });
            self.active = None;
        }
        SelectionMassAdvance {
            sample: if terminal.is_some() {
                self.sample(now)
            } else {
                sample
            },
            terminal,
        }
    }

    pub fn cancel(&mut self, now: Duration) -> Option<MotionTerminal> {
        let segment = self.active.take()?;
        let mut entries = Vec::new();
        let completed = segment.sample_into(now, self.total_mass, &mut entries);
        if completed {
            self.settled_target = Arc::clone(&segment.target_id);
            self.settled = entries;
        } else {
            self.settled_target = entries
                .iter()
                .max_by(|left, right| left.mass.total_cmp(&right.mass))
                .map(|entry| Arc::clone(&entry.id))
                .unwrap_or_else(|| Arc::clone(&segment.target_id));
            for entry in &mut entries {
                entry.velocity = 0.0;
            }
            self.settled = entries;
        }
        Some(MotionTerminal {
            run_id: segment.run_id,
            reason: if completed {
                MotionTerminalReason::Completed
            } else {
                MotionTerminalReason::Cancelled
            },
        })
    }

    fn allocate_run_id(&mut self) -> MotionRunId {
        let id = MotionRunId(self.next_run_id.max(1));
        self.next_run_id = self.next_run_id.wrapping_add(1).max(1);
        id
    }
}

impl SelectionMassSegment {
    fn sample_into(
        &self,
        now: Duration,
        total_mass: f64,
        entries: &mut Vec<SelectionMassEntry>,
    ) -> bool {
        entries.clear();
        let elapsed = now.saturating_sub(self.started);
        if elapsed >= self.spec.duration {
            entries.push(SelectionMassEntry {
                id: Arc::clone(&self.target_id),
                mass: total_mass,
                velocity: 0.0,
            });
            return true;
        }
        let seconds = self.spec.duration.as_secs_f64();
        let progress = (elapsed.as_secs_f64() / seconds).clamp(0.0, 1.0);
        for index in 0..self.ids.len() {
            let delta = self.targets[index] - self.from[index];
            let start_curve_velocity =
                finite_or_zero(self.spec.curve.velocity(0.0) * delta / seconds);
            let end_curve_velocity =
                finite_or_zero(self.spec.curve.velocity(1.0) * delta / seconds);
            let start_correction = self.incoming_velocities[index] - start_curve_velocity;
            let end_correction = -end_curve_velocity;
            let (start_basis, start_basis_velocity, end_basis, end_basis_velocity) =
                endpoint_correction_basis(progress);
            entries.push(SelectionMassEntry {
                id: Arc::clone(&self.ids[index]),
                mass: finite_or_zero(
                    self.from[index]
                        + delta * self.spec.curve.sample(progress)
                        + seconds * (start_correction * start_basis + end_correction * end_basis),
                ),
                velocity: finite_or_zero(
                    self.spec.curve.velocity(progress) * delta / seconds
                        + start_correction * start_basis_velocity
                        + end_correction * end_basis_velocity,
                ),
            });
        }
        normalize_selection_mass(entries, total_mass, &self.target_id);
        false
    }
}

fn normalize_selection_mass(
    entries: &mut Vec<SelectionMassEntry>,
    total_mass: f64,
    fallback_target: &Arc<str>,
) {
    for entry in entries.iter_mut() {
        if entry.mass <= 0.0 || !entry.mass.is_finite() {
            entry.mass = 0.0;
            entry.velocity = 0.0;
        }
    }
    let raw_sum = entries.iter().map(|entry| entry.mass).sum::<f64>();
    if !raw_sum.is_finite() || raw_sum <= f64::EPSILON {
        entries.clear();
        entries.push(SelectionMassEntry {
            id: Arc::clone(fallback_target),
            mass: total_mass,
            velocity: 0.0,
        });
        return;
    }
    let raw_velocity_sum = entries.iter().map(|entry| entry.velocity).sum::<f64>();
    for entry in entries.iter_mut() {
        let raw_mass = entry.mass;
        let raw_velocity = entry.velocity;
        entry.mass = total_mass * raw_mass / raw_sum;
        entry.velocity = total_mass * (raw_velocity * raw_sum - raw_mass * raw_velocity_sum)
            / (raw_sum * raw_sum);
    }
    let mass_error = total_mass - entries.iter().map(|entry| entry.mass).sum::<f64>();
    let velocity_error = entries.iter().map(|entry| entry.velocity).sum::<f64>();
    if let Some((index, _)) = entries
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.mass.total_cmp(&right.mass))
    {
        entries[index].mass += mass_error;
        entries[index].velocity -= velocity_error;
    }
}

fn endpoint_correction_basis(progress: f64) -> (f64, f64, f64, f64) {
    let square = progress * progress;
    let start = progress * (1.0 - progress) * (1.0 - progress);
    let start_velocity = 1.0 - 4.0 * progress + 3.0 * square;
    let end = square * (progress - 1.0);
    let end_velocity = 3.0 * square - 2.0 * progress;
    (start, start_velocity, end, end_velocity)
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nkdhr_theme::{
        MotionFamilyNodeData, MotionSemanticFamilyData, MotionStyleProfileData, MotionValuesData,
    };

    fn runtime(data: &MotionData) -> MotionRuntimeProfile {
        MotionRuntimeProfile::from_motion_data(
            data,
            FluidTuning::default(),
            CompiledMotionStyle::from_motion_data(data).unwrap(),
        )
        .unwrap()
    }

    fn spec(duration: Duration) -> MotionExecutionSpec {
        MotionExecutionSpec {
            duration,
            curve: CompiledMotionCurve::compile(&MotionCurveData::linear()).unwrap(),
            mode: MotionMode::Standard,
            domain: MotionPropertyDomain::Spatial,
            source: MotionPolicySource::AuthoredStyle,
        }
    }

    #[test]
    fn final_policy_cannot_be_bypassed_by_authored_style_or_speed() {
        let mut data = MotionData {
            speed_multiplier: 20.0,
            ..MotionData::default()
        };
        let mut style = MotionStyleProfileData::default();
        style.overrides.families.insert(
            MotionSemanticFamilyData::Toggle,
            MotionFamilyNodeData {
                values: MotionValuesData {
                    curve: Some(MotionCurveData::linear()),
                    duration_ms: Some(60_000),
                    fluid: MotionFluidOverridesData::default(),
                },
                components: BTreeMap::new(),
            },
        );
        data.style = Some(style);

        let standard = runtime(&data);
        let scope = MotionScopeData::family(MotionSemanticFamilyData::Toggle);
        assert_eq!(
            standard
                .resolve(&scope, MotionPropertyDomain::Spatial)
                .unwrap()
                .duration,
            Duration::from_secs(3)
        );

        data.mode = MotionModeData::Reduced;
        let reduced = runtime(&data);
        let spatial = reduced
            .resolve(&scope, MotionPropertyDomain::Spatial)
            .unwrap();
        assert!(spatial.is_immediate());
        assert_eq!(spatial.source, MotionPolicySource::ReducedPolicy);
        let non_spatial = reduced
            .resolve(&scope, MotionPropertyDomain::NonSpatial)
            .unwrap();
        assert_eq!(non_spatial.duration, Duration::from_millis(100));
        assert_eq!(non_spatial.source, MotionPolicySource::ReducedPolicy);
        assert_eq!(
            non_spatial.curve.sample(0.5),
            runtime(&MotionData::default()).reduced_curve.sample(0.5)
        );
        assert!(reduced.allows(MotionFeature::DirectManipulation));
        for feature in [
            MotionFeature::SpatialPath,
            MotionFeature::FluidTopology,
            MotionFeature::Trail,
            MotionFeature::Oscillation,
            MotionFeature::ProceduralVariation,
            MotionFeature::Inertia,
            MotionFeature::IdleFluid,
        ] {
            assert!(!reduced.allows(feature));
        }
        assert_eq!(
            reduced.resolve_fluid(&scope).unwrap().sample(0.5, 7),
            FluidEnvelopeSample::ZERO
        );

        data.mode = MotionModeData::Off;
        let off = runtime(&data);
        assert!(
            off.resolve(&scope, MotionPropertyDomain::NonSpatial)
                .unwrap()
                .is_immediate()
        );
        assert!(off.allows(MotionFeature::DirectManipulation));

        data.speed_multiplier = f32::MIN_POSITIVE;
        assert!(matches!(
            MotionRuntimeProfile::from_motion_data(
                &data,
                FluidTuning::default(),
                CompiledMotionStyle::from_motion_data(&data).unwrap(),
            ),
            Err(MotionRuntimeError::InvalidSpeedMultiplier)
        ));
    }

    #[test]
    fn semantic_fluid_is_scoped_deterministic_and_endpoint_preserving() {
        let mut data = MotionData::default();
        let mut style = MotionStyleProfileData::default();
        style.overrides.families.insert(
            MotionSemanticFamilyData::Toggle,
            MotionFamilyNodeData {
                values: MotionValuesData {
                    fluid: MotionFluidOverridesData {
                        neck: Some(14.0),
                        path_liveliness: Some(5.0),
                        oscillation: Some(1.25),
                        variation: Some(0.2),
                        ..MotionFluidOverridesData::default()
                    },
                    ..MotionValuesData::default()
                },
                components: BTreeMap::new(),
            },
        );
        data.style = Some(style);
        let runtime = runtime(&data);
        let scope = MotionScopeData::family(MotionSemanticFamilyData::Toggle);
        let fluid = runtime.resolve_fluid(&scope).unwrap();
        assert_eq!(fluid.parameters.neck, 14.0);
        assert_eq!(
            fluid.provenance.neck.unwrap().level,
            nkdhr_theme::MotionScopeLevelData::Family
        );
        assert_eq!(
            fluid.parameters.sample(0.0, 91, MotionMode::Expressive),
            FluidEnvelopeSample::ZERO
        );
        assert_eq!(
            fluid.parameters.sample(1.0, 91, MotionMode::Expressive),
            FluidEnvelopeSample::ZERO
        );
        let first = fluid.parameters.sample(0.4, 91, MotionMode::Expressive);
        assert_eq!(
            first,
            fluid.parameters.sample(0.4, 91, MotionMode::Expressive)
        );
        assert_ne!(
            first,
            fluid.parameters.sample(0.4, 92, MotionMode::Expressive)
        );
        let mut no_variation = fluid.parameters;
        no_variation.variation = 0.0;
        assert_eq!(
            no_variation.sample(0.4, 91, MotionMode::Expressive),
            no_variation.sample(0.4, 92, MotionMode::Expressive)
        );
        let idle_a =
            fluid
                .parameters
                .sample_idle(Duration::from_millis(100), 91, MotionMode::Standard);
        let idle_b =
            fluid
                .parameters
                .sample_idle(Duration::from_millis(200), 91, MotionMode::Standard);
        assert_ne!(idle_a, idle_b);
        assert_eq!(
            fluid
                .parameters
                .sample_idle(Duration::from_millis(200), 91, MotionMode::Reduced,),
            FluidIdleSample::ZERO
        );
    }

    #[test]
    fn scalar_retarget_preserves_visible_state_and_velocity_and_terminates_once() {
        let mut motion = KineticMotion::settled(0.0);
        let first = motion.retarget(Duration::ZERO, 10.0, spec(Duration::from_secs(1)));
        let first_run = first.began.unwrap();
        let now = Duration::from_millis(400);
        let before = motion.sample(now);
        let retarget = motion.retarget(now, -5.0, spec(Duration::from_secs(1)));
        assert_eq!(
            retarget.previous_terminal,
            Some(MotionTerminal {
                run_id: first_run,
                reason: MotionTerminalReason::Interrupted,
            })
        );
        let after = motion.sample(now);
        assert!((after.value - before.value).abs() < 1.0e-10);
        assert!((after.velocity - before.velocity).abs() < 1.0e-8);
        assert_eq!(after.target, -5.0);

        let finished = motion.advance(Duration::from_millis(1_400));
        assert_eq!(finished.sample.value, -5.0);
        assert_eq!(finished.sample.velocity, 0.0);
        assert_eq!(
            finished.terminal.unwrap().reason,
            MotionTerminalReason::Completed
        );
        assert!(
            motion
                .advance(Duration::from_millis(1_500))
                .terminal
                .is_none()
        );
    }

    #[test]
    fn immediate_policy_interrupts_old_run_and_completes_new_run() {
        let mut motion = KineticMotion::settled(0.0);
        let _ = motion.retarget(Duration::ZERO, 1.0, spec(Duration::from_secs(1)));
        let outcome = motion.retarget(Duration::from_millis(200), 9.0, spec(Duration::ZERO));
        assert_eq!(
            outcome.previous_terminal.unwrap().reason,
            MotionTerminalReason::Interrupted
        );
        assert_eq!(
            outcome.immediate_terminal.unwrap().reason,
            MotionTerminalReason::Completed
        );
        assert_eq!(motion.sample(Duration::from_millis(200)).value, 9.0);
    }

    #[test]
    fn selection_retarget_conserves_mass_tangent_and_latest_target() {
        let mut selection = SelectionMassMotion::new("left", 1.0).unwrap();
        let _ = selection.retarget(Duration::ZERO, "right", spec(Duration::from_secs(1)));
        let now = Duration::from_millis(360);
        let before = selection.sample(now);
        let outcome = selection.retarget(now, "upper", spec(Duration::from_secs(1)));
        assert_eq!(
            outcome.previous_terminal.unwrap().reason,
            MotionTerminalReason::Interrupted
        );
        let after = selection.sample(now);
        let before_by_id = before
            .entries
            .iter()
            .map(|entry| (&entry.id, (entry.mass, entry.velocity)))
            .collect::<BTreeMap<_, _>>();
        for entry in &after.entries {
            if let Some((mass, velocity)) = before_by_id.get(&entry.id) {
                assert!((entry.mass - mass).abs() < 1.0e-10);
                assert!((entry.velocity - velocity).abs() < 1.0e-8);
            } else {
                assert_eq!(entry.mass, 0.0);
                assert_eq!(entry.velocity, 0.0);
            }
        }
        assert_eq!(after.target.as_ref(), "upper");

        for time in [500, 700, 1_000, 1_300] {
            let sample = selection.sample(Duration::from_millis(time));
            assert!(sample.entries.iter().all(|entry| entry.mass >= 0.0));
            assert!(
                (sample.entries.iter().map(|entry| entry.mass).sum::<f64>() - 1.0).abs() < 1.0e-12
            );
            assert!(
                sample
                    .entries
                    .iter()
                    .map(|entry| entry.velocity)
                    .sum::<f64>()
                    .abs()
                    < 1.0e-9
            );
        }
        let finished = selection.advance(Duration::from_millis(1_360));
        assert_eq!(finished.sample.entries.len(), 1);
        assert_eq!(finished.sample.entries[0].id.as_ref(), "upper");
        assert_eq!(finished.sample.entries[0].mass, 1.0);
        assert_eq!(
            finished.terminal.unwrap().reason,
            MotionTerminalReason::Completed
        );
        assert!(
            selection
                .advance(Duration::from_millis(1_500))
                .terminal
                .is_none()
        );
    }

    #[test]
    fn cancelling_selection_freezes_visible_distribution_without_losing_mass() {
        let mut selection = SelectionMassMotion::new("first", 1.0).unwrap();
        let _ = selection.retarget(Duration::ZERO, "second", spec(Duration::from_secs(1)));
        let now = Duration::from_millis(420);
        let before = selection.sample(now);
        assert_eq!(
            selection.cancel(now).unwrap().reason,
            MotionTerminalReason::Cancelled
        );
        assert!(selection.cancel(now).is_none());
        let frozen = selection.sample(now);
        assert_eq!(frozen.entries.len(), before.entries.len());
        for (before, after) in before.entries.iter().zip(&frozen.entries) {
            assert_eq!(before.id, after.id);
            assert!((before.mass - after.mass).abs() < 1.0e-12);
            assert_eq!(after.velocity, 0.0);
        }
        assert!((frozen.entries.iter().map(|entry| entry.mass).sum::<f64>() - 1.0).abs() < 1.0e-12);
    }

    #[test]
    fn policy_settle_removes_an_active_selection_topology_exactly() {
        let mut selection = SelectionMassMotion::new("first", 1.0).unwrap();
        let _ = selection.retarget(Duration::ZERO, "second", spec(Duration::from_secs(1)));
        assert!(
            selection
                .sample(Duration::from_millis(300))
                .active_run
                .is_some()
        );
        selection.settle("third");
        let settled = selection.sample(Duration::from_millis(300));
        assert!(settled.active_run.is_none());
        assert_eq!(settled.target.as_ref(), "third");
        assert_eq!(settled.entries.len(), 1);
        assert_eq!(settled.entries[0].id.as_ref(), "third");
        assert_eq!(settled.entries[0].mass, 1.0);
        assert_eq!(settled.entries[0].velocity, 0.0);
    }

    #[test]
    fn rapid_selection_retargets_remain_finite_and_conserved() {
        let mut selection = SelectionMassMotion::new("a", 3.0).unwrap();
        let targets = ["a", "b", "c", "d", "e"];
        for step in 0..160_u64 {
            let now = Duration::from_millis(step * 13);
            if step % 7 == 0 {
                let _ = selection.retarget(
                    now,
                    targets[(step as usize / 7) % targets.len()],
                    spec(Duration::from_millis(310)),
                );
            }
            let sample = selection.sample(now);
            assert!(sample.entries.iter().all(|entry| {
                entry.mass.is_finite() && entry.velocity.is_finite() && entry.mass >= 0.0
            }));
            assert!(
                (sample.entries.iter().map(|entry| entry.mass).sum::<f64>() - 3.0).abs() < 1.0e-11
            );
            assert!(
                sample
                    .entries
                    .iter()
                    .map(|entry| entry.velocity)
                    .sum::<f64>()
                    .abs()
                    < 1.0e-8
            );
        }
    }
}
