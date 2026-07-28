use object::{Object, ObjectSection, SectionKind};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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
    components: Vec<ComponentArg>,
    compressed: Option<PathBuf>,
    output: Option<PathBuf>,
    baseline_bytes: Option<u64>,
    max_regression_percent: f64,
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
    budget_bytes: u64,
    total_bytes: u64,
    compressed_bytes: Option<u64>,
    text_bytes: u64,
    data_bytes: u64,
    other_object_bytes: u64,
    components: Vec<ComponentReport>,
    baseline: Option<BaselineReport>,
    within_budget: bool,
}

#[derive(Debug, Serialize)]
struct ComponentReport {
    kind: String,
    path: String,
    bytes: u64,
    text_bytes: u64,
    data_bytes: u64,
    other_object_bytes: u64,
}

#[derive(Debug, Serialize)]
struct BaselineReport {
    bytes: u64,
    change_bytes: i128,
    change_percent: f64,
    max_regression_percent: f64,
    within_threshold: bool,
}

#[derive(Default)]
struct Sections {
    text: u64,
    data: u64,
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
                 [--compressed <path>] [--baseline-bytes <n>] \
                 [--max-regression-percent <n>] [--output <path>]"
            );
            std::process::exit(1);
        }
    }
}

fn run(args: impl Iterator<Item = String>) -> Result<bool, String> {
    let config = parse_args(args)?;
    let report = build_report(&config)?;
    let json = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;

    if let Some(path) = &config.output {
        fs::write(path, format!("{json}\n"))
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    } else {
        println!("{json}");
    }

    let baseline_ok = report
        .baseline
        .as_ref()
        .is_none_or(|baseline| baseline.within_threshold);
    Ok(report.within_budget && baseline_ok)
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut profile = None;
    let mut components = Vec::new();
    let mut compressed = None;
    let mut output = None;
    let mut baseline_bytes = None;
    let mut max_regression_percent = 5.0;

    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--profile" => profile = Some(Profile::parse(&value)?),
            "--component" => {
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
            "--compressed" => compressed = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--baseline-bytes" => {
                baseline_bytes = Some(parse_u64("--baseline-bytes", &value)?);
            }
            "--max-regression-percent" => {
                max_regression_percent = value
                    .parse()
                    .map_err(|_| format!("invalid --max-regression-percent: {value}"))?;
                if max_regression_percent < 0.0 {
                    return Err("--max-regression-percent cannot be negative".to_string());
                }
            }
            _ => return Err(format!("unknown argument: {flag}")),
        }
    }

    if components.is_empty() {
        return Err("at least one --component is required".to_string());
    }
    Ok(Config {
        profile: profile.ok_or_else(|| "--profile is required".to_string())?,
        components,
        compressed,
        output,
        baseline_bytes,
        max_regression_percent,
    })
}

fn parse_u64(flag: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {flag}: {value}"))
}

fn build_report(config: &Config) -> Result<Report, String> {
    let components = config
        .components
        .iter()
        .map(inspect_component)
        .collect::<Result<Vec<_>, _>>()?;
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

    Ok(Report {
        schema_version: 1,
        profile: config.profile.name().to_string(),
        budget_bytes: config.profile.budget_bytes(),
        total_bytes,
        compressed_bytes,
        text_bytes: components
            .iter()
            .map(|component| component.text_bytes)
            .sum(),
        data_bytes: components
            .iter()
            .map(|component| component.data_bytes)
            .sum(),
        other_object_bytes: components
            .iter()
            .map(|component| component.other_object_bytes)
            .sum(),
        components,
        baseline,
        within_budget: total_bytes <= config.profile.budget_bytes(),
    })
}

fn inspect_component(component: &ComponentArg) -> Result<ComponentReport, String> {
    let bytes = file_size(&component.path)?;
    let sections = inspect_sections(&component.path)?;
    Ok(ComponentReport {
        kind: component.kind.clone(),
        path: component.path.display().to_string(),
        bytes,
        text_bytes: sections.text,
        data_bytes: sections.data,
        other_object_bytes: sections.other,
    })
}

fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))
}

fn inspect_sections(path: &Path) -> Result<Sections, String> {
    let data =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let Ok(object) = object::File::parse(data.as_slice()) else {
        return Ok(Sections::default());
    };

    let mut result = Sections::default();
    for section in object.sections() {
        match section.kind() {
            SectionKind::Text => result.text += section.size(),
            SectionKind::Data
            | SectionKind::ReadOnlyData
            | SectionKind::ReadOnlyString
            | SectionKind::UninitializedData
            | SectionKind::Tls => result.data += section.size(),
            _ => result.other += section.size(),
        }
    }
    Ok(result)
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
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();
        assert_eq!(config.profile, Profile::Browser);
        assert_eq!(config.components.len(), 2);
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
            components: vec![ComponentArg {
                kind: "executable".to_string(),
                path: executable,
            }],
            compressed: None,
            output: None,
            baseline_bytes: Some(1),
            max_regression_percent: 0.0,
        };
        let report = build_report(&config).unwrap();
        let baseline = report.baseline.expect("baseline report");
        assert!(baseline.change_percent > 0.0);
        assert!(!baseline.within_threshold);
    }
}
