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
    let (_, _, entry, safe_area, interactive_widget) = read_app_manifest(project_dir);
    let app_tsx = project_dir.join(&entry);
    if !app_tsx.exists() {
        bail!("missing entry {} in {}", entry, project_dir.display());
    }

    let build_dir = project_dir.join(".w3cos/mobile-build");
    if build_dir.exists() {
        fs::remove_dir_all(&build_dir)?;
    }
    fs::create_dir_all(&build_dir)?;

    println!("⚡ Transpiling {} → mobile cdylib...", app_tsx.display());
    w3cos_compiler::compile_mobile_from_file(
        &app_tsx,
        &build_dir,
        platform,
        safe_area,
        &interactive_widget,
    )?;

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
    let jni_libs = android_dir.join("app/src/main/jniLibs");
    fs::create_dir_all(&jni_libs)?;
    let jni_out = jni_libs
        .canonicalize()
        .unwrap_or_else(|_| jni_libs.clone());

    println!("🔨 Building Android arm64-v8a ({profile})...");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(build_dir).arg("ndk");
    cmd.args([
        "-t",
        "arm64-v8a",
        "-o",
        jni_out.to_str().context("jniLibs path is not valid UTF-8")?,
        "build",
    ]);
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
    let so_path = jni_libs.join("arm64-v8a").join(&so_name);
    if !so_path.exists() {
        bail!("expected {} after cargo ndk build", so_path.display());
    }
    println!("✅ Native lib: {}", so_path.display());

    let gradlew = android_dir.join("gradlew");
    if gradlew.exists() {
        println!("📦 Assembling APK via Gradle...");
        let mut gradle = Command::new("bash");
        gradle
            .current_dir(&android_dir)
            .arg("./gradlew")
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

fn read_app_manifest(project_dir: &Path) -> (String, String, String, bool, String) {
    let manifest_path = project_dir.join("w3cos.app.json");
    let mut display_name = "W3cosApp".to_string();
    let mut bundle_id = "com.example.w3cos.app".to_string();
    let mut entry = "app.tsx".to_string();
    let mut safe_area = true;
    let mut interactive_widget = "resizes-content".to_string();
    if let Ok(raw) = fs::read_to_string(&manifest_path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(name) = json.get("name").and_then(|v| v.as_str()) {
                display_name = name.to_string();
            }
            if let Some(id) = json.get("bundle_id").and_then(|v| v.as_str()) {
                bundle_id = id.to_string();
            }
            if let Some(e) = json.get("entry").and_then(|v| v.as_str()) {
                entry = e.to_string();
            }
            if let Some(sa) = json.get("safe_area").and_then(|v| v.as_bool()) {
                safe_area = sa;
            }
            if let Some(iw) = json.get("interactive_widget").and_then(|v| v.as_str()) {
                interactive_widget = iw.to_string();
            }
        }
    }
    (display_name, bundle_id, entry, safe_area, interactive_widget)
}

fn write_ios_plist(path: &Path, display_name: &str, bundle_id: &str) -> Result<()> {
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>{display_name}</string>
    <key>CFBundleExecutable</key>
    <string>W3cosApp</string>
    <key>CFBundleIdentifier</key>
    <string>{bundle_id}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>{display_name}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSRequiresIPhoneOS</key>
    <true/>
    <key>UILaunchScreen</key>
    <dict/>
    <key>UIRequiredDeviceCapabilities</key>
    <array>
        <string>arm64</string>
    </array>
    <key>UISupportedInterfaceOrientations</key>
    <array>
        <string>UIInterfaceOrientationPortrait</string>
    </array>
</dict>
</plist>
"#
    );
    fs::write(path, content)?;
    Ok(())
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
    println!("🔨 Building iOS simulator binary ({profile})...");
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

    let bin = build_dir
        .join("target")
        .join(target)
        .join(profile)
        .join("W3cosApp");
    if !bin.exists() {
        bail!("missing iOS binary: {}", bin.display());
    }

    let (display_name, bundle_id, _, _, _) = read_app_manifest(project_dir);

    let plist_src = ios_dir.join("W3cosApp/Info.plist");
    write_ios_plist(&plist_src, &display_name, &bundle_id)?;

    let app_bundle = ios_dir.join("W3cosApp.app");
    if app_bundle.exists() {
        fs::remove_dir_all(&app_bundle)?;
    }
    fs::create_dir_all(&app_bundle)?;
    fs::copy(&bin, app_bundle.join("W3cosApp"))?;
    write_ios_plist(&app_bundle.join("Info.plist"), &display_name, &bundle_id)?;
    println!("✅ iOS app bundle: {} ({})", app_bundle.display(), display_name);

    if std::env::var("W3COS_SKIP_IOS_INSTALL").ok().as_deref() == Some("1") {
        println!("ℹ️  Skipping simulator install (W3COS_SKIP_IOS_INSTALL=1)");
        return Ok(());
    }

    let udid = std::env::var("W3COS_IOS_SIM").unwrap_or_else(|_| "iPhone 17".to_string());
    println!("📱 Installing on simulator ({udid})...");
    let list_out = Command::new("xcrun")
        .args(["simctl", "list", "devices", "available"])
        .output()
        .ok();
    let mut device_id = None;
    if let Some(out) = list_out {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if line.contains(&udid) && line.contains("Shutdown") || line.contains(&udid) && line.contains("Booted") {
                if let Some(start) = line.find('(') {
                    if let Some(end) = line.find(')') {
                        device_id = Some(line[start + 1..end].to_string());
                        break;
                    }
                }
            }
        }
    }
    if let Some(id) = device_id {
        let _ = Command::new("xcrun")
            .args(["simctl", "boot", &id])
            .status();
        let _ = Command::new("open")
            .arg("-a")
            .arg("Simulator")
            .status();
        let _ = Command::new("xcrun")
            .args(["simctl", "uninstall", &id, "com.example.w3cos.app"])
            .status();
        let _ = Command::new("xcrun")
            .args(["simctl", "uninstall", &id, &bundle_id])
            .status();
        let install = Command::new("xcrun")
            .args(["simctl", "install", &id, app_bundle.to_str().unwrap()])
            .status();
        if install.map(|s| s.success()).unwrap_or(false) {
            let launch = Command::new("xcrun")
                .args(["simctl", "launch", &id, &bundle_id])
                .output();
            if let Ok(out) = launch {
                println!("{}", String::from_utf8_lossy(&out.stdout));
                println!("✅ Launched on simulator. Check Simulator window.");
            }
        }
    } else {
        println!("ℹ️  Run manually: xcrun simctl install booted {}", app_bundle.display());
    }

    Ok(())
}
