use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;

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
