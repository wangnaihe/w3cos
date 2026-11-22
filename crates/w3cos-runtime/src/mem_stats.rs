//! Opt-in memory accounting: `W3COS_MEM_STATS=1`.
//!
//! When the variable is set (to anything but `0` or empty), the event loop
//! prints a report to stderr every `W3COS_MEM_STATS_INTERVAL_MS` (default
//! 5000 ms; `0` disables the periodic report) and once more at process exit.
//! Each report is one `W3COS_MEM <label> t=<ms> <section> key=value ...` line
//! per section, so runs can be grepped and diffed. Byte figures are owned
//! storage estimates (lengths and capacities), not allocator-exact numbers;
//! compare them with the process lines (`VmRSS`, `RssAnon`).
//!
//! When the variable is unset the only cost is one relaxed atomic load per
//! event-loop turn ([`maybe_dump`]); nothing is counted on allocation paths.
//! Every counter here is computed by walking existing tables at report time.
//!
//! The exit report runs from `atexit`, after thread-local tables may already
//! be gone, so it re-prints the last periodic report together with the
//! process high-water mark instead of walking them again. Call [`dump`] from
//! the JavaScript thread for a fresh report at a point of your choosing.

use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

const UNKNOWN: u8 = 0;
const OFF: u8 = 1;
const ON: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);
static INTERVAL_MS: AtomicU64 = AtomicU64::new(5000);
static NEXT_DUE_MS: AtomicU64 = AtomicU64::new(0);
static LAST_REPORT: Mutex<String> = Mutex::new(String::new());
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// One report section: a name and `(key, value)` pairs.
pub type Section = (&'static str, Vec<(&'static str, u64)>);

fn env_enabled(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some(v) if !v.is_empty() && v != "0")
}

/// Whether `W3COS_MEM_STATS` is on. Reads the environment once.
pub fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        ON => true,
        OFF => false,
        _ => init(),
    }
}

#[cold]
fn init() -> bool {
    let on = env_enabled(std::env::var("W3COS_MEM_STATS").ok().as_deref());
    if on {
        if let Some(ms) = std::env::var("W3COS_MEM_STATS_INTERVAL_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            INTERVAL_MS.store(ms, Ordering::Relaxed);
        }
        START.get_or_init(Instant::now);
        // SAFETY: registering a plain `extern "C" fn` with no captured state.
        unsafe {
            libc::atexit(dump_at_exit);
        }
    }
    let state = if on { ON } else { OFF };
    // Another thread may have raced us; either answer comes from the same env.
    let _ = STATE.compare_exchange(UNKNOWN, state, Ordering::Relaxed, Ordering::Relaxed);
    on
}

fn elapsed_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Called once per event-loop turn. Prints a report when one is due.
#[inline]
pub fn maybe_dump() {
    if STATE.load(Ordering::Relaxed) == OFF {
        return;
    }
    maybe_dump_slow();
}

#[cold]
#[inline(never)]
fn maybe_dump_slow() {
    if !enabled() {
        return;
    }
    let interval = INTERVAL_MS.load(Ordering::Relaxed);
    if interval == 0 {
        return;
    }
    let now = elapsed_ms();
    let due = NEXT_DUE_MS.load(Ordering::Relaxed);
    if now < due {
        return;
    }
    NEXT_DUE_MS.store(now + interval, Ordering::Relaxed);
    dump("tick");
}

/// Collect and print a full report now (JavaScript thread).
pub fn dump(label: &str) {
    let report = format_report(label, elapsed_ms(), &collect());
    eprint!("{report}");
    if let Ok(mut last) = LAST_REPORT.lock() {
        *last = report;
    }
}

extern "C" fn dump_at_exit() {
    let process = format_report("exit", elapsed_ms(), &[process_section()]);
    let last = LAST_REPORT
        .try_lock()
        .map(|last| last.clone())
        .unwrap_or_default();
    eprint!("{process}");
    for line in last.lines() {
        eprintln!(
            "{}",
            line.replacen("W3COS_MEM tick", "W3COS_MEM exit-last", 1)
        );
    }
}

