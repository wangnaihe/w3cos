//! End-to-end checks of the Cranelift backend on lowered scripts. The full
//! differential against the rustc backend lives in `examples/cranelift_diff.rs`.

fn run(source: &str) -> String {
    let module = w3cos_compiler::w3ir_lowering::lower_script(source, "app:///test.js").unwrap();
    let program = w3cos_cranelift::compile_module(&module).unwrap();
    match program.run_entry() {
        Ok(_) => "OK".into(),
        Err(e) if e.is_object() => format!(
            "{}: {}",
            e.get_property("name").to_js_string(),
            e.get_property("message").to_js_string()
        ),
        Err(e) => e.to_js_string(),
    }
}

#[test]
fn numbers_and_control_flow() {
    assert_eq!(
        run(
            "let t = 0; for (let i = 0; i < 10; i++) { if (i % 2) continue; t += i * 1.5; } throw 'R:' + t;"
        ),
        "R:30"
    );
    assert_eq!(
        run("throw 'R:' + (7 % -3) + ',' + (5 | 2) + ',' + (2 ** 8) + ',' + (0 / 0);"),
        "R:1,7,256,NaN"
    );
}

#[test]
fn recursion_through_captured_binding() {
    assert_eq!(
        run(
            "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } throw 'R:' + fib(15);"
        ),
        "R:610"
    );
}

#[test]
fn closures_share_cells() {
    assert_eq!(
        run(
            "function c() { var n = 0; return function () { return ++n; }; } var f = c(); f(); throw 'R:' + f();"
        ),
        "R:2"
    );
}

#[test]
fn exceptions_reach_handlers_and_the_entry() {
    assert_eq!(
        run("try { null.x; } catch (e) { throw 'R:' + e.message; }"),
        "R:Cannot read properties of null (reading 'x')"
    );
    assert_eq!(
        run("var o = void 0; o.y;"),
        "TypeError: Cannot read properties of undefined (reading 'y')"
    );
    assert_eq!(
        run("function f() { try { return 1; } finally { throw 'F'; } } f();"),
        "F"
    );
}

#[test]
fn unsupported_constructs_are_rejected() {
    let module = w3cos_compiler::w3ir_lowering::lower_script(
        "async function f() { await 1; }",
        "app:///t.js",
    )
    .unwrap();
    assert!(w3cos_cranelift::compile_module(&module).is_err());
}
