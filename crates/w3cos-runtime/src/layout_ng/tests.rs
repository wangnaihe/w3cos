//! S1 parity: every supported tree must lay out exactly like the legacy
//! engine (the verify-mode contract), plus direct CSS2 expectations.

use super::*;
use crate::layout::pre_flatten;
use w3cos_std::style::{BoxSizing, Dimension, Display, Edges, Spacing, Style};

fn block() -> Style {
    Style {
        display: Display::Block,
        ..Style::default()
    }
}

fn node(style: Style, children: Vec<Component>) -> Component {
    Component::boxed(style, children)
}

fn px(v: f32) -> Spacing {
    Spacing::Px(v)
}

fn margins(t: f32, r: f32, b: f32, l: f32) -> Edges {
    Edges {
        top: px(t),
        right: px(r),
        bottom: px(b),
        left: px(l),
    }
}

fn sized(h: f32) -> Style {
    Style {
        height: Dimension::Px(h),
        ..block()
    }
}

/// Run both engines; return NG rects by flat index after asserting parity.
fn parity(root: &Component) -> Vec<(LayoutRect, usize)> {
    let flat = pre_flatten(root);
    assert!(block::supports_tree(&flat), "tree outside S1");
    let legacy = crate::layout::compute(root, 800.0, 600.0).expect("legacy layout");
    let ng = layout_tree(root, &flat, 800.0, 600.0).expect("ng layout");
    let diffs = verify(&flat, &legacy, &ng);
    assert_eq!(diffs, 0, "legacy {legacy:#?}\nng {ng:#?}");
    let mut ng = ng;
    ng.sort_by_key(|(_, i)| *i);
    ng
}

fn rect(out: &[(LayoutRect, usize)], index: usize) -> (f32, f32, f32, f32) {
    let r = out.iter().find(|(_, i)| *i == index).expect("rect").0;
    (r.x, r.y, r.width, r.height)
}

#[test]
fn sibling_margins_collapse_to_the_larger() {
    let a = Style {
        margin: margins(10.0, 0.0, 30.0, 0.0),
        ..sized(50.0)
    };
    let b = Style {
        margin: margins(20.0, 0.0, 0.0, 0.0),
        ..sized(40.0)
    };
    let root = node(
        block(),
        vec![node(block(), vec![node(a, vec![]), node(b, vec![])])],
    );
    let out = parity(&root);
    assert_eq!(rect(&out, 2).1, 10.0);
    assert_eq!(rect(&out, 3).1, 10.0 + 50.0 + 30.0);
}

#[test]
fn first_child_margin_collapses_through_parent_unless_bordered() {
    let child = Style {
        margin: margins(25.0, 0.0, 0.0, 0.0),
        ..sized(10.0)
    };
    let open = node(block(), vec![node(child.clone(), vec![])]);
    let bordered = node(
        Style {
            border_width: 1.0,
            ..block()
        },
        vec![node(child, vec![])],
    );
    let root = node(block(), vec![open, bordered]);
    parity(&root);
}

#[test]
fn empty_block_collapses_through_between_siblings() {
    let empty = Style {
        margin: margins(15.0, 0.0, 35.0, 0.0),
        ..block()
    };
    let root = node(
        block(),
        vec![
            node(sized(20.0), vec![]),
            node(empty, vec![]),
            node(sized(20.0), vec![]),
        ],
    );
    parity(&root);
}

#[test]
fn negative_margins_sum_with_positive() {
    let a = Style {
        margin: margins(0.0, 0.0, 30.0, 0.0),
        ..sized(20.0)
    };
    let b = Style {
        margin: margins(-10.0, 0.0, 0.0, 0.0),
        ..sized(20.0)
    };
    let root = node(block(), vec![node(a, vec![]), node(b, vec![])]);
    parity(&root);
}

#[test]
fn widths_auto_fixed_percent_border_box_and_auto_margins() {
    let auto_padded = Style {
        padding: margins(0.0, 7.0, 0.0, 9.0),
        border_width: 2.0,
        ..sized(10.0)
    };
    let centered = Style {
        width: Dimension::Px(300.0),
        margin: Edges {
            left: Spacing::Auto,
            right: Spacing::Auto,
            ..margins(0.0, 0.0, 0.0, 0.0)
        },
        ..sized(10.0)
    };
    let percent = Style {
        width: Dimension::Percent(25.0),
        ..sized(10.0)
    };
    let border_box = Style {
        width: Dimension::Px(200.0),
        padding: margins(5.0, 5.0, 5.0, 5.0),
        box_sizing: BoxSizing::BorderBox,
        ..sized(50.0)
    };
    let capped = Style {
        max_width: Dimension::Px(120.0),
        ..sized(10.0)
    };
    let root = node(
        block(),
        vec![
            node(auto_padded, vec![]),
            node(centered, vec![]),
            node(percent, vec![]),
            node(border_box, vec![]),
            node(capped, vec![]),
        ],
    );
    let out = parity(&root);
    // First sibling: 10px content + 2px border top and bottom.
    assert_eq!(rect(&out, 2), (250.0, 14.0, 300.0, 10.0));
}

