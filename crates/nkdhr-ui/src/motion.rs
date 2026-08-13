//! Product motion tokens and deterministic interruptible scalar transitions.

use std::{fmt, time::Duration};

/// A CSS-compatible cubic Bézier with monotonic time coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl CubicBezier {
    pub const LINEAR: Self = Self::unchecked(0.0, 0.0, 1.0, 1.0);
    pub const STANDARD: Self = Self::unchecked(0.2, 0.0, 0.0, 1.0);
    pub const SETTLE: Self = Self::unchecked(0.16, 1.0, 0.3, 1.0);
    pub const EXIT: Self = Self::unchecked(0.4, 0.0, 1.0, 1.0);
    pub const SOFT: Self = Self::unchecked(0.33, 1.0, 0.68, 1.0);

    const fn unchecked(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    pub fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Result<Self, MotionError> {
        let curve = Self::unchecked(x1, y1, x2, y2);
        if curve.is_valid() {
            Ok(curve)
        } else {
            Err(MotionError::InvalidCurve)
        }
    }

    pub fn is_valid(self) -> bool {
        [self.x1, self.y1, self.x2, self.y2]
            .into_iter()
            .all(f32::is_finite)
            && (0.0..=1.0).contains(&self.x1)
            && (0.0..=1.0).contains(&self.x2)
    }

    /// Resolve normalized time to normalized progress. Y may overshoot when a
    /// professional curve deliberately places a control point outside 0..=1.
    pub fn sample(self, time: f32) -> f32 {
        let time = time.clamp(0.0, 1.0);
        if time == 0.0 {
            return 0.0;
        }
        if time == 1.0 {
            return 1.0;
        }
        if self == Self::LINEAR {
            return time;
        }
        let mut lower = 0.0_f32;
        let mut upper = 1.0_f32;
        for _ in 0..18 {
            let parameter = (lower + upper) * 0.5;
            if cubic(parameter, self.x1, self.x2) < time {
                lower = parameter;
            } else {
                upper = parameter;
            }
        }
        cubic((lower + upper) * 0.5, self.y1, self.y2)
    }

    /// Convert the Phase-3 single cubic into UI-7's portable segmented data.
    /// A legacy control polygon which is not directly editable under UI-7's
    /// ordered-time rule is exactly subdivided without changing its geometry.
    pub fn to_motion_curve_data(
        self,
    ) -> Result<nkdhr_theme::MotionCurveData, nkdhr_theme::MotionCurveDataError> {
        nkdhr_theme::MotionCurveData::from_legacy_cubic([self.x1, self.y1, self.x2, self.y2])
    }

    /// Compile the losslessly migrated UI-7 representation.
    pub fn compile_motion_curve(
        self,
    ) -> Result<crate::CompiledMotionCurve, crate::MotionCurveCompileError> {
        crate::CompiledMotionCurve::from_legacy_cubic([self.x1, self.y1, self.x2, self.y2])
    }
}

