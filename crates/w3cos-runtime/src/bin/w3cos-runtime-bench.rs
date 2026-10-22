//! Reproducible runtime-speed harness.
//!
//! Scenarios:
//! - cold process start through the first painted frame of a small example page
//! - style recalc and layout of a large flex/grid/text DOM
//! - incremental relayout after a width edit, a paint-only edit, a text edit,
//!   a geometry class toggle, and inserting or removing a child
//! - CPU raster of a scrolling list
//! - JS `Value` host calls into the DOM (`createElement`, `setAttribute`,
//!   `appendChild`, `getAttribute`, `textContent`)
//!
//! The process prints one JSON object. It exits non-zero when a scenario does
//! not actually change the DOM or layout it claims to measure.

use std::env;
use std::process::Command;
use std::time::Instant;

use w3cos_core::Value;
use w3cos_dom::NodeId;
use w3cos_runtime::dom;
use w3cos_runtime::headless;
use w3cos_runtime::incremental_layout;
use w3cos_runtime::jsdom;
use w3cos_runtime::layout::{self, LayoutRect};
use w3cos_runtime::stylesheet;

const VIEW_W: f32 = 1280.0;
const VIEW_H: f32 = 720.0;
const LARGE_SECTIONS: usize = 200;
const CELLS_PER_SECTION: usize = 20;
const SCROLL_ROWS: usize = 240;
const HOST_ITERS: usize = 800;

fn main() {
    let mut args = env::args().skip(1);
    let mut output = None;
    let mut samples = 3usize;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--child" => {
                let scenario = args.next().unwrap_or_else(|| "startup".to_string());
                if scenario == "startup" {
                    run_startup_child();
                } else {
                    eprintln!("unknown child scenario {scenario}");
                    std::process::exit(2);
                }
                return;
            }
            "--output" => {
                output = Some(args.next().unwrap_or_else(|| {
                    eprintln!("--output needs a path");
                    std::process::exit(2);
                }));
            }
            "--samples" => {
                samples = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or_else(|| {
                        eprintln!("--samples needs a positive integer");
                        std::process::exit(2);
                    });
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: w3cos-runtime-bench [--output file] [--samples N]\n       w3cos-runtime-bench --child startup"
                );
                return;
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    let startup = measure_startup();
    let style = measure_style_and_incremental(samples);
    let paint = measure_scroll_paint(samples);
    let host = measure_host_calls(samples);
    let report = format!(
        "{{\n  \"harness\": \"w3cos-runtime-bench\",\n  \"profile\": \"runtime-bench\",\n  \"viewport\": [{VIEW_W}, {VIEW_H}],\n  \"startup\": {startup},\n  \"style_layout\": {},\n  \"incremental_width\": {},\n  \"incremental_background\": {},\n  \"incremental_text\": {},\n  \"incremental_class\": {},\n  \"incremental_insert\": {},\n  \"incremental_remove\": {},\n  \"paint_scroll\": {paint},\n  \"host_calls\": {host}\n}}\n",
        style.style,
        style.width,
        style.background,
        style.text,
        style.class_toggle,
        style.insert,
        style.remove,
    );
    print!("{report}");
    if let Some(path) = output {
        std::fs::write(&path, &report).unwrap_or_else(|error| {
            eprintln!("failed to write {path}: {error}");
            std::process::exit(1);
        });
    }
}

fn measure_startup() -> String {
    let exe = env::current_exe().expect("current exe");
    let started = Instant::now();
    let child = Command::new(exe)
        .arg("--child")
        .arg("startup")
        .output()
        .expect("spawn startup child");
    let wall_us = started.elapsed().as_micros();
    if !child.status.success() {
        eprintln!(
            "startup child failed: {}",
            String::from_utf8_lossy(&child.stderr)
        );
        std::process::exit(1);
    }
    let stdout = String::from_utf8_lossy(&child.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with("W3COS_BENCH_STARTUP"))
        .unwrap_or_else(|| {
            eprintln!("startup child produced no result line:\n{stdout}");
            std::process::exit(1);
        });
    let payload = line.trim_start_matches("W3COS_BENCH_STARTUP ").trim();
    format!("{{\"wall_us\": {wall_us}, \"child\": {payload}}}")
}

