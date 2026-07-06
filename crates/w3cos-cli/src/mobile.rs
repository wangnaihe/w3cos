//! `w3cos mobile init` / `w3cos mobile build` — generic mobile pipeline.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        bail!("directory already exists: {}", project_name.display());
    }

    let root = w3cos_root()?;
    let shared = root.join("templates/shared");
    let android_tpl = root.join("templates/android");
    let ios_tpl = root.join("templates/ios");

    fs::create_dir_all(project_name).context("create project dir")?;
    copy_dir_recursive(&shared, project_name).context("copy templates/shared")?;

    if platform == "android" || platform == "both" {
        copy_dir_recursive(&android_tpl, &project_name.join("android"))
            .context("copy templates/android")?;
    }

    if platform == "ios" || platform == "both" {
        copy_dir_recursive(&ios_tpl, &project_name.join("ios")).context("copy templates/ios")?;
    }

    let readme = project_name.join("README.md");
    fs::write(
        &readme,
        format!(
            r#"# {name}

W3C OS mobile project.

## Desktop smoke test

```bash
w3cos build app.tsx -o app --release && ./app
```

## Mobile build

```bash
w3cos mobile build --platform android   # APK (needs SDK + NDK)
w3cos mobile build --platform ios        # Simulator (needs Xcode)
w3cos mobile build --platform both
```

Manifest: `w3cos.app.json`
"#,
            name = project_name.display()
        ),
    )?;

    println!("✅ Mobile project: {}", project_name.display());
    if platform == "android" || platform == "both" {
        println!("   android/  Gradle + NativeActivity");
    }
    if platform == "ios" || platform == "both" {
        println!("   ios/      Xcode shell");
    }
    Ok(())
}

pub fn mobile_build(
    project_dir: &Path,
    platform: &str,
    release: bool,
) -> Result<()> {
    let app_tsx = project_dir.join("app.tsx");
    if !app_tsx.exists() {
        bail!("missing app.tsx in {}", project_dir.display());
    }

    let build_dir = project_dir.join(".w3cos/mobile-build");
    if build_dir.exists() {
        fs::remove_dir_all(&build_dir)?;
    }
    fs::create_dir_all(&build_dir)?;

    println!("⚡ Transpiling {} → mobile cdylib...", app_tsx.display());
    w3cos_compiler::compile_mobile_from_file(&app_tsx, &build_dir)?;

    match platform {
        "android" => build_android(project_dir, &build_dir, release)?,
        "ios" => build_ios(project_dir, &build_dir, release)?,
        "both" => {
            build_android(project_dir, &build_dir, release)?;
            build_ios(project_dir, &build_dir, release)?;
        }
        other => bail!("unknown platform: {other} (use android|ios|both)"),
    }

    Ok(())
}

fn cargo_ndk_available() -> bool {
    Command::new("cargo")
        .args(["ndk", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn build_android(project_dir: &Path, build_dir: &Path, release: bool) -> Result<()> {
    let android_dir = project_dir.join("android");
    if !android_dir.exists() {
        bail!(
            "android/ not found — run: w3cos mobile init . --platform android (in project dir)"
        );
    }

    if !cargo_ndk_available() {
        println!("⚠️  cargo-ndk not found. Install: cargo install cargo-ndk");
        println!("   Then set ANDROID_NDK_HOME or install NDK via Android Studio SDK Manager.");
    }

    let profile = if release { "release" } else { "debug" };
    let jni_dir = android_dir.join("app/src/main/jniLibs/arm64-v8a");
    fs::create_dir_all(&jni_dir)?;

    println!("🔨 Building Android arm64-v8a ({profile})...");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(build_dir).arg("ndk");
    cmd.args(["-t", "arm64-v8a", "-o", jni_dir.to_str().unwrap(), "build"]);
    if release {
        cmd.arg("--release");
    }
    let status = cmd.status().context("cargo ndk build failed")?;
    if !status.success() {
        bail!(
            "Android native build failed. Ensure ANDROID_NDK_HOME, rustup target aarch64-linux-android, and cargo-ndk are installed."
        );
    }

    let so_name = format!("libw3cos_mobile_app.so");
    let so_path = jni_dir.join(&so_name);
    if !so_path.exists() {
        bail!("expected {} after cargo ndk build", so_path.display());
    }
    println!("✅ Native lib: {}", so_path.display());

    let gradlew = android_dir.join("gradlew");
    if gradlew.exists() {
        println!("📦 Assembling APK via Gradle...");
        let mut gradle = Command::new(&gradlew);
        gradle
            .current_dir(&android_dir)
            .arg(if release {
                "assembleRelease"
            } else {
                "assembleDebug"
            });
        let g = gradle.status().context("gradlew failed")?;
        if g.success() {
            let apk = android_dir.join(format!(
                "app/build/outputs/apk/{}/app-{}.apk",
                if release { "release" } else { "debug" },
                if release { "release" } else { "debug" }
            ));
            println!("✅ APK: {}", apk.display());
        }
    } else {
        println!("ℹ️  No gradlew — open android/ in Android Studio and Run.");
        println!("   Or generate wrapper: cd android && gradle wrapper");
    }

    Ok(())
}

fn xcode_available() -> bool {
    Command::new("xcodebuild")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn build_ios(project_dir: &Path, build_dir: &Path, release: bool) -> Result<()> {
    let ios_dir = project_dir.join("ios");
    if !ios_dir.exists() {
        bail!("ios/ not found — run: w3cos mobile init . --platform ios");
    }

    if !xcode_available() {
        bail!(
            "Xcode required for iOS builds. Install Xcode from App Store, then:\n  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
        );
    }

    let target = "aarch64-apple-ios-sim";
    println!("🔧 Adding Rust target {target} (if needed)...");
    let _ = Command::new("rustup")
        .args(["target", "add", target])
        .status();

    let profile = if release { "release" } else { "debug" };
    println!("🔨 Building iOS simulator lib ({profile})...");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(build_dir)
        .args(["build", "--target", target]);
    if release {
        cmd.arg("--release");
    }
    let status = cmd.status().context("cargo build for iOS failed")?;
    if !status.success() {
        bail!("iOS native build failed for target {target}");
    }

    let lib_dir = build_dir.join("target").join(target).join(profile);
    let lib_path = lib_dir.join("libw3cos_mobile_app.a");
    if !lib_path.exists() {
        bail!("missing {}", lib_path.display());
    }

    let out_libs = ios_dir.join("libs");
    fs::create_dir_all(&out_libs)?;
    let dest = out_libs.join("libw3cos_mobile_app.a");
    fs::copy(&lib_path, &dest)?;
    println!("✅ Static lib: {}", dest.display());

    let xcodeproj = ios_dir.join("W3cosApp.xcodeproj");
    if xcodeproj.exists() {
        println!("📱 Building iOS app (simulator)...");
        let mut xcode = Command::new("xcodebuild");
        xcode
            .current_dir(&ios_dir)
            .args([
                "-project",
                "W3cosApp.xcodeproj",
                "-scheme",
                "W3cosApp",
                "-sdk",
                "iphonesimulator",
                "-destination",
                "platform=iOS Simulator,name=iPhone 16",
                "build",
            ]);
        let x = xcode.status().context("xcodebuild failed")?;
        if x.success() {
            println!("✅ iOS simulator build OK. Open W3cosApp.xcodeproj in Xcode and Run.");
        }
    } else {
        println!("ℹ️  Open ios/W3cosApp.xcodeproj in Xcode after template sync.");
    }

    Ok(())
}
