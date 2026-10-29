//! S1 parity: every supported tree must lay out exactly like the legacy
//! engine (the verify-mode contract), plus direct CSS2 expectations.

use super::*;
use crate::layout::pre_flatten;
use w3cos_std::style::{BoxSizing, Dimension, Display, Edges, FlexDirection, Spacing, Style};

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
    // An empty grid with no template is S5. `subgrid` stays on legacy.
    let grid_root = node(
        Style {
            display: Display::Grid,
            grid_template_columns: Some("subgrid".to_string()),
            ..Style::default()
        },
        vec![],
    );
    let flat = pre_flatten(&grid_root);
    assert!(layout_tree(&grid_root, &flat, 800.0, 600.0).is_none());
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

/// CSS 2.1 §9.7 blockifies a floated inline-block. A floated list-item
/// keeps a block principal box. Headless Chrome, body margin 0: the
/// 200px container is 100px tall; the 40×20 inline-block, block, and
/// two list-items stack at y=0/20/40/60; the shrink-to-fit inline-block
/// is 30×10 at y=80; the cleared block is 200×10 at y=90.
#[test]
fn floated_inline_block_and_list_item_match_chrome() {
    use w3cos_std::style::{Clear, Float};
    let float = |display: Display, w: f32, h: f32, clear: Clear| Style {
        display,
        width: Dimension::Px(w),
        height: Dimension::Px(h),
        float: Float::Left,
        clear,
        ..Style::default()
    };
    let root = node(
        Style {
            width: Dimension::Px(200.0),
            ..block()
        },
        vec![
            node(float(Display::InlineBlock, 40.0, 20.0, Clear::None), vec![]),
            node(float(Display::Block, 40.0, 20.0, Clear::Left), vec![]),
            node(float(Display::ListItem, 40.0, 20.0, Clear::Left), vec![]),
            node(float(Display::ListItem, 40.0, 20.0, Clear::Left), vec![]),
            node(
                Style {
                    display: Display::InlineBlock,
                    float: Float::Left,
                    clear: Clear::Left,
                    ..Style::default()
                },
                vec![node(
                    Style {
                        width: Dimension::Px(30.0),
                        height: Dimension::Px(10.0),
                        ..block()
                    },
                    vec![],
                )],
            ),
            node(
                Style {
                    height: Dimension::Px(10.0),
                    clear: Clear::Both,
                    ..block()
                },
                vec![],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert!(
        block::supports_tree(&flat),
        "{:?}",
        block::unsupported_reasons(&flat)
    );
    let out = layout_tree(&root, &flat, 800.0, 600.0).expect("float");
    assert_eq!(rect(&out, 0), (0.0, 0.0, 200.0, 100.0));
    assert_eq!(rect(&out, 1), (0.0, 0.0, 40.0, 20.0));
    assert_eq!(rect(&out, 2), (0.0, 20.0, 40.0, 20.0));
    assert_eq!(rect(&out, 3), (0.0, 40.0, 40.0, 20.0));
    assert_eq!(rect(&out, 4), (0.0, 60.0, 40.0, 20.0));
    assert_eq!(rect(&out, 5), (0.0, 80.0, 30.0, 10.0));
    assert_eq!(rect(&out, 6), (0.0, 80.0, 30.0, 10.0));
    assert_eq!(rect(&out, 7), (0.0, 90.0, 200.0, 10.0));

    let flex = node(
        block(),
        vec![node(
            Style {
                display: Display::Flex,
                float: Float::Left,
                width: Dimension::Px(40.0),
                height: Dimension::Px(20.0),
                ..Style::default()
            },
            vec![],
        )],
    );
    assert!(
        block::unsupported_reasons(&pre_flatten(&flex))
            .iter()
            .any(|reason| reason == "float:display")
    );
    let grid = node(
        block(),
        vec![node(
            Style {
                display: Display::Grid,
                float: Float::Left,
                width: Dimension::Px(40.0),
                height: Dimension::Px(20.0),
                ..Style::default()
            },
            vec![],
        )],
    );
    assert!(
        block::unsupported_reasons(&pre_flatten(&grid))
            .iter()
            .any(|reason| reason == "float:display")
    );
    let inline_parent = node(
        block(),
        vec![
            Component::text(
                "hi",
                Style {
                    display: Display::Inline,
                    ..Style::default()
                },
            ),
            node(float(Display::InlineBlock, 40.0, 20.0, Clear::None), vec![]),
        ],
    );
    assert!(
        block::unsupported_reasons(&pre_flatten(&inline_parent))
            .iter()
            .any(|reason| reason == "float:inline")
    );
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
    // position the cleared box's margin would collapse through.
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
    // Blink HasClearancePastAdjoiningFloats: the parent resolves without
    // the 400px margin and the cleared box sits right below the float.
    let flat = pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let rect = |i: usize| {
        let r = ng.iter().find(|(_, j)| *j == i).expect("rect").0;
        [r.x, r.y, r.width, r.height]
    };
    assert_eq!(rect(1), [0.0, 0.0, 100.0, 100.0]);
    assert_eq!(rect(3), [0.0, 0.0, 100.0, 50.0]);
    assert_eq!(rect(4), [0.0, 50.0, 100.0, 50.0]);

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

    // no-clearance-adjoining-opposite-float: only floats placed while the
    // BFC offset is unresolved adjoin (the left float precedes the chain,
    // the adjoining one is on the other side), so the 150px margin takes
    // the box past the left float without clearance.
    let block = || Style {
        display: Display::Block,
        ..Style::default()
    };
    let mut cleared = Style {
        display: Display::Block,
        clear: Clear::Left,
        height: px(10.0),
        ..Style::default()
    };
    cleared.margin.top = S::Px(150.0);
    let root = Component::boxed(
        block(),
        vec![
            leaf(Style {
                display: Display::Block,
                float: Float::Left,
                width: px(10.0),
                height: px(100.0),
                ..Style::default()
            }),
            Component::boxed(
                block(),
                vec![
                    Component::boxed(
                        block(),
                        vec![leaf(Style {
                            display: Display::Block,
                            float: Float::Right,
                            width: px(10.0),
                            height: px(200.0),
                            ..Style::default()
                        })],
                    ),
                    Component::boxed(block(), vec![Component::boxed(cleared, vec![])]),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let rect = |index: usize| {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        [r.x, r.y, r.width, r.height]
    };
    assert_eq!(rect(2), [0.0, 150.0, 800.0, 10.0]);
    assert_eq!(rect(4), [790.0, 150.0, 10.0, 200.0]);
    assert_eq!(rect(6), [0.0, 150.0, 800.0, 10.0]);
}

/// Blink HasClearancePastAdjoiningFloats: a cleared box whose margin
/// would otherwise drag adjoining floats down resolves the parent's BFC
/// offset without that margin and sits exactly at the clearance offset
/// (adjoining-float-before-clearance, clear-on-parent-with-margins).
#[test]
fn clearance_past_adjoining_floats_matches_blink() {
    use w3cos_std::style::{Clear, Dimension as D, Display, Edges, Float, Spacing as S, Style};
    let px = D::Px;
    let z = S::Px(0.0);
    let block = |w: D, h: D, float: Float, clear: Clear, mt: f32| Style {
        display: Display::Block,
        width: w,
        height: h,
        float,
        clear,
        margin: Edges {
            top: S::Px(mt),
            right: z,
            bottom: z,
            left: z,
        },
        ..Style::default()
    };
    let leaf = |style: Style| Component::boxed(style, vec![]);
    let (n, l, auto) = (Float::None, Float::Left, D::Auto);
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
    // Through a non-BFC wrapper with its own top margin: the wrapper (and
    // the float) resolve at that margin; the cleared child goes below.
    let outer = Component::boxed(
        Style {
            display: Display::FlowRoot,
            width: px(200.0),
            ..Style::default()
        },
        vec![Component::boxed(
            block(auto, auto, n, Clear::None, 10.0),
            vec![
                leaf(block(px(40.0), px(30.0), l, Clear::None, 0.0)),
                leaf(block(auto, px(20.0), n, Clear::Left, 25.0)),
            ],
        )],
    );
    check(
        &outer,
        &[
            (0, [0.0, 0.0, 200.0, 60.0]),
            (1, [0.0, 10.0, 200.0, 50.0]),
            (2, [0.0, 10.0, 40.0, 30.0]),
            (3, [0.0, 40.0, 200.0, 20.0]),
        ],
    );
    // negative-clearance-after-adjoining-float: a cleared self-collapsing
    // box sits at the clearance offset and its margin is kept inside the
    // parent (Blink ComputeInflowPosition / FinishLayout).
    let negative = Component::boxed(
        Style {
            display: Display::FlowRoot,
            width: px(100.0),
            ..Style::default()
        },
        vec![
            Component::boxed(
                block(auto, auto, n, Clear::None, 0.0),
                vec![
                    leaf(block(px(100.0), px(50.0), l, Clear::None, 0.0)),
                    leaf(block(auto, auto, n, Clear::Left, 200.0)),
                ],
            ),
            leaf(block(auto, px(50.0), n, Clear::None, 0.0)),
        ],
    );
    check(
        &negative,
        &[
            (0, [0.0, 0.0, 100.0, 100.0]),
            (1, [0.0, 0.0, 100.0, 50.0]),
            (2, [0.0, 0.0, 100.0, 50.0]),
            (3, [0.0, 50.0, 100.0, 0.0]),
            (4, [0.0, 50.0, 100.0, 50.0]),
        ],
    );
    // margin-collapse-033: the flag survives a following self-collapsing
    // sibling, whose margin then stays inside the parent (1 + 99).
    let sibling = Component::boxed(
        Style {
            display: Display::FlowRoot,
            width: px(100.0),
            ..Style::default()
        },
        vec![Component::boxed(
            block(auto, auto, n, Clear::None, 0.0),
            vec![
                leaf(block(px(10.0), px(1.0), l, Clear::None, 0.0)),
                leaf(block(auto, auto, n, Clear::Left, 0.0)),
                leaf(block(auto, auto, n, Clear::None, 99.0)),
            ],
        )],
    );
    check(
        &sibling,
        &[
            (0, [0.0, 0.0, 100.0, 100.0]),
            (1, [0.0, 0.0, 100.0, 100.0]),
            (3, [0.0, 1.0, 100.0, 0.0]),
        ],
    );
    // margin-collapse-clear-012: clearance = 100 - 40 (margin-top only);
    // the 80px bottom margin and the sibling's 140px collapse after it.
    let mut cleared = block(auto, auto, n, Clear::Left, 40.0);
    cleared.margin.bottom = S::Px(80.0);
    let mut sib = block(auto, auto, n, Clear::None, 0.0);
    sib.margin.bottom = S::Px(140.0);
    let mut parent = block(px(200.0), auto, n, Clear::None, 0.0);
    parent.border_top_width = Some(1.0);
    let clear012 = Component::boxed(
        Style {
            display: Display::FlowRoot,
            width: px(400.0),
            ..Style::default()
        },
        vec![Component::boxed(
            parent,
            vec![
                leaf(block(px(100.0), px(100.0), l, Clear::None, 0.0)),
                leaf(cleared),
                leaf(sib),
            ],
        )],
    );
    check(
        &clear012,
        &[(1, [0.0, 0.0, 200.0, 201.0]), (3, [0.0, 101.0, 200.0, 0.0])],
    );
}

#[test]
fn size_containment_uses_contain_intrinsic_size() {
    // contain-intrinsic-size-014: `width: max-content; contain: size;
    // contain-intrinsic-size: 111px 222px` lays out as a 111x222 content box
    // whatever its children (Blink ShouldApplySizeContainment).
    let mark = |style: &mut Style, pairs: &[(&str, &str)]| {
        let props = style.custom_properties.get_or_insert_with(Default::default);
        for (k, v) in pairs {
            props.insert((*k).into(), (*v).into());
        }
    };
    let mut contained = block();
    mark(
        &mut contained,
        &[
            ("--w3cos-internal-size-containment", "1"),
            ("--w3cos-internal-contain-intrinsic-size", "111 222"),
            ("--w3cos-internal-width-keyword", "max-content"),
        ],
    );
    let root = node(
        block(),
        vec![
            node(contained, vec![node(sized(300.0), vec![])]),
            node(sized(10.0), vec![]),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    assert_eq!(rect(&ng, 1), (0.0, 0.0, 111.0, 222.0));
    assert_eq!(rect(&ng, 2), (0.0, 0.0, 111.0, 300.0));
    assert_eq!(rect(&ng, 3), (0.0, 222.0, 800.0, 10.0));
    // No contain-intrinsic-size: the contents still do not size the box.
    let mut empty = block();
    mark(&mut empty, &[("--w3cos-internal-size-containment", "1")]);
    let root = node(block(), vec![node(empty, vec![node(sized(50.0), vec![])])]);
    let flat = pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    assert_eq!(rect(&ng, 1), (0.0, 0.0, 800.0, 0.0));
    // Without size containment a width keyword is not resolved yet: the
    // width stays auto (as on legacy).
    let mut keyword = block();
    mark(
        &mut keyword,
        &[("--w3cos-internal-width-keyword", "max-content")],
    );
    let root = node(block(), vec![node(keyword, vec![])]);
    let flat = pre_flatten(&root);
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    assert_eq!(rect(&ng, 1), (0.0, 0.0, 800.0, 0.0));
}

/// CSS2 9.4.1: overflow other than visible is a BFC. The box sits beside
/// a float instead of overlapping it, and a float inside it contributes
/// to the auto height (10.6.7) without its margin escaping.
#[test]
fn overflow_hidden_is_a_bfc_like_legacy() {
    use w3cos_std::style::{Dimension as D, Display, Float, Overflow};
    let float = Style {
        display: Display::Block,
        width: D::Px(40.0),
        height: D::Px(30.0),
        float: Float::Left,
        ..Style::default()
    };
    let beside = Style {
        display: Display::Block,
        height: D::Px(20.0),
        overflow: Overflow::Hidden,
        ..Style::default()
    };
    let root = node(
        Style {
            display: Display::Block,
            width: D::Px(100.0),
            ..Style::default()
        },
        vec![node(float, vec![]), node(beside.clone(), vec![])],
    );
    let out = parity(&root);
    assert_eq!(rect(&out, 1), (0.0, 0.0, 40.0, 30.0));
    assert_eq!(rect(&out, 2), (40.0, 0.0, 60.0, 20.0));
    // The float's margin stays inside the overflow box.
    let contained = node(
        Style {
            display: Display::Block,
            width: D::Px(100.0),
            overflow: Overflow::Hidden,
            ..Style::default()
        },
        vec![node(
            Style {
                display: Display::Block,
                width: D::Px(40.0),
                height: D::Px(30.0),
                float: Float::Left,
                margin: margins(10.0, 0.0, 0.0, 0.0),
                ..Style::default()
            },
            vec![],
        )],
    );
    let out = parity(&contained);
    assert_eq!(rect(&out, 0), (0.0, 0.0, 100.0, 40.0));
    assert_eq!(rect(&out, 1), (0.0, 10.0, 40.0, 30.0));
}

fn ng_rects(root: &Component) -> Vec<(LayoutRect, usize)> {
    let flat = pre_flatten(root);
    assert!(
        block::supports_tree(&flat),
        "{:?}",
        block::unsupported_reasons(&flat)
    );
    let mut ng = layout_tree(root, &flat, 800.0, 600.0).expect("ng layout");
    ng.sort_by_key(|(_, index)| *index);
    ng
}

fn close(actual: (f32, f32, f32, f32), expected: (f32, f32, f32, f32)) {
    let delta = [actual.0, actual.1, actual.2, actual.3]
        .into_iter()
        .zip([expected.0, expected.1, expected.2, expected.3])
        .map(|(got, want)| (got - want).abs())
        .fold(0.0f32, f32::max);
    assert!(delta < 0.05, "{actual:?} vs {expected:?}");
}

/// Border boxes measured in headless Chrome (body margin 0). The table's
/// own grid produces them; they are not copied from the legacy formatter.
#[test]
fn table_border_boxes_match_chrome() {
    use w3cos_std::style::{
        AlignSelf, BorderLineStyle, CustomProperties, Display, FlexDirection, Float, TextDirection,
    };
    let cell = |width: f32, height: f32| {
        node(
            Style {
                display: Display::TableCell,
                width: Dimension::Px(width),
                height: Dimension::Px(height),
                ..Style::default()
            },
            vec![],
        )
    };
    let auto_cell = |height: f32| {
        node(
            Style {
                display: Display::TableCell,
                height: Dimension::Px(height),
                ..Style::default()
            },
            vec![],
        )
    };
    let row = |cells: Vec<Component>| {
        node(
            Style {
                display: Display::TableRow,
                ..Style::default()
            },
            cells,
        )
    };
    let wrap = |table: Component| {
        node(
            Style {
                display: Display::Block,
                width: Dimension::Px(400.0),
                ..block()
            },
            vec![table],
        )
    };
    let span = |width: f32, height: f32, columns: usize, rows: usize| {
        node(
            Style {
                display: Display::TableCell,
                width: Dimension::Px(width),
                height: Dimension::Px(height),
                custom_properties: Some(CustomProperties::from_iter([
                    (
                        "--w3cos-internal-table-column-span".to_string(),
                        columns.to_string(),
                    ),
                    (
                        "--w3cos-internal-table-row-span".to_string(),
                        rows.to_string(),
                    ),
                ])),
                ..Style::default()
            },
            vec![],
        )
    };

    // Separated. The short cell stretches to the row.
    let spaced = wrap(node(
        Style {
            display: Display::Table,
            border_spacing_x: 2.0,
            border_spacing_y: 2.0,
            ..Style::default()
        },
        vec![
            row(vec![cell(40.0, 20.0), cell(60.0, 20.0)]),
            row(vec![cell(40.0, 30.0), cell(60.0, 10.0)]),
        ],
    ));
    let out = ng_rects(&spaced);
    assert_eq!(rect(&out, 1), (0.0, 0.0, 106.0, 56.0));
    assert_eq!(rect(&out, 3), (2.0, 2.0, 40.0, 20.0));
    assert_eq!(rect(&out, 4), (44.0, 2.0, 60.0, 20.0));
    assert_eq!(rect(&out, 6), (2.0, 24.0, 40.0, 30.0));
    assert_eq!(rect(&out, 7), (44.0, 24.0, 60.0, 30.0));

    // Fixed: specified columns keep their size; the auto column takes the rest.
    let fixed = wrap(node(
        Style {
            display: Display::Table,
            width: Dimension::Px(200.0),
            table_layout_fixed: true,
            ..Style::default()
        },
        vec![
            node(
                Style {
                    display: Display::TableColumn,
                    width: Dimension::Px(50.0),
                    ..Style::default()
                },
                vec![],
            ),
            row(vec![auto_cell(10.0), cell(80.0, 10.0), auto_cell(10.0)]),
        ],
    ));
    let out = ng_rects(&fixed);
    assert_eq!(rect(&out, 1).2, 200.0);
    assert_eq!(rect(&out, 4), (0.0, 0.0, 50.0, 10.0));
    assert_eq!(rect(&out, 5), (50.0, 0.0, 80.0, 10.0));
    assert_eq!(rect(&out, 6), (130.0, 0.0, 70.0, 10.0));

    // Fixed, three unspecified columns share the table.
    let equal = wrap(node(
        Style {
            display: Display::Table,
            width: Dimension::Px(90.0),
            table_layout_fixed: true,
            ..Style::default()
        },
        vec![row(vec![auto_cell(8.0), auto_cell(8.0), auto_cell(8.0)])],
    ));
    let out = ng_rects(&equal);
    assert_eq!(rect(&out, 1).2, 90.0);
    assert_eq!(rect(&out, 3), (0.0, 0.0, 30.0, 8.0));
    assert_eq!(rect(&out, 4), (30.0, 0.0, 30.0, 8.0));
    assert_eq!(rect(&out, 5), (60.0, 0.0, 30.0, 8.0));

    // Fixed and narrower than the specified columns: the table grows, auto is 0.
    let narrow = wrap(node(
        Style {
            display: Display::Table,
            width: Dimension::Px(100.0),
            table_layout_fixed: true,
            ..Style::default()
        },
        vec![row(vec![cell(50.0, 8.0), cell(80.0, 8.0), auto_cell(8.0)])],
    ));
    let out = ng_rects(&narrow);
    assert_eq!(rect(&out, 1).2, 130.0);
    assert_eq!(rect(&out, 3), (0.0, 0.0, 50.0, 8.0));
    assert_eq!(rect(&out, 4), (50.0, 0.0, 80.0, 8.0));
    assert_eq!(rect(&out, 5), (130.0, 0.0, 0.0, 8.0));

    // Fixed, every column specified: extra width is proportional. LayoutUnit
    // truncates 51.42857 to 51.421875.
    let proportional = wrap(node(
        Style {
            display: Display::Table,
            width: Dimension::Px(120.0),
            table_layout_fixed: true,
            ..Style::default()
        },
        vec![row(vec![cell(30.0, 8.0), cell(20.0, 8.0), cell(20.0, 8.0)])],
    ));
    let out = ng_rects(&proportional);
    close(rect(&out, 3), (0.0, 0.0, 51.421875, 8.0));
    close(rect(&out, 4), (51.421875, 0.0, 34.28125, 8.0));
    close(rect(&out, 5), (85.703125, 0.0, 34.296875, 8.0));

    // Percent of a definite table, then a length, then auto.
    let percent = wrap(node(
        Style {
            display: Display::Table,
            width: Dimension::Px(200.0),
            ..Style::default()
        },
        vec![row(vec![
            node(
                Style {
                    display: Display::TableCell,
                    width: Dimension::Percent(50.0),
                    height: Dimension::Px(10.0),
                    ..Style::default()
                },
                vec![],
            ),
            cell(40.0, 10.0),
            auto_cell(10.0),
        ])],
    ));
    let out = ng_rects(&percent);
    assert_eq!(rect(&out, 3), (0.0, 0.0, 100.0, 10.0));
    assert_eq!(rect(&out, 4), (100.0, 0.0, 40.0, 10.0));
    assert_eq!(rect(&out, 5), (140.0, 0.0, 60.0, 10.0));

    // Percent of an auto table: 40 / (1 - 0.5) = 80.
    let auto_percent = wrap(node(
        Style {
            display: Display::Table,
            ..Style::default()
        },
        vec![row(vec![
            node(
                Style {
                    display: Display::TableCell,
                    width: Dimension::Percent(50.0),
                    height: Dimension::Px(10.0),
                    ..Style::default()
                },
                vec![],
            ),
            cell(40.0, 10.0),
        ])],
    ));
    let out = ng_rects(&auto_percent);
    assert_eq!(rect(&out, 1).2, 80.0);
    assert_eq!(rect(&out, 3), (0.0, 0.0, 40.0, 10.0));
    assert_eq!(rect(&out, 4), (40.0, 0.0, 40.0, 10.0));

    // Colspan width includes the internal spacing. 100 - 2 distributed over two 40s.
    let colspan = wrap(node(
        Style {
            display: Display::Table,
            border_spacing_x: 2.0,
            border_spacing_y: 2.0,
            ..Style::default()
        },
        vec![
            row(vec![span(100.0, 12.0, 2, 1), cell(20.0, 12.0)]),
            row(vec![cell(40.0, 8.0), cell(40.0, 8.0), auto_cell(8.0)]),
        ],
    ));
    let out = ng_rects(&colspan);
    assert_eq!(rect(&out, 3), (2.0, 2.0, 100.0, 12.0));
    assert_eq!(rect(&out, 4), (104.0, 2.0, 20.0, 12.0));
    assert_eq!(rect(&out, 6), (2.0, 16.0, 49.0, 8.0));
    assert_eq!(rect(&out, 7), (53.0, 16.0, 49.0, 8.0));
    assert_eq!(rect(&out, 8), (104.0, 16.0, 20.0, 8.0));

    // Rowspan extra height is split equally.
    let rowspan = wrap(node(
        Style {
            display: Display::Table,
            ..Style::default()
        },
        vec![
            row(vec![span(20.0, 40.0, 1, 2), cell(30.0, 10.0)]),
            row(vec![cell(30.0, 10.0)]),
        ],
    ));
    let out = ng_rects(&rowspan);
    assert_eq!(rect(&out, 3), (0.0, 0.0, 20.0, 40.0));
    assert_eq!(rect(&out, 4), (20.0, 0.0, 30.0, 20.0));
    assert_eq!(rect(&out, 6), (20.0, 20.0, 30.0, 20.0));

    // Collapsed grid lines: max width, cell boxes from center to center.
    let collapsed = wrap(node(
        Style {
            display: Display::Table,
            border_collapse: true,
            border_width: 4.0,
            ..Style::default()
        },
        vec![row(vec![
            node(
                Style {
                    display: Display::TableCell,
                    width: Dimension::Px(40.0),
                    height: Dimension::Px(16.0),
                    border_width: 2.0,
                    ..Style::default()
                },
                vec![],
            ),
            node(
                Style {
                    display: Display::TableCell,
                    width: Dimension::Px(20.0),
                    height: Dimension::Px(16.0),
                    border_width: 6.0,
                    ..Style::default()
                },
                vec![],
            ),
        ])],
    ));
    let out = ng_rects(&collapsed);
    assert_eq!(rect(&out, 1), (0.0, 0.0, 76.0, 28.0));
    assert_eq!(rect(&out, 3), (2.0, 3.0, 45.0, 22.0));
    assert_eq!(rect(&out, 4), (47.0, 3.0, 26.0, 22.0));

    // `hidden` wins over a wider border and zeros the shared line.
    let mut hidden_border = [None; 4];
    hidden_border[1] = Some(BorderLineStyle::Hidden);
    let hidden = wrap(node(
        Style {
            display: Display::Table,
            border_collapse: true,
            border_width: 5.0,
            ..Style::default()
        },
        vec![row(vec![
            node(
                Style {
                    display: Display::TableCell,
                    width: Dimension::Px(20.0),
                    height: Dimension::Px(10.0),
                    border_width: 4.0,
                    border_styles: hidden_border,
                    ..Style::default()
                },
                vec![],
            ),
            node(
                Style {
                    display: Display::TableCell,
                    width: Dimension::Px(20.0),
                    height: Dimension::Px(10.0),
                    border_width: 8.0,
                    ..Style::default()
                },
                vec![],
            ),
        ])],
    ));
    let out = ng_rects(&hidden);
    assert_eq!(rect(&out, 1), (0.0, 0.0, 53.0, 26.0));
    assert_eq!(rect(&out, 3), (2.5, 4.0, 22.5, 18.0));
    assert_eq!(rect(&out, 4), (25.0, 4.0, 24.0, 18.0));

    // vertical-align. Baseline meets the content bottom of an empty div.
    let aligned = |align: AlignSelf, height: f32| {
        node(
            Style {
                display: Display::TableCell,
                width: Dimension::Px(20.0),
                align_self: align,
                ..Style::default()
            },
            vec![node(
                Style {
                    height: Dimension::Px(height),
                    ..block()
                },
                vec![],
            )],
        )
    };
    let valign = wrap(node(
        Style {
            display: Display::Table,
            ..Style::default()
        },
        vec![row(vec![
            aligned(AlignSelf::Baseline, 10.0),
            aligned(AlignSelf::Baseline, 30.0),
            aligned(AlignSelf::FlexEnd, 8.0),
            aligned(AlignSelf::Center, 10.0),
            aligned(AlignSelf::FlexStart, 12.0),
        ])],
    ));
    let out = ng_rects(&valign);
    assert_eq!(rect(&out, 3), (0.0, 0.0, 20.0, 30.0));
    assert_eq!(rect(&out, 4), (0.0, 20.0, 20.0, 10.0));
    assert_eq!(rect(&out, 5), (20.0, 0.0, 20.0, 30.0));
    assert_eq!(rect(&out, 6), (20.0, 0.0, 20.0, 30.0));
    assert_eq!(rect(&out, 7), (40.0, 0.0, 20.0, 30.0));
    assert_eq!(rect(&out, 8), (40.0, 22.0, 20.0, 8.0));
    assert_eq!(rect(&out, 9), (60.0, 0.0, 20.0, 30.0));
    assert_eq!(rect(&out, 10), (60.0, 10.0, 20.0, 10.0));
    assert_eq!(rect(&out, 11), (80.0, 0.0, 20.0, 30.0));
    assert_eq!(rect(&out, 12), (80.0, 0.0, 20.0, 12.0));

    // Caption is inside the table border box and outside the grid.
    let caption = |bottom: bool| {
        wrap(node(
            Style {
                display: Display::Table,
                width: Dimension::Px(80.0),
                ..Style::default()
            },
            vec![
                node(
                    Style {
                        display: Display::TableCaption,
                        height: Dimension::Px(20.0),
                        caption_side_bottom: bottom,
                        ..Style::default()
                    },
                    vec![],
                ),
                row(vec![cell(80.0, 14.0)]),
            ],
        ))
    };
    let out = ng_rects(&caption(false));
    assert_eq!(rect(&out, 1), (0.0, 0.0, 80.0, 34.0));
    assert_eq!(rect(&out, 2), (0.0, 0.0, 80.0, 20.0));
    assert_eq!(rect(&out, 4), (0.0, 20.0, 80.0, 14.0));
    let out = ng_rects(&caption(true));
    assert_eq!(rect(&out, 2), (0.0, 14.0, 80.0, 20.0));
    assert_eq!(rect(&out, 4), (0.0, 0.0, 80.0, 14.0));

    // Header, body, footer order, even when the footer is first in the tree.
    let section = |display: Display, height: f32| {
        node(
            Style {
                display,
                ..Style::default()
            },
            vec![row(vec![cell(40.0, height)])],
        )
    };
    let groups = wrap(node(
        Style {
            display: Display::Table,
            ..Style::default()
        },
        vec![
            section(Display::TableFooterGroup, 4.0),
            section(Display::TableHeaderGroup, 6.0),
            section(Display::TableRowGroup, 8.0),
        ],
    ));
    let out = ng_rects(&groups);
    assert_eq!(rect(&out, 1), (0.0, 0.0, 40.0, 18.0));
    assert_eq!(rect(&out, 2), (0.0, 14.0, 40.0, 4.0));
    assert_eq!(rect(&out, 4), (0.0, 14.0, 40.0, 4.0));
    assert_eq!(rect(&out, 5), (0.0, 0.0, 40.0, 6.0));
    assert_eq!(rect(&out, 7), (0.0, 0.0, 40.0, 6.0));
    assert_eq!(rect(&out, 8), (0.0, 6.0, 40.0, 8.0));
    assert_eq!(rect(&out, 10), (0.0, 6.0, 40.0, 8.0));

    // RTL mirrors columns inside the table border box. Extra width is proportional.
    let rtl = wrap(node(
        Style {
            display: Display::Table,
            width: Dimension::Px(100.0),
            direction: TextDirection::Rtl,
            ..Style::default()
        },
        vec![row(vec![cell(30.0, 10.0), cell(50.0, 10.0)])],
    ));
    let out = ng_rects(&rtl);
    assert_eq!(rect(&out, 3), (62.5, 0.0, 37.5, 10.0));
    assert_eq!(rect(&out, 4), (0.0, 0.0, 62.5, 10.0));

    // max-width caps the used column; min-width does not raise it above the cap.
    let capped = wrap(node(
        Style {
            display: Display::Table,
            width: Dimension::Px(100.0),
            ..Style::default()
        },
        vec![row(vec![
            node(
                Style {
                    display: Display::TableCell,
                    width: Dimension::Px(80.0),
                    min_width: Dimension::Px(20.0),
                    max_width: Dimension::Px(40.0),
                    height: Dimension::Px(10.0),
                    ..Style::default()
                },
                vec![],
            ),
            auto_cell(10.0),
        ])],
    ));
    let out = ng_rects(&capped);
    assert_eq!(rect(&out, 3), (0.0, 0.0, 40.0, 10.0));
    assert_eq!(rect(&out, 4), (40.0, 0.0, 60.0, 10.0));

    // Auto margins of a shrink-to-fit table are solved from the used width.
    let centered = node(
        Style {
            display: Display::Block,
            width: Dimension::Px(400.0),
            ..block()
        },
        vec![node(
            Style {
                display: Display::Table,
                margin: Edges {
                    top: px(0.0),
                    right: Spacing::Auto,
                    bottom: px(0.0),
                    left: Spacing::Auto,
                },
                ..Style::default()
            },
            vec![row(vec![cell(40.0, 10.0), cell(60.0, 10.0)])],
        )],
    );
    let out = ng_rects(&centered);
    assert_eq!(rect(&out, 1), (150.0, 0.0, 100.0, 10.0));

    // A float beside the table: the table shrinks to fit in the opportunity.
    let beside = node(
        Style {
            display: Display::Block,
            width: Dimension::Px(200.0),
            ..block()
        },
        vec![
            node(
                Style {
                    display: Display::Block,
                    width: Dimension::Px(40.0),
                    height: Dimension::Px(30.0),
                    float: Float::Left,
                    ..Style::default()
                },
                vec![],
            ),
            node(
                Style {
                    display: Display::Table,
                    ..Style::default()
                },
                vec![row(vec![cell(50.0, 10.0)])],
            ),
        ],
    );
    let out = ng_rects(&beside);
    assert_eq!(rect(&out, 1), (0.0, 0.0, 40.0, 30.0));
    assert_eq!(rect(&out, 2).0, 40.0);
    assert_eq!(rect(&out, 2).2, 50.0);
    assert_eq!(rect(&out, 4), (40.0, 0.0, 50.0, 10.0));

    // inline-table is an atomic inline. flex items use the assigned size.
    let inline_table = node(
        block(),
        vec![node(
            Style {
                display: Display::InlineTable,
                ..Style::default()
            },
            vec![row(vec![cell(40.0, 10.0)])],
        )],
    );
    let out = ng_rects(&inline_table);
    assert_eq!(rect(&out, 1).2, 40.0);
    assert_eq!(rect(&out, 3).2, 40.0);
    assert_eq!(rect(&out, 3).3, 10.0);

    let flex_item = node(
        block(),
        vec![node(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                width: Dimension::Px(200.0),
                ..Style::default()
            },
            vec![node(
                Style {
                    display: Display::Table,
                    ..Style::default()
                },
                vec![row(vec![cell(40.0, 10.0), cell(60.0, 10.0)])],
            )],
        )],
    );
    let out = ng_rects(&flex_item);
    assert_eq!(rect(&out, 4), (0.0, 0.0, 40.0, 10.0));
    assert_eq!(rect(&out, 5), (40.0, 0.0, 60.0, 10.0));

    // A text cell lays out. Its width follows this engine's font, not Chrome's.
    let text_cell = node(
        Style {
            display: Display::TableCell,
            ..Style::default()
        },
        vec![w3cos_std::Component::text(
            "hi",
            Style {
                display: Display::Inline,
                ..Style::default()
            },
        )],
    );
    let text_table = node(
        Style {
            display: Display::Table,
            ..block()
        },
        vec![row(vec![text_cell])],
    );
    let out = ng_rects(&text_table);
    assert!(rect(&out, 1).2 > 0.0);
}

/// `inline-grid` stays `display: flex` for legacy and is a grid container
/// on NG. A `-webkit-box` whose flex line is rejected (a text item) keeps
/// the unrecognized marker.
#[test]
fn inline_grid_is_a_grid_and_webkit_box_stays_legacy() {
    use w3cos_std::style::CustomProperties;
    let mut grid = Style {
        display: Display::Flex,
        width: Dimension::Px(100.0),
        grid_template_columns: Some("40px 60px".to_string()),
        ..Style::default()
    };
    grid.custom_properties = Some(CustomProperties::from_iter([(
        "--w3cos-internal-unrecognized-display".to_string(),
        "inline-grid".to_string(),
    )]));
    let cell = |w| {
        node(
            Style {
                width: Dimension::Px(w),
                height: Dimension::Px(10.0),
                ..block()
            },
            vec![],
        )
    };
    // Blockified only as a flex item. A block parent keeps the marker,
    // so a reference that is a lone inline-grid stays on legacy.
    let lone_root = node(block(), vec![node(grid.clone(), vec![])]);
    let lone = pre_flatten(&lone_root);
    assert!(
        !block::supports_tree(&lone),
        "{:?}",
        block::unsupported_reasons(&lone)
    );
    let flex = Style {
        display: Display::Flex,
        width: Dimension::Px(100.0),
        ..Style::default()
    };
    let root = node(
        block(),
        vec![node(flex, vec![node(grid, vec![cell(40.0), cell(60.0)])])],
    );
    let flat = pre_flatten(&root);
    assert!(
        block::supports_tree(&flat),
        "{:?}",
        block::unsupported_reasons(&flat)
    );
    let out = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let at = |index| {
        let r = out.iter().find(|(_, i)| *i == index).expect("rect").0;
        (r.x, r.y, r.width, r.height)
    };
    // Preorder: 0 root, 1 flex, 2 inline-grid, 3 first cell, 4 second cell.
    assert_eq!(at(3), (0.0, 0.0, 40.0, 10.0));
    assert_eq!(at(4), (40.0, 0.0, 60.0, 10.0));

    let mut webkit = Style {
        display: Display::Flex,
        width: Dimension::Px(100.0),
        ..block()
    };
    webkit.custom_properties = Some(CustomProperties::from_iter([(
        "--w3cos-internal-unrecognized-display".to_string(),
        "-webkit-box".to_string(),
    )]));
    let webkit_root = node(
        block(),
        vec![node(
            webkit,
            vec![Component::text("line", Style::default())],
        )],
    );
    let web = pre_flatten(&webkit_root);
    assert!(
        block::unsupported_reasons(&web)
            .iter()
            .any(|r| { r.starts_with("unrecognized-display:-webkit-box|") }),
        "{:?}",
        block::unsupported_reasons(&web)
    );

    // `display: run-in` stays `flex` for legacy. A definite box is that
    // flex box on NG, which is what the applies-to references paint.
    let mut run_in = Style {
        display: Display::Flex,
        width: Dimension::Px(96.0),
        height: Dimension::Px(96.0),
        ..block()
    };
    run_in.custom_properties = Some(CustomProperties::from_iter([(
        "--w3cos-internal-unrecognized-display".to_string(),
        "run-in".to_string(),
    )]));
    let run_root = node(block(), vec![node(run_in, vec![])]);
    let run_flat = pre_flatten(&run_root);
    assert!(
        block::supports_tree(&run_flat),
        "{:?}",
        block::unsupported_reasons(&run_flat)
    );
    let run_out = layout_tree(&run_root, &run_flat, 800.0, 600.0).expect("run-in");
    let run_rect = run_out
        .iter()
        .find(|(_, i)| *i == 1)
        .expect("run-in rect")
        .0;
    assert_eq!(
        (run_rect.x, run_rect.y, run_rect.width, run_rect.height),
        (0.0, 0.0, 96.0, 96.0)
    );
}

/// Chrome rejects `display: run-in`, so the earlier `block` applies. The
/// DOM keeps legacy `flex` plus markers; NG lays a run-in over a block out
/// as that block when it holds nothing, one text, or only blocks.
/// Headless Chrome 155, body margin 0, Ahem 10px/1:
/// - text: run-in (0,0,800,10), text (0,0,50,10), next block y=10;
/// - two block children (padding 4, margins 8/12 collapse): run-in
///   (0,0,800,40), children at (4,4) and (4,26), 792×10;
/// - margins collapse through the run-in: parent (margin-top 10) and
///   run-in (margins 20) at y=20; the next block (margin-top 30) at y=55;
/// - empty run-in with border 3 and padding 2 is 800×10.
#[test]
fn run_in_over_block_lays_out_as_that_block() {
    use w3cos_std::style::CustomProperties;
    let _ahem = super::inline_tests::AhemGuard::new();
    let run_in = |style: Style, prior_block: bool| {
        let mut markers = vec![(
            "--w3cos-internal-unrecognized-display".to_string(),
            "run-in".to_string(),
        )];
        if prior_block {
            markers.push(("--w3cos-internal-run-in-block".to_string(), "1".to_string()));
        }
        Style {
            display: Display::Flex,
            custom_properties: Some(CustomProperties::from_iter(markers)),
            ..style
        }
    };
    let font = Style {
        font_family: Some("Ahem".into()),
        font_size: 10.0,
        line_height: 1.0,
        line_height_is_normal: false,
        ..block()
    };
    let text_style = Style {
        display: Display::Inline,
        ..font.clone()
    };
    let ng = |root: &Component| {
        let flat = pre_flatten(root);
        let mut out = layout_tree(root, &flat, 800.0, 600.0).expect("ng");
        out.sort_by_key(|(_, i)| *i);
        out
    };

    // 0 root, 1 run-in, 2 text, 3 next block.
    let root = node(
        block(),
        vec![
            node(
                run_in(font.clone(), true),
                vec![Component::text("XX XX", text_style.clone())],
            ),
            node(sized(5.0), vec![]),
        ],
    );
    let out = ng(&root);
    assert_eq!(rect(&out, 1), (0.0, 0.0, 800.0, 10.0));
    assert_eq!(rect(&out, 2), (0.0, 0.0, 50.0, 10.0));
    assert_eq!(rect(&out, 3), (0.0, 10.0, 800.0, 5.0));

    // 0 root, 1 run-in, 2 and 3 children: stacked, not a flex row.
    let padded = Style {
        padding: margins(4.0, 4.0, 4.0, 4.0),
        ..block()
    };
    let first = Style {
        margin: margins(0.0, 0.0, 8.0, 0.0),
        ..sized(10.0)
    };
    let second = Style {
        margin: margins(12.0, 0.0, 0.0, 0.0),
        ..sized(10.0)
    };
    let root = node(
        block(),
        vec![node(
            run_in(padded, true),
            vec![node(first, vec![]), node(second, vec![])],
        )],
    );
    let out = ng(&root);
    assert_eq!(rect(&out, 1), (0.0, 0.0, 800.0, 40.0));
    assert_eq!(rect(&out, 2), (4.0, 4.0, 792.0, 10.0));
    assert_eq!(rect(&out, 3), (4.0, 26.0, 792.0, 10.0));

    // 0 root, 1 parent, 2 run-in, 3 next block.
    let parent = Style {
        margin: margins(10.0, 0.0, 0.0, 0.0),
        ..block()
    };
    let collapsing = Style {
        margin: margins(20.0, 0.0, 20.0, 0.0),
        ..sized(5.0)
    };
    let next = Style {
        margin: margins(30.0, 0.0, 0.0, 0.0),
        ..sized(5.0)
    };
    let root = node(
        block(),
        vec![node(
            parent,
            vec![node(run_in(collapsing, true), vec![]), node(next, vec![])],
        )],
    );
    let out = ng(&root);
    assert_eq!(rect(&out, 1), (0.0, 20.0, 800.0, 40.0));
    assert_eq!(rect(&out, 2), (0.0, 20.0, 800.0, 5.0));
    assert_eq!(rect(&out, 3), (0.0, 55.0, 800.0, 5.0));

    let bordered = Style {
        border_width: 3.0,
        padding: margins(2.0, 2.0, 2.0, 2.0),
        ..block()
    };
    let root = node(
        block(),
        vec![
            node(run_in(bordered, true), vec![]),
            node(sized(5.0), vec![]),
        ],
    );
    let out = ng(&root);
    assert_eq!(rect(&out, 1), (0.0, 0.0, 800.0, 10.0));
    assert_eq!(rect(&out, 2), (0.0, 10.0, 800.0, 5.0));

    // Without a prior block, or with text beside a block (which needs an
    // anonymous block), the run-in keeps its marker.
    for (prior_block, children) in [
        (
            false,
            vec![node(sized(5.0), vec![]), node(sized(5.0), vec![])],
        ),
        (
            true,
            vec![
                Component::text("XX", text_style.clone()),
                node(sized(5.0), vec![]),
            ],
        ),
    ] {
        let root = node(
            block(),
            vec![node(run_in(font.clone(), prior_block), children)],
        );
        let flat = pre_flatten(&root);
        assert!(block::run_in_block_styles(&flat).is_empty());
    }

    // run-in-table-row-between: a text-only `display: table-row` span in
    // an anonymous table has no cell for its line, so the page stays on
    // legacy (which paints the row) rather than dropping the text.
    let table = Style {
        display: Display::Table,
        ..Style::default()
    };
    let row_text = Style {
        display: Display::TableRow,
        ..text_style.clone()
    };
    let root = node(
        block(),
        vec![node(table, vec![Component::text("Some text.", row_text)])],
    );
    let flat = pre_flatten(&root);
    assert!(
        block::unsupported_reasons(&flat)
            .iter()
            .any(|r| r == "table:text-track"),
        "{:?}",
        block::unsupported_reasons(&flat)
    );
}

/// A definite top-level `inline-grid` is a line box, not a stretched block.
/// Chrome (body margin 0): a 100×20 grid with `40px 60px` columns places
/// its items at x=0 and x=40; two 50×100 grids sit at x=0 and x=50 on the
/// line after a 15px block. An auto width stays on the legacy marker.
#[test]
fn top_level_definite_inline_grid_shares_a_line() {
    use w3cos_std::style::CustomProperties;
    let mark = |mut style: Style| {
        style.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-unrecognized-display".to_string(),
            "inline-grid".to_string(),
        )]));
        style
    };
    let grid = mark(Style {
        display: Display::Flex,
        width: Dimension::Px(100.0),
        height: Dimension::Px(20.0),
        grid_template_columns: Some("40px 60px".to_string()),
        ..Style::default()
    });
    let cell = |w| {
        node(
            Style {
                width: Dimension::Px(w),
                height: Dimension::Px(10.0),
                ..block()
            },
            vec![],
        )
    };
    let lone = node(block(), vec![node(grid, vec![cell(40.0), cell(60.0)])]);
    let flat = pre_flatten(&lone);
    assert!(
        block::supports_tree(&flat),
        "{:?}",
        block::unsupported_reasons(&flat)
    );
    let out = layout_tree(&lone, &flat, 800.0, 600.0).expect("ng");
    let at = |out: &[(LayoutRect, usize)], index| {
        let r = out.iter().find(|(_, i)| *i == index).expect("rect").0;
        (r.x, r.y, r.width, r.height)
    };
    // Preorder: 0 root, 1 inline-grid, 2 first cell, 3 second cell.
    assert_eq!(at(&out, 1), (0.0, 0.0, 100.0, 20.0));
    assert_eq!(at(&out, 2), (0.0, 0.0, 40.0, 10.0));
    assert_eq!(at(&out, 3), (40.0, 0.0, 60.0, 10.0));

    let square = || {
        node(
            mark(Style {
                display: Display::Flex,
                width: Dimension::Px(50.0),
                height: Dimension::Px(100.0),
                ..Style::default()
            }),
            vec![],
        )
    };
    let row = node(block(), vec![node(sized(15.0), vec![]), square(), square()]);
    let row_flat = pre_flatten(&row);
    assert!(
        block::supports_tree(&row_flat),
        "{:?}",
        block::unsupported_reasons(&row_flat)
    );
    let row_out = layout_tree(&row, &row_flat, 800.0, 600.0).expect("line");
    // 0 root, 1 spacer, 2 first square, 3 second square.
    assert_eq!(at(&row_out, 2), (0.0, 15.0, 50.0, 100.0));
    assert_eq!(at(&row_out, 3), (50.0, 15.0, 50.0, 100.0));

    let auto = node(
        block(),
        vec![node(
            mark(Style {
                display: Display::Flex,
                width: Dimension::Auto,
                height: Dimension::Px(20.0),
                grid_template_columns: Some("40px 60px".to_string()),
                ..Style::default()
            }),
            vec![],
        )],
    );
    let auto_flat = pre_flatten(&auto);
    assert!(
        block::supports_tree(&auto_flat),
        "{:?}",
        block::unsupported_reasons(&auto_flat)
    );
    let auto_out = layout_tree(&auto, &auto_flat, 800.0, 600.0).expect("auto inline size");
    assert_eq!(at(&auto_out, 1), (0.0, 0.0, 100.0, 20.0));
}

/// An auto-sized top-level `inline-grid` with definite px tracks shrinks
/// to those tracks. Headless Chrome, body margin 0, no inter-element
/// whitespace: `100px / 200px` is a 100×200 border box and a stretched
/// item fills it; `align-items: start` keeps a 20×10 item at the start;
/// `100px 150px` with a 10px column gap is 260×40; `left: 8px; top: 4px`
/// offsets that border box. Two empty grids after a 15px block share the
/// bottom-margin baseline. An out-of-flow child and a margin stay on the
/// legacy marker.
#[test]
fn auto_sized_fixed_track_inline_grid_shrinks_to_its_tracks() {
    use w3cos_std::style::{AlignItems, CustomProperties, Position};
    let mark = |mut style: Style| {
        style.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-unrecognized-display".to_string(),
            "inline-grid".to_string(),
        )]));
        style
    };
    let grid = |columns: &str, rows: &str, align| {
        let mut style = mark(Style {
            display: Display::Flex,
            position: Position::Relative,
            align_items: align,
            grid_template_columns: Some(columns.to_string()),
            ..Style::default()
        });
        style.grid_ng_mut().grid_template_rows = Some(rows.to_string());
        style
    };
    let at = |out: &[(LayoutRect, usize)], index| {
        let r = out.iter().find(|(_, i)| *i == index).expect("rect").0;
        (r.x, r.y, r.width, r.height)
    };
    let stretched = node(
        block(),
        vec![node(
            grid("100px", "200px", AlignItems::Stretch),
            vec![node(block(), vec![])],
        )],
    );
    let stretched_flat = pre_flatten(&stretched);
    assert!(
        block::supports_tree(&stretched_flat),
        "{:?}",
        block::unsupported_reasons(&stretched_flat)
    );
    let stretched_out = layout_tree(&stretched, &stretched_flat, 800.0, 600.0).expect("stretch");
    // 0 root, 1 grid, 2 item.
    assert_eq!(at(&stretched_out, 1), (0.0, 0.0, 100.0, 200.0));
    assert_eq!(at(&stretched_out, 2), (0.0, 0.0, 100.0, 200.0));

    let started = node(
        block(),
        vec![node(
            grid("100px", "200px", AlignItems::FlexStart),
            vec![node(
                Style {
                    width: Dimension::Px(20.0),
                    height: Dimension::Px(10.0),
                    ..block()
                },
                vec![],
            )],
        )],
    );
    let started_flat = pre_flatten(&started);
    assert!(
        block::supports_tree(&started_flat),
        "{:?}",
        block::unsupported_reasons(&started_flat)
    );
    let started_out = layout_tree(&started, &started_flat, 800.0, 600.0).expect("start");
    assert_eq!(at(&started_out, 1), (0.0, 0.0, 100.0, 200.0));
    assert_eq!(at(&started_out, 2), (0.0, 0.0, 20.0, 10.0));

    let mut gapped = grid("100px 150px", "40px", AlignItems::Stretch);
    gapped.column_gap = Some(10.0);
    let gap_root = node(block(), vec![node(gapped, vec![])]);
    let gap_flat = pre_flatten(&gap_root);
    assert!(
        block::supports_tree(&gap_flat),
        "{:?}",
        block::unsupported_reasons(&gap_flat)
    );
    let gap_out = layout_tree(&gap_root, &gap_flat, 800.0, 600.0).expect("gap");
    assert_eq!(at(&gap_out, 1), (0.0, 0.0, 260.0, 40.0));

    let mut shifted = grid("100px", "40px", AlignItems::Stretch);
    shifted.left = Dimension::Px(8.0);
    shifted.top = Dimension::Px(4.0);
    let shift_root = node(block(), vec![node(shifted, vec![])]);
    let shift_flat = pre_flatten(&shift_root);
    let shift_out = layout_tree(&shift_root, &shift_flat, 800.0, 600.0).expect("relative");
    assert_eq!(at(&shift_out, 1), (8.0, 4.0, 100.0, 40.0));

    let row = node(
        block(),
        vec![
            node(sized(15.0), vec![]),
            node(grid("40px", "20px", AlignItems::Stretch), vec![]),
            node(grid("40px", "60px", AlignItems::Stretch), vec![]),
        ],
    );
    let row_flat = pre_flatten(&row);
    assert!(
        block::supports_tree(&row_flat),
        "{:?}",
        block::unsupported_reasons(&row_flat)
    );
    let row_out = layout_tree(&row, &row_flat, 800.0, 600.0).expect("baseline");
    assert_eq!(at(&row_out, 2), (0.0, 55.0, 40.0, 20.0));
    assert_eq!(at(&row_out, 3), (40.0, 15.0, 40.0, 60.0));

    let mut absolute_child = block();
    absolute_child.position = Position::Absolute;
    let oof = node(
        block(),
        vec![node(
            grid("100px", "100px", AlignItems::Stretch),
            vec![node(absolute_child, vec![])],
        )],
    );
    let oof_flat = pre_flatten(&oof);
    assert!(
        !block::supports_tree(&oof_flat),
        "{:?}",
        block::unsupported_reasons(&oof_flat)
    );

    let mut margined = grid("100px", "40px", AlignItems::Stretch);
    margined.margin = margins(10.0, 0.0, 0.0, 0.0);
    let margin_root = node(block(), vec![node(margined, vec![])]);
    let margin_flat = pre_flatten(&margin_root);
    assert!(
        !block::supports_tree(&margin_flat),
        "{:?}",
        block::unsupported_reasons(&margin_flat)
    );

    // A static box is the reference side of reftests that still fall back.
    let mut static_grid = grid("100px", "40px", AlignItems::Stretch);
    static_grid.position = Position::Static;
    let static_root = node(block(), vec![node(static_grid, vec![])]);
    let static_flat = pre_flatten(&static_root);
    assert!(
        !block::supports_tree(&static_flat),
        "{:?}",
        block::unsupported_reasons(&static_flat)
    );
}

