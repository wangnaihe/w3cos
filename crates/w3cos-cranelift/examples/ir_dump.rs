fn main() {
    let src = std::env::args().nth(1).unwrap();
    let m = w3cos_compiler::w3ir_lowering::lower_script(&src, "app:///t.js").unwrap();
    println!(
        "imports: {:?}",
        m.imports
            .iter()
            .map(|i| (&i.specifier, &i.imported))
            .collect::<Vec<_>>()
    );
    for f in &m.functions {
        println!(
            "fn {:?} {:?} params={:?} captures={:?} regs={}",
            f.id, f.name, f.parameters, f.captures, f.registers
        );
        for b in &f.blocks {
            for i in &b.instructions {
                println!("  {:?}: {:?}", b.id.0, i);
            }
        }
    }
    match w3cos_cranelift::compile_module(&m) {
        Ok(p) => {
            println!("compiled in {:?}: {:?}", p.compile_time, p.stats);
            println!(
                "run: {:?}",
                p.run_entry()
                    .map(|v| v.to_js_string())
                    .map_err(|e| e.to_js_string())
            );
        }
        Err(e) => println!("ERR {e}"),
    }
}