fn run_startup_child() {
    let started = Instant::now();
    jsdom::set_viewport(f64::from(VIEW_W), f64::from(VIEW_H));
    stylesheet::clear_rules();
    register_example_rules();
    let nodes = build_example_page();
    let built_us = started.elapsed().as_micros();
    let style_started = Instant::now();
    let tree = dom::to_component_tree();
    let laid_out = layout::compute(&tree, VIEW_W, VIEW_H).expect("example layout");
    let style_layout_us = style_started.elapsed().as_micros();
    let paint_started = Instant::now();
    let frame = headless::render_document_rgba(VIEW_W as u32, VIEW_H as u32).expect("first frame");
    let paint_us = paint_started.elapsed().as_micros();
    let checksum = pixel_checksum(&frame.rgba);
    let layout_checksum = rect_checksum(&laid_out);
    if checksum == 0 || laid_out.is_empty() {
        eprintln!("startup frame was empty");
        std::process::exit(1);
    }
    println!(
        "W3COS_BENCH_STARTUP {{\"nodes\": {nodes}, \"build_us\": {built_us}, \"style_layout_us\": {style_layout_us}, \"paint_us\": {paint_us}, \"checksum\": {checksum}, \"layout_checksum\": {layout_checksum}}}"
    );
}

fn register_example_rules() {
    stylesheet::register_rule(
        ".container",
        &[
            ("display", "flex"),
            ("flex-direction", "column"),
            ("padding", "24px"),
            ("gap", "12px"),
            ("background", "#0f0f1a"),
        ],
    );
    stylesheet::register_rule(".header", &[("font-size", "42px"), ("color", "#e94560")]);
    stylesheet::register_rule(".subtitle", &[("font-size", "20px"), ("color", "#a0a0b0")]);
    stylesheet::register_rule(
        ".card",
        &[
            ("background", "#1a1a2e"),
            ("padding", "16px"),
            ("margin", "8px"),
        ],
    );
    stylesheet::register_rule(
        ".card-title",
        &[("font-size", "18px"), ("color", "#ffffff")],
    );
    stylesheet::register_rule(".card-body", &[("font-size", "14px"), ("color", "#a0a0b0")]);
    stylesheet::register_rule(
        ".btn-primary",
        &[
            ("background", "#e94560"),
            ("color", "#ffffff"),
            ("padding", "12px"),
        ],
    );
    stylesheet::register_rule(
        ".btn-secondary",
        &[
            ("background", "#1a1a2e"),
            ("color", "#e94560"),
            ("padding", "12px"),
        ],
    );
    stylesheet::register_rule(".footer", &[("font-size", "14px"), ("color", "#404050")]);
}

fn build_example_page() -> usize {
    let body = dom::body_id();
    let container = dom::create_element("div");
    dom::class_list_add(container, "container");
    dom::append_child(body, container);
    let mut nodes = 2usize;
    nodes += push_text(container, "h1", "header", "W3C OS");
    nodes += push_text(
        container,
        "p",
        "subtitle",
        "TypeScript compiled to native code.",
    );
    for (title, body_text) in [
        (
            "External stylesheets",
            "Selectors match by class and element type. Inline styles override CSS rules.",
        ),
        (
            "Layout",
            "Cards sit in a column. Buttons sit in a row. Text uses the page stylesheet.",
        ),
    ] {
        let card = dom::create_element("div");
        dom::class_list_add(card, "card");
        dom::append_child(container, card);
        nodes += 1;
        nodes += push_text(card, "h2", "card-title", title);
        nodes += push_text(card, "p", "card-body", body_text);
    }
    let row = dom::create_element("div");
    dom::set_style_property(row, "display", "flex");
    dom::set_style_property(row, "gap", "12px");
    dom::append_child(container, row);
    nodes += 1;
    nodes += push_text(row, "button", "btn-primary", "Get Started");
    nodes += push_text(row, "button", "btn-secondary", "Learn More");
    nodes += push_text(container, "p", "footer", "Built with W3C OS");
    nodes
}