#[test]
fn auto_sized_inline_grid_uses_intrinsic_non_fixed_tracks() {
    use w3cos_std::style::CustomProperties;
    // Measured with headless Chrome 155.0.8059.86, 800x600 viewport,
    // body margin 0, no whitespace between items, auto width/height.
    // Each item is 10px tall; the first is 40px wide. Columns, second
    // item width, available width, gap, grid width, second item x:
    for (columns, second, available, gap, width, second_x) in [
        ("auto auto", 30.0, 800.0, 0.0, 70.0, 40.0),
        ("1fr 2fr", 30.0, 800.0, 0.0, 120.0, 40.0),
        (
            "minmax(20px,1fr) minmax(50px,max-content)",
            80.0,
            800.0,
            0.0,
            120.0,
            40.0,
        ),
        ("25% 75%", 30.0, 800.0, 0.0, 70.0, 17.5),
        ("min-content max-content", 30.0, 800.0, 0.0, 70.0, 40.0),
        ("auto auto", 30.0, 800.0, 10.0, 80.0, 50.0),
        ("auto auto", 30.0, 50.0, 0.0, 70.0, 40.0),
        ("0.5fr 1fr", 30.0, 800.0, 0.0, 80.0, 40.0),
    ] {
        let item = |width| {
            node(
                Style {
                    width: Dimension::Px(width),
                    height: Dimension::Px(10.0),
                    ..block()
                },
                vec![],
            )
        };
        let grid = Style {
            display: Display::Flex,
            grid_template_columns: Some(columns.to_string()),
            column_gap: Some(gap),
            custom_properties: Some(CustomProperties::from_iter([(
                "--w3cos-internal-unrecognized-display".to_string(),
                "inline-grid".to_string(),
            )])),
            ..Style::default()
        };
        let root = node(block(), vec![node(grid, vec![item(40.0), item(second)])]);
        let flat = pre_flatten(&root);
        assert!(
            block::supports_tree(&flat),
            "{columns}: {:?}",
            block::unsupported_reasons(&flat)
        );
        let out = layout_tree(&root, &flat, available, 600.0).expect(columns);
        let at = |index| {
            let rect = out.iter().find(|(_, i)| *i == index).expect("rect").0;
            (rect.x, rect.y, rect.width, rect.height)
        };
        assert_eq!(at(1), (0.0, 0.0, width, 10.0), "{columns}: grid");
        assert_eq!(at(2), (0.0, 0.0, 40.0, 10.0), "{columns}: first item");
        assert_eq!(
            at(3),
            (second_x, 0.0, second, 10.0),
            "{columns}: second item"
        );
    }
}

