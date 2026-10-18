//! Counters for AOT dual-version ("speculative types") functions
//! (M1 item 11).
//!
//! The AOT backend emits a typed version of a function whose hinted
//! parameters live as native `f64`s, plus the generic `Value` version. The
//! public entry checks the arguments: when every speculated argument is a
//! `Number` it runs the typed version, otherwise (guard failure) the generic
//! one. Only functions that have a typed version touch these counters.

use std::cell::Cell;

/// Per-thread entry counts since the last [`reset_stats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TypedStats {
    /// Calls that passed the entry guard and ran the typed version.
    pub typed: u64,
    /// Calls that failed the entry guard and ran the generic version.
    pub guard_failures: u64,
}

impl TypedStats {
    pub fn calls(&self) -> u64 {
        self.typed + self.guard_failures
    }

    /// Share of calls that failed the guard (0 when there were none).
    pub fn guard_failure_rate(&self) -> f64 {
        if self.calls() == 0 {
            0.0
        } else {
            self.guard_failures as f64 / self.calls() as f64
        }
    }
}

thread_local! {
    static STATS: [Cell<u64>; 2] = const { [Cell::new(0), Cell::new(0)] };
}

#[doc(hidden)]
#[inline(always)]
pub fn record_typed_entry() {
    STATS.with(|stats| stats[0].set(stats[0].get().wrapping_add(1)));
}

#[doc(hidden)]
#[inline(always)]
pub fn record_guard_failure() {
    STATS.with(|stats| stats[1].set(stats[1].get().wrapping_add(1)));
}

pub fn stats() -> TypedStats {
    STATS.with(|stats| TypedStats {
        typed: stats[0].get(),
        guard_failures: stats[1].get(),
    })
}

pub fn reset_stats() {
    STATS.with(|stats| stats.iter().for_each(|cell| cell.set(0)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_rate() {
        reset_stats();
        assert_eq!(stats().guard_failure_rate(), 0.0);
        record_typed_entry();
        record_typed_entry();
        record_typed_entry();
        record_guard_failure();
        let s = stats();
        assert_eq!((s.typed, s.guard_failures, s.calls()), (3, 1, 4));
        assert!((s.guard_failure_rate() - 0.25).abs() < 1e-12);
        reset_stats();
        assert_eq!(stats(), TypedStats::default());
    }
}
