mod attribution;
mod baseline;
mod embedded;
mod markdown;

use attribution::Attribution;
use baseline::{RegressionLimits, TargetBaseline};
use embedded::EmbeddedResource;
use object::{Object, ObjectSection, SectionKind};
use serde::Serialize;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const MIB: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    OsLinkedSimple,
    OsLinkedNormal,
    Aot,
    Runtime,
    Browser,
}

impl Profile {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "os-linked-simple" => Ok(Self::OsLinkedSimple),
            "os-linked-normal" => Ok(Self::OsLinkedNormal),
            "aot" => Ok(Self::Aot),
            "runtime" => Ok(Self::Runtime),
            "browser" => Ok(Self::Browser),
            _ => Err(format!("unknown profile: {value}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::OsLinkedSimple => "os-linked-simple",
            Self::OsLinkedNormal => "os-linked-normal",
            Self::Aot => "aot",
            Self::Runtime => "runtime",
            Self::Browser => "browser",
        }
    }

    fn budget_bytes(self) -> u64 {
        match self {
            Self::OsLinkedSimple => MIB,
            Self::OsLinkedNormal => 5 * MIB,
            Self::Aot => 20 * MIB,
            Self::Runtime => 60 * MIB,
            Self::Browser => 120 * MIB,
        }
    }
}

#[derive(Debug)]
struct Config {
    profile: Profile,
    probe: String,
    probe_description: String,
    features: Vec<String>,
    components: Vec<ComponentArg>,
    compressed: Option<PathBuf>,
    output: Option<PathBuf>,
    markdown: Option<PathBuf>,
    symbols: Option<PathBuf>,
    embedded_roots: Vec<PathBuf>,
    baseline_bytes: Option<u64>,
    max_regression_percent: f64,
    baseline_file: Option<PathBuf>,
    target: Option<String>,
    checked_in_percent: Option<f64>,
    checked_in_bytes: Option<u64>,
    update_baseline: bool,
    require_baseline: bool,
    require_symbols: bool,
    baseline_note: Option<String>,
}

#[derive(Debug)]
struct ComponentArg {
    kind: String,
    path: PathBuf,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    profile: String,
    probe: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    features: Vec<String>,
    budget_bytes: u64,
    total_bytes: u64,
    compressed_bytes: Option<u64>,
    gzip_bytes: u64,
    text_bytes: u64,
    data_bytes: u64,
    readonly_data_bytes: u64,
    bss_bytes: u64,
    other_object_bytes: u64,
    embedded_bytes: u64,
    components: Vec<ComponentReport>,
    embedded_resources: Vec<EmbeddedResource>,
    attribution: Attribution,
    baseline: Option<BaselineReport>,
    checked_in_baseline: Option<CheckedInBaselineReport>,
    within_budget: bool,
}

#[derive(Debug, Serialize)]
struct ComponentReport {
    kind: String,
    path: String,
    bytes: u64,
    gzip_bytes: u64,
    text_bytes: u64,
    data_bytes: u64,
    readonly_data_bytes: u64,
    bss_bytes: u64,
    other_object_bytes: u64,
    embedded_bytes: u64,
}

#[derive(Debug, Serialize)]
struct BaselineReport {
    bytes: u64,
    change_bytes: i128,
    change_percent: f64,
    max_regression_percent: f64,
    within_threshold: bool,
}

#[derive(Debug, Serialize)]
struct CheckedInBaselineReport {
    compared: bool,
    reason: String,
    bytes: Option<u64>,
    change_bytes: i128,
    change_percent: f64,
    max_regression_percent: f64,
    max_regression_bytes: u64,
    within_threshold: bool,
    note: String,
}

#[derive(Default)]
struct Sections {
    text: u64,
    data: u64,
    readonly: u64,
    bss: u64,
    other: u64,
}