#[test]
fn intrinsic_inline_grid_keeps_split_first_letter_text_on_legacy() {
    use w3cos_std::style::CustomProperties;
    let grid = Style {
        display: Display::Flex,
        grid_template_columns: Some("auto".to_string()),
        custom_properties: Some(CustomProperties::from_iter([(
            "--w3cos-internal-unrecognized-display".to_string(),
            "inline-grid".to_string(),
        )])),
        ..Style::default()
    };
    let mut item = sized(10.0);
    item.width = Dimension::Px(40.0);
    let plain = node(
        block(),
        vec![node(grid.clone(), vec![node(item.clone(), vec![])])],
    );
    assert!(block::supports_tree(&pre_flatten(&plain)));
    item.custom_properties = Some(CustomProperties::from_iter([(
        w3cos_dom::document::FIRST_LETTER_STYLE_MARKER.to_string(),
        "1".to_string(),
    )]));
    let split = node(block(), vec![node(grid, vec![node(item, vec![])])]);
    let flat = pre_flatten(&split);
    assert!(!block::supports_tree(&flat));
    assert!(
        block::unsupported_reasons(&flat)
            .iter()
            .any(|reason| reason.starts_with("unrecognized-display:inline-grid"))
    );
}

#[test]
fn intrinsic_inline_grid_border_boxes_include_padding_and_border() {
    use w3cos_std::style::{CustomProperties, Position};
    // Headless Chrome 155: auto/100px border-box widths, 3px padding
    // and 2px borders, auto auto columns containing 40px/30px items.
    // A static bordered box stays on legacy (`inline grid-lanes`
    // references); the relative one is laid out here.
    for (width, position, outer, second_x) in [
        (Dimension::Auto, Position::Static, 0.0, 0.0),
        (Dimension::Auto, Position::Relative, 80.0, 45.0),
        (Dimension::Px(100.0), Position::Static, 0.0, 0.0),
        (Dimension::Px(100.0), Position::Relative, 100.0, 55.0),
    ] {
        let grid = Style {
            display: Display::Flex,
            position,
            width,
            box_sizing: BoxSizing::BorderBox,
            padding: margins(3.0, 3.0, 3.0, 3.0),
            border_width: 2.0,
            grid_template_columns: Some("auto auto".to_string()),
            custom_properties: Some(CustomProperties::from_iter([(
                "--w3cos-internal-unrecognized-display".to_string(),
                "inline-grid".to_string(),
            )])),
            ..Style::default()
        };
        let item = |width| {
            node(
                Style {
                    width: Dimension::Px(width),
                    ..sized(10.0)
                },
                vec![],
            )
        };
        let root = node(block(), vec![node(grid, vec![item(40.0), item(30.0)])]);
        let flat = pre_flatten(&root);
        if outer == 0.0 {
            assert!(!block::supports_tree(&flat));
            continue;
        }
        assert!(
            block::supports_tree(&flat),
            "{:?}",
            block::unsupported_reasons(&flat)
        );
        let out = layout_tree(&root, &flat, 800.0, 600.0).expect("border box");
        assert_eq!(rect(&out, 1), (0.0, 0.0, outer, 20.0));
        assert_eq!(rect(&out, 2), (5.0, 5.0, 40.0, 10.0));
        assert_eq!(rect(&out, 3), (second_x, 5.0, 30.0, 10.0));
    }
}

