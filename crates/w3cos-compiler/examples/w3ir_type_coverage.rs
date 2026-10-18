//! Typed W3IR coverage report (M1 item 11).
//!
//! ```text
//! cargo run -p w3cos-compiler --example w3ir_type_coverage -- file.ts [more.ts…] [--functions]
//! ```
//!
//! Lowers each module to W3IR and prints how many functions carry TS hints,
//! how many can get a speculated (typed) version, and the share of registers
//! with a concrete type with and without speculation.

use w3cos_ir::types::{coverage, infer};

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let per_function = args.iter().any(|arg| arg == "--functions");
    for path in args.iter().filter(|arg| !arg.starts_with("--")) {
        let source = std::fs::read_to_string(path).expect("read source");
        let module = match w3cos_compiler::w3ir_lowering::lower_module(&source, path) {
            Ok(module) => module,
            Err(error) => {
                println!("{path}: lowering failed: {error}");
                continue;
            }
        };
        let c = coverage(&module);
        let pct = |n: usize, d: usize| {
            if d == 0 {
                0.0
            } else {
                n as f64 * 100.0 / d as f64
            }
        };
        println!(
            "{path}: functions {} hinted {} ({:.0}%) speculable {} ({:.0}%); registers {} typed proven {:.1}% speculative {:.1}% numeric proven {:.1}% speculative {:.1}%",
            c.functions,
            c.hinted_functions,
            pct(c.hinted_functions, c.functions),
            c.speculable_functions,
            pct(c.speculable_functions, c.functions),
            c.registers,
            pct(c.proven_typed_registers, c.registers),
            pct(c.speculative_typed_registers, c.registers),
            pct(c.proven_numeric_registers, c.registers),
            pct(c.speculative_numeric_registers, c.registers),
        );
        if per_function {
            for function in &module.functions {
                let typed = infer(function, true);
                println!(
                    "  {:40} params {:?} returns {:?} -> inferred return {:?}, speculated {:?}",
                    function.name.as_deref().unwrap_or("<anonymous>"),
                    function.types.parameters,
                    function.types.returns,
                    typed.returns,
                    typed.speculated_parameters,
                );
            }
        }
    }
}