fn push_text(parent: u32, tag: &str, class_name: &str, text: &str) -> usize {
    let element = dom::create_element(tag);
    dom::class_list_add(element, class_name);
    let node = dom::create_text_node(text);
    dom::append_child(element, node);
    dom::append_child(parent, element);
    2
}

struct StyleIncremental {
    style: String,
    width: String,
    background: String,
    text: String,
    class_toggle: String,
    insert: String,
    remove: String,
}

struct TimedFlush {
    us: u128,
    kind: &'static str,
    solved: u32,
    shifted: u32,
}

fn measure_style_and_incremental(samples: usize) -> StyleIncremental {
    jsdom::set_viewport(f64::from(VIEW_W), f64::from(VIEW_H));
    // One tiny frame loads the UI font before the timed samples.
    warm_font();
    let mut style_samples = Vec::with_capacity(samples);
    let mut layout_samples = Vec::with_capacity(samples);
    let mut total_samples = Vec::with_capacity(samples);
    let mut nodes = 0usize;
    let mut layout_checksum = 0u64;
    for _ in 0..samples {
        dom::reset_document();
        stylesheet::clear_rules();
        register_large_rules();
        let built = build_large_dom();
        nodes = built.nodes;
        let style_started = Instant::now();
        let tree = dom::to_component_tree();
        let style_us = style_started.elapsed().as_micros();
        let layout_started = Instant::now();
        let laid_out = layout::compute(&tree, VIEW_W, VIEW_H).expect("large layout");
        let layout_us = layout_started.elapsed().as_micros();
        layout_checksum = rect_checksum(&laid_out);
        if laid_out.len() < 1000 {
            eprintln!("large layout produced only {} boxes", laid_out.len());
            std::process::exit(1);
        }
        style_samples.push(style_us);
        layout_samples.push(layout_us);
        total_samples.push(style_us + layout_us);
    }
    // The last sample's document is still alive. Warm the retained layout,
    // then time one-node edits against it.
    if !incremental_layout::flush() {
        eprintln!("incremental warmup flush failed");
        std::process::exit(1);
    }
    let target = dom::with_document(|document| {
        document
            .get_element_by_id("cell-0")
            .expect("cell-0")
            .id
            .as_u32()
    });
    let sibling = dom::with_document(|document| {
        document
            .get_element_by_id("cell-1")
            .expect("cell-1")
            .id
            .as_u32()
    });
    let before_width = node_width(target);
    let mut width_samples = Vec::with_capacity(samples);
    for index in 0..samples {
        let px = 160 + index * 17;
        dom::set_style_property(target, "width", &format!("{px}px"));
        let sample = time_flush("width");
        if subtree_gate() {
            assert_subtree("width", &sample, nodes);
            if sample.kind != "style-subtree" {
                eprintln!("width flush kind {}", sample.kind);
                std::process::exit(1);
            }
        }
        width_samples.push(sample);
    }
    let after_width = node_width(target);
    if (after_width - before_width).abs() < 1.0 && after_width < 100.0 {
        eprintln!("width edit did not change layout: before {before_width} after {after_width}");
        std::process::exit(1);
    }
    let sibling_before = node_rect(sibling);
    let mut background_samples = Vec::with_capacity(samples);
    for index in 0..samples {
        let color = format!("#{:02x}2040", 20 + index * 40);
        dom::set_style_property(target, "background", &color);
        background_samples.push(time_flush("background"));
    }
    let sibling_after = node_rect(sibling);
    let sibling_moved = rects_differ(sibling_before, sibling_after);
    let background = dom::with_document(|document| {
        document
            .computed_style_for(NodeId::from_u32(target))
            .background
    });
    if background.a == 0 {
        eprintln!("background edit did not produce a painted color: {background:?}");
        std::process::exit(1);
    }

    // cell-40 is section 2, an even column. Text and child-list edits stay
    // inside that section. cell-80 is section 4, also a column, so the class
    // toggle does not land on the width target.
    let text_cell = element_by_id("cell-40");
    let text_node = first_child(text_cell);
    let before_text_height = node_height(text_cell);
    let mut text_samples = Vec::with_capacity(samples);
    for index in 0..samples {
        let words = "alpha beta gamma delta epsilon zeta eta theta ".repeat(8 + index * 4);
        dom::set_text_content(text_node, &words);
        let sample = time_flush("text");
        if subtree_gate() {
            assert_subtree("text", &sample, nodes);
            if sample.kind != "splice" {
                eprintln!("text flush kind {}", sample.kind);
                std::process::exit(1);
            }
        }
        text_samples.push(sample);
    }
    let after_text_height = node_height(text_cell);
    if (after_text_height - before_text_height).abs() < 1.0 {
        eprintln!(
            "text edit did not change height: before {before_text_height} after {after_text_height}"
        );
        std::process::exit(1);
    }

    let class_cell = element_by_id("cell-80");
    let before_class_width = node_width(class_cell);
    let mut class_samples = Vec::with_capacity(samples);
    let mut class_width_changed = false;
    let mut previous_class_width = before_class_width;
    for index in 0..samples {
        if index % 2 == 0 {
            dom::class_list_add(class_cell, "wide");
        } else {
            dom::class_list_remove(class_cell, "wide");
        }
        let sample = time_flush("class");
        if subtree_gate() {
            assert_subtree("class", &sample, nodes);
            if sample.kind != "style-subtree" {
                eprintln!("class flush kind {}", sample.kind);
                std::process::exit(1);
            }
        }
        let width = node_width(class_cell);
        if (width - previous_class_width).abs() >= 1.0 {
            class_width_changed = true;
        }
        previous_class_width = width;
        class_samples.push(sample);
    }
    let after_class_width = node_width(class_cell);
    if !class_width_changed {
        eprintln!(
            "class toggle did not change width: before {before_class_width} after {after_class_width}"
        );
        std::process::exit(1);
    }

    let column = dom::parent_node(text_cell).expect("column parent");
    let before_children = dom::children(column).len();
    let mut insert_samples = Vec::with_capacity(samples);
    let mut remove_samples = Vec::with_capacity(samples);
    for index in 0..samples {
        let cell = dom::create_element("div");
        dom::class_list_add(cell, "cell");
        let text = dom::create_text_node(&format!("Inserted {index} alpha beta gamma delta"));
        dom::append_child(cell, text);
        dom::append_child(column, cell);
        let sample = time_flush("insert");
        if subtree_gate() {
            assert_subtree("insert", &sample, nodes);
            if sample.kind != "splice" {
                eprintln!("insert flush kind {}", sample.kind);
                std::process::exit(1);
            }
        }
        if dom::children(column).len() != before_children + 1 {
            eprintln!("insert did not add a child");
            std::process::exit(1);
        }
        insert_samples.push(sample);
        dom::remove_child(column, cell);
        if !incremental_layout::flush() {
            eprintln!("insert reset flush failed");
            std::process::exit(1);
        }
    }
    for index in 0..samples {
        let cell = dom::create_element("div");
        dom::class_list_add(cell, "cell");
        let text = dom::create_text_node(&format!("Removed {index} alpha beta gamma delta"));
        dom::append_child(cell, text);
        dom::append_child(column, cell);
        if !incremental_layout::flush() {
            eprintln!("remove setup flush failed");
            std::process::exit(1);
        }
        dom::remove_child(column, cell);
        let sample = time_flush("remove");
        if subtree_gate() {
            assert_subtree("remove", &sample, nodes);
            if sample.kind != "splice" {
                eprintln!("remove flush kind {}", sample.kind);
                std::process::exit(1);
            }
        }
        if dom::children(column).len() != before_children {
            eprintln!("remove did not restore the child count");
            std::process::exit(1);
        }
        remove_samples.push(sample);
    }

    let style = format!(
        "{{\"nodes\": {nodes}, \"median_us\": {}, \"style_median_us\": {}, \"layout_median_us\": {}, \"samples_us\": {}, \"style_samples_us\": {}, \"layout_samples_us\": {}, \"layout_checksum\": {layout_checksum}}}",
        median(&total_samples),
        median(&style_samples),
        median(&layout_samples),
        json_us(&total_samples),
        json_us(&style_samples),
        json_us(&layout_samples),
    );
    let width = format!(
        "{}, \"before_width\": {before_width}, \"after_width\": {after_width}}}",
        mutation_json(&width_samples),
    );
    let background_us: Vec<u128> = background_samples.iter().map(|sample| sample.us).collect();
    let background_kind = background_samples
        .last()
        .map(|sample| sample.kind)
        .unwrap_or("none");
    let background = format!(
        "{{\"median_us\": {}, \"samples_us\": {}, \"kind\": \"{background_kind}\", \"sibling_moved\": {sibling_moved}}}",
        median(&background_us),
        json_us(&background_us),
    );
    StyleIncremental {
        style,
        width,
        background,
        text: format!(
            "{}, \"before_height\": {before_text_height}, \"after_height\": {after_text_height}}}",
            mutation_json(&text_samples),
        ),
        class_toggle: format!(
            "{}, \"before_width\": {before_class_width}, \"after_width\": {after_class_width}}}",
            mutation_json(&class_samples),
        ),
        insert: format!(
            "{}, \"children_before\": {before_children}, \"children_after\": {}}}",
            mutation_json(&insert_samples),
            before_children + 1,
        ),
        remove: format!(
            "{}, \"children_before\": {}, \"children_after\": {before_children}}}",
            mutation_json(&remove_samples),
            before_children + 1,
        ),
    }
}