#[test]
fn auto_sized_inline_grid_resolves_flexible_and_percent_rows() {
    use w3cos_std::style::CustomProperties;
    // Headless Chrome 155: a 40px column with 40px/30px tall items.
    // Indefinite flex rows preserve their 1:2 ratio; percentages resolve
    // after the automatic height has been measured with auto tracks.
    for (rows, height, second_y) in [("1fr 2fr", 120.0, 40.0), ("25% 75%", 70.0, 17.5)] {
        let mut grid = Style {
            display: Display::Flex,
            grid_template_columns: Some("40px".to_string()),
            custom_properties: Some(CustomProperties::from_iter([(
                "--w3cos-internal-unrecognized-display".to_string(),
                "inline-grid".to_string(),
            )])),
            ..Style::default()
        };
        grid.grid_ng_mut().grid_template_rows = Some(rows.to_string());
        let root = node(
            block(),
            vec![node(
                grid,
                vec![node(sized(40.0), vec![]), node(sized(30.0), vec![])],
            )],
        );
        let flat = pre_flatten(&root);
        assert!(
            block::supports_tree(&flat),
            "{rows}: {:?}",
            block::unsupported_reasons(&flat)
        );
        let out = layout_tree(&root, &flat, 60.0, 600.0).expect(rows);
        assert_eq!(rect(&out, 1), (0.0, 0.0, 40.0, height));
        assert_eq!(rect(&out, 2), (0.0, 0.0, 40.0, 40.0));
        assert_eq!(rect(&out, 3), (0.0, second_y, 40.0, 30.0));
    }
}

