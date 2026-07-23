mod dev;
mod mobile;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use w3cos_compiler::CompileOptions;

#[derive(Parser)]
#[command(
    name = "w3cos",
    about = "W3C OS — compile TypeScript to native binaries",
    version,
    long_about = "Compile W3C Modern Subset TypeScript/CSS into native Linux/macOS binaries.\n\
                   No browser. No interpreter. No V8. Pure native."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile a .ts or .json app into a native binary.
    Build {
        /// Path to the TypeScript or JSON source file.
        input: PathBuf,
        /// Output binary path (default: ./app).
        #[arg(short, long, default_value = "./app")]
        output: PathBuf,
        /// Build in release mode (optimized, smaller binary).
        /// Enables --strip by default for smaller output.
        #[arg(long)]
        release: bool,
        /// Strip debug symbols from the binary (enabled by default in release mode).
        #[arg(long)]
        strip: bool,
        /// Enable Link-Time Optimization for smaller, faster binaries.
        #[arg(long)]
        lto: bool,
        /// Output target: native binary (default) or web (HTML/CSS/JS).
        #[arg(long, default_value = "native")]
        target: String,
        /// Enable Chrome DevTools Protocol (native desktop or mobile debug builds).
        #[arg(long)]
        devtools: bool,
    },
    /// Compile and immediately run the application.
    Run {
        /// Path to the TypeScript or JSON source file.
        input: PathBuf,
        /// Enable Chrome DevTools Protocol (native desktop or mobile debug builds).
        #[arg(long)]
        devtools: bool,
        /// DevTools listen port (default: 9229).
        #[arg(long, default_value_t = 9229)]
        devtools_port: u16,
    },
    /// Start a dev server with hot reload (recompile + restart on file changes).
    Dev {
        /// Path to the TypeScript or JSON source file.
        input: PathBuf,
        /// Output target: native (default) or web (HTML/CSS/JS + static server).
        #[arg(long, default_value = "native")]
        target: String,
        /// Web output directory (web target only).
        #[arg(short, long, default_value = "./dist")]
        output: PathBuf,
        /// Static server port (web target only).
        #[arg(long, default_value_t = 5173)]
        port: u16,
        /// Enable Chrome DevTools Protocol (native target).
        #[arg(long, default_value_t = true)]
        devtools: bool,
        /// DevTools listen port.
        #[arg(long, default_value_t = 9229)]
        devtools_port: u16,
    },
    /// Initialize a new W3C OS project with template files.
    Init {
        /// Project name (creates a directory with this name).
        project_name: PathBuf,
    },
    /// Mobile app scaffolding (Android / iOS / HarmonyOS shell + w3cos.app.json).
    Mobile {
        #[command(subcommand)]
        command: MobileCommands,
    },
}

#[derive(Subcommand)]
enum MobileCommands {
    /// Create a new mobile project from generic templates.
    Init {
        project_name: PathBuf,
        /// Target platform: android, ios, harmony, both, or all.
        #[arg(long, default_value = "android")]
        platform: String,
    },
    /// Build mobile artifact (APK / iOS simulator).
    Build {
        /// Project directory (contains app.tsx, android/, ios/).
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Target platform: android, ios, harmony, or both.
        #[arg(long, default_value = "android")]
        platform: String,
        #[arg(long)]
        release: bool,
        /// Enable Chrome DevTools (default: on for debug, off for --release).
        #[arg(long)]
        devtools: bool,
        /// Disable Chrome DevTools even in debug builds.
        #[arg(long)]
        no_devtools: bool,
    },
    /// Watch entry + CSS, rebuild and reinstall on change.
    Dev {
        /// Project directory (contains app.tsx, android/, ios/).
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Target platform: android or ios.
        #[arg(long, default_value = "android")]
        platform: String,
        /// Release build (slower; default is debug).
        #[arg(long)]
        release: bool,
        /// Force-enable Chrome DevTools CDP server in the app.
        #[arg(long)]
        devtools: bool,
        /// Disable Chrome DevTools.
        #[arg(long)]
        no_devtools: bool,
        /// DevTools port on device/simulator (default 9229).
        #[arg(long, default_value_t = 9229)]
        devtools_port: u16,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            input,
            output,
            release,
            strip,
            lto,
            target,
            devtools,
        } => {
            let strip = if release || strip { Some(true) } else { None };
            build(
                &input,
                &output,
                release,
                strip,
                lto,
                &target,
                CompileOptions { devtools },
            )?;
        }
        Commands::Run {
            input,
            devtools,
            devtools_port,
        } => {
            let tmp = std::env::temp_dir().join("w3cos-run");
            let bin = tmp.join("target").join("debug").join("w3cos-app");
            build(
                &input,
                &bin,
                false,
                None,
                false,
                "native",
                CompileOptions { devtools },
            )?;
            println!("▶  Running...");
            let config = dev::DevConfig {
                devtools,
                devtools_port,
                web_port: 5173,
            };
            let status = {
                let mut child = dev::spawn_native_app(&bin, &config)?;
                child.wait().context("Failed to wait on app")?
            };
            std::process::exit(status.code().unwrap_or(1));
        }
        Commands::Dev {
            input,
            target,
            output,
            port,
            devtools,
            devtools_port,
        } => {
            let config = dev::DevConfig {
                devtools,
                devtools_port,
                web_port: port,
            };
            if target == "web" {
                dev_watch_web(&input, &output, &config)?;
            } else if target == "native" {
                dev_watch_native(&input, &config)?;
            } else {
                anyhow::bail!("unknown --target {target} (use native|web)");
            }
        }
        Commands::Init { project_name } => {
            init(&project_name)?;
        }
        Commands::Mobile { command } => match command {
            MobileCommands::Init {
                project_name,
                platform,
            } => mobile::mobile_init(&project_name, &platform)?,
            MobileCommands::Build {
                project,
                platform,
                release,
                devtools,
                no_devtools,
            } => {
                let devtools = mobile::resolve_mobile_devtools(release, devtools, no_devtools);
                mobile::mobile_build(&project, &platform, release, devtools)?;
            }
            MobileCommands::Dev {
                project,
                platform,
                release,
                devtools,
                no_devtools,
                devtools_port,
            } => mobile::mobile_dev(
                &project,
                &platform,
                release,
                devtools,
                no_devtools,
                devtools_port,
            )?,
        },
    }

    Ok(())
}

