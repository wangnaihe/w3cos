//! Old vs new dynamic-mode lowering of a condition-position logical chain.
//!
//! The two functions below are the exact Rust shapes `esm_lowering` emits
//! for `if (a && b || c && !d)` over `Value` locals, before and after
//! condition-position specialization:
//!
//! * old: every `&&`/`||` binds `let __l`, selects an operand `Value`, and the
//!   `if` calls `.to_bool()` on the selected value again; `!d` builds a
//!   `Value::Bool` through `logical_not`.
//! * new: each operand's truthiness is tested once as a native `bool` and
//!   Rust's `&&`/`||` short-circuit.
//!
//! `logical_shapes_agree_on_js_truthiness` (always run) checks both shapes
//! agree with JS on every combination of falsy/truthy operands.
//! `bench_condition_position_logical_chain` is `#[ignore]`d timing only:
//! `cargo test --release -p w3cos-compiler --test logical_ops_bench -- --ignored --nocapture`

use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;
use w3cos_core::Value;

#[inline(never)]
fn old_shape(a: &Value, b: &Value, c: &Value, d: &Value) -> bool {
    {
        let __l = {
            let __l = a.clone();
            if __l.to_bool() { b.clone() } else { __l }
        };
        if __l.to_bool() {
            __l
        } else {
            let __l = c.clone();
            if __l.to_bool() {
                w3cos_core::intrinsics::logical_not(&d.clone())
            } else {
                __l
            }
        }
    }
    .to_bool()
}

#[inline(never)]
fn new_shape(a: &Value, b: &Value, c: &Value, d: &Value) -> bool {
    ((a.clone()).to_bool() && (b.clone()).to_bool())
        || ((c.clone()).to_bool() && (!(d.clone()).to_bool()))
}

fn operands() -> Vec<(Value, bool)> {
    vec![
        (Value::from(0.0), false),
        (Value::from(-0.0), false),
        (Value::from(f64::NAN), false),
        (Value::string(""), false),
        (Value::Null, false),
        (Value::Undefined, false),
        (Value::Bool(false), false),
        (Value::from(1.5), true),
        (Value::string("x"), true),
        (Value::Bool(true), true),
        (Value::object(HashMap::new()), true),
        (Value::array(Vec::new()), true),
    ]
}

#[test]
fn logical_shapes_agree_on_js_truthiness() {
    let ops = operands();
    for (a, ta) in &ops {
        assert_eq!(a.to_bool(), *ta, "ToBoolean({a:?})");
        for (b, tb) in &ops {
            for (c, tc) in &ops {
                for (d, td) in &ops {
                    let expected = (*ta && *tb) || (*tc && !*td);
                    assert_eq!(old_shape(a, b, c, d), expected);
                    assert_eq!(new_shape(a, b, c, d), expected);
                }
            }
        }
    }
}

fn time(label: &str, rounds: usize, f: fn(&Value, &Value, &Value, &Value) -> bool) -> f64 {
    let ops: Vec<Value> = operands().into_iter().map(|(v, _)| v).collect();
    let n = ops.len();
    let mut hits = 0usize;
    let start = Instant::now();
    for r in 0..rounds {
        let a = &ops[r % n];
        let b = &ops[(r / n) % n];
        let c = &ops[(r / (n * n)) % n];
        let d = &ops[(r / (n * n * n)) % n];
        if f(black_box(a), black_box(b), black_box(c), black_box(d)) {
            hits += 1;
        }
    }
    let ns = start.elapsed().as_nanos() as f64 / rounds as f64;
    println!("{label}: {ns:.3} ns/eval (hits {hits})");
    black_box(hits);
    ns
}

#[test]
#[ignore = "timing only; run with --release -- --ignored --nocapture"]
fn bench_condition_position_logical_chain() {
    let rounds = 20_000_000;
    // Warm up both.
    time("warmup-old", rounds / 10, old_shape);
    time("warmup-new", rounds / 10, new_shape);
    let mut old = Vec::new();
    let mut new = Vec::new();
    for _ in 0..5 {
        old.push(time("old `let __l` + to_bool", rounds, old_shape));
        new.push(time("new native bool chain ", rounds, new_shape));
    }
    old.sort_by(|x, y| x.partial_cmp(y).unwrap());
    new.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let (o, nw) = (old[2], new[2]);
    println!(
        "MEDIAN old {o:.3} ns/eval, new {nw:.3} ns/eval, speedup {:.2}x",
        o / nw
    );
}