fn main() {
    match run(env::args().skip(1)) {
        Ok(within_limits) if within_limits => {}
        Ok(_) => std::process::exit(2),
        Err(error) => {
            eprintln!("w3cos-size-audit: {error}");
            eprintln!(
                "usage: w3cos-size-audit --profile <os-linked-simple|os-linked-normal|aot|runtime|browser> \
                 --component <executable|resource|native-library>=<path> [--component ...] \
                 [--symbols <unstripped>] [--embedded-root <dir>] [--compressed <path>] \
                 [--baseline-bytes <n>] [--max-regression-percent <n>] \
                 [--baseline-file <path>] [--probe <id>] [--target <triple>] \
                 [--require-baseline] [--update-baseline --baseline-note <text>] \
                 [--require-symbols] [--feature <name>] [--markdown <path>] [--output <path>]"
            );
            std::process::exit(1);
        }
    }
}

fn run(args: impl Iterator<Item = String>) -> Result<bool, String> {
    let config = parse_args(args)?;
    let mut report = build_report(&config)?;
    let checked_in_ok = apply_checked_in_baseline(&config, &mut report)?;
    if config.require_symbols && !report.attribution.symbols_present {
        return Err(
            "symbol attribution is empty; pass --symbols with the unstripped probe".to_string(),
        );
    }
    let json = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
    if let Some(path) = &config.output {
        fs::write(path, format!("{json}\n"))
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    } else {
        println!("{json}");
    }
    if let Some(path) = &config.markdown {
        fs::write(path, markdown::render(&report))
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    }

    let baseline_ok = report
        .baseline
        .as_ref()
        .is_none_or(|baseline| baseline.within_threshold);
    let within = report.within_budget && baseline_ok && checked_in_ok;
    if !within {
        explain_limit(&report, report.within_budget, baseline_ok, checked_in_ok);
    }
    Ok(within)
}

