pub mod codegen;
pub mod parser;
pub mod scope_analysis;
pub mod ts_transpiler;
pub mod ts_types;

use anyhow::Result;

/// Compile TypeScript/JSON source to Rust code string (no file I/O).
///
/// Auto-detects whether the source is:
/// - **UI DSL** (Column/Row/Text/Button components) → existing UI pipeline
/// - **General TypeScript** (functions, variables, logic) → SWC-based transpiler
pub fn compile_to_rust(ts_source: &str) -> Result<String> {
    if is_ui_dsl(ts_source) {
        let tree = parser::parse(ts_source)?;
        codegen::generate(&tree)
    } else {
        ts_transpiler::transpile(ts_source)
    }
}

/// Compile flags that influence Cargo.toml / import generation.
pub struct CompileFlags {
    pub needs_hashmap: bool,
    pub needs_async: bool,
    pub needs_rc: bool,
    pub needs_core: bool,
    pub needs_fetch: bool,
    pub needs_history: bool,
}

/// Compile a TypeScript source file into a standalone Rust project.
///
/// For UI apps: links against w3cos-runtime and produces a native GUI binary.
/// For general TS: produces a standalone CLI binary.
pub fn compile(ts_source: &str, output_dir: &std::path::Path) -> Result<()> {
    let is_ui = is_ui_dsl(ts_source);

    std::fs::create_dir_all(output_dir.join("src"))?;

    if is_ui {
        let rust_code = compile_to_rust(ts_source)?;
        let cargo_toml = codegen::generate_cargo_toml(output_dir)?;
        std::fs::write(output_dir.join("Cargo.toml"), cargo_toml)?;
        std::fs::write(output_dir.join("src/main.rs"), rust_code)?;
    } else {
        let output = ts_transpiler::transpile_with_flags(ts_source)?;
        let flags = CompileFlags {
            needs_hashmap: output.needs_hashmap,
            needs_async: output.needs_async,
            needs_rc: output.needs_rc,
            needs_core: output.needs_core,
            needs_fetch: output.needs_fetch,
            needs_history: output.needs_history,
        };
        let cargo_toml = generate_standalone_cargo_toml(&flags);
        std::fs::write(output_dir.join("Cargo.toml"), cargo_toml)?;
        std::fs::write(output_dir.join("src/main.rs"), output.code)?;
    }

    Ok(())
}

/// Detect whether TS source is a UI DSL (component tree) or general TypeScript.
///
/// Heuristics:
/// - JSON input → UI DSL
/// - Imports from `@w3cos/std` → UI DSL
/// - Top-level expression is Column/Row/Text/Button call → UI DSL
/// - TSX with `<Column>`, `<Row>`, etc. → UI DSL
/// - Everything else → general TypeScript
fn is_ui_dsl(source: &str) -> bool {
    let trimmed = source.trim();

    // JSON format
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return true;
    }

    // Quick scan for W3C OS UI patterns
    let has_ui_import = trimmed.contains("@w3cos/std");
    let has_component_call = ["Column(", "Row(", "Text(", "Button("]
        .iter()
        .any(|pat| trimmed.contains(pat));
    let has_tsx_component = ["<Column", "<Row", "<Text", "<Button"]
        .iter()
        .any(|pat| trimmed.contains(pat));

    // If it imports @w3cos/std or directly uses component constructors at top level
    if has_ui_import && (has_component_call || has_tsx_component) {
        return true;
    }

    // Check if the main expression (after stripping imports/export default) is a component
    let lines: Vec<&str> = trimmed.lines().collect();
    let body: String = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            !t.starts_with("import ") && !t.starts_with("export default")
        })
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let body = body.trim();

    let component_starts = [
        "Column(", "Row(", "Text(", "Button(", "Image(", "TextInput(", "Box(",
        "<Column", "<Row", "<Text", "<Button", "<Image", "<TextInput", "<Box",
    ];
    let starts_with_component = component_starts.iter().any(|pat| body.starts_with(pat));

    // Standalone component expression (no functions, no variable logic)
    if starts_with_component
        && !body.contains("function ")
        && !body.contains("const ")
        && !body.contains("let ")
    {
        return true;
    }

    false
}

