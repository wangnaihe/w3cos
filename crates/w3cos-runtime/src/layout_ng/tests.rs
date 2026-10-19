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
        if std::env::var_os("W3COS_NG_PROBE_EDGES").is_some() {
            let pad = n.style.padding_lengths();
            let mar = n.style.margin_lengths();
            println!(
                "    edges #{i}: p=[{} {} {} {}] m=[{} {} {} {}] b={}/{:?},{:?},{:?},{:?}",
                pad.top,
                pad.right,
                pad.bottom,
                pad.left,
                mar.top,
                mar.right,
                mar.bottom,
                mar.left,
                n.style.border_width,
                n.style.border_top_width,
                n.style.border_right_width,
                n.style.border_bottom_width,
                n.style.border_left_width
            );
        }
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

/// CSS2 max-height-100: `height: 100px; max-height: 0` has a zero used
/// block size, so (Blink FinishLayout) it collapses through and the
/// preceding sibling's bottom margin escapes the parent, as in Chrome.
#[test]
fn zero_used_height_collapses_through() {
    use w3cos_std::style::{Dimension, Display, Edges, Spacing, Style};
    let block = |height: f32, max: Option<f32>, bottom: f32| {
        let mut style = Style {
            display: Display::Block,
            height: Dimension::Px(height),
            ..Style::default()
        };
        if let Some(max) = max {
            style.max_height = Dimension::Px(max);
        }
        style.margin = Edges {
            top: Spacing::Px(0.0),
            right: Spacing::Px(0.0),
            bottom: Spacing::Px(bottom),
            left: Spacing::Px(0.0),
        };
        Component::boxed(style, Vec::new())
    };
    let body = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![block(18.0, None, 16.0), block(100.0, Some(0.0), 0.0)],
    );
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![body],
    );
    let flat = crate::layout::pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let rect = |i: usize| ng.iter().find(|(_, n)| *n == i).unwrap().0;
    assert_eq!(
        rect(1).height,
        18.0,
        "body ends at the first child's border edge"
    );
    assert_eq!(rect(3).height, 0.0);
}

/// Blink ResolveInlineAutoMargins: `margin-left: auto` on an overflowing
/// block (width 50 + margin-right 75 > 100) resolves to 0, not -25.
#[test]
fn auto_start_margin_does_not_go_negative() {
    let mut test = Style {
        display: Display::Block,
        width: Dimension::Px(50.0),
        height: Dimension::Px(5.0),
        ..Style::default()
    };
    test.margin = Edges {
        top: Spacing::Px(0.0),
        right: Spacing::Px(75.0),
        bottom: Spacing::Px(0.0),
        left: Spacing::Auto,
    };
    let wrapper = Style {
        display: Display::Block,
        width: Dimension::Px(100.0),
        ..Style::default()
    };
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![Component::boxed(
            wrapper,
            vec![Component::boxed(test, Vec::new())],
        )],
    );
    let flat = pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let rect = ng.iter().find(|(_, n)| *n == 2).unwrap().0;
    assert_eq!(rect.x, 0.0);
}

