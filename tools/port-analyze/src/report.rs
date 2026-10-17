//! Assemble the clearance report from a source tree.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::scan::{self, DynamicHit, FileScan};

pub const SCHEMA: &str = "w3cos-port-analyze-v1";
const SITE_CAP: usize = 500;

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: &'static str,
    pub root: String,
    pub files_scanned: usize,
    pub bytes_scanned: u64,
    pub package_json_count: usize,
    pub npmrc: BTreeMap<String, String>,
    pub summary: Summary,
    pub electron: ElectronSection,
    pub node_builtins: BTreeMap<String, Usage>,
    pub native_modules: Vec<NativeHit>,
    pub absent_native_modules: Vec<String>,
    pub dynamic: DynamicSection,
    pub clearance_list: Vec<String>,
    pub test_clearance_list: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub electron_api_count: usize,
    pub node_builtin_count: usize,
    pub native_module_count: usize,
    pub dynamic_product_count: usize,
    pub dynamic_test_count: usize,
    pub clearance_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ElectronSection {
    pub apis: Vec<String>,
    pub import_sites: Vec<RecordedSite>,
}

#[derive(Debug, Serialize)]
pub struct Usage {
    pub count: usize,
    pub sites: Vec<RecordedSite>,
}

#[derive(Debug, Serialize)]
pub struct RecordedSite {
    pub file: String,
    pub line: u32,
    pub snippet: String,
}

#[derive(Debug, Serialize)]
pub struct NativeHit {
    pub name: String,
    pub version: String,
    pub declared_in: String,
    pub section: String,
    pub role: String,
    pub plan: String,
}

#[derive(Debug, Serialize)]
pub struct DynamicSection {
    pub counts: BTreeMap<String, usize>,
    pub test_counts: BTreeMap<String, usize>,
    pub sites: Vec<DynamicRecord>,
    pub test_sites: Vec<DynamicRecord>,
}

#[derive(Debug, Serialize)]
pub struct DynamicRecord {
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub detail: String,
    pub snippet: String,
}

#[derive(Clone)]
struct NativeSpec {
    role: String,
    plan: String,
}

struct Accumulator {
    files_scanned: usize,
    bytes_scanned: u64,
    package_json_count: usize,
    npmrc: BTreeMap<String, String>,
    electron: BTreeSet<String>,
    electron_sites: Vec<RecordedSite>,
    node: BTreeMap<String, Vec<RecordedSite>>,
    dynamic: Vec<DynamicRecord>,
    test_dynamic: Vec<DynamicRecord>,
    dependencies: Vec<NativeHit>,
    warnings: Vec<String>,
}

impl Accumulator {
    fn new() -> Self {
        Self {
            files_scanned: 0,
            bytes_scanned: 0,
            package_json_count: 0,
            npmrc: BTreeMap::new(),
            electron: BTreeSet::new(),
            electron_sites: Vec::new(),
            node: BTreeMap::new(),
            dynamic: Vec::new(),
            test_dynamic: Vec::new(),
            dependencies: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

pub fn analyze(root: &Path) -> Result<Report> {
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("Could not resolve {}", root.display()))?;
    let table = native_table()?;
    let mut acc = Accumulator::new();
    walk(&root, &root, &table, &mut acc)?;
    Ok(acc.finish(&root, &table))
}

fn walk(
    root: &Path,
    dir: &Path,
    table: &BTreeMap<String, NativeSpec>,
    acc: &mut Accumulator,
) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("Could not read {}", dir.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        paths.push(entry?.path());
    }
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            walk(root, &path, table, acc)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        if name == "package.json" {
            acc.package_json_count += 1;
            read_package(&path, root, table, acc)?;
            continue;
        }
        if name == ".npmrc" && path.parent() == Some(root.as_ref()) {
            acc.npmrc = parse_npmrc(&fs::read_to_string(&path)?);
            continue;
        }
        if !is_source_name(name) {
            continue;
        }
        if meta.len() > 4_000_000 {
            acc.warnings.push(format!(
                "skipped {} ({} bytes)",
                display_rel(root, &path),
                meta.len()
            ));
            continue;
        }
        let bytes = fs::read(&path)?;
        if bytes.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let scan = scan::scan_source(&text);
        acc.files_scanned += 1;
        acc.bytes_scanned += bytes.len() as u64;
        merge_scan(root, &path, &scan, acc);
    }
    Ok(())
}

fn merge_scan(root: &Path, path: &Path, scan: &FileScan, acc: &mut Accumulator) {
    let file = display_rel(root, path);
    let test_file = is_test_path(&file);
    acc.electron.extend(scan.electron_apis.iter().cloned());
    for site in &scan.electron_sites {
        push_capped(
            &mut acc.electron_sites,
            RecordedSite {
                file: file.clone(),
                line: site.line,
                snippet: site.snippet.clone(),
            },
            &mut acc.warnings,
            "electron import sites",
        );
    }
    for (builtin, sites) in &scan.node_builtins {
        let bucket = acc.node.entry(builtin.clone()).or_default();
        for site in sites {
            bucket.push(RecordedSite {
                file: file.clone(),
                line: site.line,
                snippet: site.snippet.clone(),
            });
        }
    }
    for hit in &scan.dynamic {
        let record = dynamic_record(&file, hit);
        if test_file {
            acc.test_dynamic.push(record);
        } else {
            acc.dynamic.push(record);
        }
    }
}