fn subtree_gate() -> bool {
    !matches!(
        std::env::var("W3COS_SUBTREE_RELAYOUT").ok().as_deref(),
        Some("0") | Some("false")
    )
}

fn time_flush(label: &str) -> TimedFlush {
    let started = Instant::now();
    let ok = incremental_layout::flush();
    let us = started.elapsed().as_micros();
    if !ok {
        eprintln!("{label} flush failed");
        std::process::exit(1);
    }
    let stats = incremental_layout::last_flush_stats();
    TimedFlush {
        us,
        kind: stats.kind,
        solved: stats.solved_nodes,
        shifted: stats.shifted_nodes,
    }
}

/// A one-node edit that lays out the whole document is the old path.
fn assert_subtree(label: &str, sample: &TimedFlush, nodes: usize) {
    if sample.kind == "retained" || sample.kind == "rebuild" || sample.solved as usize * 4 >= nodes
    {
        eprintln!(
            "{label} solved the whole tree: kind {} solved {} shifted {} nodes {nodes}",
            sample.kind, sample.solved, sample.shifted
        );
        std::process::exit(1);
    }
}

fn mutation_json(samples: &[TimedFlush]) -> String {
    let us: Vec<u128> = samples.iter().map(|sample| sample.us).collect();
    let last = samples.last().expect("samples");
    format!(
        "{{\"median_us\": {}, \"samples_us\": {}, \"kind\": \"{}\", \"solved_nodes\": {}, \"shifted_nodes\": {}",
        median(&us),
        json_us(&us),
        last.kind,
        last.solved,
        last.shifted,
    )
}

