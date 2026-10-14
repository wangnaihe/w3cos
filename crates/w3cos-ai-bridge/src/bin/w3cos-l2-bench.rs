//! Fixed 10-task bench for an OpenAI-compatible L2 endpoint.
//!
//! ```text
//! W3COS_L2=1 \
//! W3COS_L2_BASE_URL=http://127.0.0.1:8081/v1 \
//! W3COS_L2_MODEL=qwen2.5-1.5b-instruct \
//! W3COS_L2_LABEL="llama.cpp + Qwen2.5-1.5B" \
//! cargo run -p w3cos-ai-bridge --bin w3cos-l2-bench --release
//! ```
//!
//! Prints one JSON object and a markdown row. Model weights are not in this
//! binary. Optional `W3COS_L2_SERVER_PID` adds that process's VmRSS.

use std::time::Instant;

use w3cos_ai_bridge::agent_loop::{self, TASKS};
use w3cos_ai_bridge::planner::{L2Mode, OpenAiPlanner, mode_from_env};

fn main() {
    let mode = match mode_from_env() {
        Ok(L2Mode::Remote(config)) => config,
        Ok(_) => {
            eprintln!("set W3COS_L2=1 and W3COS_L2_BASE_URL");
            std::process::exit(2);
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };
    let label = std::env::var("W3COS_L2_LABEL").unwrap_or_else(|_| mode.model.clone());
    let planner = OpenAiPlanner { config: mode };
    let rss_before = server_rss_kb();
    let started = Instant::now();
    let mut passed = 0usize;
    let mut latencies = Vec::new();
    let mut ttfts = Vec::new();
    let mut rows = Vec::new();
    for task in TASKS {
        let trace = agent_loop::drive_until_done(&planner, task.utterance);
        let ok = agent_loop::task_passed(task.id, &trace);
        if ok {
            passed += 1;
        }
        latencies.extend(trace.latencies_ms.iter().copied());
        ttfts.extend(trace.ttft_ms.iter().copied());
        let median_step = median(&trace.latencies_ms);
        let median_ttft = median(&trace.ttft_ms);
        rows.push(format!(
            "| {} | {} | {} | {} | {} |",
            task.id,
            if ok { "pass" } else { "fail" },
            median_ttft,
            median_step,
            trace.final_text.replace('|', "/")
        ));
        eprintln!(
            "[{}] {} tools={} apps={:?} line={} output={} final={}",
            task.id,
            if ok { "pass" } else { "fail" },
            trace.tools.join(","),
            trace.apps,
            trace.terminal_line,
            trace.terminal_output,
            trace.final_text.replace('\n', " ")
        );
    }
    let rss_after = server_rss_kb();
    let summary = serde_json::json!({
        "label": label,
        "passed": passed,
        "total": TASKS.len(),
        "success_rate": passed as f64 / TASKS.len() as f64,
        "median_ttft_ms": median(&ttfts),
        "median_step_ms": median(&latencies),
        "wall_ms": started.elapsed().as_millis(),
        "server_rss_kb_before": rss_before,
        "server_rss_kb_after": rss_after,
    });
    println!("{summary}");
    println!();
    println!("| task | result | median ttft ms | median step ms | final |");
    println!("| --- | --- | --- | --- | --- |");
    for row in rows {
        println!("{row}");
    }
    eprintln!(
        "{label}: {passed}/{}  median ttft {} ms  median step {} ms  rss {} -> {} KiB",
        TASKS.len(),
        median(&ttfts),
        median(&latencies),
        rss_before.unwrap_or(0),
        rss_after.unwrap_or(0)
    );
}

fn median(values: &[u128]) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn server_rss_kb() -> Option<u64> {
    let pid = std::env::var("W3COS_L2_SERVER_PID").ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb);
        }
    }
    None
}
