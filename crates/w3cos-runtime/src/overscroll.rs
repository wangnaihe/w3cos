//! Compositor-only scroll boundary affordance.
//!
//! Logical scroll offsets remain clamped to their scroll range. This state is
//! a visual translation applied after layout, matching browser engines where
//! rubber-banding never changes `scrollTop` or triggers reflow.

const MAX_DISPLACEMENT_RATIO: f32 = 0.18;
const MIN_MAX_DISPLACEMENT: f32 = 48.0;
const MAX_MAX_DISPLACEMENT: f32 = 120.0;
const DRAG_RESISTANCE: f32 = 0.52;
const SPRING_STIFFNESS: f32 = 300.0;
const SPRING_DAMPING: f32 = 30.0;
const STOP_DISPLACEMENT: f32 = 0.25;
const STOP_VELOCITY: f32 = 5.0;
const MAX_PHYSICS_STEP: f32 = 1.0 / 120.0;
const MAX_RELEASE_VELOCITY: f32 = 1_800.0;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OverscrollState {
    /// Visual content translation. Positive moves content down.
    pub(crate) displacement_y: f32,
    pub(crate) velocity_y: f32,
}

impl OverscrollState {
    pub(crate) fn is_active(self) -> bool {
        self.displacement_y.abs() >= STOP_DISPLACEMENT || self.velocity_y.abs() >= STOP_VELOCITY
    }

    /// Consume drag input that moves an existing rubber-band back to zero.
    /// Returns the remaining logical scroll delta.
    pub(crate) fn consume_restoring_drag(&mut self, scroll_delta_y: f32) -> f32 {
        let visual_delta = -scroll_delta_y;
        if self.displacement_y == 0.0
            || visual_delta == 0.0
            || self.displacement_y.signum() == visual_delta.signum()
        {
            return scroll_delta_y;
        }

        let restored = visual_delta.abs().min(self.displacement_y.abs()) * visual_delta.signum();
        self.displacement_y += restored;
        self.velocity_y = 0.0;
        let consumed_scroll = -restored;
        if self.displacement_y.abs() < f32::EPSILON {
            self.displacement_y = 0.0;
        }
        scroll_delta_y - consumed_scroll
    }

    pub(crate) fn drag_past_boundary(&mut self, unconsumed_scroll_y: f32, viewport_height: f32) {
        if unconsumed_scroll_y == 0.0 {
            return;
        }
        let limit = overscroll_limit(viewport_height);
        let remaining_ratio = (1.0 - self.displacement_y.abs() / limit).clamp(0.08, 1.0);
        self.displacement_y += -unconsumed_scroll_y * DRAG_RESISTANCE * remaining_ratio;
        self.displacement_y = self.displacement_y.clamp(-limit, limit);
        self.velocity_y = 0.0;
    }

    pub(crate) fn release(&mut self, visual_velocity_y: f32) {
        self.velocity_y = visual_velocity_y.clamp(-MAX_RELEASE_VELOCITY, MAX_RELEASE_VELOCITY);
    }

    /// Time-based critically damped-ish spring, integrated in bounded steps so
    /// 60 Hz and 120 Hz devices converge along the same trajectory.
    pub(crate) fn tick(&mut self, elapsed_seconds: f32) -> bool {
        let mut remaining = elapsed_seconds.clamp(0.0, 0.05);
        while remaining > 0.0 {
            let dt = remaining.min(MAX_PHYSICS_STEP);
            let acceleration =
                -SPRING_STIFFNESS * self.displacement_y - SPRING_DAMPING * self.velocity_y;
            self.velocity_y += acceleration * dt;
            self.displacement_y += self.velocity_y * dt;
            remaining -= dt;
        }
        if !self.is_active() {
            self.displacement_y = 0.0;
            self.velocity_y = 0.0;
            false
        } else {
            true
        }
    }
}

fn overscroll_limit(viewport_height: f32) -> f32 {
    (viewport_height * MAX_DISPLACEMENT_RATIO).clamp(MIN_MAX_DISPLACEMENT, MAX_MAX_DISPLACEMENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_is_resisted_and_bounded() {
        let mut state = OverscrollState::default();
        for _ in 0..100 {
            state.drag_past_boundary(-20.0, 700.0);
        }
        assert!(state.displacement_y > 0.0);
        assert!(state.displacement_y <= 120.0);
    }

    #[test]
    fn reversing_drag_restores_before_scrolling() {
        let mut state = OverscrollState {
            displacement_y: 30.0,
            velocity_y: 0.0,
        };
        assert_eq!(state.consume_restoring_drag(20.0), 0.0);
        assert_eq!(state.displacement_y, 10.0);
        assert_eq!(state.consume_restoring_drag(20.0), 10.0);
        assert_eq!(state.displacement_y, 0.0);
    }

    #[test]
    fn spring_converges_at_60_and_120_hz() {
        fn simulate(dt: f32) -> OverscrollState {
            let mut state = OverscrollState {
                displacement_y: 90.0,
                velocity_y: 250.0,
            };
            for _ in 0..(2.0 / dt) as usize {
                state.tick(dt);
            }
            state
        }
        let at_60 = simulate(1.0 / 60.0);
        let at_120 = simulate(1.0 / 120.0);
        assert_eq!(at_60.displacement_y, 0.0);
        assert_eq!(at_120.displacement_y, 0.0);
    }
}
