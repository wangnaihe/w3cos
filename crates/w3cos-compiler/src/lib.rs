pub mod codegen;
pub mod parser;

use anyhow::Result;

/// Compile TypeScript/JSON source to Rust code string (no file I/O).
/// Useful for testing and programmatic use.
pub fn compile_to_rust(ts_source: &str) -> Result<String> {
    let tree = parser::parse(ts_source)?;
    codegen::generate(&tree)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_to_rust_simple_text() {
        let rust = compile_to_rust(r##"Text("hello", { style: { color: "#fff" } })"##).unwrap();
        assert!(rust.contains("Component::text(\"hello\""));
        assert!(rust.contains("fn main()"));
        assert!(rust.contains("build_ui()"));
        assert!(rust.contains("w3cos_runtime::run_app"));
    }

    #[test]
    fn compile_to_rust_column_with_children() {
        let rust = compile_to_rust(r#"Column({ children: [Text("a"), Text("b")] })"#).unwrap();
        assert!(rust.contains("Component::column"));
        assert!(rust.contains("Component::text(\"a\""));
        assert!(rust.contains("Component::text(\"b\""));
    }

    #[test]
    fn compile_to_rust_full_pipeline() {
        let input = r##"Column({
            style: { gap: 8, padding: 16 },
            children: [
                Text("Title", { style: { font_size: 24 } }),
                Button("Submit", { style: { background: "#e94560" } })
            ]
        })"##;
        let rust = compile_to_rust(input).unwrap();
        assert!(rust.contains("Component::column"));
        assert!(rust.contains("Component::text(\"Title\""));
        assert!(rust.contains("Component::button(\"Submit\""));
        assert!(rust.contains("gap: 8_f32"));
        assert!(rust.contains("padding: Edges::all(16_f32)"));
        assert!(rust.contains("Color::from_hex(\"#e94560\")"));
    }
}
