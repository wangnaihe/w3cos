pub mod parser;
pub mod codegen;

use anyhow::Result;

/// Compile a TypeScript source file (W3C Modern Subset) into a standalone
/// Rust project that links against w3cos-runtime and produces a native binary.
pub fn compile(ts_source: &str, output_dir: &std::path::Path) -> Result<()> {
    let tree = parser::parse(ts_source)?;
    let rust_code = codegen::generate(&tree)?;

    std::fs::create_dir_all(output_dir.join("src"))?;

    let cargo_toml = codegen::generate_cargo_toml(output_dir)?;
    std::fs::write(output_dir.join("Cargo.toml"), cargo_toml)?;
    std::fs::write(output_dir.join("src/main.rs"), rust_code)?;

    Ok(())
}
