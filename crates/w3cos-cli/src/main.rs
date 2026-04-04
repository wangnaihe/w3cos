use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;
use std::fs;

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
    },
    /// Compile and immediately run the application.
    Run {
        /// Path to the TypeScript or JSON source file.
        input: PathBuf,
    },
    /// Start a dev server with hot reload (recompile + restart on file changes).
    Dev {
        /// Path to the TypeScript or JSON source file.
        input: PathBuf,
    },
    /// Initialize a new W3C OS project with template files.
    Init {
        /// Project name (creates a directory with this name).
        project_name: PathBuf,
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
        } => {
            // Enable strip by default in release mode unless explicitly disabled
            let strip = if release || strip { Some(true) } else { None };
            build(&input, &output, release, strip, lto)?;
        }
        Commands::Run { input } => {
            let tmp = std::env::temp_dir().join("w3cos-run");
            let bin = tmp.join("target").join("debug").join("w3cos-app");
            build(&input, &bin, false, None, false)?;
            println!("▶  Running...");
            let status = Command::new(&bin)
                .status()
                .context("Failed to run compiled binary")?;
            std::process::exit(status.code().unwrap_or(1));
        }
        Commands::Dev { input } => {
            dev_watch(&input)?;
        }
        Commands::Init { project_name } => {
            init(&project_name)?;
        }
    }

    Ok(())
}

fn build(
    input: &PathBuf,
    output: &PathBuf,
    release: bool,
    strip: Option<bool>,
    lto: bool,
) -> Result<()> {
    let source = std::fs::read_to_string(input)
        .with_context(|| format!("Could not read {}", input.display()))?;

    let build_dir = std::env::temp_dir().join("w3cos-build");
    if build_dir.exists() {
        std::fs::remove_dir_all(&build_dir)?;
    }

    println!("⚡ Transpiling {} → Rust...", input.display());
    w3cos_compiler::compile(&source, &build_dir)?;

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

    // Check if directory already exists
    if project_name.exists() {
        anyhow::bail!("Error: Directory '{}' already exists", project_name.display());
    }

    // Create project directory
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

    // Create README.md
    let readme_path = project_name.join("README.md");
    let readme_content = format!(r#"# {}

A W3C OS Application.

## Getting Started

### Prerequisites

- Rust (latest stable)
- w3cos CLI

### Usage

```bash
# Run the application
w3cos run app.tsx

# Or build a native binary
w3cos build app.tsx -o myapp --release

# Run the binary
./myapp
```

## Project Structure

```
{}
├── app.tsx         # Main application entry point (TSX)
└── README.md       # This file
```

## License

Apache-2.0
"#, project_name.display(), project_name.display());
    fs::write(&readme_path, readme_content)
        .with_context(|| format!("Could not create {}", readme_path.display()))?;
    println!("✅ Created: {}", readme_path.display());

    println!("\n✨ Project initialized successfully!");
    println!("📁 Next steps:");
    println!("   cd {}", project_name.display());
    println!("   w3cos run app.tsx");

    Ok(())
}

fn dev_watch(input: &PathBuf) -> Result<()> {
    use std::time::{Duration, SystemTime};

    let input = std::fs::canonicalize(input)
        .with_context(|| format!("Could not find {}", input.display()))?;

    println!("🔄 W3C OS Dev Mode — watching {}", input.display());
    println!("   Press Ctrl+C to stop\n");

    let mut last_modified = file_mtime(&input);
    let tmp = std::env::temp_dir().join("w3cos-dev");
    let bin = tmp.join("target").join("debug").join("w3cos-app");

    loop {
        // Build
        println!("⚡ Building...");
        if let Err(e) = build(&input, &bin, false, None, false) {
            eprintln!("❌ Build failed: {e}");
            wait_for_change(&input, &mut last_modified);
            continue;
        }
        println!("✅ Built successfully");

        // Run
        println!("▶  Running...\n");
        let mut child = Command::new(&bin)
            .spawn()
            .context("Failed to run compiled binary")?;

        // Watch for file changes while the app is running
        loop {
            std::thread::sleep(Duration::from_millis(500));

            let current_mtime = file_mtime(&input);
            if current_mtime != last_modified {
                last_modified = current_mtime;
                println!("\n🔄 File changed — rebuilding...");
                let _ = child.kill();
                let _ = child.wait();
                break;
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    println!("\n⏹  App exited (code: {})", status.code().unwrap_or(-1));
                    wait_for_change(&input, &mut last_modified);
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
    }
}

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
}

fn wait_for_change(path: &std::path::Path, last_modified: &mut Option<SystemTime>) {
    println!("👀 Waiting for file changes...");
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let current = file_mtime(path);
        if current != *last_modified {
            *last_modified = current;
            return;
        }
    }
}

use std::time::SystemTime;