fn element_by_id(id: &str) -> u32 {
    dom::with_document(|document| {
        document
            .get_element_by_id(id)
            .unwrap_or_else(|| panic!("{id}"))
            .id
            .as_u32()
    })
}

fn first_child(id: u32) -> u32 {
    dom::with_document(|document| {
        document
            .get_node(NodeId::from_u32(id))
            .first_child
            .expect("child")
            .as_u32()
    })
}

fn node_height(id: u32) -> f32 {
    node_rect(id).height
}

fn register_large_rules() {
    stylesheet::register_rule("div", &[("font-size", "14px"), ("color", "#222222")]);
    stylesheet::register_rule(
        ".column",
        &[
            ("display", "flex"),
            ("flex-direction", "column"),
            ("width", "100%"),
        ],
    );
    stylesheet::register_rule(".grid", &[("display", "grid"), ("width", "100%")]);
    stylesheet::register_rule(".cell", &[("padding", "4px"), ("margin", "2px")]);
    stylesheet::register_rule(".cell.alt", &[("background", "#eef2ff")]);
    stylesheet::register_rule(".cell.wide", &[("width", "180px")]);
    stylesheet::register_rule(".grid > .cell", &[("min-height", "18px")]);
}

struct LargeDom {
    nodes: usize,
}

