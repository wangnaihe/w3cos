//! Frame timing for benchmark / regression gates (`W3COS_PERF=1`).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_SAMPLES: usize = 240;

#[derive(Clone, Copy, Default)]
struct FrameSample {
    cpu_frame_us: u64,
    layout_us: u64,
    paint_us: u64,
    react_commit_us: u64,
    react_entry_us: u64,
    react_host_us: u64,
    react_builder_us: u64,
    react_reconcile_us: u64,
    react_drop_us: u64,
    observer_us: u64,
    total_us: u64,
    paint_recorded: bool,
}

static SCENARIO: Mutex<String> = Mutex::new(String::new());
static BACKEND: Mutex<String> = Mutex::new(String::new());
static SAMPLES: Mutex<Vec<FrameSample>> = Mutex::new(Vec::new());
static FRAME_START: Mutex<Option<Instant>> = Mutex::new(None);
static PAINT_PATHS: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());

use std::sync::atomic::{AtomicBool, Ordering};

static FORCE_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    if FORCE_ENABLED.load(Ordering::Relaxed) {
        return true;
    }
    std::env::var("W3COS_PERF").ok().as_deref() == Some("1")
}

pub fn force_enable() {
    FORCE_ENABLED.store(true, Ordering::Relaxed);
}

pub fn set_scenario(name: &str) {
    if !enabled() {
        return;
    }
    if let Ok(mut s) = SCENARIO.lock() {
        *s = name.to_string();
    }
    if let Ok(mut samples) = SAMPLES.lock() {
        samples.clear();
    }
    if let Ok(mut started) = FRAME_START.lock() {
        *started = None;
    }
    if let Ok(mut paths) = PAINT_PATHS.lock() {
        paths.clear();
    }
}

pub fn set_backend(name: &str) {
    if let Ok(mut backend) = BACKEND.lock() {
        *backend = name.to_string();
    }
}

pub fn begin_frame() {
    if !enabled() {
        return;
    }
    if let Ok(mut t) = FRAME_START.lock() {
        *t = Some(Instant::now());
    }
    if let Ok(mut samples) = SAMPLES.lock() {
        samples.push(FrameSample::default());
        if samples.len() > MAX_SAMPLES {
            let drop = samples.len() - MAX_SAMPLES;
            samples.drain(0..drop);
        }
    }
}

pub fn record_layout(elapsed: Duration) {
    if !enabled() {
        return;
    }
    let layout_us = elapsed.as_micros() as u64;
    if let Ok(mut samples) = SAMPLES.lock()
        && let Some(sample) = samples.last_mut()
    {
        sample.layout_us = layout_us;
        sample.total_us = phase_sum_us(sample);
    }
}

fn phase_sum_us(sample: &FrameSample) -> u64 {
    sample
        .layout_us
        .saturating_add(sample.paint_us)
        .saturating_add(sample.react_commit_us)
        .saturating_add(sample.observer_us)
}

fn frame_in_progress() -> bool {
    FRAME_START
        .lock()
        .ok()
        .is_some_and(|started| started.is_some())
}

/// Finish the current rendering opportunity and record continuous main-thread
/// wall time from before rAF/prepaint through snapshot/post-paint work.
///
/// This is a CPU-side deadline metric. It is not GPU completion or display
/// presentation time.
pub fn end_frame() {
    if !enabled() {
        return;
    }
    let started = FRAME_START
        .lock()
        .ok()
        .and_then(|mut started| started.take());
    let Some(started) = started else {
        return;
    };
    if let Ok(mut samples) = SAMPLES.lock()
        && let Some(sample) = samples.last_mut()
    {
        if sample.paint_recorded {
            sample.cpu_frame_us = started.elapsed().as_micros() as u64;
        } else {
            // Surface acquisition/size failures are not completed frames and
            // must not become zero-duration benchmark samples.
            samples.pop();
        }
    }
}