/// CI only keeps this process's stderr. The markdown report stays on the
/// runner, so a failing gate has to say which row moved.
fn explain_limit(report: &Report, budget_ok: bool, baseline_ok: bool, checked_in_ok: bool) {
    let target = report.target.as_deref().unwrap_or("unknown-target");
    if !budget_ok {
        eprintln!(
            "w3cos-size-audit: {} {target} is {} bytes, over the {} byte budget",
            report.probe, report.total_bytes, report.budget_bytes
        );
    }
    if !baseline_ok && let Some(baseline) = &report.baseline {
        eprintln!(
            "w3cos-size-audit: {} grew {} bytes ({:.4}%) from {} bytes; limit {:.4}%",
            report.probe,
            baseline.change_bytes,
            baseline.change_percent,
            baseline.bytes,
            baseline.max_regression_percent
        );
    }
    if !checked_in_ok && let Some(checked) = &report.checked_in_baseline {
        eprintln!(
            "w3cos-size-audit: {} {target} is {} bytes, {} bytes ({:.4}%) from the checked-in {} byte baseline; fails above {:.4}% or {} bytes",
            report.probe,
            report.total_bytes,
            checked.change_bytes,
            checked.change_percent,
            checked.bytes.unwrap_or(0),
            checked.max_regression_percent,
            checked.max_regression_bytes
        );
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut args = args.peekable();
    let mut profile = None;
    let mut probe = None;
    let mut probe_description = String::new();
    let mut features = Vec::new();
    let mut components = Vec::new();
    let mut compressed = None;
    let mut output = None;
    let mut markdown = None;
    let mut symbols = None;
    let mut embedded_roots = Vec::new();
    let mut baseline_bytes = None;
    let mut max_regression_percent = 5.0;
    let mut baseline_file = None;
    let mut target = None;
    let mut checked_in_percent = None;
    let mut checked_in_bytes = None;
    let mut update_baseline = false;
    let mut require_baseline = false;
    let mut require_symbols = false;
    let mut baseline_note = None;

    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--update-baseline" => update_baseline = true,
            "--require-baseline" => require_baseline = true,
            "--require-symbols" => require_symbols = true,
            "--profile" => profile = Some(Profile::parse(&required(&mut args, &flag)?)?),
            "--probe" => probe = Some(required(&mut args, &flag)?),
            "--probe-description" => probe_description = required(&mut args, &flag)?,
            "--feature" => features.push(required(&mut args, &flag)?),
            "--component" => {
                let value = required(&mut args, &flag)?;
                let (kind, path) = value
                    .split_once('=')
                    .ok_or_else(|| "--component must be kind=path".to_string())?;
                if !matches!(kind, "executable" | "resource" | "native-library") {
                    return Err(format!("unknown component kind: {kind}"));
                }
                components.push(ComponentArg {
                    kind: kind.to_string(),
                    path: PathBuf::from(path),
                });
            }
            "--compressed" => compressed = Some(PathBuf::from(required(&mut args, &flag)?)),
            "--output" => output = Some(PathBuf::from(required(&mut args, &flag)?)),
            "--markdown" => markdown = Some(PathBuf::from(required(&mut args, &flag)?)),
            "--symbols" => symbols = Some(PathBuf::from(required(&mut args, &flag)?)),
            "--embedded-root" => embedded_roots.push(PathBuf::from(required(&mut args, &flag)?)),
            "--baseline-bytes" => {
                baseline_bytes = Some(parse_u64("--baseline-bytes", &required(&mut args, &flag)?)?);
            }
            "--max-regression-percent" => {
                max_regression_percent = parse_percent(&required(&mut args, &flag)?)?;
            }
            "--baseline-file" => baseline_file = Some(PathBuf::from(required(&mut args, &flag)?)),
            "--target" => target = Some(required(&mut args, &flag)?),
            "--max-checked-in-percent" => {
                checked_in_percent = Some(parse_percent(&required(&mut args, &flag)?)?);
            }
            "--max-checked-in-bytes" => {
                checked_in_bytes = Some(parse_u64(
                    "--max-checked-in-bytes",
                    &required(&mut args, &flag)?,
                )?);
            }
            "--baseline-note" => baseline_note = Some(required(&mut args, &flag)?),
            _ => return Err(format!("unknown argument: {flag}")),
        }
    }

    if components.is_empty() {
        return Err("at least one --component is required".to_string());
    }
    let profile = profile.ok_or_else(|| "--profile is required".to_string())?;
    Ok(Config {
        probe: probe.unwrap_or_else(|| profile.name().to_string()),
        profile,
        probe_description,
        features,
        components,
        compressed,
        output,
        markdown,
        symbols,
        embedded_roots,
        baseline_bytes,
        max_regression_percent,
        baseline_file,
        target,
        checked_in_percent,
        checked_in_bytes,
        update_baseline,
        require_baseline,
        require_symbols,
        baseline_note,
    })
}