/// Format sections as `W3COS_MEM <label> t=<ms> <section> k=v ...` lines.
pub fn format_report(label: &str, t_ms: u64, sections: &[Section]) -> String {
    let mut out = String::new();
    for (name, values) in sections {
        let _ = write!(out, "W3COS_MEM {label} t={t_ms} {name}");
        for (key, value) in values {
            let _ = write!(out, " {key}={value}");
        }
        out.push('\n');
    }
    out
}

/// `VmRSS` / `VmHWM` / `RssAnon` / `RssFile` / `RssShmem` / `VmSwap` in bytes.
pub fn process_section() -> Section {
    let mut values = Vec::new();
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        values = parse_proc_status(&status);
    }
    ("process", values)
}

fn parse_proc_status(status: &str) -> Vec<(&'static str, u64)> {
    const KEYS: [(&str, &str); 6] = [
        ("VmRSS:", "rss"),
        ("VmHWM:", "rss_peak"),
        ("RssAnon:", "rss_anon"),
        ("RssFile:", "rss_file"),
        ("RssShmem:", "rss_shmem"),
        ("VmSwap:", "swap"),
    ];
    let mut values = Vec::new();
    for line in status.lines() {
        for (prefix, key) in KEYS {
            if let Some(rest) = line.strip_prefix(prefix)
                && let Some(kb) = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
            {
                values.push((key, kb * 1024));
            }
        }
    }
    values
}

fn n(value: usize) -> u64 {
    value as u64
}

