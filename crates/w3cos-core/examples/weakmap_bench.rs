//! 100k-entry WeakMap `set`/`get` micro-benchmark.
//!
//! `cargo run --release -p w3cos-core --example weakmap_bench [-- N]`
//!
//! Keys are host `Rc` objects (collectable without a page reset), the case
//! where weak tables matter for long-running apps.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use w3cos_core::{JsObject, Value};

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(100_000);
    let map = w3cos_core::intrinsics::construct(&w3cos_core::weak::weak_map_class(), vec![]);
    let keys: Vec<Value> = (0..n)
        .map(|_| Value::Object(Rc::new(RefCell::new(JsObject::new()))))
        .collect();

    let started = Instant::now();
    for (index, key) in keys.iter().enumerate() {
        map.call_method("set", vec![key.clone(), Value::Number(index as f64)]);
    }
    let set_elapsed = started.elapsed();

    let started = Instant::now();
    let mut sum = 0.0;
    for key in &keys {
        sum += map.call_method("get", vec![key.clone()]).to_number();
    }
    let get_elapsed = started.elapsed();
    assert_eq!(sum, (n * (n - 1) / 2) as f64);

    println!(
        "weakmap n={n} set={:.1}ms ({:.0}ns/op) get={:.1}ms ({:.0}ns/op)",
        set_elapsed.as_secs_f64() * 1e3,
        set_elapsed.as_nanos() as f64 / n as f64,
        get_elapsed.as_secs_f64() * 1e3,
        get_elapsed.as_nanos() as f64 / n as f64,
    );
}