#[test]
fn auto_sized_inline_grid_shrinks_and_wraps_auto_track_text() {
    use w3cos_std::style::CustomProperties;
    let _ahem = super::inline_tests::AhemGuard::new();
    let font = Style {
        font_family: Some("Ahem".into()),
        font_size: 10.0,
        line_height: 1.0,
        line_height_is_normal: false,
        ..block()
    };
    let grid = Style {
        display: Display::Flex,
        grid_template_columns: Some("auto auto".to_string()),
        custom_properties: Some(CustomProperties::from_iter([(
            "--w3cos-internal-unrecognized-display".to_string(),
            "inline-grid".to_string(),
        )])),
        ..font.clone()
    };
    let item = |text: &str| node(font.clone(), vec![Component::text(text, font.clone())]);
    let root = node(block(), vec![node(grid, vec![item("XX XX"), item("X X")])]);
    let flat = pre_flatten(&root);
    assert!(
        block::supports_tree(&flat),
        "{:?}",
        block::unsupported_reasons(&flat)
    );
    let out = layout_tree(&root, &flat, 60.0, 600.0).expect("intrinsic text");
    // Chrome 155, Ahem 10px/1: max-content columns are 50/30, minimum
    // columns are 20/10. In 60px they grow to 35/25 and wrap to 20px tall.
    assert_eq!(rect(&out, 1), (0.0, 0.0, 60.0, 20.0));
    assert_eq!(rect(&out, 2), (0.0, 0.0, 35.0, 20.0));
    assert_eq!(rect(&out, 4), (35.0, 0.0, 25.0, 20.0));
}

