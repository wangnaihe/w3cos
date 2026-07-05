//! `w3cos mobile init` — scaffold from generic templates (no product coupling).

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

fn w3cos_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest_dir
        .join("../..")
        .canonicalize()
        .context("locate w3cos repo root")?)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

pub fn mobile_init(project_name: &PathBuf, platform: &str) -> Result<()> {
    if project_name.exists() {
        anyhow::bail!("directory already exists: {}", project_name.display());
    }

    let root = w3cos_root()?;
    let shared = root.join("templates/shared");
    let android_tpl = root.join("templates/android");

    fs::create_dir_all(project_name).context("create project dir")?;

    copy_dir_recursive(&shared, project_name).context("copy templates/shared")?;

    if platform == "android" || platform == "both" {
        let android_dst = project_name.join("android");
        copy_dir_recursive(&android_tpl, &android_dst).context("copy templates/android")?;
    }

    if platform == "ios" || platform == "both" {
        println!("⚠️  iOS template not yet available (M5). Skipped.");
    }

    let readme = project_name.join("README.md");
    let content = format!(
        r#"# {name}

W3C OS mobile project (generic scaffold).

## Desktop test

```bash
w3cos build app.tsx -o app --release
./app
```

## Android

See `android/README.md`.

Manifest: `w3cos.app.json`
"#,
        name = project_name.display()
    );
    fs::write(&readme, content)?;

    println!("✅ Mobile project: {}", project_name.display());
    println!("   app.tsx, w3cos.app.json");
    if platform == "android" || platform == "both" {
        println!("   android/  (Gradle shell)");
    }
    println!("\nNext: cd {} && w3cos build app.tsx -o app --release", project_name.display());
    Ok(())
}

pub fn mobile_build_hint() -> Result<()> {
    println!("w3cos mobile build — coming in M2.");
    println!("Manual M1: see docs/MOBILE.md and templates/android/README.md");
    Ok(())
}