pub fn record_paint(elapsed: Duration) {
    if !enabled() {
        return;
    }
    let paint_us = elapsed.as_micros() as u64;
    if let Ok(mut samples) = SAMPLES.lock() {
        if let Some(last) = samples.last_mut() {
            last.paint_us = paint_us;
            last.paint_recorded = true;
            last.total_us = phase_sum_us(last);
        }
    }
}

pub fn record_react_commit(elapsed: Duration) {
    record_post_paint_phase(elapsed, |sample, elapsed_us| {
        sample.react_commit_us = sample.react_commit_us.saturating_add(elapsed_us);
    });
}

pub fn record_react_builder(elapsed: Duration) {
    record_react_subphase(elapsed, |sample, elapsed_us| {
        sample.react_builder_us = sample.react_builder_us.saturating_add(elapsed_us);
    });
}

pub fn record_react_entry(elapsed: Duration) {
    record_react_subphase(elapsed, |sample, elapsed_us| {
        sample.react_entry_us = sample.react_entry_us.saturating_add(elapsed_us);
    });
}

pub fn record_react_host(elapsed: Duration) {
    record_react_subphase(elapsed, |sample, elapsed_us| {
        sample.react_host_us = sample.react_host_us.saturating_add(elapsed_us);
    });
}

pub fn record_react_reconcile(elapsed: Duration) {
    record_react_subphase(elapsed, |sample, elapsed_us| {
        sample.react_reconcile_us = sample.react_reconcile_us.saturating_add(elapsed_us);
    });
}

pub fn record_react_drop(elapsed: Duration) {
    record_react_subphase(elapsed, |sample, elapsed_us| {
        sample.react_drop_us = sample.react_drop_us.saturating_add(elapsed_us);
    });
}

fn record_react_subphase(elapsed: Duration, update: impl FnOnce(&mut FrameSample, u64)) {
    if !enabled() || !frame_in_progress() {
        return;
    }
    let elapsed_us = elapsed.as_micros() as u64;
    if let Ok(mut samples) = SAMPLES.lock()
        && let Some(sample) = samples.last_mut()
    {
        update(sample, elapsed_us);
    }
}

pub fn record_observer_delivery(elapsed: Duration) {
    record_post_paint_phase(elapsed, |sample, elapsed_us| {
        sample.observer_us = sample.observer_us.saturating_add(elapsed_us);
    });
}

fn record_post_paint_phase(elapsed: Duration, update: impl FnOnce(&mut FrameSample, u64)) {
    if !enabled() || !frame_in_progress() {
        return;
    }
    let elapsed_us = elapsed.as_micros() as u64;
    if let Ok(mut samples) = SAMPLES.lock()
        && let Some(sample) = samples.last_mut()
    {
        update(sample, elapsed_us);
        sample.total_us = phase_sum_us(sample);
    }
}