fn required(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn parse_percent(value: &str) -> Result<f64, String> {
    let parsed = value
        .parse()
        .map_err(|_| format!("invalid percent: {value}"))?;
    if parsed < 0.0 {
        return Err("regression percent cannot be negative".to_string());
    }
    Ok(parsed)
}

fn parse_u64(flag: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {flag}: {value}"))
}

fn build_report(config: &Config) -> Result<Report, String> {
    for root in &config.embedded_roots {
        if !root.exists() {
            return Err(format!("embedded root does not exist: {}", root.display()));
        }
    }
    let inspected = config
        .components
        .iter()
        .map(|component| inspect_component(component, &config.embedded_roots))
        .collect::<Result<Vec<_>, _>>()?;
    let mut embedded_resources = Vec::new();
    for (_, resources) in &inspected {
        embedded_resources.extend(resources.iter().cloned());
    }
    embedded_resources.sort_by(|left, right| {
        left.offset
            .cmp(&right.offset)
            .then(left.kind.cmp(&right.kind))
    });
    let components: Vec<ComponentReport> = inspected
        .into_iter()
        .map(|(component, _)| component)
        .collect();
    let total_bytes = components.iter().map(|component| component.bytes).sum();
    let compressed_bytes = config.compressed.as_deref().map(file_size).transpose()?;
    let baseline = config.baseline_bytes.map(|bytes| {
        let change_bytes = i128::from(total_bytes) - i128::from(bytes);
        let change_percent = if bytes == 0 {
            if total_bytes == 0 { 0.0 } else { f64::INFINITY }
        } else {
            change_bytes as f64 * 100.0 / bytes as f64
        };
        BaselineReport {
            bytes,
            change_bytes,
            change_percent,
            max_regression_percent: config.max_regression_percent,
            within_threshold: change_percent <= config.max_regression_percent,
        }
    });
    let attribution = attribute_symbols(config)?;

    Ok(Report {
        schema_version: 2,
        profile: config.profile.name().to_string(),
        probe: config.probe.clone(),
        target: config.target.clone(),
        features: config.features.clone(),
        budget_bytes: config.profile.budget_bytes(),
        total_bytes,
        compressed_bytes,
        gzip_bytes: components
            .iter()
            .map(|component| component.gzip_bytes)
            .sum(),
        text_bytes: components
            .iter()
            .map(|component| component.text_bytes)
            .sum(),
        data_bytes: components
            .iter()
            .map(|component| component.data_bytes)
            .sum(),
        readonly_data_bytes: components
            .iter()
            .map(|component| component.readonly_data_bytes)
            .sum(),
        bss_bytes: components.iter().map(|component| component.bss_bytes).sum(),
        other_object_bytes: components
            .iter()
            .map(|component| component.other_object_bytes)
            .sum(),
        embedded_bytes: embedded::embedded_bytes(&embedded_resources),
        components,
        embedded_resources,
        attribution,
        baseline,
        checked_in_baseline: None,
        within_budget: total_bytes <= config.profile.budget_bytes(),
    })
}

fn attribute_symbols(config: &Config) -> Result<Attribution, String> {
    if let Some(path) = &config.symbols {
        let bytes = fs::read(path)
            .map_err(|error| format!("cannot read symbols {}: {error}", path.display()))?;
        let mut attribution = attribution::attribute_object(&bytes);
        attribution.symbols_path = Some(path.display().to_string());
        return Ok(attribution);
    }
    if let Some(component) = config
        .components
        .iter()
        .find(|component| component.kind == "executable")
    {
        let bytes = fs::read(&component.path)
            .map_err(|error| format!("cannot read {}: {error}", component.path.display()))?;
        let mut attribution = attribution::attribute_object(&bytes);
        if attribution.symbols_present {
            attribution.symbols_path = Some(component.path.display().to_string());
        }
        return Ok(attribution);
    }
    Ok(attribution::attribute_object(&[]))
}

fn apply_checked_in_baseline(config: &Config, report: &mut Report) -> Result<bool, String> {
    let Some(path) = &config.baseline_file else {
        return Ok(true);
    };
    let target = match &config.target {
        Some(target) => target.clone(),
        None => host_target()?,
    };
    report.target = Some(target.clone());
    if !path.exists() && !config.update_baseline {
        if config.require_baseline {
            return Err(format!(
                "checked-in baseline {} does not exist for {}",
                path.display(),
                target
            ));
        }
        report.checked_in_baseline = Some(unchecked("missing-file"));
        return Ok(true);
    }
    let mut file = if path.exists() {
        baseline::load(path)?
    } else {
        baseline::empty_file()
    };
    let limits = RegressionLimits {
        max_percent: config
            .checked_in_percent
            .unwrap_or(file.threshold.max_regression_percent),
        max_bytes: config
            .checked_in_bytes
            .unwrap_or(file.threshold.max_regression_bytes),
    };
    let existing = file
        .probes
        .get(&config.probe)
        .and_then(|probe| probe.targets.get(&target));
    if existing.is_none() && config.require_baseline && !config.update_baseline {
        return Err(format!(
            "no checked-in baseline for probe {} on {target}. Update it on this same target with \
             scripts/audit-linkage-sizes.sh --update-baseline --baseline-note \"why\"; do not copy \
             another architecture's row",
            config.probe
        ));
    }
    let comparison = baseline::compare(existing, report.total_bytes, limits);
    report.checked_in_baseline = Some(CheckedInBaselineReport {
        compared: comparison.compared,
        reason: comparison.reason.to_string(),
        bytes: comparison.baseline_bytes,
        change_bytes: comparison.change_bytes,
        change_percent: comparison.change_percent,
        max_regression_percent: limits.max_percent,
        max_regression_bytes: limits.max_bytes,
        within_threshold: comparison.within_threshold,
        note: comparison.note,
    });
    if config.update_baseline {
        let note = config
            .baseline_note
            .clone()
            .ok_or_else(|| "--baseline-note is required with --update-baseline".to_string())?;
        baseline::upsert(
            &mut file,
            &config.probe,
            &config.probe_description,
            &config.features,
            &target,
            TargetBaseline {
                total_bytes: report.total_bytes,
                text_bytes: report.text_bytes,
                data_bytes: report.data_bytes,
                readonly_data_bytes: report.readonly_data_bytes,
                gzip_bytes: report.gzip_bytes,
                embedded_bytes: report.embedded_bytes,
                feature_symbol_bytes: report
                    .attribution
                    .features
                    .iter()
                    .map(|feature| (feature.feature.clone(), feature.bytes))
                    .collect(),
                note,
            },
        )?;
        baseline::store(path, &file)?;
        return Ok(true);
    }
    Ok(comparison.within_threshold)
}

fn unchecked(reason: &str) -> CheckedInBaselineReport {
    CheckedInBaselineReport {
        compared: false,
        reason: reason.to_string(),
        bytes: None,
        change_bytes: 0,
        change_percent: 0.0,
        max_regression_percent: baseline::DEFAULT_MAX_REGRESSION_PERCENT,
        max_regression_bytes: baseline::DEFAULT_MAX_REGRESSION_BYTES,
        within_threshold: true,
        note: String::new(),
    }
}

fn host_target() -> Result<String, String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|error| format!("cannot run rustc -vV: {error}"))?;
    if !output.status.success() {
        return Err("rustc -vV failed".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "rustc -vV did not report a host triple".to_string())
}