#[test]
fn percent_height_resolves_against_definite_parent() {
    let parent = Style {
        height: Dimension::Px(200.0),
        ..block()
    };
    let child = Style {
        height: Dimension::Percent(50.0),
        ..block()
    };
    let auto_parent_child = Style {
        height: Dimension::Percent(50.0),
        ..block()
    };
    let root = node(
        block(),
        vec![
            node(parent, vec![node(child, vec![])]),
            node(
                block(),
                vec![node(auto_parent_child, vec![node(sized(30.0), vec![])])],
            ),
        ],
    );
    parity(&root);
}

#[test]
fn unsupported_trees_fall_back() {
    let flex_root = node(Style::default(), vec![]);
    let flat = pre_flatten(&flex_root);
    assert!(layout_tree(&flex_root, &flat, 800.0, 600.0).is_none());
}

#[test]
fn self_collapsing_siblings_before_first_content_escape_with_it() {
    // Chrome: the min-height box sits at its parent's top; all leading
    // margins (12, -8, -8, -8, 5) collapse through the parent.
    let m = |t, b| Style {
        margin: margins(t, 0.0, b, 0.0),
        ..block()
    };
    let tall = Style {
        min_height: Dimension::Px(15.0),
        ..m(-8.0, 5.0)
    };
    let inner = Style {
        min_height: Dimension::Px(15.0),
        ..m(5.0, 30.0)
    };
    let root = node(
        block(),
        vec![node(
            m(12.0, 0.0),
            vec![
                node(m(-8.0, 0.0), vec![]),
                node(
                    Style {
                        height: Dimension::Px(0.0),
                        ..m(-8.0, -8.0)
                    },
                    vec![],
                ),
                node(tall, vec![node(inner, vec![])]),
            ],
        )],
    );
    let flat = pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let mut ng = ng;
    ng.sort_by_key(|(_, i)| *i);
    assert_eq!(rect(&ng, 1).1, rect(&ng, 4).1, "{ng:#?}");
}

/// Small deterministic PRNG (xorshift64*), no external dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn pick(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn length(&mut self, choices: &[f32]) -> f32 {
        choices[self.pick(choices.len() as u64) as usize]
    }
}

fn random_style(rng: &mut Rng) -> Style {
    let mut style = block();
    let m = [0.0, 0.0, 5.0, 12.0, 30.0, -8.0];
    style.margin = margins(
        rng.length(&m),
        rng.length(&m),
        rng.length(&m),
        rng.length(&m),
    );
    if rng.pick(4) == 0 {
        let p = [0.0, 3.0, 10.0];
        style.padding = margins(
            rng.length(&p),
            rng.length(&p),
            rng.length(&p),
            rng.length(&p),
        );
    }
    if rng.pick(4) == 0 {
        style.border_width = rng.length(&[1.0, 4.0]);
    }
    style.height = match rng.pick(6) {
        0 => Dimension::Px(rng.length(&[0.0, 20.0, 45.0])),
        1 => Dimension::Percent(rng.length(&[10.0, 50.0])),
        _ => Dimension::Auto,
    };
    style.width = match rng.pick(5) {
        0 => Dimension::Px(rng.length(&[100.0, 333.0])),
        1 => Dimension::Percent(rng.length(&[30.0, 75.0])),
        _ => Dimension::Auto,
    };
    if rng.pick(6) == 0 {
        style.margin.left = Spacing::Auto;
        style.margin.right = Spacing::Auto;
    }
    if rng.pick(8) == 0 {
        style.min_height = Dimension::Px(15.0);
    }
    if rng.pick(8) == 0 {
        style.box_sizing = BoxSizing::BorderBox;
    }
    if rng.pick(10) == 0 {
        style.display = Display::FlowRoot;
    }
    style
}

fn random_tree(rng: &mut Rng, depth: u32) -> Component {
    let children = if depth == 0 { 0 } else { rng.pick(4) };
    let kids = (0..children).map(|_| random_tree(rng, depth - 1)).collect();
    node(random_style(rng), kids)
}