/// S3a positioning: relative offsets (CSS2 9.4.3), absolute boxes at their
/// static position (past pending margins, or the parent's top while its
/// margins collapse through), insets / auto margins / percentages against
/// the positioned ancestor's padding box (10.3.7, 10.6.4), shrink-to-fit,
/// the initial containing block and fixed boxes.  Rects from Chrome.
#[test]
fn positioned_boxes_match_chrome() {
    use w3cos_std::style::{Dimension as D, Display, Edges, Position, Spacing as S, Style};
    let px = D::Px;
    let block = |position: Position, w: D, h: D| Style {
        display: Display::Block,
        position,
        width: w,
        height: h,
        ..Style::default()
    };
    let leaf = |style: Style| Component::boxed(style, vec![]);
    let inset = |mut s: Style, l: D, t: D, r: D, b: D| {
        s.left = l;
        s.top = t;
        s.right = r;
        s.bottom = b;
        s
    };
    let margin = |mut s: Style, top: S, right: S, bottom: S, left: S| {
        s.margin = Edges {
            top,
            right,
            bottom,
            left,
        };
        s
    };
    let (rel, abs, st) = (Position::Relative, Position::Absolute, Position::Static);
    let auto = D::Auto;
    let z = S::Px(0.0);
    let mut cb = margin(block(rel, px(200.0), px(100.0)), S::Px(10.0), z, z, z);
    cb.border_width = 1.0;
    cb.padding = Edges {
        top: S::Px(5.0),
        right: S::Px(5.0),
        bottom: S::Px(5.0),
        left: S::Px(5.0),
    };
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            leaf(inset(
                block(rel, px(50.0), px(10.0)),
                px(10.0),
                px(5.0),
                auto,
                auto,
            )),
            leaf(inset(
                block(rel, px(50.0), px(10.0)),
                auto,
                auto,
                px(10.0),
                px(5.0),
            )),
            leaf(inset(
                block(rel, px(50.0), px(10.0)),
                D::Percent(10.0),
                D::Percent(50.0),
                auto,
                auto,
            )),
            Component::boxed(
                cb,
                vec![
                    leaf(margin(block(st, auto, px(10.0)), z, z, S::Px(20.0), z)),
                    leaf(block(abs, px(10.0), px(10.0))),
                    leaf(margin(block(st, auto, px(10.0)), S::Px(5.0), z, z, z)),
                    leaf(margin(
                        inset(block(abs, auto, auto), px(3.0), px(4.0), px(7.0), px(9.0)),
                        S::Px(1.0),
                        S::Px(1.0),
                        S::Px(1.0),
                        S::Px(1.0),
                    )),
                    leaf(margin(
                        inset(
                            block(abs, px(50.0), px(10.0)),
                            px(0.0),
                            px(0.0),
                            px(0.0),
                            px(0.0),
                        ),
                        S::Auto,
                        S::Auto,
                        S::Auto,
                        S::Auto,
                    )),
                    leaf(inset(
                        block(abs, px(30.0), px(10.0)),
                        auto,
                        auto,
                        px(10.0),
                        auto,
                    )),
                    leaf(inset(
                        block(abs, D::Percent(10.0), D::Percent(10.0)),
                        D::Percent(10.0),
                        D::Percent(10.0),
                        auto,
                        auto,
                    )),
                    Component::boxed(
                        inset(block(abs, auto, auto), auto, px(50.0), auto, auto),
                        vec![leaf(block(st, px(40.0), px(5.0)))],
                    ),
                    leaf(margin(
                        block(abs, px(5.0), px(5.0)),
                        S::Px(3.0),
                        z,
                        z,
                        S::Px(7.0),
                    )),
                    Component::boxed(
                        margin(block(st, auto, auto), S::Px(30.0), z, z, z),
                        vec![
                            leaf(block(abs, px(5.0), px(5.0))),
                            leaf(margin(block(st, auto, px(5.0)), S::Px(8.0), z, z, z)),
                        ],
                    ),
                ],
            ),
            leaf(inset(
                block(Position::Fixed, px(20.0), px(20.0)),
                auto,
                auto,
                px(0.0),
                px(0.0),
            )),
            leaf(block(st, auto, px(10.0))),
            leaf(block(abs, px(5.0), px(5.0))),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 20] = [
        (1, [10.0, 5.0, 50.0, 10.0]),
        (2, [-10.0, 5.0, 50.0, 10.0]),
        (3, [80.0, 20.0, 50.0, 10.0]),
        (4, [0.0, 40.0, 212.0, 112.0]),
        (5, [6.0, 46.0, 200.0, 10.0]),
        (6, [6.0, 76.0, 10.0, 10.0]),
        (7, [6.0, 76.0, 200.0, 10.0]),
        (8, [5.0, 46.0, 198.0, 95.0]),
        (9, [81.0, 91.0, 50.0, 10.0]),
        (10, [171.0, 86.0, 30.0, 10.0]),
        (11, [22.0, 52.0, 21.0, 11.0]),
        (12, [6.0, 91.0, 40.0, 5.0]),
        (13, [6.0, 91.0, 40.0, 5.0]),
        (14, [13.0, 89.0, 5.0, 5.0]),
        (15, [6.0, 116.0, 200.0, 5.0]),
        (16, [6.0, 116.0, 5.0, 5.0]),
        (17, [6.0, 116.0, 200.0, 5.0]),
        (18, [780.0, 580.0, 20.0, 20.0]),
        (19, [0.0, 152.0, 800.0, 10.0]),
        (20, [0.0, 162.0, 5.0, 5.0]),
    ];
    let mut bad = Vec::new();
    for (index, want) in expected {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        if [r.x, r.y, r.width, r.height] != want {
            bad.push(format!(
                "#{index}: {:?} want {want:?}",
                [r.x, r.y, r.width, r.height]
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// S3b floats and clearance (Chrome rects of the equivalent page):
/// pending margins stay below a float, a float that does not fit moves to
/// the next float bottom, clearance only when the hypothetical position is
/// above the clear offset, a nested float at its BFC offset, and the BFC
/// root's height includes floats.
#[test]
fn floats_and_clearance_match_chrome() {
    use w3cos_std::style::{Clear, Dimension as D, Display, Edges, Float, Spacing as S, Style};
    let px = D::Px;
    let z = S::Px(0.0);
    let block = |w: D, h: f32, float: Float, clear: Clear, mt: f32, mb: f32| Style {
        display: Display::Block,
        width: w,
        height: px(h),
        float,
        clear,
        margin: Edges {
            top: S::Px(mt),
            right: z,
            bottom: S::Px(mb),
            left: z,
        },
        ..Style::default()
    };
    let leaf = |style: Style| Component::boxed(style, vec![]);
    let (n, l, r) = (Float::None, Float::Left, Float::Right);
    let auto = D::Auto;
    let root = Component::boxed(
        Style {
            display: Display::Block,
            width: px(300.0),
            ..Style::default()
        },
        vec![
            leaf(block(auto, 10.0, n, Clear::None, 0.0, 20.0)),
            leaf(block(px(50.0), 30.0, l, Clear::None, 0.0, 0.0)),
            leaf(block(auto, 10.0, n, Clear::None, 50.0, 0.0)),
            leaf(block(px(50.0), 30.0, l, Clear::None, 0.0, 0.0)),
            leaf(block(px(100.0), 10.0, r, Clear::None, 0.0, 0.0)),
            leaf(block(px(220.0), 10.0, l, Clear::None, 0.0, 0.0)),
            leaf(block(auto, 10.0, n, Clear::Left, 5.0, 0.0)),
            Component::boxed(
                block(auto, 10.0, n, Clear::None, 5.0, 0.0),
                vec![leaf(block(px(10.0), 10.0, r, Clear::None, 3.0, 0.0))],
            ),
            leaf(block(auto, 5.0, n, Clear::Both, 40.0, 0.0)),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 11] = [
        (0, [0.0, 0.0, 300.0, 170.0]),
        (1, [0.0, 0.0, 300.0, 10.0]),
        (2, [0.0, 30.0, 50.0, 30.0]),
        (3, [0.0, 60.0, 300.0, 10.0]),
        (4, [0.0, 70.0, 50.0, 30.0]),
        (5, [200.0, 70.0, 100.0, 10.0]),
        (6, [50.0, 80.0, 220.0, 10.0]),
        (7, [0.0, 100.0, 300.0, 10.0]),
        (8, [0.0, 115.0, 300.0, 10.0]),
        (9, [290.0, 118.0, 10.0, 10.0]),
        (10, [0.0, 165.0, 300.0, 5.0]),
    ];
    let mut bad = Vec::new();
    for (index, want) in expected {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        if [r.x, r.y, r.width, r.height] != want {
            bad.push(format!(
                "#{index}: {:?} want {want:?}",
                [r.x, r.y, r.width, r.height]
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// CSS2 9.5.1 rule 3 bounds a float by opposite floats even outside its
/// containing block (floats-rule3-outside-left-001), and the hypothetical
/// position of a cleared box includes margins collapsing through its
/// parent (no-clearance-due-to-large-margin).
#[test]
fn float_rule3_and_collapsed_clearance_match_chrome() {
    use w3cos_std::style::{Clear, Dimension as D, Display, Edges, Float, Spacing as S, Style};
    let px = D::Px;
    let z = S::Px(0.0);
    let block = |w: D, h: D, float: Float| Style {
        display: Display::Block,
        width: w,
        height: h,
        float,
        ..Style::default()
    };
    let leaf = |style: Style| Component::boxed(style, vec![]);
    let check = |root: &Component, expected: &[(usize, [f32; 4])]| {
        let flat = pre_flatten(root);
        assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
        let ng = layout_tree(root, &flat, 800.0, 600.0).expect("ng");
        let mut bad = Vec::new();
        for &(index, want) in expected {
            let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
            if [r.x, r.y, r.width, r.height] != want {
                bad.push(format!(
                    "#{index}: {:?} want {want:?}",
                    [r.x, r.y, r.width, r.height]
                ));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    };
    let mut narrow = block(D::Auto, D::Auto, Float::None);
    narrow.margin = Edges {
        top: z,
        right: S::Px(100.0),
        bottom: z,
        left: z,
    };
    let rule3 = Component::boxed(
        block(px(500.0), px(500.0), Float::None),
        vec![
            leaf(block(px(50.0), px(300.0), Float::Right)),
            Component::boxed(narrow, vec![leaf(block(px(425.0), px(10.0), Float::Left))]),
        ],
    );
    check(
        &rule3,
        &[
            (1, [450.0, 0.0, 50.0, 300.0]),
            (2, [0.0, 0.0, 400.0, 0.0]),
            (3, [0.0, 0.0, 425.0, 10.0]),
        ],
    );
    let mut padded = block(D::Auto, D::Auto, Float::None);
    padded.padding.top = S::Px(1.0);
    let mut cleared = block(D::Auto, px(10.0), Float::None);
    cleared.clear = Clear::Left;
    cleared.margin.top = S::Px(150.0);
    let collapsed = Component::boxed(
        block(px(300.0), D::Auto, Float::None),
        vec![
            leaf(block(px(100.0), px(100.0), Float::Left)),
            Component::boxed(
                padded,
                vec![Component::boxed(
                    block(D::Auto, D::Auto, Float::None),
                    vec![leaf(cleared)],
                )],
            ),
        ],
    );
    check(
        &collapsed,
        &[
            (1, [0.0, 0.0, 100.0, 100.0]),
            (2, [0.0, 0.0, 300.0, 161.0]),
            (3, [0.0, 151.0, 300.0, 10.0]),
            (4, [0.0, 151.0, 300.0, 10.0]),
        ],
    );
}

/// The DOM's synthetic float rows (anonymous-float-group,
/// cleared-float-wrapper) are transparent: their floats are placed in the
/// parent's BFC, the row gets their bounding box.
#[test]
fn float_group_rows_are_transparent() {
    use w3cos_std::style::{Clear, Dimension as D, Display, Float, Style};
    let px = D::Px;
    let block = |w: D, h: f32, float: Float, clear: Clear| Style {
        display: Display::Block,
        width: w,
        height: px(h),
        float,
        clear,
        ..Style::default()
    };
    let leaf = |style: Style| Component::boxed(style, vec![]);
    let row = |marker: &str, float: Float, clear: Clear| {
        let mut style = Style {
            display: Display::Flex,
            width: if float == Float::None {
                D::Percent(100.0)
            } else {
                D::Auto
            },
            float,
            clear,
            ..Style::default()
        };
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(marker.into(), "1".into());
        style
    };
    let root = Component::boxed(
        Style {
            display: Display::Block,
            width: px(300.0),
            ..Style::default()
        },
        vec![
            leaf(block(D::Auto, 10.0, Float::None, Clear::None)),
            Component::row(
                row(block::FLOAT_GROUP_MARKERS[0], Float::None, Clear::None),
                vec![
                    leaf(block(px(50.0), 30.0, Float::Left, Clear::None)),
                    leaf(block(px(50.0), 30.0, Float::Left, Clear::None)),
                ],
            ),
            Component::row(
                row(block::FLOAT_GROUP_MARKERS[1], Float::Left, Clear::Left),
                vec![
                    leaf(block(px(20.0), 10.0, Float::Left, Clear::Left)),
                    leaf(block(px(20.0), 10.0, Float::Left, Clear::None)),
                ],
            ),
            leaf(block(D::Auto, 10.0, Float::None, Clear::None)),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 8] = [
        (2, [0.0, 10.0, 300.0, 30.0]),
        (3, [0.0, 10.0, 50.0, 30.0]),
        (4, [50.0, 10.0, 50.0, 30.0]),
        (5, [0.0, 40.0, 40.0, 10.0]),
        (6, [0.0, 40.0, 20.0, 10.0]),
        (7, [20.0, 40.0, 20.0, 10.0]),
        (8, [0.0, 10.0, 300.0, 10.0]),
        (0, [0.0, 0.0, 300.0, 50.0]),
    ];
    let mut bad = Vec::new();
    for (index, want) in expected {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        if [r.x, r.y, r.width, r.height] != want {
            bad.push(format!(
                "#{index}: {:?} want {want:?}",
                [r.x, r.y, r.width, r.height]
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// A float whose margin box is negative fits beside earlier floats even
/// when they already fill the band (negative-margin-float-positioning).
#[test]
fn negative_margin_float_fits_beside_floats() {
    use w3cos_std::style::{Dimension as D, Display, Edges, Float, Spacing as S, Style};
    let px = D::Px;
    let float = |ml: f32, mr: f32| Style {
        display: Display::Block,
        width: px(50.0),
        height: px(100.0),
        float: Float::Left,
        margin: Edges {
            top: S::Px(0.0),
            right: S::Px(mr),
            bottom: S::Px(0.0),
            left: S::Px(ml),
        },
        ..Style::default()
    };
    let root = Component::boxed(
        Style {
            display: Display::Block,
            width: px(100.0),
            ..Style::default()
        },
        vec![
            Component::boxed(float(50.0, 50.0), vec![]),
            Component::boxed(float(-150.0, 0.0), vec![]),
        ],
    );
    let flat = pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let rect = |index: usize| {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        [r.x, r.y, r.width, r.height]
    };
    assert_eq!(rect(1), [50.0, 0.0, 50.0, 100.0]);
    assert_eq!(rect(2), [0.0, 0.0, 50.0, 100.0]);
    assert_eq!(rect(0), [0.0, 0.0, 100.0, 100.0]);
}

/// S3c HandleNewFormattingContext: a BFC root's border box avoids floats
/// (its margins may overlap them), shrinks to the layout opportunity when
/// its width is auto, and moves down when a fixed width does not fit.
#[test]
fn bfc_roots_avoid_floats() {
    use w3cos_std::style::{Dimension as D, Display, Edges, Float, Spacing as S, Style};
    let px = D::Px;
    let z = S::Px(0.0);
    let boxed = |display: Display, float: Float, w: D, h: f32, margin: [f32; 4]| Style {
        display,
        float,
        width: w,
        height: px(h),
        margin: Edges {
            top: S::Px(margin[0]),
            right: S::Px(margin[1]),
            bottom: z,
            left: S::Px(margin[3]),
        },
        ..Style::default()
    };
    let leaf = |style: Style| Component::boxed(style, vec![]);
    let (fr, b) = (Display::FlowRoot, Display::Block);
    let mut a = boxed(fr, Float::None, D::Auto, 10.0, [0.0, 10.0, 0.0, 20.0]);
    a.border_width = 1.0;
    let root = Component::boxed(
        Style {
            display: Display::Block,
            width: px(200.0),
            ..Style::default()
        },
        vec![
            leaf(boxed(b, Float::Left, px(50.0), 30.0, [0.0; 4])),
            leaf(a),
            leaf(boxed(fr, Float::None, px(160.0), 10.0, [0.0; 4])),
            leaf(boxed(fr, Float::None, D::Auto, 10.0, [0.0, 0.0, 0.0, 60.0])),
            leaf(boxed(b, Float::Right, px(40.0), 20.0, [0.0; 4])),
            leaf(boxed(
                fr,
                Float::None,
                px(100.0),
                30.0,
                [5.0, 0.0, 0.0, 0.0],
            )),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 7] = [
        (0, [0.0, 0.0, 200.0, 85.0]),
        (1, [0.0, 0.0, 50.0, 30.0]),
        (2, [50.0, 0.0, 140.0, 12.0]),
        (3, [0.0, 30.0, 160.0, 10.0]),
        (4, [60.0, 40.0, 140.0, 10.0]),
        (5, [160.0, 50.0, 40.0, 20.0]),
        (6, [0.0, 55.0, 100.0, 30.0]),
    ];
    let mut bad = Vec::new();
    for (index, want) in expected {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        if [r.x, r.y, r.width, r.height] != want {
            bad.push(format!(
                "#{index}: {:?} want {want:?}",
                [r.x, r.y, r.width, r.height]
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// A BFC root that is its parent's first in-flow child moves below floats
/// inside the parent while the margins still collapse through it, and a
/// cleared first child whose large margin collapses through its parent is
/// estimated at the collapsed position (display-flow-root-002,
/// no-clearance-due-to-large-margin; Chrome rects).
#[test]
fn first_children_beside_floats_match_chrome() {
    use w3cos_std::style::{Clear, Dimension as D, Display, Float, Spacing as S, Style};
    let px = D::Px;
    let float = |float: Float| Style {
        display: Display::Block,
        float,
        width: px(250.0),
        height: px(100.0),
        ..Style::default()
    };
    let leaf = |style: Style| Component::boxed(style, vec![]);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![Component::boxed(
            Style {
                display: Display::Block,
                width: px(400.0),
                ..Style::default()
            },
            vec![
                leaf(float(Float::Right)),
                leaf(float(Float::Left)),
                leaf(Style {
                    display: Display::FlowRoot,
                    width: px(0.0),
                    height: px(200.0),
                    ..Style::default()
                }),
            ],
        )],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let rect = |index: usize| {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        [r.x, r.y, r.width, r.height]
    };
    assert_eq!(rect(1), [0.0, 0.0, 400.0, 300.0]);
    assert_eq!(rect(2), [150.0, 0.0, 250.0, 100.0]);
    assert_eq!(rect(3), [0.0, 100.0, 250.0, 100.0]);
    assert_eq!(rect(4), [250.0, 100.0, 0.0, 200.0]);

    let mut cleared = Style {
        display: Display::Block,
        height: px(18.0),
        clear: Clear::Left,
        ..Style::default()
    };
    cleared.margin.top = S::Px(150.0);
    let mut padded = Style {
        display: Display::Block,
        ..Style::default()
    };
    padded.padding.top = S::Px(1.0);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            leaf(Style {
                display: Display::Block,
                float: Float::Left,
                width: px(100.0),
                height: px(100.0),
                ..Style::default()
            }),
            Component::boxed(
                padded,
                vec![Component::boxed(
                    Style {
                        display: Display::Block,
                        ..Style::default()
                    },
                    vec![Component::boxed(cleared, vec![])],
                )],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let rect = |index: usize| {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        [r.x, r.y, r.width, r.height]
    };
    assert_eq!(rect(3), [0.0, 151.0, 800.0, 18.0]);
    assert_eq!(rect(4), [0.0, 151.0, 800.0, 18.0]);

    // adjoining-float-before-clearance: the float sits at the unresolved
    // position the cleared box's margin would collapse through; that needs
    // the BFC offset resolved before clearance, so it stays on legacy.
    let mut cleared = Style {
        display: Display::Block,
        height: px(50.0),
        clear: Clear::Left,
        ..Style::default()
    };
    cleared.margin.top = S::Px(400.0);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![Component::boxed(
            Style {
                display: Display::Block,
                width: px(100.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: Display::Block,
                        ..Style::default()
                    },
                    vec![leaf(Style {
                        display: Display::Block,
                        float: Float::Left,
                        width: px(100.0),
                        height: px(50.0),
                        ..Style::default()
                    })],
                ),
                Component::boxed(cleared, vec![]),
            ],
        )],
    );
    let flat = pre_flatten(&root);
    assert!(layout_tree(&root, &flat, 800.0, 600.0).is_none());

    // margin-collapse-039 shape: past clearance (ForcedBfcBlockOffset) the
    // first child's margin no longer collapses through the cleared box, so
    // a float inside it sits at the cleared border edge, below the float.
    let mut first = Style {
        display: Display::Block,
        height: px(10.0),
        ..Style::default()
    };
    first.margin.top = S::Px(50.0);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            leaf(Style {
                display: Display::Block,
                float: Float::Left,
                width: px(100.0),
                height: px(100.0),
                ..Style::default()
            }),
            Component::boxed(
                Style {
                    display: Display::Block,
                    clear: Clear::Left,
                    ..Style::default()
                },
                vec![Component::boxed(
                    first,
                    vec![leaf(Style {
                        display: Display::Block,
                        float: Float::Left,
                        width: px(30.0),
                        height: px(30.0),
                        ..Style::default()
                    })],
                )],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let rect = |index: usize| {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        [r.x, r.y, r.width, r.height]
    };
    assert_eq!(rect(2), [0.0, 100.0, 800.0, 10.0]);
    assert_eq!(rect(3), [0.0, 100.0, 800.0, 10.0]);
    assert_eq!(rect(4), [0.0, 100.0, 30.0, 30.0]);
}
