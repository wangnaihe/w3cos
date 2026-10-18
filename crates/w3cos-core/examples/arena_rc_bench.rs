//! Page-arena object alloc / property-access / clone micro-benchmark.
//!
//! `cargo run --release -p w3cos-core --example arena_rc_bench [-- N]`
//!
//! Measures the per-operation cost of escaping (page-arena) objects, and how
//! many slots are still live after the values are dropped (before a page
//! reset). Run before/after slot refcounting to compare.

use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;

use w3cos_core::Value;
use w3cos_core::page_arena;

fn ns_per(elapsed: std::time::Duration, n: usize) -> f64 {
    elapsed.as_nanos() as f64 / n as f64
}

#[inline(never)]
fn clone_drop_loop(value: &Value, n: usize) -> std::time::Duration {
    let started = Instant::now();
    for _ in 0..n {
        black_box(black_box(value).clone());
    }
    started.elapsed()
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(1_000_000);
    page_arena::reset();

    // 1. Allocate an escaping two-field object and drop it right away.
    let started = Instant::now();
    for index in 0..n {
        let mut props = HashMap::with_capacity(2);
        props.insert("x".to_string(), Value::Number(index as f64));
        props.insert("y".to_string(), Value::Number(1.0));
        black_box(Value::object(props));
    }
    let alloc = started.elapsed();
    let live_after_alloc = page_arena::live_objects();
    let slab_after_alloc = page_arena::slab_bytes();
    page_arena::reset();

    // 2. Read a property of one arena object N times.
    let mut props = HashMap::new();
    props.insert("x".to_string(), Value::Number(2.0));
    props.insert("y".to_string(), Value::Number(3.0));
    let object = Value::object(props);
    let started = Instant::now();
    let mut sum = 0.0;
    for _ in 0..n {
        sum += black_box(&object)
            .get_property("x")
            .as_number()
            .unwrap_or(0.0);
    }
    let access = started.elapsed();
    black_box(sum);

    // 3. Write a property N times.
    let started = Instant::now();
    for index in 0..n {
        black_box(&object).set_property("y", Value::Number(index as f64));
    }
    let write = started.elapsed();

    // 3b. The same read / write through a static inline cache (AOT sites
    // with a constant key), plus a prototype read through an IC.
    static GET_IC: w3cos_core::PropIc = w3cos_core::PropIc::new();
    static SET_IC: w3cos_core::PropIc = w3cos_core::PropIc::new();
    static PROTO_IC: w3cos_core::PropIc = w3cos_core::PropIc::new();
    w3cos_core::ic::reset_stats();
    let started = Instant::now();
    let mut sum = 0.0;
    for _ in 0..n {
        sum += black_box(&object)
            .get_property_ic("x", &GET_IC)
            .as_number()
            .unwrap_or(0.0);
    }
    let access_ic = started.elapsed();
    black_box(sum);
    let started = Instant::now();
    for index in 0..n {
        black_box(&object).set_property_ic("y", Value::Number(index as f64), &SET_IC);
    }
    let write_ic = started.elapsed();
    let mut proto_props = HashMap::new();
    proto_props.insert("method".to_string(), Value::Number(1.0));
    let proto = Value::object(proto_props);
    let instance = Value::object(HashMap::from([("own".to_string(), Value::Number(0.0))]));
    w3cos_core::class::set_prototype_of(&instance, &proto);
    let started = Instant::now();
    for _ in 0..n {
        black_box(black_box(&instance).get_property("method"));
    }
    let proto_get = started.elapsed();
    let started = Instant::now();
    for _ in 0..n {
        black_box(black_box(&instance).get_property_ic("method", &PROTO_IC));
    }
    let proto_ic = started.elapsed();
    let ic_stats = w3cos_core::ic::stats();

    // 4. Clone + drop the handle N times; 4b. the same loop on a number is
    // the floor (enum dispatch, no count). Separate non-inlined loops so
    // the rest of `main` does not change their codegen.
    let clone = clone_drop_loop(&object, n);
    let clone_number = clone_drop_loop(&Value::Number(1.0), n);

    // 5. Build an array of N/10 objects, drop it, count survivors.
    let m = (n / 10).max(1);
    let before = page_arena::live_objects();
    let started = Instant::now();
    {
        let items: Vec<Value> = (0..m)
            .map(|index| {
                let mut props = HashMap::with_capacity(1);
                props.insert("i".to_string(), Value::Number(index as f64));
                Value::object(props)
            })
            .collect();
        black_box(Value::array(items));
    }
    let array_cycle = started.elapsed();
    let array_live = page_arena::live_objects() - before;

    println!("n={n}");
    println!(
        "alloc+drop object     {:8.1} ns/op  live_after={live_after_alloc} slab_bytes={slab_after_alloc}",
        ns_per(alloc, n)
    );
    println!("get_property          {:8.1} ns/op", ns_per(access, n));
    println!("set_property          {:8.1} ns/op", ns_per(write, n));
    println!("get_property_ic       {:8.1} ns/op", ns_per(access_ic, n));
    println!("set_property_ic       {:8.1} ns/op", ns_per(write_ic, n));
    println!("proto get             {:8.1} ns/op", ns_per(proto_get, n));
    println!("proto get_ic          {:8.1} ns/op", ns_per(proto_ic, n));
    println!(
        "ic hit rate           {:8.4}  ({} hits / {} misses)",
        ic_stats.hit_rate(),
        ic_stats.hits(),
        ic_stats.misses()
    );
    println!("clone+drop handle     {:8.1} ns/op", ns_per(clone, n));
    println!(
        "clone+drop number     {:8.1} ns/op",
        ns_per(clone_number, n)
    );
    println!(
        "array of {m} objs     {:8.1} ns/obj live_after_drop={array_live}",
        ns_per(array_cycle, m)
    );
}