fn dynamic_record(file: &str, hit: &DynamicHit) -> DynamicRecord {
    DynamicRecord {
        kind: hit.kind.to_string(),
        file: file.to_string(),
        line: hit.line,
        detail: hit.detail.clone(),
        snippet: hit.snippet.clone(),
    }
}

fn push_capped<T>(bucket: &mut Vec<T>, value: T, warnings: &mut Vec<String>, label: &str) {
    if bucket.len() < SITE_CAP {
        bucket.push(value);
        return;
    }
    let warning = format!("{label} truncated to {SITE_CAP}");
    if !warnings.iter().any(|existing| existing == &warning) {
        warnings.push(warning);
    }
}

fn read_package(
    path: &Path,
    root: &Path,
    table: &BTreeMap<String, NativeSpec>,
    acc: &mut Accumulator,
) -> Result<()> {
    let text = fs::read_to_string(path)?;
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            acc.warnings
                .push(format!("{}: {error}", display_rel(root, path)));
            return Ok(());
        }
    };
    let declared_in = display_rel(root, path);
    for section in ["dependencies", "optionalDependencies", "devDependencies"] {
        let Some(deps) = value.get(section).and_then(|item| item.as_object()) else {
            continue;
        };
        let mut names: Vec<_> = deps.keys().cloned().collect();
        names.sort();
        for name in names {
            let Some(spec) = table.get(&name) else {
                continue;
            };
            let version = match &deps[&name] {
                serde_json::Value::String(version) => version.clone(),
                other => other.to_string(),
            };
            acc.dependencies.push(NativeHit {
                name,
                version,
                declared_in: declared_in.clone(),
                section: section.to_string(),
                role: spec.role.clone(),
                plan: spec.plan.clone(),
            });
        }
    }
    Ok(())
}

impl Accumulator {
    fn finish(self, root: &Path, table: &BTreeMap<String, NativeSpec>) -> Report {
        let mut dynamic_counts = BTreeMap::new();
        for site in &self.dynamic {
            *dynamic_counts.entry(site.kind.clone()).or_insert(0) += 1;
        }
        let mut test_counts = BTreeMap::new();
        for site in &self.test_dynamic {
            *test_counts.entry(site.kind.clone()).or_insert(0) += 1;
        }
        let mut warnings = self.warnings;
        let mut node_builtins = BTreeMap::new();
        for (name, mut sites) in self.node {
            let count = sites.len();
            truncate_sites(&mut sites, &mut warnings, &format!("node:{name} sites"));
            node_builtins.insert(name, Usage { count, sites });
        }
        let present: BTreeSet<_> = self
            .dependencies
            .iter()
            .map(|hit| hit.name.clone())
            .collect();
        let mut absent: Vec<_> = table
            .keys()
            .filter(|name| !present.contains(*name))
            .cloned()
            .collect();
        absent.sort();
        let mut clearance = BTreeSet::new();
        for api in &self.electron {
            clearance.insert(format!("electron:{api}"));
        }
        for builtin in node_builtins.keys() {
            clearance.insert(format!("node:{builtin}"));
        }
        for hit in &self.dependencies {
            let prefix = if hit.role == "review" || hit.role == "build-tool" {
                "native-review"
            } else {
                "native"
            };
            clearance.insert(format!("{prefix}:{}", hit.name));
        }
        for site in &self.dynamic {
            clearance.insert(format!("dynamic:{}:{}:{}", site.kind, site.file, site.line));
        }
        let mut test_clearance = BTreeSet::new();
        for site in &self.test_dynamic {
            test_clearance.insert(format!("dynamic:{}:{}:{}", site.kind, site.file, site.line));
        }
        let dynamic_product_count = self.dynamic.len();
        let dynamic_test_count = self.test_dynamic.len();
        let mut dynamic_sites = self.dynamic;
        let mut test_sites = self.test_dynamic;
        truncate_sites(&mut dynamic_sites, &mut warnings, "dynamic sites");
        truncate_sites(&mut test_sites, &mut warnings, "test dynamic sites");
        let summary = Summary {
            electron_api_count: self.electron.len(),
            node_builtin_count: node_builtins.len(),
            native_module_count: present.len(),
            dynamic_product_count,
            dynamic_test_count,
            clearance_count: clearance.len(),
        };
        Report {
            schema: SCHEMA,
            root: root.display().to_string(),
            files_scanned: self.files_scanned,
            bytes_scanned: self.bytes_scanned,
            package_json_count: self.package_json_count,
            npmrc: self.npmrc,
            summary,
            electron: ElectronSection {
                apis: self.electron.into_iter().collect(),
                import_sites: self.electron_sites,
            },
            node_builtins,
            native_modules: self.dependencies,
            absent_native_modules: absent,
            dynamic: DynamicSection {
                counts: dynamic_counts,
                test_counts,
                sites: dynamic_sites,
                test_sites,
            },
            clearance_list: clearance.into_iter().collect(),
            test_clearance_list: test_clearance.into_iter().collect(),
            warnings,
        }
    }
}