/// Generate a minimal Cargo.toml for standalone (non-UI) Rust programs.
fn generate_standalone_cargo_toml(flags: &CompileFlags) -> String {
    let mut toml = String::from(
        r#"[package]
name = "w3cos-app"
version = "0.1.0"
edition = "2024"

[dependencies]
"#,
    );

    if flags.needs_core {
        toml.push_str("w3cos-core = { path = \"../../crates/w3cos-core\" }\n");
    }
    if flags.needs_async {
        toml.push_str("tokio = { version = \"1\", features = [\"full\"] }\n");
    }
    if flags.needs_fetch || flags.needs_history {
        toml.push_str("w3cos-runtime = { path = \"../../crates/w3cos-runtime\" }\n");
    }

    toml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_to_rust_simple_text() {
        let rust = compile_to_rust(r##"Text("hello", { style: { color: "#fff" } })"##).unwrap();
        assert!(rust.contains("Component::text(\"hello\""));
        assert!(rust.contains("fn build_ui()"));
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

    #[test]
    fn compile_general_ts() {
        let input = r#"
function greet(name: string): string {
    return "Hello, " + name;
}
console.log(greet("W3C OS"));
"#;
        let rust = compile_to_rust(input).unwrap();
        assert!(rust.contains("fn greet("), "got: {rust}");
        assert!(rust.contains("fn main()"), "got: {rust}");
        assert!(rust.contains("println!"), "got: {rust}");
    }

    #[test]
    fn compile_general_ts_showcase() {
        let input = r#"
interface User {
    name: string;
    age: number;
    email?: string;
}

function greet(name: string): string {
    return "Hello, " + name + "!";
}

function fibonacci(n: number): number {
    if (n <= 1) { return n; }
    let a: number = 0;
    let b: number = 1;
    for (let i = 2; i < n; i++) {
        let temp = b;
        b = a + b;
        a = temp;
    }
    return b;
}

let message = greet("W3C OS");
console.log(message);

let numbers: number[] = [1, 2, 3, 4, 5];

let items: number[] = [];
items.push(10);
items.push(20);
console.log("Items:", items);
console.log("Length:", items.length);

for (let i = 0; i < 10; i++) {
    console.log("fib:", fibonacci(i));
}

let score: number = 85;
if (score >= 90) {
    console.log("Grade: A");
} else if (score >= 80) {
    console.log("Grade: B");
} else {
    console.log("Grade: F");
}

let countdown: number = 5;
while (countdown > 0) {
    console.log("Countdown:", countdown);
    countdown -= 1;
}
console.log("Done!");
"#;
        let rust = compile_to_rust(input).unwrap();
        eprintln!("=== Generated Rust ===\n{rust}\n=== End ===");

        // Verify key constructs
        assert!(rust.contains("struct User"), "missing struct: {rust}");
        assert!(rust.contains("fn greet("), "missing greet: {rust}");
        assert!(rust.contains("fn fibonacci("), "missing fibonacci: {rust}");
        assert!(rust.contains("fn main()"), "missing main: {rust}");
        assert!(rust.contains("for i in"), "missing range for: {rust}");
        assert!(rust.contains("while countdown"), "missing while: {rust}");
        assert!(rust.contains("else if"), "missing else if: {rust}");
        assert!(rust.contains(".push("), "missing push: {rust}");
        assert!(rust.contains(".len()"), "missing len: {rust}");
        assert!(rust.contains("println!"), "missing println: {rust}");
    }

    #[test]
    fn standalone_cargo_toml_no_deps() {
        let flags = CompileFlags {
            needs_hashmap: false,
            needs_async: false,
            needs_rc: false,
            needs_core: false,
            needs_fetch: false,
            needs_history: false,
        };
        let toml = generate_standalone_cargo_toml(&flags);
        assert!(!toml.contains("tokio"), "should not include tokio: {toml}");
    }

    #[test]
    fn standalone_cargo_toml_with_tokio() {
        let flags = CompileFlags {
            needs_hashmap: false,
            needs_async: true,
            needs_rc: false,
            needs_core: false,
            needs_fetch: false,
            needs_history: false,
        };
        let toml = generate_standalone_cargo_toml(&flags);
        assert!(toml.contains("tokio"), "should include tokio: {toml}");
        assert!(toml.contains("features"), "should have features: {toml}");
    }

    #[test]
    fn compile_async_to_dir() {
        let input = r#"
            async function fetchData(): Promise<string> {
                return await fetch("http://example.com");
            }
            let data = await fetchData();
            console.log(data);
        "#;

        let dir = std::env::temp_dir().join("w3cos_test_async_compile");
        let _ = std::fs::remove_dir_all(&dir);
        compile(input, &dir).expect("compile failed");

        let main_rs = std::fs::read_to_string(dir.join("src/main.rs")).unwrap();
        assert!(main_rs.contains("async fn"), "missing async: {main_rs}");
        assert!(main_rs.contains("#[tokio::main]"), "missing tokio::main: {main_rs}");

        let cargo_toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(cargo_toml.contains("tokio"), "missing tokio dep: {cargo_toml}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compile_closure_to_dir() {
        let input = r#"
            function makeCounter(): () => number {
                let count = 0;
                return () => { count += 1; return count; };
            }
            let c = makeCounter();
            console.log(c());
        "#;

        let dir = std::env::temp_dir().join("w3cos_test_closure_compile");
        let _ = std::fs::remove_dir_all(&dir);
        compile(input, &dir).expect("compile failed");

        let main_rs = std::fs::read_to_string(dir.join("src/main.rs")).unwrap();
        assert!(main_rs.contains("use std::rc::Rc;"), "missing Rc import: {main_rs}");
        assert!(main_rs.contains("use std::cell::RefCell;"), "missing RefCell import: {main_rs}");
        assert!(main_rs.contains("Rc::new(RefCell::new("), "missing Rc wrapping: {main_rs}");

        let cargo_toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(!cargo_toml.contains("tokio"), "should not have tokio: {cargo_toml}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_ui_vs_general() {
        assert!(is_ui_dsl(
            r#"import { Column } from "@w3cos/std"; Column({ children: [] })"#
        ));
        assert!(is_ui_dsl(r#"Column({ children: [Text("hi")] })"#));
        assert!(is_ui_dsl(r#"<Column><Text>hi</Text></Column>"#));
        assert!(!is_ui_dsl(
            r#"function main() { console.log("hello"); }"#
        ));
        assert!(!is_ui_dsl(r#"let x = 42; console.log(x);"#));
    }
}