fn build(
    input: &PathBuf,
    output: &PathBuf,
    release: bool,
    strip: Option<bool>,
    lto: bool,
    target: &str,
    options: CompileOptions,
) -> Result<()> {
    let input_abs = std::fs::canonicalize(input)
        .with_context(|| format!("Could not find {}", input.display()))?;

    if target == "web" {
        if options.devtools {
            println!("ℹ️  --devtools ignored for web target.");
        }
        println!("⚡ Transpiling {} → HTML/CSS/JS...", input.display());
        if output.exists() {
            if output.is_dir() {
                std::fs::remove_dir_all(output)?;
            } else {
                anyhow::bail!(
                    "web output must be a directory, got file: {}",
                    output.display()
                );
            }
        }
        std::fs::create_dir_all(output)?;
        w3cos_compiler::compile_web_from_file(&input_abs, output)?;
        println!(
            "✅ Web output: {}/ (index.html, styles.css, app.js)",
            output.display()
        );
        return Ok(());
    }

    if target != "native" {
        anyhow::bail!("unknown --target {target} (use native|web)");
    }

    let build_dir = std::env::temp_dir().join("w3cos-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir)?;
    }

    println!("⚡ Transpiling {} → Rust...", input.display());
    w3cos_compiler::compile_from_file_with_options(&input_abs, &build_dir, &options)?;

    println!("🔨 Compiling native binary...");
    let mut cmd = Command::new("cargo");
    cmd.arg("build").current_dir(&build_dir);
    if release {
        cmd.arg("--release");
    }
    if strip.unwrap_or(false) {
        cmd.env("CARGO_PROFILE_RELEASE_STRIP", "true");
        cmd.env("CARGO_PROFILE_DEBUG_STRIP", "true");
        println!("  📦 Strip: enabled");
    }
    if lto {
        cmd.env("CARGO_PROFILE_RELEASE_LTO", "true");
        cmd.env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1");
        println!("  📦 LTO: enabled (codegen-units=1)");
    }

    let status = cmd.status().context("cargo build failed")?;
    if !status.success() {
        anyhow::bail!("Compilation failed");
    }

    let profile = if release { "release" } else { "debug" };
    let built_bin = build_dir.join("target").join(profile).join("w3cos-app");

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&built_bin, output)
        .with_context(|| format!("Could not copy binary to {}", output.display()))?;

    let size = std::fs::metadata(output)?.len();
    let size_str = if size > 1_000_000 {
        format!("{:.1} MB", size as f64 / 1_000_000.0)
    } else {
        format!("{:.0} KB", size as f64 / 1_000.0)
    };

    println!("✅ Output: {} ({})", output.display(), size_str);
    Ok(())
}

