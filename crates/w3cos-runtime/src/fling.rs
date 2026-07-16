//! Touch fling curve based on Chromium's `ui::MobileScroller`.
//!
//! Chromium and Android `OverScroller` use the same sampled spline so fling
//! distance and duration grow non-linearly with release velocity. Sampling by
//! absolute elapsed time also makes the path independent of display refresh
//! rate and dropped frames.

use std::sync::OnceLock;

const SAMPLE_COUNT: usize = 100;
const DECELERATION_RATE: f32 = 2.358_201_8; // ln(0.78) / ln(0.9)
const INFLEXION: f32 = 0.35;
const FLING_FRICTION: f32 = 0.015;
const GRAVITY_EARTH: f32 = 9.806_65;
const INCHES_PER_METER: f32 = 39.37;
const BASELINE_PPI: f32 = 160.0;
const TUNING_FRICTION: f32 = 0.84;

#[derive(Clone, Copy, Debug)]
pub(crate) struct FlingSample {
    pub offset: f32,
    pub velocity: f32,
    pub active: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MobileFlingCurve {
    duration_seconds: f32,
    distance: f32,
}

impl MobileFlingCurve {
    pub(crate) fn new(velocity: f32) -> Self {
        let speed = velocity.abs().max(1.0);
        let physical_coeff = compute_deceleration(TUNING_FRICTION);
        let deceleration = (INFLEXION * speed / (FLING_FRICTION * physical_coeff)).ln();
        let decel_minus_one = DECELERATION_RATE - 1.0;
        let duration_seconds = (deceleration / decel_minus_one).exp();
        let distance = FLING_FRICTION
            * physical_coeff
            * (DECELERATION_RATE / decel_minus_one * deceleration).exp()
            * velocity.signum();
        Self {
            duration_seconds,
            distance,
        }
    }

    #[cfg(test)]
    pub(crate) fn duration_seconds(self) -> f32 {
        self.duration_seconds
    }

    #[cfg(test)]
    pub(crate) fn terminal_offset(self) -> f32 {
        self.distance
    }

    pub(crate) fn sample(self, elapsed_seconds: f32) -> FlingSample {
        if elapsed_seconds >= self.duration_seconds {
            return FlingSample {
                offset: self.distance,
                velocity: 0.0,
                active: false,
            };
        }
        let progress = (elapsed_seconds / self.duration_seconds).clamp(0.0, 1.0);
        let (distance_coefficient, velocity_coefficient) = spline_coefficients(progress);
        FlingSample {
            offset: distance_coefficient * self.distance,
            velocity: velocity_coefficient * self.distance / self.duration_seconds,
            active: true,
        }
    }
}

fn compute_deceleration(friction: f32) -> f32 {
    GRAVITY_EARTH * INCHES_PER_METER * BASELINE_PPI * friction
}

fn spline_coefficients(progress: f32) -> (f32, f32) {
    let samples = spline_positions();
    let scaled = progress * SAMPLE_COUNT as f32;
    let index = scaled.floor() as usize;
    if index >= SAMPLE_COUNT {
        return (1.0, 0.0);
    }
    let lower_t = index as f32 / SAMPLE_COUNT as f32;
    let upper_t = (index + 1) as f32 / SAMPLE_COUNT as f32;
    let lower = samples[index];
    let upper = samples[index + 1];
    let velocity_coefficient = (upper - lower) / (upper_t - lower_t);
    (
        lower + (progress - lower_t) * velocity_coefficient,
        velocity_coefficient,
    )
}

fn spline_positions() -> &'static [f32; SAMPLE_COUNT + 1] {
    static POSITIONS: OnceLock<[f32; SAMPLE_COUNT + 1]> = OnceLock::new();
    POSITIONS.get_or_init(|| {
        let start_tension = 0.5;
        let end_tension = 1.0;
        let p1 = start_tension * INFLEXION;
        let p2 = 1.0 - end_tension * (1.0 - INFLEXION);
        let mut positions = [0.0; SAMPLE_COUNT + 1];
        let mut x_min = 0.0;
        for (index, position) in positions.iter_mut().enumerate().take(SAMPLE_COUNT) {
            let alpha = index as f32 / SAMPLE_COUNT as f32;
            let mut x_max = 1.0;
            let (x, coefficient) = loop {
                let x = x_min + (x_max - x_min) * 0.5;
                let coefficient = 3.0 * x * (1.0 - x);
                let tx = coefficient * ((1.0 - x) * p1 + x * p2) + x * x * x;
                if (tx - alpha).abs() < 1e-5 {
                    break (x, coefficient);
                }
                if tx > alpha {
                    x_max = x;
                } else {
                    x_min = x;
                }
            };
            *position = coefficient * ((1.0 - x) * start_tension + x) + x * x * x;
        }
        positions[SAMPLE_COUNT] = 1.0;
        positions
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_reference_velocity_has_expected_duration_and_distance() {
        let curve = MobileFlingCurve::new(1_500.0);
        assert!((curve.duration_seconds() - 0.748).abs() < 0.01);
        assert!((curve.terminal_offset() - 393.0).abs() < 3.0);
    }

    #[test]
    fn higher_velocity_grows_duration_and_distance_non_linearly() {
        let slow = MobileFlingCurve::new(1_000.0);
        let fast = MobileFlingCurve::new(2_000.0);
        assert!(fast.duration_seconds() > slow.duration_seconds());
        assert!(fast.terminal_offset() > slow.terminal_offset() * 2.0);
    }

    #[test]
    fn absolute_time_sampling_is_refresh_rate_independent() {
        let curve = MobileFlingCurve::new(-2_000.0);
        let at_half = curve.sample(curve.duration_seconds() * 0.5);
        assert!(at_half.active);
        assert!(at_half.offset < 0.0);
        assert!(at_half.velocity < 0.0);
        let end = curve.sample(curve.duration_seconds());
        assert!(!end.active);
        assert_eq!(end.offset, curve.terminal_offset());
        assert_eq!(end.velocity, 0.0);
    }
}
