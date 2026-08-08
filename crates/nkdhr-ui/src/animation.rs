//! Host-clocked animation primitives without visual defaults.

use std::{cell::Cell, rc::Rc, time::Duration, time::Instant};

use crate::UiError;

/// Monotonic time source supplied by a UI host.
pub trait Clock: 'static {
    fn now(&self) -> Duration;
}

/// Production clock measured from its construction time.
#[derive(Debug)]
pub struct SystemClock {
    started: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.started.elapsed()
    }
}

/// Deterministic clock advanced explicitly by tests or a simulated host.
#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    now: Rc<Cell<Duration>>,
}

impl ManualClock {
    pub fn set(&self, now: Duration) {
        self.now.set(now);
    }

    pub fn advance(&self, amount: Duration) {
        self.now.set(self.now.get().saturating_add(amount));
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        self.now.get()
    }
}

/// A normalized timeline. It deliberately contains no default easing,
/// duration or visual policy; consumers transform `progress` themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeline {
    started: Duration,
    duration: Duration,
}

impl Timeline {
    pub fn new(started: Duration, duration: Duration) -> Result<Self, UiError> {
        if duration.is_zero() {
            return Err(UiError::InvalidAnimationDuration);
        }
        Ok(Self { started, duration })
    }

    pub const fn started(self) -> Duration {
        self.started
    }

    pub const fn duration(self) -> Duration {
        self.duration
    }

    pub fn progress(self, now: Duration) -> f32 {
        let elapsed = now.saturating_sub(self.started);
        (elapsed.as_secs_f64() / self.duration.as_secs_f64()).clamp(0.0, 1.0) as f32
    }

    pub fn is_finished(self, now: Duration) -> bool {
        now.saturating_sub(self.started) >= self.duration
    }
}

/// Interpolate scalar values after the caller applies its chosen easing.
pub fn lerp(start: f32, end: f32, progress: f32) -> f32 {
    start + (end - start) * progress.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_makes_timeline_sampling_deterministic() {
        let clock = ManualClock::default();
        let timeline = Timeline::new(clock.now(), Duration::from_millis(200)).unwrap();
        clock.advance(Duration::from_millis(50));
        assert_eq!(timeline.progress(clock.now()), 0.25);
        assert!(!timeline.is_finished(clock.now()));
        clock.advance(Duration::from_millis(200));
        assert_eq!(timeline.progress(clock.now()), 1.0);
        assert!(timeline.is_finished(clock.now()));
    }
}