fn truncate_sites<T>(sites: &mut Vec<T>, warnings: &mut Vec<String>, label: &str) {
    if sites.len() > SITE_CAP {
        sites.truncate(SITE_CAP);
        warnings.push(format!("{label} truncated to {SITE_CAP}"));
    }
}

fn native_table() -> Result<BTreeMap<String, NativeSpec>> {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("../../vscode-baseline/native-modules.json"))
            .context("native-modules.json is not valid JSON")?;
    if value.get("schema").and_then(|item| item.as_str()) != Some("w3cos-vscode-native-modules-v1")
    {
        bail!("unexpected native module table schema");
    }
    let modules = value
        .get("modules")
        .and_then(|item| item.as_array())
        .context("native module table has no modules")?;
    let mut table = BTreeMap::new();
    for module in modules {
        let name = module
            .get("name")
            .and_then(|item| item.as_str())
            .context("native module is missing a name")?
            .to_string();
        let role = module
            .get("role")
            .and_then(|item| item.as_str())
            .context("native module is missing a role")?
            .to_string();
        if !matches!(
            role.as_str(),
            "rewrite"
                | "wasm"
                | "ship-binary"
                | "stub"
                | "defer"
                | "windows-only"
                | "replaced"
                | "review"
                | "build-tool"
        ) {
            bail!("unknown native module role {role} for {name}");
        }
        let plan = module
            .get("plan")
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .to_string();
        table.insert(name, NativeSpec { role, plan });
    }
    Ok(table)
}

fn display_rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_source_name(name: &str) -> bool {
    const EXTS: &[&str] = &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"];
    EXTS.iter().any(|ext| name.ends_with(ext)) && !name.ends_with(".d.ts")
}

fn is_test_path(path: &str) -> bool {
    path.split('/').any(|segment| {
        segment == "test" || segment == "tests" || segment == "__tests__" || segment == "testing"
    }) || path.contains(".test.")
        || path.contains(".spec.")
}

pub fn parse_npmrc(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        values.insert(key.trim().to_string(), value.to_string());
    }
    values
}

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "out",
    "out-build",
    "out-vscode",
    "out-vscode-min",
    ".git",
    ".build",
    "dist",
    "coverage",
    ".vscode",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_tree_builds_a_clearance_list() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("package.json"),
            r#"{"name":"fixture","version":"0.0.1","main":"./out/main.js","dependencies":{"node-pty":"1.2.0","left-pad":"1.0.0"}}"#,
        )
        .unwrap();
        fs::write(
            root.path().join(".npmrc"),
            "runtime=\"electron\"\ntarget=\"43.7.7\"\nbuild_from_source=\"true\"\n",
        )
        .unwrap();
        let src = root.path().join("src");
        fs::create_dir_all(src.join("test")).unwrap();
        fs::write(
            src.join("main.ts"),
            "import { app, BrowserWindow } from \"electron\";\nrequire(\"fs\");\neval(\"x\");\n",
        )
        .unwrap();
        fs::write(
            src.join("test").join("hidden.test.ts"),
            "eval(\"test-only\");\n",
        )
        .unwrap();
        let skipped = root.path().join("node_modules").join("pkg");
        fs::create_dir_all(&skipped).unwrap();
        fs::write(skipped.join("index.js"), "eval(\"vendor\");\n").unwrap();
        fs::write(root.path().join("types.d.ts"), "eval(\"types\");\n").unwrap();

        let report = analyze(root.path()).unwrap();
        assert_eq!(report.schema, SCHEMA);
        assert_eq!(report.files_scanned, 2);
        assert_eq!(
            report.npmrc.get("target").map(String::as_str),
            Some("43.7.7")
        );
        assert_eq!(
            report.electron.apis,
            vec!["BrowserWindow".to_string(), "app".to_string()]
        );
        assert!(report.node_builtins.contains_key("fs"));
        assert_eq!(report.native_modules.len(), 1);
        assert_eq!(report.native_modules[0].name, "node-pty");
        assert_eq!(report.native_modules[0].role, "rewrite");
        assert!(
            report
                .clearance_list
                .iter()
                .any(|item| item == "electron:app")
        );
        assert!(report.clearance_list.iter().any(|item| item == "node:fs"));
        assert!(
            report
                .clearance_list
                .iter()
                .any(|item| item == "native:node-pty")
        );
        assert!(
            report
                .clearance_list
                .iter()
                .any(|item| item.starts_with("dynamic:eval:src/main.ts:"))
        );
        assert!(
            report
                .clearance_list
                .iter()
                .all(|item| !item.contains("hidden.test"))
        );
        assert!(
            report
                .clearance_list
                .iter()
                .all(|item| !item.contains("vendor"))
        );
        assert!(
            report
                .test_clearance_list
                .iter()
                .any(|item| item.contains("hidden.test.ts"))
        );
        assert!(
            report
                .absent_native_modules
                .iter()
                .any(|name| name == "keytar")
        );
    }
}