/// Every section, walked now on the calling (JavaScript) thread.
pub fn collect() -> Vec<Section> {
    let mut sections = vec![process_section()];

    let heap = w3cos_core::heap::heap_snapshot();
    sections.push((
        "js_heap",
        vec![
            ("live_bytes", n(heap.live_bytes)),
            ("peak_live_bytes", n(heap.peak_live_bytes)),
            ("objects", n(heap.live_objects)),
            ("arrays", n(heap.live_arrays)),
            ("functions", n(heap.live_functions)),
            ("total_allocations", heap.total_allocations),
            ("total_allocated_bytes", heap.total_allocated_bytes),
        ],
    ));
    sections.push((
        "js_arena",
        vec![
            ("objects", n(w3cos_core::page_arena::live_objects())),
            ("arrays", n(w3cos_core::page_arena::live_arrays())),
            ("functions", n(w3cos_core::page_arena::live_functions())),
            ("slab_bytes", n(w3cos_core::page_arena::slab_bytes())),
        ],
    ));
    let strings = w3cos_core::page_arena::string_stats();
    sections.push((
        "js_strings",
        vec![
            ("interned", n(strings.strings)),
            ("bytes", n(strings.bytes)),
            ("large", n(strings.large_strings)),
            ("large_bytes", n(strings.large_bytes)),
            ("reserved_bytes", n(strings.reserved_bytes)),
        ],
    ));
    let buffers = w3cos_core::binary::buffer_stats();
    sections.push((
        "js_buffers",
        vec![
            ("buffers", n(buffers.live_buffers)),
            ("bytes", n(buffers.bytes)),
            ("capacity_bytes", n(buffers.capacity_bytes)),
            ("view_entries", n(buffers.view_entries)),
        ],
    ));

    if let Some(doc) = crate::dom::try_with_document(|doc| doc.memory_stats()) {
        sections.push((
            "dom",
            vec![
                ("live_nodes", n(doc.live_nodes)),
                ("node_slots", n(doc.node_slots)),
                ("node_slot_bytes", n(doc.node_slot_bytes)),
                ("free_slots", n(doc.free_slots)),
                ("page_string_bytes", n(doc.page_string_bytes)),
                ("inline_styles", n(doc.inline_style_declarations)),
                ("layout_rect_bytes", n(doc.layout_rect_bytes)),
            ],
        ));
        sections.push((
            "styles",
            vec![
                ("style_size", n(doc.style_size)),
                ("interned", n(doc.interned_styles)),
                ("interned_unshared", n(doc.interned_styles_unshared)),
                ("interned_bytes", n(doc.interned_style_bytes)),
                ("custom_property_maps", n(doc.custom_property_maps)),
                ("custom_property_bytes", n(doc.custom_property_bytes)),
                (
                    "computed_cache_entries",
                    n(doc.computed_style_cache_entries),
                ),
                ("computed_cache_bytes", n(doc.computed_style_cache_bytes)),
                ("cascade_cache_entries", n(doc.cascade_cache_entries)),
                ("pseudo_cache_entries", n(doc.pseudo_cache_entries)),
            ],
        ));
    }

    let layout = crate::incremental_layout::layout_cache_stats();
    sections.push((
        "layout",
        vec![
            ("cached", u64::from(layout.cached)),
            ("component_nodes", n(layout.component_nodes)),
            ("layout_nodes", n(layout.layout_nodes)),
            ("width_memos", n(layout.width_memos)),
            ("memo_rects", n(layout.memo_rects)),
            ("client_rect_nodes", n(layout.client_rect_nodes)),
        ],
    ));
    sections.push(("bindings", crate::jsdom::binding_table_stats()));

    let fonts = crate::font_face::font_memory_stats();
    sections.push((
        "fonts",
        vec![
            ("refs", n(fonts.font_refs)),
            ("blobs", n(fonts.blobs)),
            ("blob_bytes", n(fonts.blob_bytes)),
            ("unique_content_bytes", n(fonts.unique_content_bytes)),
            ("duplicate_bytes", n(fonts.duplicate_bytes)),
            ("parsed", n(fonts.parsed_fonts)),
        ],
    ));
    let images = crate::image_loader::image_cache_stats();
    sections.push((
        "images",
        vec![
            ("entries", n(images.entries)),
            ("key_bytes", n(images.key_bytes)),
            ("pixel_buffers", n(images.pixel_buffers)),
            ("pixel_bytes", n(images.pixel_bytes)),
            ("svg_source_bytes", n(images.svg_source_bytes)),
            ("animated", n(images.animated_images)),
        ],
    ));
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_flag_parsing() {
        assert!(!env_enabled(None));
        assert!(!env_enabled(Some("")));
        assert!(!env_enabled(Some("0")));
        assert!(env_enabled(Some("1")));
        assert!(env_enabled(Some(" yes ")));
    }

    #[test]
    fn proc_status_values_are_bytes() {
        let status = "Name:\tx\nVmHWM:\t  2048 kB\nVmRSS:\t  1024 kB\nRssAnon:\t 512 kB\n";
        let values = parse_proc_status(status);
        assert!(values.contains(&("rss", 1024 * 1024)));
        assert!(values.contains(&("rss_peak", 2048 * 1024)));
        assert!(values.contains(&("rss_anon", 512 * 1024)));
    }

    #[test]
    fn report_lines_are_greppable() {
        let report = format_report(
            "tick",
            42,
            &[
                ("js_heap", vec![("objects", 3), ("arrays", 4)]),
                ("fonts", vec![]),
            ],
        );
        assert_eq!(
            report,
            "W3COS_MEM tick t=42 js_heap objects=3 arrays=4\nW3COS_MEM tick t=42 fonts\n"
        );
    }

    #[test]
    fn collect_reports_every_section_and_live_document_nodes() {
        let sections = collect();
        let names: Vec<_> = sections.iter().map(|(name, _)| *name).collect();
        for expected in [
            "process",
            "js_heap",
            "js_arena",
            "js_strings",
            "js_buffers",
            "dom",
            "styles",
            "layout",
            "bindings",
            "fonts",
            "images",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        let value = |section: &str, key: &str| {
            sections
                .iter()
                .find(|(name, _)| *name == section)
                .and_then(|(_, values)| values.iter().find(|(k, _)| *k == key))
                .map(|(_, v)| *v)
        };
        let nodes_before = value("dom", "live_nodes").unwrap();
        let _div = crate::dom::create_element("div");
        let nodes_after = collect()
            .into_iter()
            .find(|(name, _)| *name == "dom")
            .and_then(|(_, values)| values.into_iter().find(|(k, _)| *k == "live_nodes"))
            .map(|(_, v)| v)
            .unwrap();
        assert_eq!(nodes_after, nodes_before + 1);
        let _ = w3cos_core::Value::string(&"z".repeat(w3cos_core::page_arena::LARGE_STRING_BYTES));
        assert!(collect().iter().any(|(name, values)| *name == "js_strings"
            && values.iter().any(|(k, v)| *k == "large" && *v >= 1)));
    }
}