pub fn record_paint_path(path: &'static str) {
    if !enabled() {
        return;
    }
    if let Ok(mut paths) = PAINT_PATHS.lock() {
        *paths.entry(path).or_default() += 1;
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn stats(values: &[u64]) -> serde_json::Value {
    if values.is_empty() {
        return serde_json::json!({
            "count": 0,
            "mean_ms": 0.0,
            "p50_ms": 0.0,
            "p95_ms": 0.0,
            "p99_ms": 0.0,
            "max_ms": 0.0,
            "budget_16ms_pct": 0.0
        });
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let sum: u64 = sorted.iter().sum();
    let under_budget = sorted.iter().filter(|&&v| v <= 16_700).count();
    serde_json::json!({
        "count": sorted.len(),
        "mean_ms": (sum as f64 / sorted.len() as f64) / 1000.0,
        "p50_ms": percentile(&sorted, 0.50) as f64 / 1000.0,
        "p95_ms": percentile(&sorted, 0.95) as f64 / 1000.0,
        "p99_ms": percentile(&sorted, 0.99) as f64 / 1000.0,
        "max_ms": *sorted.last().unwrap_or(&0) as f64 / 1000.0,
        "budget_16ms_pct": (under_budget as f64 / sorted.len() as f64) * 100.0
    })
}

pub fn summary_json() -> serde_json::Value {
    let scenario = SCENARIO.lock().ok().map(|g| g.clone()).unwrap_or_default();
    let samples = SAMPLES.lock().ok().map(|g| g.clone()).unwrap_or_default();
    let backend = BACKEND.lock().ok().map(|g| g.clone()).unwrap_or_default();
    let paint_paths = PAINT_PATHS
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    let layout: Vec<u64> = samples.iter().map(|s| s.layout_us).collect();
    let paint: Vec<u64> = samples.iter().map(|s| s.paint_us).collect();
    let react_commit: Vec<u64> = samples.iter().map(|s| s.react_commit_us).collect();
    let react_builder: Vec<u64> = samples.iter().map(|s| s.react_builder_us).collect();
    let react_entry: Vec<u64> = samples.iter().map(|s| s.react_entry_us).collect();
    let react_host: Vec<u64> = samples.iter().map(|s| s.react_host_us).collect();
    let react_reconcile: Vec<u64> = samples.iter().map(|s| s.react_reconcile_us).collect();
    let react_drop: Vec<u64> = samples.iter().map(|s| s.react_drop_us).collect();
    let observer: Vec<u64> = samples.iter().map(|s| s.observer_us).collect();
    let cpu_frame: Vec<u64> = samples.iter().map(|s| s.cpu_frame_us).collect();
    let total: Vec<u64> = samples.iter().map(|s| s.total_us).collect();
    serde_json::json!({
        "scenario": scenario,
        "backend": backend,
        "cpu_frame": stats(&cpu_frame),
        "phase_sum": stats(&total),
        "display_frame": {
            "available": false,
            "reason": "platform presentation timestamps are not instrumented"
        },
        "metric_semantics": {
            "cpu_frame": "continuous main-thread wall time from rendering opportunity through post-paint work",
            "phase_sum": "diagnostic sum of selected CPU phases; not a frame clock",
            "paint": "CPU render and presentation submission call; not displayed latency"
        },
        "layout": stats(&layout),
        "paint": stats(&paint),
        "react_commit": stats(&react_commit),
        "react_builder": stats(&react_builder),
        "react_entry": stats(&react_entry),
        "react_host": stats(&react_host),
        "react_reconcile": stats(&react_reconcile),
        "react_drop": stats(&react_drop),
        "observer": stats(&observer),
        "paint_paths": paint_paths,
        "viewport": viewport_label(),
        "evidence": crate::uitest::scenario_evidence_json(),
    })
}

fn viewport_label() -> &'static str {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        "native"
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        "1200x800"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_empty() {
        assert_eq!(percentile(&[], 0.95), 0);
    }

    #[test]
    fn percentile_single() {
        assert_eq!(percentile(&[100], 0.95), 100);
    }

    #[test]
    fn summary_separates_cpu_wall_time_from_unavailable_display_time() {
        force_enable();
        set_scenario("metric-semantics");
        begin_frame();
        std::thread::sleep(Duration::from_millis(1));
        record_layout(Duration::from_micros(100));
        record_paint(Duration::from_micros(200));
        end_frame();

        let summary = summary_json();
        assert_eq!(summary["cpu_frame"]["count"], 1);
        assert!(summary.get("frame").is_none());
        assert_eq!(summary["phase_sum"]["count"], 1);
        assert_eq!(summary["display_frame"]["available"], false);

        let before = summary["phase_sum"]["p95_ms"].as_f64().unwrap();
        record_react_commit(Duration::from_millis(5));
        assert_eq!(
            summary_json()["phase_sum"]["p95_ms"].as_f64().unwrap(),
            before,
            "work outside an active rendering opportunity must not mutate the previous frame"
        );
    }
}