fn inspect_component(
    component: &ComponentArg,
    roots: &[PathBuf],
) -> Result<(ComponentReport, Vec<EmbeddedResource>), String> {
    let bytes = fs::read(&component.path)
        .map_err(|error| format!("cannot read {}: {error}", component.path.display()))?;
    let sections = inspect_sections(&bytes);
    let resources = embedded::scan_bytes(&bytes, roots);
    let embedded_bytes = embedded::embedded_bytes(&resources);
    Ok((
        ComponentReport {
            kind: component.kind.clone(),
            path: component.path.display().to_string(),
            bytes: bytes.len() as u64,
            gzip_bytes: gzip_len(&bytes)?,
            text_bytes: sections.text,
            data_bytes: sections.data,
            readonly_data_bytes: sections.readonly,
            bss_bytes: sections.bss,
            other_object_bytes: sections.other,
            embedded_bytes,
        },
        resources,
    ))
}

fn gzip_len(bytes: &[u8]) -> Result<u64, String> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder
        .write_all(bytes)
        .map_err(|error| format!("gzip failed: {error}"))?;
    let compressed = encoder
        .finish()
        .map_err(|error| format!("gzip failed: {error}"))?;
    Ok(compressed.len() as u64)
}

fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))
}

fn inspect_sections(bytes: &[u8]) -> Sections {
    let Ok(object) = object::File::parse(bytes) else {
        return Sections::default();
    };
    let mut result = Sections::default();
    for section in object.sections() {
        match section.kind() {
            SectionKind::Text => result.text += section.size(),
            SectionKind::ReadOnlyData | SectionKind::ReadOnlyString => {
                result.data += section.size();
                result.readonly += section.size();
            }
            SectionKind::Data | SectionKind::Tls => result.data += section.size(),
            SectionKind::UninitializedData => {
                result.data += section.size();
                result.bss += section.size();
            }
            _ => result.other += section.size(),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_budgets_are_stable() {
        assert_eq!(Profile::OsLinkedSimple.budget_bytes(), MIB);
        assert_eq!(Profile::OsLinkedNormal.budget_bytes(), 5 * MIB);
        assert_eq!(Profile::Aot.budget_bytes(), 20 * MIB);
        assert_eq!(Profile::Runtime.budget_bytes(), 60 * MIB);
        assert_eq!(Profile::Browser.budget_bytes(), 120 * MIB);
    }

    #[test]
    fn parses_non_overlapping_components() {
        let config = parse_args(
            [
                "--profile",
                "browser",
                "--component",
                "executable=target/browser",
                "--component",
                "resource=assets/fonts.pack",
                "--require-baseline",
                "--feature",
                "dynamic-js",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();
        assert_eq!(config.profile, Profile::Browser);
        assert_eq!(config.components.len(), 2);
        assert!(config.require_baseline);
        assert_eq!(config.features, vec!["dynamic-js".to_string()]);
        assert_eq!(config.probe, "browser");
    }

    #[test]
    fn rejects_unknown_component_kind() {
        let error = parse_args(
            ["--profile", "aot", "--component", "package=target/app"]
                .into_iter()
                .map(String::from),
        )
        .unwrap_err();
        assert!(error.contains("unknown component kind"));
    }

    #[test]
    fn baseline_threshold_detects_a_regression() {
        let executable = std::env::current_exe().unwrap();
        let config = Config {
            profile: Profile::Browser,
            probe: "browser".into(),
            probe_description: String::new(),
            features: Vec::new(),
            components: vec![ComponentArg {
                kind: "executable".to_string(),
                path: executable,
            }],
            compressed: None,
            output: None,
            markdown: None,
            symbols: None,
            embedded_roots: Vec::new(),
            baseline_bytes: Some(1),
            max_regression_percent: 0.0,
            baseline_file: None,
            target: None,
            checked_in_percent: None,
            checked_in_bytes: None,
            update_baseline: false,
            require_baseline: false,
            require_symbols: false,
            baseline_note: None,
        };
        let report = build_report(&config).unwrap();
        let baseline = report.baseline.as_ref().expect("baseline report");
        assert!(baseline.change_percent > 0.0);
        assert!(!baseline.within_threshold);
        assert!(report.gzip_bytes > 0);
        assert!(report.gzip_bytes < report.total_bytes);
        assert_eq!(report.schema_version, 2);
        let rendered = markdown::render(&report);
        assert!(rendered.contains("Executable sections"));
        assert!(rendered.contains("Crate attribution"));
    }

    #[test]
    fn checked_in_update_writes_only_the_requested_target() {
        let dir = std::env::temp_dir().join(format!("w3cos-size-baseline-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("probe.bin");
        fs::write(&executable, vec![0u8; 4096]).unwrap();
        let baseline = dir.join("baselines.json");
        let config = Config {
            profile: Profile::Aot,
            probe: "aot".into(),
            probe_description: "ordinary".into(),
            features: Vec::new(),
            components: vec![ComponentArg {
                kind: "resource".into(),
                path: executable,
            }],
            compressed: None,
            output: None,
            markdown: None,
            symbols: None,
            embedded_roots: Vec::new(),
            baseline_bytes: None,
            max_regression_percent: 5.0,
            baseline_file: Some(baseline.clone()),
            target: Some("x86_64-unknown-linux-gnu".into()),
            checked_in_percent: None,
            checked_in_bytes: None,
            update_baseline: true,
            require_baseline: false,
            require_symbols: false,
            baseline_note: Some("measured on this target".into()),
        };
        let mut report = build_report(&config).unwrap();
        assert!(apply_checked_in_baseline(&config, &mut report).unwrap());
        let file = baseline::load(&baseline).unwrap();
        assert_eq!(
            file.probes["aot"].targets["x86_64-unknown-linux-gnu"].total_bytes,
            4096
        );
        assert_eq!(file.threshold.max_regression_percent, 3.0);
        assert_eq!(file.threshold.max_regression_bytes, 200 * 1024);
        assert!(
            file.retired
                .iter()
                .any(|item| item.name == "monaco-full-runtime-arm64-macos")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