fn build_large_dom() -> LargeDom {
    let body = dom::body_id();
    let mut nodes = 1usize;
    let mut cell_index = 0usize;
    for section_index in 0..LARGE_SECTIONS {
        let section = dom::create_element("div");
        if section_index % 2 == 0 {
            dom::class_list_add(section, "column");
        } else {
            dom::class_list_add(section, "grid");
            dom::set_style_property(section, "grid-template-columns", "1fr 1fr 1fr 1fr");
        }
        dom::append_child(body, section);
        nodes += 1;
        for _ in 0..CELLS_PER_SECTION {
            let cell = dom::create_element("div");
            dom::class_list_add(cell, "cell");
            if cell_index % 2 == 0 {
                dom::class_list_add(cell, "alt");
            }
            dom::set_attribute(cell, "id", &format!("cell-{cell_index}"));
            let text = dom::create_text_node(&format!("Item {cell_index}"));
            dom::append_child(cell, text);
            dom::append_child(section, cell);
            nodes += 2;
            cell_index += 1;
        }
    }
    LargeDom { nodes }
}

fn measure_scroll_paint(samples: usize) -> String {
    dom::reset_document();
    stylesheet::clear_rules();
    jsdom::set_viewport(800.0, 600.0);
    stylesheet::register_rule(
        ".row",
        &[
            ("height", "24px"),
            ("font-size", "14px"),
            ("padding", "2px 8px"),
            ("color", "#202020"),
        ],
    );
    stylesheet::register_rule(".row.alt", &[("background", "#f4f4f8")]);
    let list = dom::create_element("div");
    dom::set_style_property(list, "height", "600px");
    dom::set_style_property(list, "overflow", "scroll");
    dom::set_style_property(list, "display", "flex");
    dom::set_style_property(list, "flex-direction", "column");
    dom::append_child(dom::body_id(), list);
    for index in 0..SCROLL_ROWS {
        let row = dom::create_element("div");
        dom::class_list_add(row, "row");
        if index % 2 == 0 {
            dom::class_list_add(row, "alt");
        }
        let text = dom::create_text_node(&format!(
            "Row {index}  The quick brown fox jumps over the lazy dog."
        ));
        dom::append_child(row, text);
        dom::append_child(list, row);
    }
    let first_started = Instant::now();
    let first = headless::render_document_rgba(800, 600).expect("first list frame");
    let first_us = first_started.elapsed().as_micros();
    let mut checksum = pixel_checksum(&first.rgba);
    if checksum == 0 {
        eprintln!("scroll list raster is empty");
        std::process::exit(1);
    }
    let mut scroll_samples = Vec::with_capacity(samples);
    for index in 0..samples {
        dom::set_scroll_offset(list, None, Some(24.0 * (index as f32 + 1.0)));
        let started = Instant::now();
        let frame = headless::render_document_rgba(800, 600).expect("scrolled frame");
        scroll_samples.push(started.elapsed().as_micros());
        checksum = checksum.wrapping_add(pixel_checksum(&frame.rgba));
    }
    format!(
        "{{\"rows\": {SCROLL_ROWS}, \"first_us\": {first_us}, \"scroll_median_us\": {}, \"scroll_samples_us\": {}, \"checksum\": {checksum}}}",
        median(&scroll_samples),
        json_us(&scroll_samples),
    )
}

