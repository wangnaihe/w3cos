//! Frame timing for benchmark / regression gates (`W3COS_PERF=1`).

use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_SAMPLES: usize = 240;

#[derive(Clone, Copy, Default)]
struct FrameSample {
    layout_us: u64,
    paint_us: u64,
    total_us: u64,
}

static SCENARIO: Mutex<String> = Mutex::new(String::new());
static BACKEND: Mutex<String> = Mutex::new(String::new());
static SAMPLES: Mutex<Vec<FrameSample>> = Mutex::new(Vec::new());
static FRAME_START: Mutex<Option<Instant>> = Mutex::new(None);

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
}

pub fn record_layout(elapsed: Duration) {
    if !enabled() {
        return;
    }
    let layout_us = elapsed.as_micros() as u64;
    if let Ok(mut samples) = SAMPLES.lock() {
        samples.push(FrameSample {
            layout_us,
            paint_us: 0,
            total_us: layout_us,
        });
        if samples.len() > MAX_SAMPLES {
            let drop = samples.len() - MAX_SAMPLES;
            samples.drain(0..drop);
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
            last.total_us = last.layout_us.saturating_add(paint_us);
        } else {
            samples.push(FrameSample {
                layout_us: 0,
                paint_us,
                total_us: paint_us,
            });
        }
        if samples.len() > MAX_SAMPLES {
            let drop = samples.len() - MAX_SAMPLES;
            samples.drain(0..drop);
        }
    }
    if let Ok(mut t) = FRAME_START.lock() {
        *t = None;
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
    let layout: Vec<u64> = samples.iter().map(|s| s.layout_us).collect();
    let paint: Vec<u64> = samples.iter().map(|s| s.paint_us).collect();
    let total: Vec<u64> = samples.iter().map(|s| s.total_us).collect();
    serde_json::json!({
        "scenario": scenario,
        "backend": backend,
        "frame": stats(&total),
        "layout": stats(&layout),
        "paint": stats(&paint),
        "viewport": viewport_label(),
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
}