/// Non-fixed tracks on a definite top-level `inline-grid`, and baseline
/// alignment of the line. Headless Chrome, body margin 0: a 40×20 grid and
/// a 40×60 grid after a 15px block sit at (0,55) and (40,15) (the baseline
/// is the bottom margin edge). A 40×40 grid whose first item is 10px tall
/// sits 30px below an empty 40×40 sibling, because the item's bottom is
/// the baseline. `auto auto` in 200px places a 30px item at x=105. `1fr 2fr`
/// in 210px is 70/140. `minmax(20px,1fr) minmax(50px,max-content)` places
/// an 80px item at x=120. `min-content max-content` in 300px keeps the
/// items at x=0 and x=40.
#[test]
fn top_level_inline_grid_non_fixed_tracks_share_a_baseline() {
    use w3cos_std::style::CustomProperties;
    let mark = |mut style: Style| {
        style.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-unrecognized-display".to_string(),
            "inline-grid".to_string(),
        )]));
        style
    };
    let grid = |w, h, columns: Option<&str>| {
        mark(Style {
            display: Display::Flex,
            width: Dimension::Px(w),
            height: Dimension::Px(h),
            grid_template_columns: columns.map(str::to_string),
            ..Style::default()
        })
    };
    let at = |out: &[(LayoutRect, usize)], index| {
        let r = out.iter().find(|(_, i)| *i == index).expect("rect").0;
        (r.x, r.y, r.width, r.height)
    };
    let box10 = |w| {
        node(
            Style {
                width: Dimension::Px(w),
                height: Dimension::Px(10.0),
                ..block()
            },
            vec![],
        )
    };

    let bases = node(
        block(),
        vec![
            node(sized(15.0), vec![]),
            node(grid(40.0, 20.0, None), vec![]),
            node(grid(40.0, 60.0, None), vec![]),
        ],
    );
    let bases_flat = pre_flatten(&bases);
    assert!(
        block::supports_tree(&bases_flat),
        "{:?}",
        block::unsupported_reasons(&bases_flat)
    );
    let bases_out = layout_tree(&bases, &bases_flat, 800.0, 600.0).expect("baselines");
    assert_eq!(at(&bases_out, 2), (0.0, 55.0, 40.0, 20.0));
    assert_eq!(at(&bases_out, 3), (40.0, 15.0, 40.0, 60.0));

    let item_base = node(
        block(),
        vec![
            node(grid(40.0, 40.0, None), vec![box10(10.0)]),
            node(grid(40.0, 40.0, None), vec![]),
        ],
    );
    let item_flat = pre_flatten(&item_base);
    assert!(
        block::supports_tree(&item_flat),
        "{:?}",
        block::unsupported_reasons(&item_flat)
    );
    let item_out = layout_tree(&item_base, &item_flat, 800.0, 600.0).expect("item baseline");
    // 0 root, 1 first grid, 2 its item, 3 empty grid.
    assert_eq!(at(&item_out, 1), (0.0, 30.0, 40.0, 40.0));
    assert_eq!(at(&item_out, 2), (0.0, 30.0, 10.0, 10.0));
    assert_eq!(at(&item_out, 3), (40.0, 0.0, 40.0, 40.0));

    let auto = node(
        block(),
        vec![node(
            grid(200.0, 30.0, Some("auto auto")),
            vec![box10(40.0), box10(30.0)],
        )],
    );
    let auto_flat = pre_flatten(&auto);
    assert!(
        block::supports_tree(&auto_flat),
        "{:?}",
        block::unsupported_reasons(&auto_flat)
    );
    let auto_out = layout_tree(&auto, &auto_flat, 800.0, 600.0).expect("auto");
    assert_eq!(at(&auto_out, 1), (0.0, 0.0, 200.0, 30.0));
    assert_eq!(at(&auto_out, 2), (0.0, 0.0, 40.0, 10.0));
    assert_eq!(at(&auto_out, 3), (105.0, 0.0, 30.0, 10.0));

    let bar = node(
        Style {
            height: Dimension::Px(10.0),
            ..block()
        },
        vec![],
    );
    let fr = node(
        block(),
        vec![node(
            grid(210.0, 20.0, Some("1fr 2fr")),
            vec![bar.clone(), bar],
        )],
    );
    let fr_flat = pre_flatten(&fr);
    assert!(
        block::supports_tree(&fr_flat),
        "{:?}",
        block::unsupported_reasons(&fr_flat)
    );
    let fr_out = layout_tree(&fr, &fr_flat, 800.0, 600.0).expect("fr");
    assert_eq!(at(&fr_out, 2), (0.0, 0.0, 70.0, 10.0));
    assert_eq!(at(&fr_out, 3), (70.0, 0.0, 140.0, 10.0));

    let minmax = node(
        block(),
        vec![node(
            grid(
                200.0,
                20.0,
                Some("minmax(20px,1fr) minmax(50px,max-content)"),
            ),
            vec![box10(40.0), box10(80.0)],
        )],
    );
    let minmax_flat = pre_flatten(&minmax);
    assert!(
        block::supports_tree(&minmax_flat),
        "{:?}",
        block::unsupported_reasons(&minmax_flat)
    );
    let minmax_out = layout_tree(&minmax, &minmax_flat, 800.0, 600.0).expect("minmax");
    assert_eq!(at(&minmax_out, 2), (0.0, 0.0, 40.0, 10.0));
    assert_eq!(at(&minmax_out, 3), (120.0, 0.0, 80.0, 10.0));

    let intrinsic = node(
        block(),
        vec![node(
            grid(300.0, 20.0, Some("min-content max-content")),
            vec![box10(40.0), box10(70.0)],
        )],
    );
    let intrinsic_flat = pre_flatten(&intrinsic);
    assert!(
        block::supports_tree(&intrinsic_flat),
        "{:?}",
        block::unsupported_reasons(&intrinsic_flat)
    );
    let intrinsic_out = layout_tree(&intrinsic, &intrinsic_flat, 800.0, 600.0).expect("intrinsic");
    assert_eq!(at(&intrinsic_out, 1), (0.0, 0.0, 300.0, 20.0));
    assert_eq!(at(&intrinsic_out, 2), (0.0, 0.0, 40.0, 10.0));
    assert_eq!(at(&intrinsic_out, 3), (40.0, 0.0, 70.0, 10.0));

    let ordinary = node(
        block(),
        vec![node(
            Style {
                display: Display::Grid,
                width: Dimension::Px(300.0),
                height: Dimension::Px(20.0),
                grid_template_columns: Some("min-content max-content".to_string()),
                ..Style::default()
            },
            vec![box10(40.0)],
        )],
    );
    let ordinary_flat = pre_flatten(&ordinary);
    assert!(
        block::unsupported_reasons(&ordinary_flat)
            .iter()
            .any(|reason| *reason == "grid:intrinsic-track"),
        "{:?}",
        block::unsupported_reasons(&ordinary_flat)
    );
}

/// `-webkit-box` is the legacy flex box. Headless Chrome, body margin 0:
/// a 100px horizontal box places 40px and 60px children at x=0 and x=40.
/// `-webkit-inline-box` uses that same block flex box (the used display
/// stays `flex`). `-webkit-box-orient` is not stored.
#[test]
fn legacy_webkit_box_matches_a_horizontal_flex_row() {
    use w3cos_std::style::{CustomProperties, FlexDirection};
    // The UA sheet sets `flex-direction: row`. `Style::default()` is column,
    // which is not the used value of an element.
    let mark = |value: &str| {
        let mut style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            width: Dimension::Px(100.0),
            ..Style::default()
        };
        style.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-unrecognized-display".to_string(),
            value.to_string(),
        )]));
        style
    };
    let child = |w| {
        node(
            Style {
                width: Dimension::Px(w),
                height: Dimension::Px(10.0),
                ..block()
            },
            vec![],
        )
    };
    for value in ["-webkit-box", "-webkit-inline-box"] {
        let root = node(
            block(),
            vec![node(mark(value), vec![child(40.0), child(60.0)])],
        );
        let flat = pre_flatten(&root);
        assert!(
            block::supports_tree(&flat),
            "{value} {:?}",
            block::unsupported_reasons(&flat)
        );
        let out = layout_tree(&root, &flat, 800.0, 600.0).expect(value);
        let at = |index| {
            let r = out.iter().find(|(_, i)| *i == index).expect("rect").0;
            (r.x, r.y, r.width, r.height)
        };
        // 0 root, 1 box, 2 first child, 3 second child.
        assert_eq!(at(1), (0.0, 0.0, 100.0, 10.0), "{value}");
        assert_eq!(at(2), (0.0, 0.0, 40.0, 10.0), "{value}");
        assert_eq!(at(3), (40.0, 0.0, 60.0, 10.0), "{value}");
    }
}

/// CSS 2.1 10.3.2 / 10.3.4: an explicit content-box size wins over the
/// missing-image intrinsic size, a block replaced element does not stretch,
/// and auto margins center that border box.
#[test]
fn replaced_content_box_matches_legacy() {
    use w3cos_std::style::{Dimension as D, Display, Spacing};
    let image = |display, margin_left, margin_right| {
        Component::image(
            "missing.png",
            Style {
                display,
                width: D::Px(80.0),
                height: D::Px(40.0),
                margin: Edges {
                    top: Spacing::Px(0.0),
                    right: margin_right,
                    bottom: Spacing::Px(0.0),
                    left: margin_left,
                },
                ..Style::default()
            },
        )
    };
    let block_image = node(
        Style {
            display: Display::Block,
            width: D::Px(200.0),
            ..Style::default()
        },
        vec![image(Display::Block, Spacing::Auto, Spacing::Auto)],
    );
    let out = parity(&block_image);
    assert_eq!(rect(&out, 1), (60.0, 0.0, 80.0, 40.0));
    // An inline image is an atomic inline in the line box (see
    // inline_images_match_chrome); a border-box image still takes legacy.
    let mut border_box = image(Display::Block, Spacing::Px(0.0), Spacing::Px(0.0));
    border_box.style.box_sizing = w3cos_std::style::BoxSizing::BorderBox;
    let root = node(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![border_box],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), vec!["image".to_string()]);
}

/// Headless Chrome, body margin 0. A canvas is a replaced box: attributes
/// are the intrinsic size, a specified aspect-ratio replaces that ratio,
/// and min/max scale both axes when width and height are auto.
#[test]
fn definite_canvas_matches_chrome() {
    use w3cos_std::style::CustomProperties;
    let canvas = |w: u32, h: u32, style: Style| Component::canvas(w, h, style);
    let block_canvas = |w: u32, h: u32, extra: Style| {
        canvas(
            w,
            h,
            Style {
                display: Display::Block,
                ..extra
            },
        )
    };
    let lay = |child: Component| {
        let root = node(block(), vec![child]);
        let flat = pre_flatten(&root);
        assert!(
            block::supports_tree(&flat),
            "{:?}",
            block::unsupported_reasons(&flat)
        );
        layout_tree(&root, &flat, 800.0, 600.0).expect("canvas")
    };
    let at = |out: &[(crate::layout::LayoutRect, usize)], index: usize| rect(out, index);

    // width/height attributes, display:block.
    let out = lay(block_canvas(40, 20, Style::default()));
    assert_eq!(at(&out, 1), (0.0, 0.0, 40.0, 20.0));

    // Missing attributes are the HTML default 300×150.
    let out = lay(block_canvas(300, 150, Style::default()));
    assert_eq!(at(&out, 1), (0.0, 0.0, 300.0, 150.0));

    // width 40px on a 100×50 canvas: height follows the intrinsic ratio.
    let out = lay(block_canvas(
        100,
        50,
        Style {
            width: Dimension::Px(40.0),
            ..Style::default()
        },
    ));
    assert_eq!(at(&out, 1), (0.0, 0.0, 40.0, 20.0));

    // height 20px: width follows the ratio.
    let out = lay(block_canvas(
        100,
        50,
        Style {
            height: Dimension::Px(20.0),
            ..Style::default()
        },
    ));
    assert_eq!(at(&out, 1), (0.0, 0.0, 40.0, 20.0));

    // Both axes specified. The ratio does not override them.
    let out = lay(block_canvas(
        100,
        50,
        Style {
            width: Dimension::Px(40.0),
            height: Dimension::Px(40.0),
            ..Style::default()
        },
    ));
    assert_eq!(at(&out, 1), (0.0, 0.0, 40.0, 40.0));

    // min-width 200 scales 100×50 to 200×100. max-width 40 and max-height
    // 20 each scale it to 40×20.
    let out = lay(block_canvas(
        100,
        50,
        Style {
            min_width: Dimension::Px(200.0),
            ..Style::default()
        },
    ));
    assert_eq!(at(&out, 1), (0.0, 0.0, 200.0, 100.0));
    let out = lay(block_canvas(
        100,
        50,
        Style {
            max_width: Dimension::Px(40.0),
            ..Style::default()
        },
    ));
    assert_eq!(at(&out, 1), (0.0, 0.0, 40.0, 20.0));
    let out = lay(block_canvas(
        100,
        50,
        Style {
            max_height: Dimension::Px(20.0),
            ..Style::default()
        },
    ));
    assert_eq!(at(&out, 1), (0.0, 0.0, 40.0, 20.0));

    // A specified width plus min-height grows only the height.
    let out = lay(block_canvas(
        100,
        50,
        Style {
            width: Dimension::Px(40.0),
            min_height: Dimension::Px(30.0),
            ..Style::default()
        },
    ));
    assert_eq!(at(&out, 1), (0.0, 0.0, 40.0, 30.0));

    // aspect-ratio 5/1 replaces the 100×50 attribute ratio.
    let mut ratio = Style {
        width: Dimension::Px(100.0),
        ..Style::default()
    };
    ratio.custom_properties = Some(CustomProperties::from_iter([(
        "--w3cos-internal-aspect-ratio".to_string(),
        "5".to_string(),
    )]));
    let out = lay(block_canvas(100, 50, ratio));
    assert_eq!(at(&out, 1), (0.0, 0.0, 100.0, 20.0));

    // Content-box padding 3 and border 2 around an 80×40 canvas.
    let out = lay(block_canvas(
        80,
        40,
        Style {
            padding: margins(3.0, 3.0, 3.0, 3.0),
            border_top_width: Some(2.0),
            border_right_width: Some(2.0),
            border_bottom_width: Some(2.0),
            border_left_width: Some(2.0),
            ..Style::default()
        },
    ));
    assert_eq!(at(&out, 1), (0.0, 0.0, 90.0, 50.0));

    // Inline-block stays on legacy: the NG line is 24.1875px and Chrome's
    // is 24px, so a block reference of the same border box would be one-sided.
    let inline = node(
        block(),
        vec![canvas(
            40,
            20,
            Style {
                display: Display::InlineBlock,
                ..Style::default()
            },
        )],
    );
    let flat = pre_flatten(&inline);
    assert_eq!(
        block::unsupported_reasons(&flat),
        vec!["kind:other".to_string()]
    );
}

