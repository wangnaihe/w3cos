fn main() {
    let src = std::env::args().nth(1).unwrap();
    let m = w3cos_compiler::w3ir_lowering::lower_script(&src, "app:///t.js").unwrap();
    let f = m.functions.iter().find(|f| f.id == m.entry).unwrap();
    let g = w3cos_compiler::w3ir_aot::generate_sync_function_from_module(&m, f, "case_0").unwrap();
    println!("{g}");
}
