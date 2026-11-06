//! Same-target checked-in size baselines.
//!
//! A regression is an increase above 3% or above 200 KiB, whichever trips
//! first. Baselines are keyed by the Rust host triple so an arm64 measurement
//! is never compared with an x86_64 runner.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const DEFAULT_MAX_REGRESSION_PERCENT: f64 = 3.0;
pub const DEFAULT_MAX_REGRESSION_BYTES: u64 = 200 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineFile {
    pub schema_version: u32,
    pub threshold: BaselineThreshold,
    pub probes: BTreeMap<String, ProbeBaseline>,
    pub retired: Vec<RetiredBaseline>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineThreshold {
    pub max_regression_percent: f64,
    pub max_regression_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeBaseline {
    pub description: String,
    pub features: Vec<String>,
    pub targets: BTreeMap<String, TargetBaseline>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetBaseline {
    pub total_bytes: u64,
    pub text_bytes: u64,
    pub data_bytes: u64,
    pub readonly_data_bytes: u64,
    pub gzip_bytes: u64,
    pub embedded_bytes: u64,
    /// Symbol-table bytes for known feature buckets. Not a link-size gate.
    #[serde(default)]
    pub feature_symbol_bytes: BTreeMap<String, u64>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetiredBaseline {
    pub name: String,
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegressionLimits {
    pub max_percent: f64,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub compared: bool,
    pub within_threshold: bool,
    pub reason: &'static str,
    pub baseline_bytes: Option<u64>,
    pub change_bytes: i128,
    pub change_percent: f64,
    pub limits: RegressionLimits,
    pub note: String,
}

pub fn regression_exceeds(baseline: u64, current: u64, limits: RegressionLimits) -> bool {
    if current <= baseline {
        return false;
    }
    let delta = current - baseline;
    if delta > limits.max_bytes {
        return true;
    }
    if baseline == 0 {
        return current > 0;
    }
    let percent = delta as f64 * 100.0 / baseline as f64;
    percent > limits.max_percent
}

pub fn compare(
    baseline: Option<&TargetBaseline>,
    current: u64,
    limits: RegressionLimits,
) -> Comparison {
    let Some(baseline) = baseline else {
        return Comparison {
            compared: false,
            within_threshold: true,
            reason: "missing-target",
            baseline_bytes: None,
            change_bytes: 0,
            change_percent: 0.0,
            limits,
            note: String::new(),
        };
    };
    let change_bytes = i128::from(current) - i128::from(baseline.total_bytes);
    let change_percent = if baseline.total_bytes == 0 {
        if current == 0 { 0.0 } else { f64::INFINITY }
    } else {
        change_bytes as f64 * 100.0 / baseline.total_bytes as f64
    };
    Comparison {
        compared: true,
        within_threshold: !regression_exceeds(baseline.total_bytes, current, limits),
        reason: "compared",
        baseline_bytes: Some(baseline.total_bytes),
        change_bytes,
        change_percent,
        limits,
        note: baseline.note.clone(),
    }
}

pub fn load(path: &Path) -> Result<BaselineFile, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read baseline {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("invalid baseline {}: {error}", path.display()))
}

pub fn store(path: &Path, file: &BaselineFile) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(file).map_err(|error| error.to_string())?;
    fs::write(path, format!("{json}\n"))
        .map_err(|error| format!("cannot write baseline {}: {error}", path.display()))
}

pub fn empty_file() -> BaselineFile {
    BaselineFile {
        schema_version: 1,
        threshold: BaselineThreshold {
            max_regression_percent: DEFAULT_MAX_REGRESSION_PERCENT,
            max_regression_bytes: DEFAULT_MAX_REGRESSION_BYTES,
        },
        probes: BTreeMap::new(),
        retired: vec![RetiredBaseline {
            name: "monaco-full-runtime-arm64-macos".to_string(),
            status: "retired".to_string(),
            reason: "The single about-172-MB-unstripped / about-120-MB-stripped arm64 macOS Monaco artifact mixed application modules with every default runtime feature and is not a same-runner measurement. CI tracks per-closure linkage baselines in this file instead. Render, font, and image-codec product slices are outside this gate.".to_string(),
        }],
    }
}

pub fn upsert(
    file: &mut BaselineFile,
    probe: &str,
    description: &str,
    features: &[String],
    target: &str,
    measurement: TargetBaseline,
) -> Result<(), String> {
    let note = measurement.note.trim();
    if note.is_empty() {
        return Err("--baseline-note is required when updating a checked-in baseline".to_string());
    }
    let entry = file
        .probes
        .entry(probe.to_string())
        .or_insert(ProbeBaseline {
            description: description.to_string(),
            features: features.to_vec(),
            targets: BTreeMap::new(),
        });
    if !description.is_empty() {
        entry.description = description.to_string();
    }
    if !features.is_empty() {
        entry.features = features.to_vec();
    }
    entry.targets.insert(
        target.to_string(),
        TargetBaseline {
            note: note.to_string(),
            ..measurement
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn limits() -> RegressionLimits {
        RegressionLimits {
            max_percent: DEFAULT_MAX_REGRESSION_PERCENT,
            max_bytes: DEFAULT_MAX_REGRESSION_BYTES,
        }
    }

    #[test]
    fn threshold_is_either_three_percent_or_two_hundred_kib() {
        let limits = limits();
        assert!(!regression_exceeds(
            10_000_000,
            10_000_000 + 200 * 1024,
            limits
        ));
        assert!(regression_exceeds(
            10_000_000,
            10_000_000 + 200 * 1024 + 1,
            limits
        ));
        assert!(!regression_exceeds(1_000_000, 1_030_000, limits));
        assert!(regression_exceeds(1_000_000, 1_030_001, limits));
        assert!(!regression_exceeds(1_000_000, 900_000, limits));
    }

    #[test]
    fn missing_target_is_not_a_cross_arch_failure() {
        let comparison = compare(None, 123, limits());
        assert!(!comparison.compared);
        assert!(comparison.within_threshold);
        assert_eq!(comparison.reason, "missing-target");
    }

    #[test]
    fn update_requires_a_note_and_preserves_other_targets() {
        let mut file = empty_file();
        let measurement = TargetBaseline {
            total_bytes: 10,
            text_bytes: 6,
            data_bytes: 4,
            readonly_data_bytes: 1,
            gzip_bytes: 3,
            embedded_bytes: 0,
            feature_symbol_bytes: BTreeMap::new(),
            note: "initial".into(),
        };
        upsert(
            &mut file,
            "aot",
            "ordinary",
            &[],
            "x86_64-unknown-linux-gnu",
            measurement.clone(),
        )
        .unwrap();
        let mut other = measurement.clone();
        other.total_bytes = 11;
        other.note = "mac".into();
        upsert(&mut file, "aot", "", &[], "aarch64-apple-darwin", other).unwrap();
        assert_eq!(file.probes["aot"].targets.len(), 2);
        assert!(
            upsert(
                &mut file,
                "aot",
                "",
                &[],
                "x86_64-unknown-linux-gnu",
                TargetBaseline {
                    note: "  ".into(),
                    ..measurement
                },
            )
            .is_err()
        );
    }
}