#[test]
fn colspan_percent_auto_matches_percent_pair() {
    use w3cos_std::style::{CustomProperties, WhiteSpace};
    let cell = |width: Dimension, children: Vec<Component>| {
        node(
            Style {
                display: Display::TableCell,
                width,
                border_width: 1.0,
                ..Style::default()
            },
            children,
        )
    };
    let marker = node(
        Style {
            display: Display::Block,
            width: Dimension::Percent(100.0),
            height: Dimension::Px(20.0),
            ..Style::default()
        },
        vec![],
    );
    let words = Component::text(
        "Lorem ipsum dolor sit amet consectetuer adipiscing",
        Style {
            white_space: WhiteSpace::NoWrap,
            ..Style::default()
        },
    );
    let span_cell = node(
        Style {
            display: Display::TableCell,
            white_space: WhiteSpace::NoWrap,
            border_width: 1.0,
            custom_properties: Some(CustomProperties::from_iter([(
                "--w3cos-internal-table-column-span".to_string(),
                "2".to_string(),
            )])),
            ..Style::default()
        },
        vec![words],
    );
    let table = |second: Dimension| {
        node(
            Style {
                display: Display::Block,
                width: Dimension::Px(800.0),
                ..block()
            },
            vec![node(
                Style {
                    display: Display::Table,
                    width: Dimension::Px(400.0),
                    border_collapse: true,
                    ..Style::default()
                },
                vec![
                    node(
                        Style {
                            display: Display::TableRow,
                            ..Style::default()
                        },
                        vec![
                            cell(Dimension::Percent(90.0), vec![marker.clone()]),
                            cell(second, vec![]),
                        ],
                    ),
                    node(
                        Style {
                            display: Display::TableRow,
                            ..Style::default()
                        },
                        vec![span_cell.clone()],
                    ),
                ],
            )],
        )
    };
    let auto = ng_rects(&table(Dimension::Auto));
    let pair = ng_rects(&table(Dimension::Percent(10.0)));
    let table_auto = rect(&auto, 1);
    let table_pair = rect(&pair, 1);
    let marker_auto = rect(&auto, 4);
    let marker_pair = rect(&pair, 4);
    assert!(
        (table_auto.2 - table_pair.2).abs() < 1.0 && (marker_auto.2 - marker_pair.2).abs() < 1.0,
        "auto table {table_auto:?} marker {marker_auto:?}\npair table {table_pair:?} marker {marker_pair:?}"
    );
}

#[test]
fn text_leaf_cell_contributes_its_inline_size() {
    use w3cos_std::style::CustomProperties;
    // HTML `td` padding is 1px and the UA marker adds another 1px on an
    // auto width. The leaf and the boxed cell must still share a column.
    let cell_style = Style {
        display: Display::TableCell,
        padding: margins(1.0, 1.0, 1.0, 1.0),
        custom_properties: Some(CustomProperties::from_iter([(
            "--w3cos-internal-table-cell-ua-padding".to_string(),
            "1".to_string(),
        )])),
        ..Style::default()
    };
    let leaf = |text: &str| Component::text(text, cell_style.clone());
    let boxed = |text: &str| {
        node(
            cell_style.clone(),
            vec![Component::text(
                text,
                Style {
                    display: Display::Inline,
                    ..Style::default()
                },
            )],
        )
    };
    let table = |cells: Vec<Component>| {
        node(
            block(),
            vec![node(
                Style {
                    display: Display::Table,
                    border_collapse: true,
                    ..Style::default()
                },
                vec![node(
                    Style {
                        display: Display::TableRow,
                        ..Style::default()
                    },
                    cells,
                )],
            )],
        )
    };
    let leaf_root = table(vec![leaf("X"), leaf("X"), leaf("X"), leaf("X")]);
    let box_root = table(vec![boxed("X"), boxed("X"), boxed("X"), boxed("X")]);
    let leaf_out = ng_rects(&leaf_root);
    let box_out = ng_rects(&box_root);
    let leaf_w = rect(&leaf_out, 1).2;
    let box_w = rect(&box_out, 1).2;
    assert!(
        (leaf_w - box_w).abs() < 1.0,
        "text-leaf table {leaf_w} boxed table {box_w}"
    );
    let flat = pre_flatten(&leaf_root);
    let leaf_y = layout_tree_with_text(&leaf_root, &flat, 800.0, 600.0)
        .expect("ng")
        .1
        .into_iter()
        .filter(|piece| piece.text == "X")
        .map(|piece| piece.rect.y)
        .collect::<Vec<_>>();
    let box_flat = pre_flatten(&box_root);
    let box_y = box_out
        .iter()
        .filter(|(rect, index)| {
            matches!(box_flat[*index].kind, w3cos_std::ComponentKind::Text { .. })
                && rect.width > 0.0
        })
        .map(|(rect, _)| rect.y)
        .collect::<Vec<_>>();
    assert_eq!(leaf_y.len(), 4, "{leaf_y:?}");
    assert_eq!(box_y.len(), 4, "{box_y:?}");
    for (leaf, boxed) in leaf_y.iter().zip(&box_y) {
        assert!(
            (leaf - boxed).abs() < 0.05,
            "text-leaf glyph y {leaf_y:?} boxed {box_y:?}"
        );
    }
}

#[test]
fn inline_block_leading_block_shares_the_line_baseline() {
    use w3cos_std::style::AlignSelf;
    let text = |content: &str| {
        Component::text(
            content,
            Style {
                display: Display::Inline,
                ..Style::default()
            },
        )
    };
    let paragraph = |children: Vec<Component>| {
        node(
            Style {
                margin: margins(16.0, 0.0, 16.0, 0.0),
                ..block()
            },
            children,
        )
    };
    let page = |children: Vec<Component>| {
        node(
            block(),
            vec![node(
                Style {
                    display: Display::Table,
                    height: Dimension::Px(200.0),
                    box_sizing: BoxSizing::BorderBox,
                    ..Style::default()
                },
                vec![node(
                    Style {
                        display: Display::TableRow,
                        ..Style::default()
                    },
                    vec![node(
                        Style {
                            display: Display::TableCell,
                            align_self: AlignSelf::FlexEnd,
                            ..Style::default()
                        },
                        vec![paragraph(children)],
                    )],
                )],
            )],
        )
    };
    let simple = page(vec![text("a"), text("bcd"), text("e")]);
    let mixed = page(vec![
        text("a"),
        node(
            Style {
                display: Display::InlineBlock,
                ..Style::default()
            },
            vec![
                Component::text(
                    "x",
                    Style {
                        display: Display::Block,
                        ..Style::default()
                    },
                ),
                text("bcd"),
            ],
        ),
        text("e"),
    ]);
    let simple_out = ng_rects(&simple);
    let mixed_out = ng_rects(&mixed);
    let y_of = |root: &Component, out: &[(LayoutRect, usize)], needle: &str| {
        let flat = pre_flatten(root);
        let index = flat
            .iter()
            .position(|node| {
                matches!(&node.kind, w3cos_std::ComponentKind::Text { content } if content == needle)
            })
            .expect(needle);
        rect(out, index).1
    };
    let simple_h = rect(&simple_out, 1).3;
    let mixed_h = rect(&mixed_out, 1).3;
    let simple_a = y_of(&simple, &simple_out, "a");
    let mixed_a = y_of(&mixed, &mixed_out, "a");
    assert!(
        (simple_h - mixed_h).abs() < 1.0,
        "table height {simple_h} vs {mixed_h}"
    );
    assert!(
        (simple_a - mixed_a).abs() < 1.0,
        "baseline y {simple_a} vs {mixed_a}"
    );
}

/// `display: -webkit-box; -webkit-box-orient: vertical` lays out as a block
/// flow root, and `-webkit-line-clamp: N` keeps its first N lines with an
/// ellipsis on the last. Expected border boxes measured in headless Chrome
/// (tools/layout-ng chrome-boxes, Ahem 20px/1, width 240px): the box is
/// 40px (wrap, 2 of 3 lines), 60px (no clamp, clamp 3), 40px (pre, 2 of 4),
/// 80px (pre, clamp 5 over 4 lines), 20px (pre, 1 of 2); the next block
/// follows at the box's height.
#[test]
fn webkit_box_vertical_clamps_lines() {
    use w3cos_std::style::{CustomProperties, WhiteSpace};
    let _ahem = super::inline_tests::AhemGuard::new();
    let font = Style {
        font_family: Some("Ahem".into()),
        font_size: 20.0,
        line_height: 1.0,
        line_height_is_normal: false,
        ..block()
    };
    // (content, pre, clamp, orient, Chrome height, kept pieces)
    let cases: [(&str, bool, Option<&str>, &str, f32, &[&str]); 7] = [
        (
            "XXXX XXXX XXXX XXXX XXXX",
            false,
            Some("2"),
            "vertical",
            40.0,
            &["XXXX XXXX ", "XXXX XXXX…"],
        ),
        (
            "XXXX XXXX XXXX XXXX XXXX",
            false,
            None,
            "vertical",
            60.0,
            &["XXXX XXXX ", "XXXX XXXX ", "XXXX"],
        ),
        (
            "A\nB\nC\nD",
            true,
            Some("2"),
            "vertical",
            40.0,
            &["A", "B…"],
        ),
        ("A\nB\nC\nD", true, Some("5"), "vertical", 80.0, &[]),
        (
            "XXXX XXXX XXXX XXXX XXXX",
            false,
            Some("3"),
            "vertical",
            60.0,
            &["XXXX XXXX ", "XXXX XXXX ", "XXXX"],
        ),
        ("AA\nBB", true, Some("1"), "vertical", 20.0, &["AA…"]),
        // A horizontal box is the flex row the engine already lays out.
        ("XXXX XXXX", false, Some("1"), "horizontal", 20.0, &[]),
    ];
    for (n, (content, pre, clamp, orient, height, pieces)) in cases.into_iter().enumerate() {
        let mut markers = vec![
            (
                "--w3cos-internal-unrecognized-display".to_string(),
                "-webkit-box".to_string(),
            ),
            (
                "--w3cos-internal-webkit-box-orient".to_string(),
                orient.to_string(),
            ),
        ];
        if let Some(lines) = clamp {
            markers.push((
                "--w3cos-internal-webkit-line-clamp".to_string(),
                lines.to_string(),
            ));
        }
        let white_space = if pre {
            WhiteSpace::Pre
        } else {
            WhiteSpace::Normal
        };
        let boxed = Style {
            display: Display::Flex,
            width: Dimension::Px(240.0),
            white_space,
            custom_properties: Some(CustomProperties::from_iter(markers)),
            ..font.clone()
        };
        let text_style = Style {
            display: Display::Inline,
            white_space,
            ..font.clone()
        };
        // 0 root, 1 box, 2 text, 3 next block.
        let root = node(
            block(),
            vec![
                node(boxed, vec![Component::text(content, text_style)]),
                node(sized(10.0), vec![]),
            ],
        );
        let flat = pre_flatten(&root);
        let Some((rects, texts)) = layout_tree_with_text(&root, &flat, 800.0, 600.0) else {
            // (The horizontal box keeps its flex-row rejection of text.)
            assert_eq!(orient, "horizontal", "case {n}");
            continue;
        };
        assert_eq!(rect(&rects, 1), (0.0, 0.0, 240.0, height), "case {n}");
        assert_eq!(rect(&rects, 3), (0.0, height, 800.0, 10.0), "case {n}");
        let got: Vec<&str> = texts.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(got, pieces, "case {n}");
    }
}