fn measure_host_calls(samples: usize) -> String {
    dom::reset_document();
    stylesheet::clear_rules();
    let document = jsdom::document_value();
    let parent = document.call_method("createElement", vec![Value::string("div")]);
    let body = document
        .get_property("body")
        .call_method("appendChild", vec![parent.clone()]);
    std::hint::black_box(&body);
    let tag = Value::string("span");
    let id_name = Value::string("id");
    let id_value = Value::string("row");
    let text = Value::string("item");
    for _ in 0..32 {
        host_round(&document, &parent, &tag, &id_name, &id_value, &text);
    }
    let mut samples_ns = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        for _ in 0..HOST_ITERS {
            host_round(&document, &parent, &tag, &id_name, &id_value, &text);
        }
        let elapsed = started.elapsed().as_nanos();
        samples_ns.push(elapsed / (HOST_ITERS as u128 * 5));
    }
    let parent_id = jsdom::node_id_of(&parent).expect("parent id");
    let children =
        dom::with_document(|document| document.children_ids(NodeId::from_u32(parent_id)).len());
    let expected = 32 + samples * HOST_ITERS;
    if children != expected {
        eprintln!("host calls appended {children} nodes, expected {expected}");
        std::process::exit(1);
    }
    format!(
        "{{\"iterations_per_sample\": {HOST_ITERS}, \"calls_per_iteration\": 5, \"median_ns_per_call\": {}, \"samples_ns_per_call\": {}}}",
        median(&samples_ns),
        json_us(&samples_ns),
    )
}

fn host_round(
    document: &Value,
    parent: &Value,
    tag: &Value,
    id_name: &Value,
    id_value: &Value,
    text: &Value,
) {
    let element = document.call_method("createElement", vec![tag.clone()]);
    element.call_method("setAttribute", vec![id_name.clone(), id_value.clone()]);
    element.set_property("textContent", text.clone());
    let appended = parent.call_method("appendChild", vec![element.clone()]);
    let read = appended.call_method("getAttribute", vec![id_name.clone()]);
    std::hint::black_box(read);
}

fn warm_font() {
    dom::reset_document();
    let text = dom::create_text_node("Warm");
    dom::append_child(dom::body_id(), text);
    let tree = dom::to_component_tree();
    let _ = layout::compute(&tree, 64.0, 64.0);
}

fn node_width(id: u32) -> f32 {
    node_rect(id).width
}

fn node_rect(id: u32) -> LayoutRect {
    let rect = dom::with_document(|document| document.get_layout_rect(NodeId::from_u32(id)));
    LayoutRect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

fn rects_differ(left: LayoutRect, right: LayoutRect) -> bool {
    (left.x - right.x).abs() > 0.5
        || (left.y - right.y).abs() > 0.5
        || (left.width - right.width).abs() > 0.5
        || (left.height - right.height).abs() > 0.5
}

fn rect_checksum(layout: &[(LayoutRect, usize)]) -> u64 {
    let mut acc = layout.len() as u64;
    for (rect, index) in layout {
        acc = acc.wrapping_add(*index as u64);
        acc = acc.wrapping_add(rect.width.to_bits() as u64);
        acc = acc.wrapping_add(rect.height.to_bits() as u64);
    }
    std::hint::black_box(acc)
}

fn pixel_checksum(rgba: &[u8]) -> u64 {
    let mut acc = rgba.len() as u64;
    for (index, byte) in rgba.iter().enumerate().step_by(64) {
        acc = acc.wrapping_add(u64::from(*byte).wrapping_mul((index as u64 % 251) + 1));
    }
    std::hint::black_box(acc)
}

fn median(samples: &[u128]) -> u128 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn json_us(samples: &[u128]) -> String {
    let body = samples
        .iter()
        .map(|sample| sample.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{body}]")
}