fn cubic(parameter: f32, first: f32, second: f32) -> f32 {
    let inverse = 1.0 - parameter;
    3.0 * inverse * inverse * parameter * first
        + 3.0 * inverse * parameter * parameter * second
        + parameter * parameter * parameter
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionMode {
    Off,
    Reduced,
    Standard,
    Expressive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionFamily {
    HoverIn,
    HoverOut,
    Press,
    Release,
    Focus,
    Toggle,
    SliderTrail,
    ListTransfer,
    TextInputFocus,
    Validation,
    ScrollbarShow,
    ScrollbarHide,
    Overscroll,
    TooltipEnter,
    TooltipExit,
    PopoverEnter,
    PopoverExit,
    PanelEnter,
    PanelExit,
    DrawerEnter,
    DrawerExit,
    Workspace,
    Wallpaper,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSpec {
    pub duration: Duration,
    pub curve: CubicBezier,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidTuning {
    pub neck_variation: f32,
    pub trail_variation: f32,
    pub phase_variation: f32,
    pub maximum_path_offset: f32,
    pub toggle_stretch: f32,
    pub slider_trail: f32,
    pub transfer_base: Duration,
    pub transfer_per_unit_ms: f32,
    pub transfer_maximum: Duration,
    pub bud_duration: Duration,
    pub bud_stagger: Duration,
    pub group_maximum: Duration,
}

impl Default for FluidTuning {
    fn default() -> Self {
        Self {
            neck_variation: 0.06,
            trail_variation: 0.08,
            phase_variation: 0.10,
            maximum_path_offset: 3.0,
            toggle_stretch: 6.0,
            slider_trail: 6.0,
            transfer_base: Duration::from_millis(280),
            transfer_per_unit_ms: 0.18,
            transfer_maximum: Duration::from_millis(650),
            bud_duration: Duration::from_millis(180),
            bud_stagger: Duration::from_millis(60),
            group_maximum: Duration::from_millis(700),
        }
    }
}

impl FluidTuning {
    pub fn transfer_duration(&self, projected_distance: f32) -> Duration {
        let distance = if projected_distance.is_finite() {
            projected_distance.max(0.0)
        } else {
            0.0
        };
        let milliseconds = self.transfer_base.as_secs_f64() * 1000.0
            + f64::from(self.transfer_per_unit_ms) * f64::from(distance);
        Duration::from_secs_f64(
            (milliseconds / 1000.0)
                .min(self.transfer_maximum.as_secs_f64())
                .max(0.0),
        )
    }

    /// Stable bounded differences for shell material. Standard controls do
    /// not call this; shell events opt in with one stable event seed.
    pub fn variation(&self, seed: u64) -> FluidVariation {
        FluidVariation {
            neck_multiplier: 1.0 + signed_unit(seed, 0) * self.neck_variation,
            trail_multiplier: 1.0 + signed_unit(seed, 1) * self.trail_variation,
            phase_offset: signed_unit(seed, 2) * self.phase_variation,
            path_offset: signed_unit(seed, 3) * self.maximum_path_offset,
        }
    }

    pub fn is_valid(&self) -> bool {
        [
            self.neck_variation,
            self.trail_variation,
            self.phase_variation,
        ]
        .into_iter()
        .all(|value| value.is_finite() && (0.0..=0.5).contains(&value))
            && [
                self.maximum_path_offset,
                self.toggle_stretch,
                self.slider_trail,
                self.transfer_per_unit_ms,
            ]
            .into_iter()
            .all(|value| value.is_finite() && value >= 0.0)
            && self.transfer_base <= self.transfer_maximum
            && self.bud_duration <= self.group_maximum
            && self.bud_stagger <= self.group_maximum
    }

    fn scaled_speed(mut self, multiplier: f32) -> Self {
        let duration_factor = 1.0 / f64::from(multiplier);
        self.transfer_base = scale_duration(self.transfer_base, duration_factor);
        self.transfer_per_unit_ms /= multiplier;
        self.transfer_maximum = scale_duration(self.transfer_maximum, duration_factor);
        self.bud_duration = scale_duration(self.bud_duration, duration_factor);
        self.bud_stagger = scale_duration(self.bud_stagger, duration_factor);
        self.group_maximum = scale_duration(self.group_maximum, duration_factor);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidVariation {
    pub neck_multiplier: f32,
    pub trail_multiplier: f32,
    pub phase_offset: f32,
    pub path_offset: f32,
}

fn signed_unit(seed: u64, channel: u64) -> f32 {
    let mut value = seed.wrapping_add(channel.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    let normalized = (value >> 40) as f32 / ((1_u64 << 24) - 1) as f32;
    normalized * 2.0 - 1.0
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionDurations {
    pub reduced_transition: Duration,
    pub hover_in: Duration,
    pub hover_out: Duration,
    pub press: Duration,
    pub release: Duration,
    pub focus: Duration,
    pub toggle: Duration,
    pub slider_trail: Duration,
    pub list_transfer: Duration,
    pub text_input_focus: Duration,
    pub validation: Duration,
    pub scrollbar_show: Duration,
    pub scrollbar_hide: Duration,
    pub overscroll: Duration,
    pub tooltip_enter: Duration,
    pub tooltip_exit: Duration,
    pub popover_enter: Duration,
    pub popover_exit: Duration,
    pub panel_enter: Duration,
    pub panel_exit: Duration,
    pub drawer_enter: Duration,
    pub drawer_exit: Duration,
    pub workspace: Duration,
    pub wallpaper: Duration,
}

impl Default for MotionDurations {
    fn default() -> Self {
        Self {
            reduced_transition: Duration::from_millis(100),
            hover_in: Duration::from_millis(120),
            hover_out: Duration::from_millis(160),
            press: Duration::from_millis(70),
            release: Duration::from_millis(140),
            focus: Duration::from_millis(140),
            toggle: Duration::from_millis(220),
            slider_trail: Duration::from_millis(90),
            list_transfer: Duration::from_millis(180),
            text_input_focus: Duration::from_millis(160),
            validation: Duration::from_millis(220),
            scrollbar_show: Duration::from_millis(100),
            scrollbar_hide: Duration::from_millis(220),
            overscroll: Duration::from_millis(260),
            tooltip_enter: Duration::from_millis(140),
            tooltip_exit: Duration::from_millis(110),
            popover_enter: Duration::from_millis(180),
            popover_exit: Duration::from_millis(150),
            panel_enter: Duration::from_millis(280),
            panel_exit: Duration::from_millis(220),
            drawer_enter: Duration::from_millis(320),
            drawer_exit: Duration::from_millis(240),
            workspace: Duration::from_millis(300),
            wallpaper: Duration::from_millis(800),
        }
    }
}

impl MotionDurations {
    fn get(self, family: MotionFamily) -> Duration {
        match family {
            MotionFamily::HoverIn => self.hover_in,
            MotionFamily::HoverOut => self.hover_out,
            MotionFamily::Press => self.press,
            MotionFamily::Release => self.release,
            MotionFamily::Focus => self.focus,
            MotionFamily::Toggle => self.toggle,
            MotionFamily::SliderTrail => self.slider_trail,
            MotionFamily::ListTransfer => self.list_transfer,
            MotionFamily::TextInputFocus => self.text_input_focus,
            MotionFamily::Validation => self.validation,
            MotionFamily::ScrollbarShow => self.scrollbar_show,
            MotionFamily::ScrollbarHide => self.scrollbar_hide,
            MotionFamily::Overscroll => self.overscroll,
            MotionFamily::TooltipEnter => self.tooltip_enter,
            MotionFamily::TooltipExit => self.tooltip_exit,
            MotionFamily::PopoverEnter => self.popover_enter,
            MotionFamily::PopoverExit => self.popover_exit,
            MotionFamily::PanelEnter => self.panel_enter,
            MotionFamily::PanelExit => self.panel_exit,
            MotionFamily::DrawerEnter => self.drawer_enter,
            MotionFamily::DrawerExit => self.drawer_exit,
            MotionFamily::Workspace => self.workspace,
            MotionFamily::Wallpaper => self.wallpaper,
        }
    }

    fn is_valid(self) -> bool {
        let maximum = Duration::from_secs(60);
        [
            self.reduced_transition,
            self.hover_in,
            self.hover_out,
            self.press,
            self.release,
            self.focus,
            self.toggle,
            self.slider_trail,
            self.list_transfer,
            self.text_input_focus,
            self.validation,
            self.scrollbar_show,
            self.scrollbar_hide,
            self.overscroll,
            self.tooltip_enter,
            self.tooltip_exit,
            self.popover_enter,
            self.popover_exit,
            self.panel_enter,
            self.panel_exit,
            self.drawer_enter,
            self.drawer_exit,
            self.workspace,
            self.wallpaper,
        ]
        .into_iter()
        .all(|duration| duration <= maximum)
    }

    fn scaled_speed(self, multiplier: f32) -> Self {
        let factor = 1.0 / f64::from(multiplier);
        Self {
            reduced_transition: scale_duration(self.reduced_transition, factor),
            hover_in: scale_duration(self.hover_in, factor),
            hover_out: scale_duration(self.hover_out, factor),
            press: scale_duration(self.press, factor),
            release: scale_duration(self.release, factor),
            focus: scale_duration(self.focus, factor),
            toggle: scale_duration(self.toggle, factor),
            slider_trail: scale_duration(self.slider_trail, factor),
            list_transfer: scale_duration(self.list_transfer, factor),
            text_input_focus: scale_duration(self.text_input_focus, factor),
            validation: scale_duration(self.validation, factor),
            scrollbar_show: scale_duration(self.scrollbar_show, factor),
            scrollbar_hide: scale_duration(self.scrollbar_hide, factor),
            overscroll: scale_duration(self.overscroll, factor),
            tooltip_enter: scale_duration(self.tooltip_enter, factor),
            tooltip_exit: scale_duration(self.tooltip_exit, factor),
            popover_enter: scale_duration(self.popover_enter, factor),
            popover_exit: scale_duration(self.popover_exit, factor),
            panel_enter: scale_duration(self.panel_enter, factor),
            panel_exit: scale_duration(self.panel_exit, factor),
            drawer_enter: scale_duration(self.drawer_enter, factor),
            drawer_exit: scale_duration(self.drawer_exit, factor),
            workspace: scale_duration(self.workspace, factor),
            wallpaper: scale_duration(self.wallpaper, factor),
        }
    }
}

fn scale_duration(duration: Duration, factor: f64) -> Duration {
    Duration::from_secs_f64(duration.as_secs_f64() * factor)
}

/// Approved default profile. It is plain data so UI-7 can replace individual
/// families without changing widget code.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionProfile {
    pub mode: MotionMode,
    pub standard: CubicBezier,
    pub settle: CubicBezier,
    pub exit: CubicBezier,
    pub soft: CubicBezier,
    pub durations: MotionDurations,
    pub fluid: FluidTuning,
}

impl Default for MotionProfile {
    fn default() -> Self {
        Self {
            mode: MotionMode::Standard,
            standard: CubicBezier::STANDARD,
            settle: CubicBezier::SETTLE,
            exit: CubicBezier::EXIT,
            soft: CubicBezier::SOFT,
            durations: MotionDurations::default(),
            fluid: FluidTuning::default(),
        }
    }
}

impl MotionProfile {
    pub fn spatial_motion_enabled(&self) -> bool {
        matches!(self.mode, MotionMode::Standard | MotionMode::Expressive)
    }

    pub fn spec(&self, family: MotionFamily) -> MotionSpec {
        if self.mode == MotionMode::Off {
            return MotionSpec {
                duration: Duration::ZERO,
                curve: CubicBezier::LINEAR,
            };
        }
        if self.mode == MotionMode::Reduced {
            return MotionSpec {
                duration: self.durations.reduced_transition,
                curve: self.standard,
            };
        }
        let curve = match family {
            MotionFamily::Release
            | MotionFamily::Toggle
            | MotionFamily::ListTransfer
            | MotionFamily::Overscroll
            | MotionFamily::PanelEnter
            | MotionFamily::DrawerEnter => self.settle,
            MotionFamily::HoverOut
            | MotionFamily::ScrollbarHide
            | MotionFamily::TooltipExit
            | MotionFamily::PopoverExit
            | MotionFamily::PanelExit
            | MotionFamily::DrawerExit => self.exit,
            MotionFamily::Workspace | MotionFamily::Wallpaper => self.soft,
            _ => self.standard,
        };
        MotionSpec {
            duration: self.durations.get(family),
            curve,
        }
    }

    /// Scale every duration-bearing motion token while preserving curves and
    /// geometric amplitudes. `1.0` is the authored profile, `1.5` runs at 150%
    /// speed, and `0.5` at 50% speed.
    pub fn with_speed_multiplier(mut self, multiplier: f32) -> Result<Self, MotionError> {
        if !multiplier.is_finite() || multiplier <= 0.0 {
            return Err(MotionError::InvalidSpeedMultiplier);
        }
        self.durations = self.durations.scaled_speed(multiplier);
        self.fluid = self.fluid.scaled_speed(multiplier);
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), MotionError> {
        if ![self.standard, self.settle, self.exit, self.soft]
            .into_iter()
            .all(CubicBezier::is_valid)
        {
            return Err(MotionError::InvalidCurve);
        }
        if !self.fluid.is_valid() || !self.durations.is_valid() {
            return Err(MotionError::InvalidFluidTuning);
        }
        Ok(())
    }
}

/// One scalar which retargets from its currently visible value. It never
/// queues clips, so rapid input preserves continuity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalarMotion {
    from: f32,
    target: f32,
    started: Duration,
    spec: MotionSpec,
}

impl ScalarMotion {
    pub const fn settled(value: f32) -> Self {
        Self {
            from: value,
            target: value,
            started: Duration::ZERO,
            spec: MotionSpec {
                duration: Duration::ZERO,
                curve: CubicBezier::LINEAR,
            },
        }
    }

    pub fn value(self, now: Duration) -> f32 {
        if self.spec.duration.is_zero() {
            return self.target;
        }
        let elapsed = now.saturating_sub(self.started).as_secs_f64();
        let duration = self.spec.duration.as_secs_f64();
        let progress = (elapsed / duration).clamp(0.0, 1.0) as f32;
        self.from + (self.target - self.from) * self.spec.curve.sample(progress)
    }

    pub fn target(self) -> f32 {
        self.target
    }

    pub fn is_active(self, now: Duration) -> bool {
        !self.spec.duration.is_zero()
            && now.saturating_sub(self.started) < self.spec.duration
            && self.from != self.target
    }

    pub fn retarget(&mut self, now: Duration, target: f32, spec: MotionSpec) {
        if self.target == target && self.is_active(now) {
            return;
        }
        let visible = self.value(now);
        self.from = visible;
        self.target = target;
        self.started = now;
        self.spec = spec;
        if spec.duration.is_zero() {
            self.from = target;
        }
    }

    pub fn settle(&mut self, value: f32) {
        *self = Self::settled(value);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionError {
    InvalidCurve,
    InvalidFluidTuning,
    InvalidSpeedMultiplier,
}

impl fmt::Display for MotionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCurve => {
                formatter.write_str("motion curve must be finite with x coordinates in 0..=1")
            }
            Self::InvalidFluidTuning => formatter.write_str("invalid fluid motion tuning"),
            Self::InvalidSpeedMultiplier => {
                formatter.write_str("motion speed multiplier must be finite and positive")
            }
        }
    }
}

impl std::error::Error for MotionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_bezier_preserves_endpoints_and_linear_progress() {
        assert_eq!(CubicBezier::STANDARD.sample(0.0), 0.0);
        assert!((CubicBezier::STANDARD.sample(1.0) - 1.0).abs() < 0.0001);
        assert!((CubicBezier::LINEAR.sample(0.35) - 0.35).abs() < 0.0001);
        assert!(CubicBezier::new(-0.1, 0.0, 1.0, 1.0).is_err());
    }

    #[test]
    fn scalar_motion_retargets_from_visible_value() {
        let mut motion = ScalarMotion::settled(0.0);
        let spec = MotionSpec {
            duration: Duration::from_millis(200),
            curve: CubicBezier::LINEAR,
        };
        motion.retarget(Duration::ZERO, 1.0, spec);
        assert!((motion.value(Duration::from_millis(100)) - 0.5).abs() < 0.0001);
        motion.retarget(Duration::from_millis(100), 0.0, spec);
        assert!((motion.value(Duration::from_millis(100)) - 0.5).abs() < 0.0001);
        assert!((motion.value(Duration::from_millis(300)) - 0.0).abs() < 0.0001);
    }

    #[test]
    fn accessibility_modes_remove_spatial_motion_authority() {
        let mut profile = MotionProfile::default();
        assert!(profile.spatial_motion_enabled());
        profile.mode = MotionMode::Reduced;
        assert!(!profile.spatial_motion_enabled());
        assert_eq!(
            profile.spec(MotionFamily::Toggle).duration,
            Duration::from_millis(100)
        );
        profile.mode = MotionMode::Off;
        assert!(profile.spec(MotionFamily::Toggle).duration.is_zero());
    }

    #[test]
    fn fluid_variation_is_stable_bounded_and_duration_is_distance_limited() {
        let fluid = FluidTuning::default();
        let first = fluid.variation(42);
        let second = fluid.variation(42);
        assert_eq!(first, second);
        assert!((first.neck_multiplier - 1.0).abs() <= fluid.neck_variation);
        assert!((first.trail_multiplier - 1.0).abs() <= fluid.trail_variation);
        assert!(first.path_offset.abs() <= fluid.maximum_path_offset);
        assert_eq!(fluid.transfer_duration(0.0), Duration::from_millis(280));
        assert_eq!(
            fluid.transfer_duration(10_000.0),
            Duration::from_millis(650)
        );
        assert!(fluid.is_valid());
    }

    #[test]
    fn speed_multiplier_scales_control_and_fluid_time_without_changing_curves() {
        let authored = MotionProfile::default();
        let faster = authored.clone().with_speed_multiplier(2.0).unwrap();
        assert_eq!(
            faster.spec(MotionFamily::DrawerEnter).duration,
            Duration::from_millis(160)
        );
        assert_eq!(
            faster.fluid.transfer_duration(0.0),
            Duration::from_millis(140)
        );
        assert_eq!(faster.settle, authored.settle);
        assert!(authored.with_speed_multiplier(0.0).is_err());
    }
}