/// A text run directly inside a flex container is an anonymous flex item
/// (CSS Flexbox 4); it is laid out as block text with its min-content as
/// the automatic minimum. Border boxes measured in headless Chrome
/// (getBoundingClientRect, Ahem 20px/1, Chrome 156) for a
/// flex row with margins, `flex: 1 / 2` growth, shrinking to the longest word, a column
/// and nested rows that size to the sum of their items.
#[test]
fn flex_text_items_match_chrome() {
    let _ahem = super::inline_tests::AhemGuard::new();
    let font = Style {
        font_family: Some("Ahem".into()),
        font_size: 20.0,
        line_height: 1.0,
        line_height_is_normal: false,
        ..block()
    };
    let flex = |direction: FlexDirection, width: Option<f32>| Style {
        display: Display::Flex,
        flex_direction: direction,
        width: width.map_or(Dimension::Auto, Dimension::Px),
        ..font.clone()
    };
    let item = |mut style: Style| {
        style.display = Display::InlineBlock;
        style
    };
    let case = std::cell::Cell::new(0);
    let run = |root: Component| {
        case.set(case.get() + 1);
        let flat = pre_flatten(&root);
        let (rects, _) = layout_tree_with_text(&root, &flat, 800.0, 600.0)
            .unwrap_or_else(|| panic!("case {} stays on NG", case.get()));
        rects
    };
    let check = |rects: &[(LayoutRect, usize)], expected: &[(usize, (f32, f32, f32, f32))]| {
        for (index, want) in expected {
            assert_eq!(rect(rects, *index), *want, "box {index}");
        }
    };

    // row-text: margins on a text item, then a sized one that overflows.
    let a = item(Style {
        margin: margins(0.0, 10.0, 0.0, 10.0),
        ..font.clone()
    });
    let b = item(Style {
        width: Dimension::Px(60.0),
        margin: Edges {
            left: px(5.0),
            ..margins(0.0, 0.0, 0.0, 0.0)
        },
        ..font.clone()
    });
    let rects = run(node(
        block(),
        vec![node(
            flex(FlexDirection::Row, Some(300.0)),
            vec![Component::text("XX XX", a), Component::text("YYYYYYYY", b)],
        )],
    ));
    check(
        &rects,
        &[
            (1, (0.0, 0.0, 300.0, 20.0)),
            (2, (10.0, 0.0, 100.0, 20.0)),
            (3, (125.0, 0.0, 60.0, 20.0)),
        ],
    );

    // row-shrink: the first item shrinks to its longest word and wraps.
    let rects = run(node(
        block(),
        vec![node(
            flex(FlexDirection::Row, Some(100.0)),
            vec![
                Component::text("XXXX XXXX", item(font.clone())),
                Component::text("YYY", item(font.clone())),
            ],
        )],
    ));
    check(
        &rects,
        &[
            (1, (0.0, 0.0, 100.0, 40.0)),
            (2, (0.0, 0.0, 80.0, 40.0)),
            (3, (80.0, 0.0, 60.0, 40.0)),
        ],
    );

    // row-grow: flex 1 and flex 2 over auto bases.
    let grow = |factor: f32| Style {
        flex_grow: factor,
        flex_shrink: 1.0,
        flex_basis: Dimension::Px(0.0),
        ..item(font.clone())
    };
    let rects = run(node(
        block(),
        vec![node(
            flex(FlexDirection::Row, Some(300.0)),
            vec![
                Component::text("XX", grow(1.0)),
                Component::text("Y", Style { ..grow(2.0) }),
            ],
        )],
    ));
    check(
        &rects,
        &[
            (1, (0.0, 0.0, 300.0, 20.0)),
            (2, (0.0, 0.0, 100.0, 20.0)),
            (3, (100.0, 0.0, 200.0, 20.0)),
        ],
    );

    // col: a column stretches text items to its width.
    let rects = run(node(
        block(),
        vec![node(
            flex(FlexDirection::Column, Some(200.0)),
            vec![
                Component::text("XX XX", item(font.clone())),
                Component::text(
                    "YYYYYY",
                    item(Style {
                        width: Dimension::Px(80.0),
                        height: Dimension::Px(50.0),
                        ..font.clone()
                    }),
                ),
            ],
        )],
    ));
    check(
        &rects,
        &[
            (1, (0.0, 0.0, 200.0, 70.0)),
            (2, (0.0, 0.0, 200.0, 20.0)),
            (3, (0.0, 20.0, 80.0, 50.0)),
        ],
    );

    // nested: a row container that is itself a flex item sizes to the SUM
    // of its items' margin boxes (342 = 2 + 2 * (160 + 10)), not the widest.
    let wide = |_: ()| {
        item(Style {
            width: Dimension::Em(8.0),
            height: Dimension::Em(1.2),
            margin: margins(5.0, 5.0, 5.0, 5.0),
            ..font.clone()
        })
    };
    let inner = Style {
        border_width: 1.0,
        ..flex(FlexDirection::Row, None)
    };
    let rects = run(node(
        block(),
        vec![node(
            flex(FlexDirection::Row, Some(700.0)),
            vec![node(
                inner,
                vec![
                    Component::text("one", wide(())),
                    Component::text("two", wide(())),
                ],
            )],
        )],
    ));
    check(
        &rects,
        &[
            (1, (0.0, 0.0, 700.0, 36.0)),
            (2, (0.0, 0.0, 342.0, 36.0)),
            (3, (6.0, 6.0, 160.0, 24.0)),
            (4, (176.0, 6.0, 160.0, 24.0)),
        ],
    );

    // nested-auto: gap included; the sibling text item follows the row.
    let inner = Style {
        gap: 7.0,
        ..flex(FlexDirection::Row, None)
    };
    let rects = run(node(
        block(),
        vec![node(
            flex(FlexDirection::Row, Some(700.0)),
            vec![
                node(
                    inner,
                    vec![
                        Component::text("XXX", item(font.clone())),
                        Component::text("YY", item(font.clone())),
                    ],
                ),
                Component::text("ZZZZ", item(font.clone())),
            ],
        )],
    ));
    check(
        &rects,
        &[
            (2, (0.0, 0.0, 107.0, 20.0)),
            (3, (0.0, 0.0, 60.0, 20.0)),
            (4, (67.0, 0.0, 40.0, 20.0)),
            (5, (107.0, 0.0, 80.0, 20.0)),
        ],
    );

    // A text item with padding or a border stays on the legacy engine.
    let padded = Style {
        padding: margins(0.0, 5.0, 0.0, 5.0),
        ..item(font.clone())
    };
    let root = node(
        block(),
        vec![node(
            flex(FlexDirection::Row, Some(300.0)),
            vec![Component::text("XX", padded)],
        )],
    );
    let flat = pre_flatten(&root);
    assert!(layout_tree_with_text(&root, &flat, 800.0, 600.0).is_none());

    // A text run paints from the top of its rect with no line context, so a
    // single line carries the font-content-box fragment an IFC line would
    // (half-leading above the glyphs): 20px Ahem in a 40px line sits at y=10.
    let normal = Style {
        line_height: 2.0,
        display: Display::Inline,
        ..font.clone()
    };
    let root = node(
        block(),
        vec![node(
            flex(FlexDirection::Row, Some(300.0)),
            vec![Component::text("XX", normal)],
        )],
    );
    let flat = pre_flatten(&root);
    let (rects, texts) = layout_tree_with_text(&root, &flat, 800.0, 600.0)
        .expect("a single-line text item lays out in NG");
    check(&rects, &[(2, (0.0, 0.0, 40.0, 40.0))]);
    let piece = texts
        .iter()
        .find(|piece| piece.index == 2)
        .expect("text item has a line fragment");
    assert_eq!(
        (
            piece.rect.x,
            piece.rect.y,
            piece.rect.width,
            piece.rect.height
        ),
        (0.0, 10.0, 40.0, 20.0)
    );
}

/// Inline `<img>` is an atomic inline in the line box (CSS 2.1 10.8.1:
/// its baseline is the bottom margin edge), and min/max clamp the used
/// size. Border boxes measured in headless Chrome 156 (getBoundingClientRect,
/// Ahem 20px/1, 300px wide; testdata/chrome-image.json). Images carry an
/// explicit width and height so the missing file's size does not matter.
#[test]
fn inline_images_match_chrome() {
    use w3cos_std::style::CustomProperties;
    let _ahem = super::inline_tests::AhemGuard::new();
    let font = Style {
        font_family: Some("Ahem".into()),
        font_size: 20.0,
        line_height: 1.0,
        line_height_is_normal: false,
        ..block()
    };
    let text_style = Style {
        display: Display::Inline,
        ..font.clone()
    };
    let img = |extra: Style| {
        Component::image(
            "missing.png",
            Style {
                display: Display::Inline,
                ..extra
            },
        )
    };
    let px = Dimension::Px;
    // (vertical-align: middle is the align_self Center cascade form with no
    // keyword marker; top / bottom carry the marker.)
    let top = || {
        Some(CustomProperties::from_iter([(
            "--w3cos-internal-vertical-align-keyword".to_string(),
            "top".to_string(),
        )]))
    };
    let sized_img = |w: f32, h: f32| Style {
        width: px(w),
        height: px(h),
        ..font.clone()
    };
    let run = |n: usize, children: Vec<Component>| {
        let parent = Style {
            width: px(300.0),
            ..font.clone()
        };
        let root = node(block(), vec![node(parent, children)]);
        let flat = pre_flatten(&root);
        assert!(
            block::supports_tree(&flat),
            "case {n}: {:?}",
            block::unsupported_reasons(&flat)
        );
        let (rects, _) = layout_tree_with_text(&root, &flat, 800.0, 600.0)
            .unwrap_or_else(|| panic!("case {n} stays on NG"));
        rects
    };
    let xx = || Component::text("XX", text_style.clone());

    // img1: on the baseline between text; the line is 24px (4px descent).
    let r = run(1, vec![xx(), img(sized_img(30.0, 20.0)), xx()]);
    assert_eq!(rect(&r, 1), (0.0, 0.0, 300.0, 24.0));
    assert_eq!(rect(&r, 3), (40.0, 0.0, 30.0, 20.0));
    // img2: taller than the strut ascent: the line grows to 44px.
    let r = run(2, vec![xx(), img(sized_img(30.0, 40.0)), xx()]);
    assert_eq!(rect(&r, 1), (0.0, 0.0, 300.0, 44.0));
    assert_eq!(rect(&r, 3), (40.0, 0.0, 30.0, 40.0));
    // img3: vertical-align: middle.
    let r = run(
        3,
        vec![
            xx(),
            img(Style {
                align_self: w3cos_std::style::AlignSelf::Center,
                ..sized_img(30.0, 40.0)
            }),
            xx(),
        ],
    );
    assert_eq!(rect(&r, 1), (0.0, 0.0, 300.0, 40.0));
    assert_eq!(rect(&r, 3), (40.0, 0.0, 30.0, 40.0));
    // img4: inline-block image with margins; the margin box is 30px.
    let r = run(
        4,
        vec![
            xx(),
            img(Style {
                display: Display::InlineBlock,
                margin: margins(5.0, 3.0, 5.0, 3.0),
                ..sized_img(30.0, 20.0)
            }),
            xx(),
        ],
    );
    assert_eq!(rect(&r, 1), (0.0, 0.0, 300.0, 34.0));
    assert_eq!(rect(&r, 3), (43.0, 5.0, 30.0, 20.0));
    // img5: alone, it sits on the strut's baseline (16px ascent).
    let r = run(5, vec![img(sized_img(30.0, 10.0))]);
    assert_eq!(rect(&r, 1), (0.0, 0.0, 300.0, 20.0));
    assert_eq!(rect(&r, 2), (0.0, 6.0, 30.0, 10.0));
    // img6: max-width clamps the width.
    let r = run(
        6,
        vec![
            xx(),
            img(Style {
                max_width: px(60.0),
                ..sized_img(100.0, 20.0)
            }),
            xx(),
        ],
    );
    assert_eq!(rect(&r, 1), (0.0, 0.0, 300.0, 24.0));
    assert_eq!(rect(&r, 3), (40.0, 0.0, 60.0, 20.0));
    // img7: min-height and a min-width in em raise the specified size.
    let r = run(
        7,
        vec![
            xx(),
            img(Style {
                min_height: px(30.0),
                min_width: Dimension::Em(3.0),
                ..sized_img(20.0, 10.0)
            }),
            xx(),
        ],
    );
    assert_eq!(rect(&r, 1), (0.0, 0.0, 300.0, 34.0));
    assert_eq!(rect(&r, 3), (40.0, 0.0, 60.0, 30.0));
    // img8: max-width in em; a second image aligned to the line top.
    let r = run(
        8,
        vec![
            xx(),
            img(Style {
                max_width: Dimension::Em(2.0),
                ..sized_img(100.0, 20.0)
            }),
            xx(),
            img(Style {
                custom_properties: top(),
                align_self: w3cos_std::style::AlignSelf::FlexStart,
                ..sized_img(10.0, 10.0)
            }),
        ],
    );
    assert_eq!(rect(&r, 1), (0.0, 0.0, 300.0, 24.0));
    assert_eq!(rect(&r, 3), (40.0, 0.0, 40.0, 20.0));
    assert_eq!(rect(&r, 5), (120.0, 0.0, 10.0, 10.0));
}

/// A block image whose max-width is below its min-width: the min wins
/// (CSS 2.1 10.4) and the ratio is kept, as in Chrome (WPT
/// css-sizing/aspect-ratio/replaced-element-041; 60x60 image in a 0px box,
/// min-width 100px, max-width 100%).
#[test]
fn replaced_min_over_max_keeps_min() {
    let root = node(
        block(),
        vec![node(
            Style {
                width: Dimension::Px(0.0),
                ..block()
            },
            vec![Component::image(
                "missing.png",
                Style {
                    display: Display::Block,
                    min_width: Dimension::Px(100.0),
                    max_width: Dimension::Percent(1.0),
                    ..Style::default()
                },
            )],
        )],
    );
    let out = ng_rects(&root);
    assert_eq!(rect(&out, 2).2, 100.0);
}
