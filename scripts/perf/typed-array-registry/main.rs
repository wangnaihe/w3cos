//! Engine API workload, not a browser or JavaScript end-to-end benchmark.
use std::{hint::black_box, time::Instant};
use w3cos_core::{Value, binary, page_arena};

// Allocation counting is opt-in: do not use this binary for timing results.
#[cfg(feature = "allocation-counting")]
mod allocations {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    pub static LIVE: AtomicUsize = AtomicUsize::new(0);
    pub static PEAK: AtomicUsize = AtomicUsize::new(0);
    pub struct Counting;
    fn add(bytes: usize) {
        let now = LIVE.fetch_add(bytes, Relaxed) + bytes;
        PEAK.fetch_max(now, Relaxed);
    }
    // SAFETY: Forward the caller's pointers/layouts unchanged to System. The
    // wrapper obeys the same allocation contract; bookkeeping uses only atomic
    // integer operations (no allocation, dereference, or allocator reentrancy).
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                add(layout.size());
            }
            ptr
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc_zeroed(layout) };
            if !ptr.is_null() {
                add(layout.size());
            }
            ptr
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) };
            LIVE.fetch_sub(layout.size(), Relaxed);
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let new = unsafe { System.realloc(ptr, layout, size) };
            if !new.is_null() {
                if size >= layout.size() {
                    add(size - layout.size());
                } else {
                    LIVE.fetch_sub(layout.size() - size, Relaxed);
                }
            }
            new
        }
    }
}
#[cfg(feature = "allocation-counting")]
#[global_allocator]
static ALLOCATOR: allocations::Counting = allocations::Counting;

#[cfg(feature = "allocation-counting")]
fn memory(count: usize) {
    use std::sync::atomic::Ordering::Relaxed;
    let before = allocations::LIVE.load(Relaxed);
    allocations::PEAK.store(before, Relaxed);
    let kept = views(count);
    let held = allocations::LIVE.load(Relaxed);
    let peak = allocations::PEAK.load(Relaxed);
    assert_eq!(kept.len(), count);
    drop(kept);
    page_arena::reset();
    {
        // Trigger lazy registry cleanup after epoch change, same as engine use.
        let plain = Value::array(vec![Value::Number(7.0)]);
        assert_eq!(plain.get_property("0").to_number(), 7.0);
    }
    let after = allocations::LIVE.load(Relaxed);
    println!(
        "memory,{count},{before},{held},{peak},{after},{},{}",
        held as i128 - before as i128,
        after as i128 - before as i128
    );
}

const ROWS: usize = 64;
const WIDTH: usize = 32;

fn views(count: usize) -> Vec<Value> {
    (0..count)
        .map(|i| binary::typed_array_value(vec![Value::Number((i % 251 + 1) as f64); WIDTH]))
        .collect()
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("mixed");
    let count: usize = args
        .get(2)
        .map(String::as_str)
        .unwrap_or("1024")
        .parse()
        .unwrap();
    let iterations: usize = args
        .get(3)
        .map(String::as_str)
        .unwrap_or("20000")
        .parse()
        .unwrap();
    if mode == "memory" {
        #[cfg(feature = "allocation-counting")]
        {
            memory(count);
            return;
        }
        #[cfg(not(feature = "allocation-counting"))]
        panic!("memory mode requires --features allocation-counting");
    }
    assert!(iterations > 0);
    // One sample per process: live page-arena arrays cannot be reclaimed merely
    // by dropping Values, so repeated steady-state rounds would change N.
    let (elapsed, checksum, expected) = match mode {
        "ordinary" | "mixed" | "read" | "method" => {
            let kept = views(count);
            let rows: Vec<_> = (0..ROWS)
                .map(|i| Value::array(vec![Value::Number((i + 1) as f64), Value::Number(0.0)]))
                .collect();
            let expected: u64 = (0..iterations)
                .map(|i| {
                    let base = (i % ROWS + 1) as u64;
                    let input = if mode == "mixed" && count > 0 {
                        (i % count % 251 + 1) as u64
                    } else {
                        (i % 251) as u64
                    };
                    match mode {
                        "read" => base,
                        "method" => input,
                        _ => base + (base ^ input) + input,
                    }
                })
                .sum();
            let started = Instant::now();
            let mut checksum = 0_u64;
            for i in 0..iterations {
                let row = &rows[i % ROWS];
                if mode == "read" {
                    checksum += row.get_property("0").to_number() as u64;
                    continue;
                }
                let input = if mode == "mixed" && count > 0 {
                    kept[i % count].get_property("0").to_number() as u64
                } else {
                    (i % 251) as u64
                };
                if mode != "method" {
                    let base = row.get_property("0").to_number() as u64;
                    row.set_property("1", Value::Number((base ^ input) as f64));
                    checksum += base + row.get_property("1").to_number() as u64;
                }
                row.call_method("push", vec![Value::Number(input as f64)]);
                checksum += row.call_method("pop", vec![]).to_number() as u64;
            }
            let elapsed = started.elapsed();
            for row in &rows {
                assert_eq!(row.get_property("length").to_number(), 2.0);
            }
            black_box((&rows, &kept));
            (elapsed, checksum, expected)
        }
        "construct" => {
            // count is the number of already-live unrelated views; iterations
            // is the number of additional views allocated during measurement.
            let background = views(count);
            let started = Instant::now();
            let created = views(iterations);
            let elapsed = started.elapsed();
            let checksum: u64 = created
                .iter()
                .map(|view| {
                    let bytes = binary::bytes_of(view).expect("typed view bytes");
                    assert_eq!(bytes.len(), WIDTH);
                    bytes.iter().map(|byte| u64::from(*byte)).sum::<u64>()
                })
                .sum();
            let expected = (0..iterations)
                .map(|i| (i % 251 + 1) as u64 * WIDTH as u64)
                .sum();
            black_box((&background, &created));
            (elapsed, checksum, expected)
        }
        "churn" | "lifecycle" => {
            // count views per navigation epoch, iterations epochs. Include
            // allocation, validation, dropping, page reset, and first ordinary
            // access after reset. No page Values survive across the reset.
            let expected = iterations as u64
                * (if mode == "lifecycle" {
                    count as u64
                } else {
                    (0..count).map(|i| (i % 251 + 1) as u64).sum::<u64>()
                } + 7);
            let started = Instant::now();
            let mut checksum = 0_u64;
            for _ in 0..iterations {
                {
                    let batch = views(count);
                    if mode == "lifecycle" {
                        checksum += batch.len() as u64;
                    } else {
                        for view in &batch {
                            checksum += view.get_property("0").to_number() as u64;
                        }
                    }
                    black_box(&batch);
                }
                page_arena::reset();
                {
                    let ordinary = Value::array(vec![Value::Number(7.0)]);
                    checksum += ordinary.get_property("0").to_number() as u64;
                }
            }
            let elapsed = started.elapsed();
            (elapsed, checksum, expected)
        }
        _ => panic!("mode must be read, method, ordinary, mixed, construct, churn, or lifecycle"),
    };
    assert_eq!(checksum, expected, "workload checksum");
    println!(
        "{mode},{count},{iterations},{},{checksum}",
        elapsed.as_nanos()
    );
}
