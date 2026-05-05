//! Web Worker demo — offloads a Sieve-of-Eratosthenes computation onto a
//! background thread. The parent process posts an upper bound, the worker
//! returns the list of primes plus a count summary. Demonstrates:
//!
//! * `Worker::spawn` + `WorkerScope::recv` for a long-running compute task.
//! * `Worker::post_message` + `Worker::try_recv` for non-blocking parent IO.
//! * `Worker::terminate` to shut the worker down deterministically.
//!
//! Run with: `cargo run -p w3cos-runtime --example worker_prime_sieve`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use w3cos_runtime::worker::{Worker, WorkerEvent, WorkerOptions};

fn main() {
    let worker = Worker::spawn(WorkerOptions::named("prime-sieve"), |scope| {
        while let Some(msg) = scope.recv() {
            let upper = msg
                .get("upper")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;

            let started = Instant::now();
            let primes = sieve(upper);
            let elapsed = started.elapsed().as_micros() as u64;

            let response = json!({
                "upper": upper,
                "count": primes.len(),
                "first_ten": primes.iter().take(10).copied().collect::<Vec<_>>(),
                "last": primes.last().copied().unwrap_or(0),
                "elapsed_us": elapsed,
            });
            if scope.post_message(response).is_err() {
                break;
            }
        }
    });

    println!("[parent] dispatching jobs to background worker...");
    for upper in [10_000_u64, 100_000, 1_000_000] {
        worker.post_message(json!({"upper": upper})).unwrap();
    }

    let mut received = 0;
    let deadline = Instant::now() + Duration::from_secs(30);
    while received < 3 && Instant::now() < deadline {
        for event in worker.poll_events() {
            match event {
                WorkerEvent::Message(v) => {
                    println!(
                        "[parent] primes ≤ {} → count={}, first_ten={}, last={}, took {} µs",
                        v["upper"], v["count"], v["first_ten"], v["last"], v["elapsed_us"]
                    );
                    received += 1;
                }
                WorkerEvent::Error(msg) => eprintln!("[worker error] {msg}"),
                WorkerEvent::Exit => println!("[parent] worker exited"),
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    worker.terminate();
    println!("[parent] worker terminated cleanly");
}

fn sieve(upper: usize) -> Vec<u64> {
    if upper < 2 {
        return Vec::new();
    }
    let mut is_prime = vec![true; upper + 1];
    is_prime[0] = false;
    is_prime[1] = false;
    let mut i = 2usize;
    while i * i <= upper {
        if is_prime[i] {
            let mut j = i * i;
            while j <= upper {
                is_prime[j] = false;
                j += i;
            }
        }
        i += 1;
    }
    is_prime
        .iter()
        .enumerate()
        .filter_map(|(n, &keep)| if keep { Some(n as u64) } else { None })
        .collect()
}
