use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteManifest {
    pub schema_version: u32,
    pub upstream: String,
    pub revision: String,
    pub viewport: Viewport,
    pub tests: Vec<TestCase>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCase {
    pub path: String,
    pub kind: TestKind,
    #[serde(default)]
    pub expected_subtests_min: Option<usize>,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub relation: Option<ReferenceRelation>,
    #[serde(default)]
    pub fuzzy: FuzzyAllowance,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestKind {
    Testharness,
    Reftest,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRelation {
    Match,
    Mismatch,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FuzzyAllowance {
    #[serde(default)]
    pub max_difference: u8,
    #[serde(default)]
    pub total_pixels: u64,
}

impl SuiteManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read WPT suite manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid WPT suite manifest {}", path.display()))?;
        manifest.validate_shape()?;
        Ok(manifest)
    }

    pub fn validate_checkout(&self, root: &Path) -> Result<()> {
        if !root.is_dir() {
            bail!("WPT checkout does not exist: {}", root.display());
        }
        let head = git_output(root, &["rev-parse", "HEAD"])?;
        if head != self.revision {
            bail!(
                "WPT revision mismatch: manifest pins {}, checkout is {}",
                self.revision,
                head
            );
        }
        let dirty = git_output(root, &["status", "--porcelain=v1"])?;
        if !dirty.is_empty() {
            bail!("WPT checkout must be clean at the pinned revision");
        }
        self.validate_files(root)
    }

    pub fn validate_files(&self, root: &Path) -> Result<()> {
        for test in &self.tests {
            ensure_regular_file(root, &test.path, "test")?;
            if let Some(reference) = &test.reference {
                ensure_regular_file(root, reference, "reference")?;
            }
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!(
                "unsupported WPT suite schema_version {}",
                self.schema_version
            );
        }
        if self.upstream != "https://github.com/web-platform-tests/wpt" {
            bail!("WPT suite upstream must use the canonical repository");
        }
        if self.revision.len() != 40
            || !self
                .revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("WPT revision must be a full lowercase Git SHA");
        }
        if self.viewport.width == 0 || self.viewport.height == 0 {
            bail!("WPT viewport dimensions must be positive");
        }
        if self.tests.is_empty() {
            bail!("WPT suite must contain at least one test");
        }

        let mut paths = HashSet::new();
        for test in &self.tests {
            validate_relative_path(&test.path)?;
            if !paths.insert(test.path.as_str()) {
                bail!("duplicate WPT test path {}", test.path);
            }
            match test.kind {
                TestKind::Testharness => {
                    if test.reference.is_some() || test.relation.is_some() {
                        bail!(
                            "testharness case {} must not declare a reference",
                            test.path
                        );
                    }
                }
                TestKind::Reftest => {
                    let Some(reference) = &test.reference else {
                        bail!("reftest case {} requires reference", test.path);
                    };
                    validate_relative_path(reference)?;
                    if test.relation.is_none() {
                        bail!("reftest case {} requires relation", test.path);
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_relative_path(path: &str) -> Result<()> {
    let parsed = Path::new(path);
    if path.is_empty()
        || parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("WPT paths must be normalized relative paths: {path:?}");
    }
    if path.contains('\\') || path.contains('?') || path.contains('#') {
        bail!("WPT manifest paths must not contain URL syntax: {path:?}");
    }
    Ok(())
}

fn ensure_regular_file(root: &Path, relative: &str, role: &str) -> Result<PathBuf> {
    let path = root.join(relative);
    if !path.is_file() {
        bail!("WPT {role} file is missing: {}", path.display());
    }
    Ok(path)
}

fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("failed to invoke git for {}", root.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed for {}: {}",
            args.join(" "),
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> SuiteManifest {
        SuiteManifest {
            schema_version: 1,
            upstream: "https://github.com/web-platform-tests/wpt".into(),
            revision: "a".repeat(40),
            viewport: Viewport {
                width: 800,
                height: 600,
            },
            tests: vec![TestCase {
                path: "dom/example.html".into(),
                kind: TestKind::Testharness,
                expected_subtests_min: Some(1),
                reference: None,
                relation: None,
                fuzzy: FuzzyAllowance::default(),
            }],
        }
    }

    #[test]
    fn accepts_a_pinned_normalized_suite() {
        valid_manifest().validate_shape().unwrap();
    }

    #[test]
    fn rejects_traversal_and_silent_reftest_defaults() {
        let mut traversal = valid_manifest();
        traversal.tests[0].path = "../outside.html".into();
        assert!(traversal.validate_shape().is_err());

        let mut reftest = valid_manifest();
        reftest.tests[0].kind = TestKind::Reftest;
        assert!(reftest.validate_shape().is_err());
    }

    #[test]
    fn verifies_every_declared_file() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("dom")).unwrap();
        std::fs::write(directory.path().join("dom/example.html"), "<!doctype html>").unwrap();
        valid_manifest().validate_files(directory.path()).unwrap();

        std::fs::remove_file(directory.path().join("dom/example.html")).unwrap();
        assert!(valid_manifest().validate_files(directory.path()).is_err());
    }
}