/// Serialize one generated case for the Chrome oracle
/// (tools/layout-ng/chrome_oracle.py): HTML with one div per flat node
/// (the root is a flow-root, like the root element's BFC) plus both rect
/// sets keyed by flat index.
fn dump_case(
    path: &std::ffi::OsStr,
    case: usize,
    flat: &[crate::layout::FlatNodeInfo],
    legacy: &[(LayoutRect, usize)],
    ng: &[(LayoutRect, usize)],
) {
    use std::io::Write;
    fn css_dim(d: Dimension) -> String {
        match d {
            Dimension::Px(v) => format!("{v}px"),
            Dimension::Percent(v) => format!("{v}%"),
            _ => "auto".into(),
        }
    }
    fn css_spacing(s: Spacing) -> String {
        match s {
            Spacing::Px(v) => format!("{v}px"),
            Spacing::Percent(v) => format!("{v}%"),
            _ => "auto".into(),
        }
    }
    fn open(flat: &[crate::layout::FlatNodeInfo], i: usize, html: &mut String) {
        let s = flat[i].style;
        let display = if i == 0 || s.display == Display::FlowRoot {
            "flow-root"
        } else {
            "block"
        };
        html.push_str(&format!(
            "<div id=n{i} style=\"display:{display};width:{};height:{};min-height:{};max-height:{};min-width:{};max-width:{};margin:{} {} {} {};padding:{} {} {} {};border:{}px solid black;box-sizing:{}\">",
            css_dim(s.width),
            css_dim(s.height),
            if s.min_height == Dimension::Auto { "auto".into() } else { css_dim(s.min_height) },
            if s.max_height == Dimension::Auto { "none".into() } else { css_dim(s.max_height) },
            if s.min_width == Dimension::Auto { "auto".into() } else { css_dim(s.min_width) },
            if s.max_width == Dimension::Auto { "none".into() } else { css_dim(s.max_width) },
            css_spacing(s.margin.top),
            css_spacing(s.margin.right),
            css_spacing(s.margin.bottom),
            css_spacing(s.margin.left),
            css_spacing(s.padding.top),
            css_spacing(s.padding.right),
            css_spacing(s.padding.bottom),
            css_spacing(s.padding.left),
            s.border_width,
            if s.box_sizing == BoxSizing::BorderBox { "border-box" } else { "content-box" },
        ));
        let mut child = i + 1;
        while child < flat[i].subtree_end {
            open(flat, child, html);
            child = flat[child].subtree_end.max(child + 1);
        }
        html.push_str("</div>");
    }
    let mut html = String::new();
    open(flat, 0, &mut html);
    let rects = |set: &[(LayoutRect, usize)]| {
        let mut v: Vec<_> = set
            .iter()
            .map(|(r, i)| (*i, [r.x, r.y, r.width, r.height]))
            .collect();
        v.sort_by_key(|(i, _)| *i);
        v.into_iter()
            .map(|(i, r)| format!("\"{i}\":{r:?}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let line = format!(
        "{{\"case\":{case},\"html\":{:?},\"legacy\":{{{}}},\"ng\":{{{}}}}}\n",
        html,
        rects(legacy),
        rects(ng)
    );
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("dump");
    file.write_all(line.as_bytes()).expect("dump write");
}

/// Generated block trees: NG must produce exactly the legacy engine's box
/// set.  Geometry is judged against Chrome, not legacy (legacy disagrees
/// with Chrome on many of these trees): dump with W3COS_NG_FUZZ_DUMP and run
/// tools/layout-ng/chrome_oracle.py.
#[test]
fn generated_block_trees_cover_the_legacy_box_set() {
    let mut rng = Rng(0x5eed_1a70_75ce_0001);
    let mut geometry_diffs = 0;
    for case in 0..400 {
        let root = node(block(), vec![random_tree(&mut rng, 3)]);
        let flat = pre_flatten(&root);
        let legacy = crate::layout::compute(&root, 800.0, 600.0).expect("legacy");
        let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
        let mut legacy_ids: Vec<usize> = legacy.iter().map(|(_, i)| *i).collect();
        let mut ng_ids: Vec<usize> = ng.iter().map(|(_, i)| *i).collect();
        legacy_ids.sort_unstable();
        ng_ids.sort_unstable();
        assert_eq!(legacy_ids, ng_ids, "case {case}: box sets differ");
        assert!(
            ng.iter().all(|(r, _)| r.width >= 0.0 && r.height >= 0.0),
            "case {case}"
        );
        geometry_diffs += usize::from(verify(&flat, &legacy, &ng) != 0);
        if let Some(path) = std::env::var_os("W3COS_NG_FUZZ_DUMP") {
            dump_case(&path, case, &flat, &legacy, &ng);
        }
    }
    eprintln!("[layout-ng] generated trees differing from legacy: {geometry_diffs}/400");
}

fn dump(flat: &[crate::layout::FlatNodeInfo]) {
    for (i, info) in flat.iter().enumerate() {
        let s = info.style;
        let depth = std::iter::successors(info.parent, |p| flat[*p].parent).count();
        eprintln!(
            "{:indent$}#{i} {:?} w={:?} h={:?} minh={:?} m=({:?},{:?},{:?},{:?}) p=({:?},{:?},{:?},{:?}) b={} {:?}",
            "",
            s.display,
            s.width,
            s.height,
            s.min_height,
            s.margin.top,
            s.margin.right,
            s.margin.bottom,
            s.margin.left,
            s.padding.top,
            s.padding.right,
            s.padding.bottom,
            s.padding.left,
            s.border_width,
            s.box_sizing,
            indent = depth * 2
        );
    }
}

/// Checked-in Chrome expectations for the generated corpus
/// (testdata/generated-blocks-chrome.json, written by
/// tools/layout-ng/chrome_oracle.py --write-golden; rects in 1/64 px).
#[test]
fn generated_block_trees_match_chrome() {
    let golden: std::collections::HashMap<String, std::collections::HashMap<String, [i64; 4]>> =
        serde_json::from_str(include_str!("testdata/generated-blocks-chrome.json"))
            .expect("golden json");
    let mut rng = Rng(0x5eed_1a70_75ce_0001);
    let mut failures = Vec::new();
    for case in 0..400usize {
        let root = node(block(), vec![random_tree(&mut rng, 3)]);
        let flat = pre_flatten(&root);
        let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
        let expected = &golden[&case.to_string()];
        for (r, index) in &ng {
            let got = [r.x, r.y, r.width, r.height].map(|v| (v * 64.0).round() as i64);
            if expected.get(&index.to_string()) != Some(&got) {
                failures.push((case, *index, got, expected.get(&index.to_string()).copied()));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} boxes differ from Chrome, first: {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

/// Exploration aid: `W3COS_NG_PROBE=file.html cargo test probe_html -- --nocapture`
/// prints the component tree and legacy rects of a document.
#[cfg(feature = "dynamic-js")]
#[test]
fn probe_html() {
    let Some(path) = std::env::var_os("W3COS_NG_PROBE") else {
        return;
    };
    let html = std::fs::read_to_string(path).expect("probe html");
    let root = crate::browser_page_domain::load_html_snapshot(&html, "file:///probe.html")
        .expect("snapshot");
    let flat = pre_flatten(&root);
    let legacy = crate::layout::compute(&root, 800.0, 600.0).expect("legacy layout");
    let reasons = block::unsupported_reasons(&flat);
    println!("reasons: {reasons:?}");
    let ng = if reasons.is_empty() {
        layout_tree(&root, &flat, 800.0, 600.0)
    } else {
        None
    };
    if let Some(ng) = &ng {
        println!("verify diffs: {}", verify(&flat, &legacy, ng));
    }
    let mut depth = vec![0usize; flat.len()];
    for (i, n) in flat.iter().enumerate() {
        if let Some(p) = n.parent {
            depth[i] = depth[p] + 1;
        }
        let r = legacy.iter().find(|(_, j)| *j == i).map(|(r, _)| *r);
        let kind = match n.kind {
            w3cos_std::ComponentKind::Text { content } => format!("Text {content:?}"),
            other => format!("{other:?}").chars().take(30).collect(),
        };
        println!(
            "{:3} {}{} disp={:?} ws={:?} lh={:?}/{}/{:?} fs={} ta={:?} h={:?} minh={:?} -> {:?} ng {:?} {}",
            i,
            "  ".repeat(depth[i]),
            kind,
            n.style.display,
            n.style.white_space,
            n.style.line_height,
            n.style.line_height_is_normal,
            n.style.line_height_computed_px,
            n.style.font_size,
            n.style.text_align,
            n.style.height,
            n.style.min_height,
            r.map(|r| (r.x, r.y, r.width, r.height)),
            ng.as_ref()
                .and_then(|ng| ng.iter().find(|(_, j)| *j == i))
                .map(|(r, _)| (r.x, r.y, r.width, r.height)),
            n.style
                .custom_properties
                .iter()
                .flat_map(|p| p.keys())
                .filter(|k| k.starts_with("--w3cos-internal"))
                .map(|k| k.trim_start_matches("--w3cos-internal-"))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
}