fn init(project_name: &PathBuf) -> Result<()> {
    println!("🚀 Initializing W3C OS project: {}", project_name.display());

    if project_name.exists() {
        anyhow::bail!(
            "Error: Directory '{}' already exists",
            project_name.display()
        );
    }

    fs::create_dir_all(project_name)
        .with_context(|| format!("Could not create directory {}", project_name.display()))?;

    let app_tsx_path = project_name.join("app.tsx");
    let app_tsx_content = r##"import { Column, Text, Button } from "@w3cos/std"

export default
<Column style={{ gap: 20, padding: 48, alignItems: "center", background: "#1e1e2e" }}>
  <Text style={{ fontSize: 32, color: "#f0f0ff", fontWeight: 700 }}>My App</Text>
  <Text style={{ fontSize: 16, color: "#a0a0b0" }}>Built with W3C OS</Text>
  <Button style={{ background: "#6c5ce7", color: "#ffffff", borderRadius: 8 }}>Hello World</Button>
</Column>
"##;
    fs::write(&app_tsx_path, app_tsx_content)
        .with_context(|| format!("Could not create {}", app_tsx_path.display()))?;
    println!("✅ Created: {}", app_tsx_path.display());

    let readme_path = project_name.join("README.md");
    let readme_content = format!(
        r#"# {}

A W3C OS Application.

## Getting Started

### Prerequisites

- Rust (latest stable)
- w3cos CLI

### Usage

```bash
# Dev mode with Chrome DevTools
w3cos dev app.tsx

# Run once
w3cos run app.tsx --devtools

# Build a native binary
w3cos build app.tsx -o myapp --release

# Web preview
w3cos dev app.tsx --target web -o dist
```

## Project Structure

```
{}
├── app.tsx         # Main application entry point (TSX)
└── README.md       # This file
```

## License

Apache-2.0
"#,
        project_name.display(),
        project_name.display()
    );
    fs::write(&readme_path, readme_content)
        .with_context(|| format!("Could not create {}", readme_path.display()))?;
    println!("✅ Created: {}", readme_path.display());

    println!("\n✨ Project initialized successfully!");
    println!("📁 Next steps:");
    println!("   cd {}", project_name.display());
    println!("   w3cos dev app.tsx");

    Ok(())
}

fn dev_watch_native(input: &PathBuf, config: &dev::DevConfig) -> Result<()> {
    use std::process::Child;

    let input = std::fs::canonicalize(input)
        .with_context(|| format!("Could not find {}", input.display()))?;
    let watch_paths = dev::watch_paths_for(&input)?;
    let watch_list: Vec<_> = watch_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    println!("🔄 W3C OS Dev Mode (native) — watching:");
    for p in &watch_list {
        println!("   • {p}");
    }
    println!("   Press Ctrl+C to stop\n");

    let mut last_mtimes = dev::snapshot_mtimes(&watch_paths);
    let tmp = std::env::temp_dir().join("w3cos-dev");
    let bin = tmp.join("target").join("debug").join("w3cos-app");
    let options = CompileOptions {
        devtools: config.devtools,
    };

    loop {
        println!("⚡ Building...");
        if let Err(e) = build(&input, &bin, false, None, false, "native", options) {
            eprintln!("❌ Build failed: {e}");
            dev::wait_for_change(&watch_paths, &mut last_mtimes);
            continue;
        }
        println!("✅ Built successfully");

        println!("▶  Running...\n");
        let mut child: Child = dev::spawn_native_app(&bin, config)?;

        loop {
            std::thread::sleep(std::time::Duration::from_millis(400));

            if dev::any_mtime_changed(&watch_paths, &last_mtimes) {
                dev::refresh_mtimes(&watch_paths, &mut last_mtimes);
                println!("\n🔄 File changed — rebuilding...");
                let _ = child.kill();
                let _ = child.wait();
                break;
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    println!("\n⏹  App exited (code: {})", status.code().unwrap_or(-1));
                    dev::wait_for_change(&watch_paths, &mut last_mtimes);
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
    }
}

fn dev_watch_web(input: &PathBuf, output: &PathBuf, config: &dev::DevConfig) -> Result<()> {
    let input = std::fs::canonicalize(input)
        .with_context(|| format!("Could not find {}", input.display()))?;
    let watch_paths = dev::watch_paths_for(&input)?;
    let watch_list: Vec<_> = watch_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    println!("🔄 W3C OS Dev Mode (web) — watching:");
    for p in &watch_list {
        println!("   • {p}");
    }
    println!("   Press Ctrl+C to stop\n");

    let mut last_mtimes = dev::snapshot_mtimes(&watch_paths);
    let _server_stop = dev::start_web_server(output, config.web_port)?;

    loop {
        println!("⚡ Building web...");
        if let Err(e) = build(
            &input,
            output,
            false,
            None,
            false,
            "web",
            CompileOptions::default(),
        ) {
            eprintln!("❌ Build failed: {e}");
            dev::wait_for_change(&watch_paths, &mut last_mtimes);
            continue;
        }
        println!(
            "✅ Built — refresh http://127.0.0.1:{}/index.html",
            config.web_port
        );

        dev::wait_for_change(&watch_paths, &mut last_mtimes);
        println!("\n🔄 File changed — rebuilding...");
    }
}
