//! S2a inline formatting context: generated IFC corpus judged against
//! Chrome (tools/layout-ng/chrome_oracle.py, Ahem so glyph advances and
//! font metrics are exact in both engines).

use super::*;
use crate::layout::pre_flatten;
use w3cos_std::style::{Dimension, Display, Edges, Spacing, Style, TextAlign, WhiteSpace};

const AHEM_OWNER: u64 = 0x1a70_75ce_a4e1;

/// Registers the pinned WPT Ahem face for the guard's lifetime.
/// Registers the Ahem fixture for one test.  Tests holding it run one at a
/// time: the registry owner is shared, so a guard dropped by a concurrent
/// test would unregister the font under another.
pub(super) struct AhemGuard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

static AHEM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl AhemGuard {
    pub(super) fn new() -> Self {
        let lock = AHEM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/layout_ng/testdata/Ahem.ttf"
        ))
        .expect("Ahem fixture");
        crate::font_face::FontRegistry::global()
            .register_for_owner(
                AHEM_OWNER,
                crate::font_face::FontFace {
                    family: "Ahem".into(),
                    src: crate::font_face::FontSource::Bytes(bytes),
                    ..Default::default()
                },
            )
            .expect("register Ahem");
        AhemGuard(lock)
    }
}

impl Drop for AhemGuard {
    fn drop(&mut self) {
        crate::font_face::FontRegistry::global().clear_owner(AHEM_OWNER);
    }
}

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
    fn of<T: Copy>(&mut self, choices: &[T]) -> T {
        choices[self.pick(choices.len() as u64) as usize]
    }
}

#[derive(Clone, Copy, Debug)]
enum LineHeight {
    Number(f32),
    Px(f32),
}

fn font(style: &mut Style, size: f32, line_height: LineHeight) {
    style.font_family = Some("Ahem".into());
    style.font_size = size;
    style.line_height_is_normal = false;
    match line_height {
        LineHeight::Number(n) => {
            style.line_height = n;
            style.line_height_computed_px = None;
        }
        LineHeight::Px(px) => {
            style.line_height = px / size;
            style.line_height_computed_px = Some(px);
        }
    }
}

fn line_height_css(style: &Style) -> String {
    match style.line_height_computed_px {
        Some(px) => format!("{px}px"),
        None => format!("{}", style.line_height),
    }
}

/// One generated case: a block root with IFC containers of Ahem words.
fn random_case(rng: &mut Rng) -> Component {
    random_case_with(rng, 0)
}

/// `ext`: white-space pre (preserved, repeated and line-edge spaces) and
/// multi-word items whose internal opportunities NG must not need.
/// `level` 2 adds pre-wrap and pre-line containers.
fn random_case_with(rng: &mut Rng, level: u8) -> Component {
    let ext = level >= 1;
    let mut containers = Vec::new();
    for _ in 0..1 + rng.pick(3) {
        // Sizes with an integral 0.8em Ahem ascent: Blink rounds font
        // ascent/descent to integers, the non-Skia test backend does not,
        // and that font-backend precision is not what this corpus tests.
        let size = rng.of(&[10.0, 15.0, 20.0]);
        let line_height = if rng.pick(2) == 0 {
            LineHeight::Number(rng.of(&[1.0, 1.25, 2.0]))
        } else {
            LineHeight::Px(rng.of(&[12.0, 20.0, 37.0]))
        };
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(rng.of(&[40.0, 75.0, 120.0, 200.0, 333.0])),
            text_align: rng.of(&[TextAlign::Start, TextAlign::Center, TextAlign::Right]),
            white_space: match (level, rng.pick(10)) {
                (_, 0) => WhiteSpace::NoWrap,
                (1, 1..=3) => WhiteSpace::Pre,
                (2, 1) => WhiteSpace::Pre,
                (2, 2..=5) => WhiteSpace::PreWrap,
                (2, 6..=7) => WhiteSpace::PreLine,
                _ => WhiteSpace::Normal,
            },
            ..Style::default()
        };
        let m = rng.of(&[0.0, 5.0, 10.0]);
        style.margin = Edges {
            top: Spacing::Px(m),
            right: Spacing::Px(0.0),
            bottom: Spacing::Px(m),
            left: Spacing::Px(0.0),
        };
        font(&mut style, size, line_height);
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        let mut items = Vec::new();
        let count = 1 + rng.pick(10);
        for i in 0..count {
            let pre = matches!(style.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap);
            let mut text = String::new();
            if ext && pre && rng.pick(4) == 0 {
                text.push_str(&" ".repeat(1 + rng.pick(2) as usize));
            }
            text.push_str(&"X".repeat(1 + rng.pick(6) as usize));
            if ext && rng.pick(4) == 0 {
                for _ in 0..1 + rng.pick(2) {
                    let gap = if pre { 1 + rng.pick(2) as usize } else { 1 };
                    text.push_str(&" ".repeat(gap));
                    text.push_str(&"X".repeat(1 + rng.pick(4) as usize));
                }
            }
            let separator = rng.pick(10);
            let last = i + 1 == count;
            if separator < 6 && !last {
                text.push(' ');
            }
            let mut item = Style {
                display: Display::Inline,
                white_space: style.white_space,
                text_align: style.text_align,
                ..Style::default()
            };
            if rng.pick(5) == 0 {
                let own = rng.of(&[10.0, 15.0, 25.0, 30.0, 40.0]);
                let lh = if rng.pick(2) == 0 {
                    line_height
                } else {
                    LineHeight::Number(1.0)
                };
                font(&mut item, own, lh);
            } else {
                font(&mut item, size, line_height);
            }
            items.push(Component::text(text, item.clone()));
            if (6..8).contains(&separator) && !last {
                items.push(Component::text(" ", item));
            }
        }
        containers.push(Component::boxed(style, items));
    }
    let root = Style {
        display: Display::Block,
        ..Style::default()
    };
    Component::boxed(root, containers)
}

fn case_html(flat: &[crate::layout::FlatNodeInfo]) -> String {
    let mut html = String::from("<div id=n0 style=\"display:flow-root\">");
    let mut i = 1;
    while i < flat.len() {
        let s = flat[i].style;
        let Dimension::Px(width) = s.width else {
            unreachable!()
        };
        let Spacing::Px(margin) = s.margin.top else {
            unreachable!()
        };
        html.push_str(&format!(
            "<div id=n{i} style=\"width:{width}px;margin:{margin}px 0;font:{}px/{} Ahem;text-align:{};white-space:{}\">",
            s.font_size,
            line_height_css(s),
            match s.text_align {
                TextAlign::Center => "center",
                TextAlign::Right => "right",
                _ => "start",
            },
            match s.white_space {
                WhiteSpace::NoWrap => "nowrap",
                WhiteSpace::Pre => "pre",
                WhiteSpace::PreWrap => "pre-wrap",
                WhiteSpace::PreLine => "pre-line",
                _ => "normal",
            },
        ));
        let end = flat[i].subtree_end;
        for t in i + 1..end {
            let ts = flat[t].style;
            let w3cos_std::ComponentKind::Text { content } = flat[t].kind else {
                unreachable!()
            };
            html.push_str(&format!(
                "<span id=n{t} style=\"font-size:{}px;line-height:{}\">{}</span>",
                ts.font_size,
                line_height_css(ts),
                content
            ));
        }
        html.push_str("</div>");
        i = end;
    }
    html.push_str("</div>");
    html
}

const CASES: usize = 300;
const SEED: u64 = 0x5eed_1a70_0115_0002;

fn rects_json(set: &[(LayoutRect, usize)]) -> String {
    let mut v: Vec<_> = set
        .iter()
        .map(|(r, i)| (*i, [r.x, r.y, r.width, r.height]))
        .collect();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter()
        .map(|(i, r)| format!("\"{i}\":{r:?}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Every generated IFC is inside S2a, and NG lays it out.  With
/// W3COS_NG_INLINE_DUMP=path the cases (HTML + NG/legacy rects) are written
/// for the Chrome oracle.
#[test]
fn generated_inline_trees_are_supported() {
    let _ahem = AhemGuard::new();
    let mut rng = Rng(SEED);
    let dump = std::env::var_os("W3COS_NG_INLINE_DUMP");
    let mut lines = String::new();
    for case in 0..CASES {
        let root = random_case(&mut rng);
        let flat = pre_flatten(&root);
        assert_eq!(
            block::unsupported_reasons(&flat),
            Vec::<String>::new(),
            "case {case}"
        );
        let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
        if dump.is_some() {
            let legacy = crate::layout::compute(&root, 800.0, 600.0).expect("legacy");
            lines.push_str(&format!(
                "{{\"case\":{case},\"fonts\":[\"Ahem\"],\"html\":{:?},\"legacy\":{{{}}},\"ng\":{{{}}}}}\n",
                case_html(&flat),
                rects_json(&legacy),
                rects_json(&ng)
            ));
        }
    }
    if let Some(path) = dump {
        std::fs::write(path, lines).expect("dump");
    }
}

/// Checked-in Chrome expectations (testdata/generated-inline-chrome.json;
/// rects in 1/64 px).  A whitespace-only item removed at a line edge has no
/// NG fragment and a zero-width Chrome rect.
#[test]
fn generated_inline_trees_match_chrome() {
    let golden: std::collections::HashMap<String, std::collections::HashMap<String, [i64; 4]>> =
        serde_json::from_str(include_str!("testdata/generated-inline-chrome.json"))
            .expect("golden json");
    let _ahem = AhemGuard::new();
    let mut rng = Rng(SEED);
    let mut failures = Vec::new();
    for case in 0..CASES {
        let root = random_case(&mut rng);
        let flat = pre_flatten(&root);
        let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
        let expected = &golden[&case.to_string()];
        for (key, want) in expected {
            let index: usize = key.parse().expect("index");
            let got = ng
                .iter()
                .find(|(_, i)| *i == index)
                .map(|(r, _)| [r.x, r.y, r.width, r.height].map(|v| (v * 64.0).round() as i64));
            let ok = match got {
                Some(got) => got == *want,
                None => want[2] == 0,
            };
            if !ok {
                failures.push((case, index, got, *want));
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

const EXT_CASES: usize = 300;
const EXT_SEED: u64 = 0x5eed_1a70_0115_0003;
const WRAP_SEED: u64 = 0x5eed_1a70_0115_0004;

type Golden = std::collections::HashMap<String, std::collections::HashMap<String, [i64; 4]>>;

/// Extended corpora: NG may fall back at runtime when a line would need a
/// break inside an item; the rest must be supported and are dumped with
/// `dump_env`=path for the Chrome oracle.
fn ext_trees_are_supported(level: u8, seed: u64, dump_env: &str, min_handled: usize) {
    let _ahem = AhemGuard::new();
    let mut rng = Rng(seed);
    let dump = std::env::var_os(dump_env);
    let mut lines = String::new();
    let mut handled = 0;
    for case in 0..EXT_CASES {
        let root = random_case_with(&mut rng, level);
        let flat = pre_flatten(&root);
        assert_eq!(
            block::unsupported_reasons(&flat),
            Vec::<String>::new(),
            "case {case}"
        );
        let Some(ng) = layout_tree(&root, &flat, 800.0, 600.0) else {
            continue;
        };
        handled += 1;
        if dump.is_some() {
            let legacy = crate::layout::compute(&root, 800.0, 600.0).expect("legacy");
            lines.push_str(&format!(
                "{{\"case\":{case},\"fonts\":[\"Ahem\"],\"html\":{:?},\"legacy\":{{{}}},\"ng\":{{{}}}}}\n",
                case_html(&flat),
                rects_json(&legacy),
                rects_json(&ng)
            ));
        }
    }
    assert!(
        handled >= min_handled,
        "NG handled only {handled}/{EXT_CASES}"
    );
    if let Some(path) = dump {
        std::fs::write(path, lines).expect("dump");
    }
}

/// Every case NG lays out matches Chrome, and every golden case is still
/// laid out by NG.
fn ext_trees_match_chrome(level: u8, seed: u64, golden: &str) {
    let golden: Golden = serde_json::from_str(golden).expect("golden json");
    let _ahem = AhemGuard::new();
    let mut rng = Rng(seed);
    let mut failures = Vec::new();
    for case in 0..EXT_CASES {
        let root = random_case_with(&mut rng, level);
        let flat = pre_flatten(&root);
        let ng = layout_tree(&root, &flat, 800.0, 600.0);
        let Some(expected) = golden.get(&case.to_string()) else {
            assert!(ng.is_none(), "case {case} has no Chrome golden");
            continue;
        };
        let ng = ng.unwrap_or_else(|| panic!("case {case} fell back"));
        for (key, want) in expected {
            let index: usize = key.parse().expect("index");
            let got = ng
                .iter()
                .find(|(_, i)| *i == index)
                .map(|(r, _)| [r.x, r.y, r.width, r.height].map(|v| (v * 64.0).round() as i64));
            let ok = match got {
                Some(got) => got == *want,
                None => want[2] == 0,
            };
            if !ok {
                failures.push((case, index, got, *want));
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

/// pre, repeated/edge spaces, multi-word items (seed ...0003).
#[test]
fn generated_inline_ext_trees_are_supported() {
    ext_trees_are_supported(1, EXT_SEED, "W3COS_NG_INLINE_EXT_DUMP", EXT_CASES / 3);
}

#[test]
fn generated_inline_ext_trees_match_chrome() {
    ext_trees_match_chrome(
        1,
        EXT_SEED,
        include_str!("testdata/generated-inline-ext-chrome.json"),
    );
}

/// pre-wrap (hanging preserved spaces, breaks only after a space run) and
/// pre-line (seed ...0004).
#[test]
fn generated_inline_wrap_trees_are_supported() {
    // Items with leading preserved spaces often need a break inside them.
    ext_trees_are_supported(2, WRAP_SEED, "W3COS_NG_INLINE_WRAP_DUMP", EXT_CASES / 5);
}

#[test]
fn generated_inline_wrap_trees_match_chrome() {
    ext_trees_match_chrome(
        2,
        WRAP_SEED,
        include_str!("testdata/generated-inline-wrap-chrome.json"),
    );
}

/// Quirks "line height calculation quirk": a line holding only span text
/// ignores the root inline box's strut; root-level text (the space between
/// two spans) brings it back.  Expected rects are Chrome's for
/// `<div style="font:20px/1 Ahem;width:300px"><span style="font-size:10px">XX</span></div>`
/// plus a second div `<span ..>XX</span> <span ..>X</span>`, with and without
/// `<!doctype html>`.
#[test]
fn quirks_mode_drops_the_root_strut() {
    let _ahem = AhemGuard::new();
    let tree = |quirks: bool| {
        let container = || {
            let mut style = Style {
                display: Display::Block,
                width: Dimension::Px(300.0),
                ..Style::default()
            };
            font(&mut style, 20.0, LineHeight::Number(1.0));
            style
                .custom_properties
                .get_or_insert_with(Default::default)
                .insert(inline::IFC_MARKER.into(), "1".into());
            style
        };
        let item = |size: f32| {
            let mut style = Style {
                display: Display::Inline,
                ..Style::default()
            };
            font(&mut style, size, LineHeight::Number(1.0));
            style
        };
        let mut root = Style {
            display: Display::Block,
            ..Style::default()
        };
        if quirks {
            root.custom_properties
                .get_or_insert_with(Default::default)
                .insert(w3cos_dom::document::QUIRKS_MODE_MARKER.into(), "1".into());
        }
        Component::boxed(
            root,
            vec![
                Component::boxed(container(), vec![Component::text("XX", item(10.0))]),
                Component::boxed(
                    container(),
                    vec![
                        Component::text("XX", item(10.0)),
                        Component::text(" ", item(20.0)),
                        Component::text("X", item(10.0)),
                    ],
                ),
            ],
        )
    };
    // (index, [x, y, w, h]) from Chrome, relative to the root.
    let quirks: [(usize, [f32; 4]); 5] = [
        (1, [0.0, 0.0, 300.0, 10.0]),
        (2, [0.0, 0.0, 20.0, 10.0]),
        (3, [0.0, 10.0, 300.0, 20.0]),
        (4, [0.0, 18.0, 20.0, 10.0]),
        (6, [40.0, 18.0, 10.0, 10.0]),
    ];
    let standards: [(usize, [f32; 4]); 5] = [
        (1, [0.0, 0.0, 300.0, 20.0]),
        (2, [0.0, 8.0, 20.0, 10.0]),
        (3, [0.0, 20.0, 300.0, 20.0]),
        (4, [0.0, 28.0, 20.0, 10.0]),
        (6, [40.0, 28.0, 10.0, 10.0]),
    ];
    for (mode, expected) in [(true, quirks), (false, standards)] {
        let root = tree(mode);
        let flat = pre_flatten(&root);
        assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
        let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
        for (index, want) in expected {
            let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
            assert_eq!(
                [r.x, r.y, r.width, r.height],
                want,
                "quirks={mode} #{index}"
            );
        }
    }
}

/// CSS Text 3 4.1.3 (WPT white-space-pre-wrap-trailing-spaces-001): at the
/// end of the block, preserved pre-wrap spaces hang only conditionally, so
/// the part that fits counts for alignment.  Rects from Chrome.
#[test]
fn pre_wrap_spaces_at_block_end_hang_conditionally() {
    let _ahem = AhemGuard::new();
    let block = |width: f32, align: TextAlign, text: &str| {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(width),
            text_align: align,
            white_space: WhiteSpace::PreWrap,
            ..Style::default()
        };
        font(&mut style, 20.0, LineHeight::Number(1.0));
        let mut item = Style {
            display: Display::Inline,
            white_space: WhiteSpace::PreWrap,
            ..Style::default()
        };
        font(&mut item, 20.0, LineHeight::Number(1.0));
        Component::boxed(style, vec![Component::text(text, item)])
    };
    let spaces = " ".repeat(32);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            block(140.0, TextAlign::Right, "A  "),
            block(180.0, TextAlign::Center, &format!("    S{spaces}")),
            block(180.0, TextAlign::Center, " S "),
            block(140.0, TextAlign::Right, "AA AA  "),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    // (text index, x, width) from Chrome.
    for (index, x, width) in [
        (2, 80.0, 60.0),
        (4, 0.0, 740.0),
        (6, 60.0, 60.0),
        (8, 0.0, 140.0),
    ] {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        // The overflowing space run's advance (Chrome 740) is capped by the
        // shared text measurement in both engines; only the offset matters.
        let width = if index == 4 { r.width } else { width };
        assert_eq!((r.x, r.width), (x, width), "#{index}");
    }
}

/// NGLineBreaker forced breaks: a `<br>` leaf ends the line, adds no
/// advance, removes collapsible spaces before it; consecutive breaks make an
/// empty strut-height line; a final break adds none.  Rects from Chrome.
#[test]
fn forced_breaks_end_lines() {
    let _ahem = AhemGuard::new();
    let container = |line_height: f32, align: TextAlign| {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(300.0),
            text_align: align,
            ..Style::default()
        };
        font(&mut style, 20.0, LineHeight::Number(line_height));
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        style
    };
    let text = |content: &str, size: f32, line_height: f32| {
        let mut style = Style {
            display: Display::Inline,
            ..Style::default()
        };
        font(&mut style, size, LineHeight::Number(line_height));
        Component::text(content, style)
    };
    let br = "\u{2028}";
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            Component::boxed(
                container(1.0, TextAlign::Start),
                vec![
                    text("XX", 20.0, 1.0),
                    text("Y", 10.0, 1.0),
                    text(br, 20.0, 1.0),
                    text("XXX", 20.0, 1.0),
                ],
            ),
            Component::boxed(
                container(1.0, TextAlign::Start),
                vec![
                    text("XX ", 20.0, 1.0),
                    text("Y", 10.0, 1.0),
                    text(" ", 20.0, 1.0),
                    text(br, 20.0, 1.0),
                    text("XXX ", 20.0, 1.0),
                    text("Y", 10.0, 1.0),
                ],
            ),
            Component::boxed(
                container(2.0, TextAlign::Center),
                vec![
                    text("XX", 20.0, 2.0),
                    text("Y", 10.0, 2.0),
                    text(br, 20.0, 2.0),
                    text(br, 20.0, 2.0),
                    text("XXX", 20.0, 2.0),
                ],
            ),
            Component::boxed(
                container(1.0, TextAlign::Start),
                vec![
                    text("XX", 20.0, 1.0),
                    text("Y", 10.0, 1.0),
                    text(br, 20.0, 1.0),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    // (index, [x, y, w, h]) from Chrome (text rects; containers).
    let expected: [(usize, [f32; 4]); 16] = [
        (1, [0.0, 0.0, 300.0, 40.0]),
        (2, [0.0, 0.0, 40.0, 20.0]),
        (3, [40.0, 8.0, 10.0, 10.0]),
        (5, [0.0, 20.0, 60.0, 20.0]),
        (6, [0.0, 40.0, 300.0, 40.0]),
        (7, [0.0, 40.0, 60.0, 20.0]),
        (8, [60.0, 48.0, 10.0, 10.0]),
        (11, [0.0, 60.0, 80.0, 20.0]),
        (12, [80.0, 68.0, 10.0, 10.0]),
        (13, [0.0, 80.0, 300.0, 120.0]),
        (14, [125.0, 90.0, 40.0, 20.0]),
        (15, [165.0, 98.0, 10.0, 10.0]),
        (18, [120.0, 170.0, 60.0, 20.0]),
        (19, [0.0, 200.0, 300.0, 20.0]),
        (20, [0.0, 200.0, 40.0, 20.0]),
        (21, [40.0, 208.0, 10.0, 10.0]),
    ];
    for (index, want) in expected {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        assert_eq!([r.x, r.y, r.width, r.height], want, "#{index}");
    }
}

/// Preserved newlines (CSS Text 3 4.1.1) in a sole leaf: one line per
/// segment, an empty segment is an empty line, a trailing newline of a
/// paragraph leaf adds none.  Container heights from Chrome.
#[test]
fn preserved_newlines_make_lines() {
    let _ahem = AhemGuard::new();
    let case = |white_space: WhiteSpace, line_height: f32, text: &str, paragraph: bool| {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(300.0),
            white_space,
            ..Style::default()
        };
        font(&mut style, 20.0, LineHeight::Number(line_height));
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        let mut item = Style {
            display: Display::Inline,
            white_space,
            ..Style::default()
        };
        font(&mut item, 20.0, LineHeight::Number(line_height));
        if paragraph {
            item.custom_properties
                .get_or_insert_with(Default::default)
                .insert(inline::LINE_WIDTH_MARKER.into(), "1".into());
        }
        Component::boxed(style, vec![Component::text(text, item)])
    };
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            case(WhiteSpace::Pre, 1.0, "X\nYYY", false),
            case(WhiteSpace::PreWrap, 2.0, "AA BB\nCC", false),
            case(WhiteSpace::PreLine, 2.0, "AA   BB\nCC\n", true),
            case(WhiteSpace::Pre, 2.0, "A\n\nB", false),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    for (index, y, height) in [
        (1, 0.0, 40.0),
        (3, 40.0, 80.0),
        (5, 120.0, 80.0),
        (7, 200.0, 120.0),
    ] {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        assert_eq!((r.y, r.height), (y, height), "#{index}");
    }
}

/// NGInlineBoxState edges: a decorated inline box's word items carry its
/// inline-start (first) and inline-end (last) margin/border/padding as
/// advance and the block-axis padding/border around the painted box; they
/// never grow the line box (CSS 2.1 10.8.1).  Rects from Chrome (span
/// border boxes split per word item).
#[test]
fn inline_box_edges_take_advance_not_line_height() {
    let _ahem = AhemGuard::new();
    let container = || {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(300.0),
            ..Style::default()
        };
        font(&mut style, 20.0, LineHeight::Number(1.0));
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        style
    };
    let text = |content: &str, edges: Option<(Edges, Edges, [f32; 4])>| {
        let mut style = Style {
            display: Display::Inline,
            ..Style::default()
        };
        font(&mut style, 20.0, LineHeight::Number(1.0));
        if let Some((padding, margin, [t, r, b, l])) = edges {
            style.padding = padding;
            style.margin = margin;
            style.border_top_width = Some(t);
            style.border_right_width = Some(r);
            style.border_bottom_width = Some(b);
            style.border_left_width = Some(l);
        }
        Component::text(content, style)
    };
    let px = Spacing::Px;
    let e = |t: f32, r: f32, b: f32, l: f32| Edges {
        top: px(t),
        right: px(r),
        bottom: px(b),
        left: px(l),
    };
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            // AA <span style="border:5px;padding:3px 7px">BB</span> CC
            Component::boxed(
                container(),
                vec![
                    text("AA ", None),
                    text(
                        "BB",
                        Some((e(3.0, 7.0, 3.0, 7.0), e(0.0, 0.0, 0.0, 0.0), [5.0; 4])),
                    ),
                    text(" ", None),
                    text("CC", None),
                ],
            ),
            // AA <span style="padding:10px 4px;margin:0 6px">BB CC</span> DD
            Component::boxed(
                container(),
                vec![
                    text("AA ", None),
                    text(
                        "BB",
                        Some((e(10.0, 0.0, 10.0, 4.0), e(0.0, 0.0, 0.0, 6.0), [0.0; 4])),
                    ),
                    text(
                        " ",
                        Some((e(10.0, 0.0, 10.0, 0.0), e(0.0, 0.0, 0.0, 0.0), [0.0; 4])),
                    ),
                    text(
                        "CC",
                        Some((e(10.0, 4.0, 10.0, 0.0), e(0.0, 6.0, 0.0, 0.0), [0.0; 4])),
                    ),
                    text(" ", None),
                    text("DD", None),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 12] = [
        (1, [0.0, 0.0, 300.0, 20.0]),
        (2, [0.0, 0.0, 60.0, 20.0]),
        (3, [60.0, -8.0, 64.0, 36.0]),
        (4, [124.0, 0.0, 20.0, 20.0]),
        (5, [144.0, 0.0, 40.0, 20.0]),
        (6, [0.0, 20.0, 300.0, 20.0]),
        (7, [0.0, 20.0, 60.0, 20.0]),
        (8, [66.0, 10.0, 44.0, 40.0]),
        (9, [110.0, 10.0, 20.0, 40.0]),
        (10, [130.0, 10.0, 44.0, 40.0]),
        (11, [180.0, 20.0, 20.0, 20.0]),
        (12, [200.0, 20.0, 40.0, 20.0]),
    ];
    for (index, want) in expected {
        let r = ng.iter().find(|(_, i)| *i == index).expect("rect").0;
        assert_eq!([r.x, r.y, r.width, r.height], want, "#{index}");
    }
}

/// Nested inline boxes (NGInlineBoxState stack): text in a box aligns on
/// the shared baseline with the box's own strut, open/close tags add the
/// boxes' inline edges, and each box gets one border-box fragment around its
/// font content area.  Rects from Chrome (Ahem):
///   <div>AA <span fs10 lh30px>BB <span fs40 lh1>CC</span> DD</span> EE</div>
///   <div>AA <span padding:3px 7px;border:5px;margin:0 4px>BB
///        <span padding:0 2px;fs30 lh1>CC</span></span> DD</div>
///   <div width:100px>AAAA <span fs10 lh1>BB</span> CC</div>
///   <div center>A <span padding:0 10px;fs30 lh50px>B</span></div>
#[test]
fn nested_inline_boxes_share_the_baseline() {
    let _ahem = AhemGuard::new();
    let container = |width: f32, align: TextAlign| {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(width),
            text_align: align,
            ..Style::default()
        };
        font(&mut style, 20.0, LineHeight::Number(1.0));
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        style
    };
    let styled = |size: f32, lh: LineHeight| {
        let mut style = Style {
            display: Display::Inline,
            ..Style::default()
        };
        font(&mut style, size, lh);
        style
    };
    let text =
        |content: &str, size: f32, lh: LineHeight| Component::text(content, styled(size, lh));
    let px = Spacing::Px;
    let n1 = LineHeight::Number(1.0);
    let s3 = Style {
        padding: Edges {
            top: px(3.0),
            right: px(7.0),
            bottom: px(3.0),
            left: px(7.0),
        },
        margin: Edges {
            top: px(0.0),
            right: px(4.0),
            bottom: px(0.0),
            left: px(4.0),
        },
        border_width: 5.0,
        ..styled(20.0, n1)
    };
    let s4 = Style {
        padding: Edges {
            top: px(0.0),
            right: px(2.0),
            bottom: px(0.0),
            left: px(2.0),
        },
        ..styled(30.0, n1)
    };
    let s6 = Style {
        padding: Edges {
            top: px(0.0),
            right: px(10.0),
            bottom: px(0.0),
            left: px(10.0),
        },
        ..styled(30.0, LineHeight::Px(50.0))
    };
    let lh30 = LineHeight::Px(30.0);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            Component::boxed(
                container(300.0, TextAlign::Start),
                vec![
                    text("AA ", 20.0, n1),
                    Component::boxed(
                        styled(10.0, lh30),
                        vec![
                            text("BB ", 10.0, lh30),
                            Component::boxed(styled(40.0, n1), vec![text("CC", 40.0, n1)]),
                            text(" DD", 10.0, lh30),
                        ],
                    ),
                    text(" EE", 20.0, n1),
                ],
            ),
            Component::boxed(
                container(300.0, TextAlign::Start),
                vec![
                    text("AA ", 20.0, n1),
                    Component::boxed(
                        s3,
                        vec![
                            text("BB ", 20.0, n1),
                            Component::boxed(s4, vec![text("CC", 30.0, n1)]),
                        ],
                    ),
                    text(" DD", 20.0, n1),
                ],
            ),
            Component::boxed(
                container(100.0, TextAlign::Start),
                vec![
                    text("AAAA ", 20.0, n1),
                    Component::boxed(styled(10.0, n1), vec![text("BB", 10.0, n1)]),
                    text(" CC", 20.0, n1),
                ],
            ),
            Component::boxed(
                container(300.0, TextAlign::Center),
                vec![
                    text("A ", 20.0, n1),
                    Component::boxed(s6, vec![text("B", 30.0, LineHeight::Px(50.0))]),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 24] = [
        (1, [0.0, 0.0, 300.0, 44.0]),
        (2, [0.0, 16.0, 60.0, 20.0]),
        (3, [60.0, 24.0, 140.0, 10.0]),
        (4, [60.0, 24.0, 30.0, 10.0]),
        (5, [90.0, 0.0, 80.0, 40.0]),
        (6, [90.0, 0.0, 80.0, 40.0]),
        (7, [170.0, 24.0, 30.0, 10.0]),
        (8, [200.0, 16.0, 60.0, 20.0]),
        (9, [0.0, 44.0, 300.0, 30.0]),
        (10, [0.0, 52.0, 60.0, 20.0]),
        (11, [64.0, 44.0, 148.0, 36.0]),
        (12, [76.0, 52.0, 60.0, 20.0]),
        (13, [136.0, 44.0, 64.0, 30.0]),
        (14, [138.0, 44.0, 60.0, 30.0]),
        (15, [216.0, 52.0, 60.0, 20.0]),
        (16, [0.0, 74.0, 100.0, 40.0]),
        (17, [0.0, 74.0, 80.0, 20.0]),
        (18, [0.0, 102.0, 20.0, 10.0]),
        (19, [0.0, 102.0, 20.0, 10.0]),
        (20, [20.0, 94.0, 60.0, 20.0]),
        (21, [0.0, 114.0, 300.0, 50.0]),
        (22, [105.0, 132.0, 40.0, 20.0]),
        (23, [145.0, 124.0, 50.0, 30.0]),
        (24, [155.0, 124.0, 30.0, 30.0]),
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

/// `vertical-align: top | bottom` (CSS 2.1 10.8.1, Blink
/// NGInlineLayoutStateStack): the aligned subtree (box or item plus its
/// baseline-aligned descendants) is placed against the line box edge and
/// grows the line away from it when taller.  Rects from Chrome (Ahem):
///   <div>AA <span top fs40 lh1>BB</span> CC</div>
///   <div>AA <span bottom fs10 lh50px>BB <span fs30 lh1>CC</span></span> DD</div>
///   <div>AA <span top fs10 lh1>bb</span> CC <span fs40 lh1>DD</span></div>
///   <div>X <span top><span fs80>A</span> x</span></div>
#[test]
fn line_relative_vertical_align_places_subtrees() {
    let _ahem = AhemGuard::new();
    let container = || {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(300.0),
            ..Style::default()
        };
        font(&mut style, 20.0, LineHeight::Number(1.0));
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        style
    };
    let styled = |size: f32, lh: LineHeight, valign: Option<&str>| {
        let mut style = Style {
            display: Display::Inline,
            ..Style::default()
        };
        font(&mut style, size, lh);
        if let Some(keyword) = valign {
            style.align_self = if keyword == "top" {
                w3cos_std::style::AlignSelf::FlexStart
            } else {
                w3cos_std::style::AlignSelf::FlexEnd
            };
            style
                .custom_properties
                .get_or_insert_with(Default::default)
                .insert(inline::VALIGN_MARKER.into(), keyword.into());
        }
        style
    };
    let n1 = LineHeight::Number(1.0);
    let text =
        |content: &str, size: f32, lh: LineHeight| Component::text(content, styled(size, lh, None));
    let lh50 = LineHeight::Px(50.0);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            Component::boxed(
                container(),
                vec![
                    text("AA ", 20.0, n1),
                    Component::boxed(styled(40.0, n1, Some("top")), vec![text("BB", 40.0, n1)]),
                    text(" CC", 20.0, n1),
                ],
            ),
            Component::boxed(
                container(),
                vec![
                    text("AA ", 20.0, n1),
                    Component::boxed(
                        styled(10.0, lh50, Some("bottom")),
                        vec![
                            text("BB ", 10.0, lh50),
                            Component::boxed(styled(30.0, n1, None), vec![text("CC", 30.0, n1)]),
                        ],
                    ),
                    text(" DD", 20.0, n1),
                ],
            ),
            Component::boxed(
                container(),
                vec![
                    text("AA ", 20.0, n1),
                    Component::text("bb", styled(10.0, n1, Some("top"))),
                    text(" CC ", 20.0, n1),
                    text("DD", 40.0, n1),
                ],
            ),
            Component::boxed(
                container(),
                vec![
                    text("X ", 20.0, n1),
                    Component::boxed(
                        styled(20.0, n1, Some("top")),
                        vec![
                            Component::boxed(styled(80.0, n1, None), vec![text("A", 80.0, n1)]),
                            text(" x", 20.0, n1),
                        ],
                    ),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 23] = [
        (1, [0.0, 0.0, 300.0, 40.0]),
        (2, [0.0, 0.0, 60.0, 20.0]),
        (3, [60.0, 0.0, 80.0, 40.0]),
        (4, [60.0, 0.0, 80.0, 40.0]),
        (5, [140.0, 0.0, 60.0, 20.0]),
        (6, [0.0, 40.0, 300.0, 50.0]),
        (7, [0.0, 70.0, 60.0, 20.0]),
        (8, [60.0, 60.0, 90.0, 10.0]),
        (9, [60.0, 60.0, 30.0, 10.0]),
        (10, [90.0, 44.0, 60.0, 30.0]),
        (11, [90.0, 44.0, 60.0, 30.0]),
        (12, [150.0, 70.0, 60.0, 20.0]),
        (13, [0.0, 90.0, 300.0, 40.0]),
        (14, [0.0, 106.0, 60.0, 20.0]),
        (15, [60.0, 90.0, 20.0, 10.0]),
        (16, [80.0, 106.0, 80.0, 20.0]),
        (17, [160.0, 90.0, 80.0, 40.0]),
        (18, [0.0, 130.0, 300.0, 80.0]),
        (19, [0.0, 130.0, 40.0, 20.0]),
        (20, [40.0, 178.0, 120.0, 20.0]),
        (21, [40.0, 130.0, 80.0, 80.0]),
        (22, [40.0, 130.0, 80.0, 80.0]),
        (23, [120.0, 178.0, 40.0, 20.0]),
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

/// Inline-blocks as atomic inlines (NGInlineItem kAtomicInline): the margin
/// box is the item, its baseline the last line box's or, without one, the
/// bottom margin edge (CSS 2.1 10.8.1); lines may break before and after it.
/// Rects from Chrome (Ahem):
///   <div>AA <ib 50x40 margin:3px 4px;border:2px;padding:1px></ib> BB</div>
///   <div>AA <ib width:60px;fs10 lh1;padding-top:5px>xy</ib> BB</div>
///   <div width:100px>AAAA<ib 40x10></ib>BB</div>
///   <div>AA<ib 20x20 margin-bottom:10px></ib></div>
#[test]
fn inline_blocks_are_atomic_items() {
    let _ahem = AhemGuard::new();
    let container = |width: f32| {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(width),
            ..Style::default()
        };
        font(&mut style, 20.0, LineHeight::Number(1.0));
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        style
    };
    let n1 = LineHeight::Number(1.0);
    let text = |content: &str, size: f32| {
        let mut style = Style {
            display: Display::Inline,
            ..Style::default()
        };
        font(&mut style, size, LineHeight::Number(1.0));
        Component::text(content, style)
    };
    let px = Spacing::Px;
    let edges = |t: f32, r: f32, b: f32, l: f32| Edges {
        top: px(t),
        right: px(r),
        bottom: px(b),
        left: px(l),
    };
    let ib = |w: f32, h: Option<f32>, margin: Edges, padding: Edges, border: f32, size: f32| {
        let mut style = Style {
            display: Display::InlineBlock,
            width: Dimension::Px(w),
            height: h.map_or(Dimension::Auto, Dimension::Px),
            margin,
            padding,
            border_width: border,
            ..Style::default()
        };
        font(&mut style, size, n1);
        style
    };
    let zero = edges(0.0, 0.0, 0.0, 0.0);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            Component::boxed(
                container(300.0),
                vec![
                    text("AA ", 20.0),
                    Component::boxed(
                        ib(
                            50.0,
                            Some(40.0),
                            edges(3.0, 4.0, 3.0, 4.0),
                            edges(1.0, 1.0, 1.0, 1.0),
                            2.0,
                            20.0,
                        ),
                        vec![],
                    ),
                    text(" BB", 20.0),
                ],
            ),
            Component::boxed(
                container(300.0),
                vec![
                    text("AA ", 20.0),
                    Component::boxed(
                        ib(60.0, None, zero, edges(5.0, 0.0, 0.0, 0.0), 0.0, 10.0),
                        vec![text("xy", 10.0)],
                    ),
                    text(" BB", 20.0),
                ],
            ),
            Component::boxed(
                container(100.0),
                vec![
                    text("AAAA", 20.0),
                    Component::boxed(ib(40.0, Some(10.0), zero, zero, 0.0, 20.0), vec![]),
                    text("BB", 20.0),
                ],
            ),
            Component::boxed(
                container(300.0),
                vec![
                    text("AA", 20.0),
                    Component::boxed(
                        ib(
                            20.0,
                            Some(20.0),
                            edges(0.0, 0.0, 10.0, 0.0),
                            zero,
                            0.0,
                            20.0,
                        ),
                        vec![],
                    ),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 16] = [
        (1, [0.0, 0.0, 300.0, 56.0]),
        (2, [0.0, 36.0, 60.0, 20.0]),
        (3, [64.0, 3.0, 56.0, 46.0]),
        (4, [124.0, 36.0, 60.0, 20.0]),
        (5, [0.0, 56.0, 300.0, 20.0]),
        (6, [0.0, 56.0, 60.0, 20.0]),
        (7, [60.0, 59.0, 60.0, 15.0]),
        (8, [60.0, 64.0, 20.0, 10.0]),
        (9, [120.0, 56.0, 60.0, 20.0]),
        (10, [0.0, 76.0, 100.0, 40.0]),
        (11, [0.0, 76.0, 80.0, 20.0]),
        (12, [0.0, 102.0, 40.0, 10.0]),
        (13, [40.0, 96.0, 40.0, 20.0]),
        (14, [0.0, 116.0, 300.0, 34.0]),
        (15, [0.0, 130.0, 40.0, 20.0]),
        (16, [40.0, 116.0, 20.0, 20.0]),
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

/// CSS2 10.3.9 shrink-to-fit inline-blocks (NGBlockNode::ComputeMinMaxSizes):
/// max-content that fits, min-content vs. available, block children's
/// margin boxes, nested atomics, max-width below min-content.  Rects from
/// Chrome (Ahem).
#[test]
fn inline_blocks_shrink_to_fit() {
    let _ahem = AhemGuard::new();
    let n1 = LineHeight::Number(1.0);
    let container = |width: f32| {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(width),
            ..Style::default()
        };
        font(&mut style, 20.0, n1);
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        style
    };
    let text = |content: &str, size: f32| {
        let mut style = Style {
            display: Display::Inline,
            ..Style::default()
        };
        font(&mut style, size, n1);
        Component::text(content, style)
    };
    let ib = |size: f32| {
        let mut style = Style {
            display: Display::InlineBlock,
            ..Style::default()
        };
        font(&mut style, size, n1);
        style
    };
    let block = |w: f32, margin_left: f32| Style {
        display: Display::Block,
        width: Dimension::Px(w),
        height: Dimension::Px(10.0),
        margin: Edges {
            left: Spacing::Px(margin_left),
            ..Edges::default()
        },
        ..Style::default()
    };
    let px = Spacing::Px;
    let mut padded = ib(20.0);
    padded.padding = Edges {
        top: px(5.0),
        right: px(5.0),
        bottom: px(5.0),
        left: px(5.0),
    };
    padded.border_width = 2.0;
    let mut outer = ib(20.0);
    outer.border_width = 3.0;
    outer.margin.left = px(4.0);
    outer.margin.right = px(4.0);
    outer
        .custom_properties
        .get_or_insert_with(Default::default)
        .insert(inline::IFC_MARKER.into(), "1".into());
    let mut narrow = ib(20.0);
    narrow.max_width = Dimension::Px(30.0);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            Component::boxed(
                container(300.0),
                vec![
                    text("AA ", 20.0),
                    Component::boxed(ib(10.0), vec![text("xy zz", 10.0)]),
                    text(" BB", 20.0),
                ],
            ),
            Component::boxed(
                container(100.0),
                vec![Component::boxed(
                    ib(20.0),
                    vec![text("AAAA ", 20.0), text("BBBB ", 20.0), text("CC", 20.0)],
                )],
            ),
            Component::boxed(
                container(300.0),
                vec![
                    text("A", 20.0),
                    Component::boxed(
                        padded,
                        vec![
                            Component::boxed(block(40.0, 7.0), vec![]),
                            Component::boxed(block(20.0, 0.0), vec![]),
                        ],
                    ),
                    text("B", 20.0),
                ],
            ),
            Component::boxed(
                container(300.0),
                vec![
                    text("A", 20.0),
                    Component::boxed(
                        outer,
                        vec![Component::boxed(ib(10.0), vec![text("XX Y", 10.0)])],
                    ),
                ],
            ),
            Component::boxed(
                container(300.0),
                vec![
                    text("A", 20.0),
                    Component::boxed(narrow, vec![text("BB ", 20.0), text("CC", 20.0)]),
                    text("D", 20.0),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 27] = [
        (1, [0.0, 0.0, 300.0, 20.0]),
        (2, [0.0, 0.0, 60.0, 20.0]),
        (3, [60.0, 8.0, 50.0, 10.0]),
        (4, [60.0, 8.0, 50.0, 10.0]),
        (5, [110.0, 0.0, 60.0, 20.0]),
        (6, [0.0, 20.0, 100.0, 60.0]),
        (7, [0.0, 20.0, 100.0, 60.0]),
        (8, [0.0, 20.0, 80.0, 20.0]),
        (9, [0.0, 40.0, 80.0, 20.0]),
        (10, [0.0, 60.0, 40.0, 20.0]),
        (11, [0.0, 80.0, 300.0, 38.0]),
        (12, [0.0, 98.0, 20.0, 20.0]),
        (13, [20.0, 80.0, 61.0, 34.0]),
        (14, [34.0, 87.0, 40.0, 10.0]),
        (15, [27.0, 97.0, 20.0, 10.0]),
        (16, [81.0, 98.0, 20.0, 20.0]),
        (17, [0.0, 118.0, 300.0, 26.0]),
        (18, [0.0, 121.0, 20.0, 20.0]),
        (19, [24.0, 118.0, 46.0, 26.0]),
        (20, [27.0, 129.0, 40.0, 10.0]),
        (21, [27.0, 129.0, 40.0, 10.0]),
        (22, [0.0, 144.0, 300.0, 40.0]),
        (23, [0.0, 164.0, 20.0, 20.0]),
        (24, [20.0, 144.0, 30.0, 40.0]),
        (25, [20.0, 144.0, 40.0, 20.0]),
        (26, [20.0, 164.0, 40.0, 20.0]),
        (27, [50.0, 164.0, 20.0, 20.0]),
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

/// Atomic inlines with `vertical-align: top | bottom` (their margin box is
/// the aligned subtree) and text leaves the cascade gives `display:
/// inline-block` (atomic block text leaves).  Rects from Chrome (Ahem).
#[test]
fn atomic_inlines_align_to_the_line_box() {
    use w3cos_std::style::AlignSelf;
    let _ahem = AhemGuard::new();
    let n1 = LineHeight::Number(1.0);
    let container = || {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(300.0),
            ..Style::default()
        };
        font(&mut style, 20.0, n1);
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        style
    };
    let text = |content: &str| {
        let mut style = Style {
            display: Display::Inline,
            ..Style::default()
        };
        font(&mut style, 20.0, n1);
        Component::text(content, style)
    };
    let align = |style: &mut Style, top: bool| {
        style.align_self = if top {
            AlignSelf::FlexStart
        } else {
            AlignSelf::FlexEnd
        };
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                inline::VALIGN_MARKER.into(),
                if top { "top" } else { "bottom" }.into(),
            );
    };
    let ib = |w: f32, h: f32, valign: Option<bool>| {
        let mut style = Style {
            display: Display::InlineBlock,
            width: Dimension::Px(w),
            height: Dimension::Px(h),
            ..Style::default()
        };
        font(&mut style, 20.0, n1);
        if let Some(top) = valign {
            align(&mut style, top);
        }
        Component::boxed(style, vec![])
    };
    let text_ib = |content: &str, size: f32, valign: Option<bool>| {
        let mut style = Style {
            display: Display::InlineBlock,
            ..Style::default()
        };
        font(&mut style, size, n1);
        if let Some(top) = valign {
            align(&mut style, top);
        }
        Component::text(content, style)
    };
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            Component::boxed(
                container(),
                vec![
                    text("AA"),
                    ib(20.0, 50.0, Some(true)),
                    text("BB"),
                    ib(20.0, 30.0, Some(false)),
                ],
            ),
            Component::boxed(
                container(),
                vec![
                    text("AA"),
                    text_ib("xy", 10.0, None),
                    text("B"),
                    text_ib("C", 40.0, Some(true)),
                ],
            ),
            Component::boxed(
                container(),
                vec![
                    text("A"),
                    text_ib("p q", 10.0, Some(false)),
                    ib(10.0, 60.0, None),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 14] = [
        (1, [0.0, 0.0, 300.0, 50.0]),
        (2, [0.0, 0.0, 40.0, 20.0]),
        (3, [40.0, 0.0, 20.0, 50.0]),
        (4, [60.0, 0.0, 40.0, 20.0]),
        (5, [100.0, 20.0, 20.0, 30.0]),
        (6, [0.0, 50.0, 300.0, 40.0]),
        (7, [0.0, 50.0, 40.0, 20.0]),
        (8, [40.0, 58.0, 20.0, 10.0]),
        (9, [60.0, 50.0, 20.0, 20.0]),
        (10, [80.0, 50.0, 40.0, 40.0]),
        (11, [0.0, 90.0, 300.0, 64.0]),
        (12, [0.0, 134.0, 20.0, 20.0]),
        (13, [20.0, 144.0, 30.0, 10.0]),
        (14, [50.0, 90.0, 10.0, 60.0]),
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

/// direction: rtl without reordering: block children placed from the right
/// edge (CSS2 10.3.3, Blink ResolveInlineAutoMargins against the container
/// direction), and rtl IFCs of strong-L text (one UAX #9 level-2 run, so
/// logical order) aligned start = right, overflowing lines start-aligned.
/// Rects from Chrome (Ahem).
#[test]
fn rtl_blocks_and_lines_start_on_the_right() {
    use w3cos_std::style::TextDirection;
    let _ahem = AhemGuard::new();
    let n1 = LineHeight::Number(1.0);
    let rtl = |mut style: Style| {
        style.direction = TextDirection::Rtl;
        style
    };
    let child = |width: Dimension, left: Spacing, right: Spacing, border: f32| {
        Component::boxed(
            rtl(Style {
                display: Display::Block,
                width,
                height: Dimension::Px(10.0),
                margin: Edges {
                    left,
                    right,
                    ..Edges::default()
                },
                border_width: border,
                ..Style::default()
            }),
            vec![],
        )
    };
    let paragraph = |width: f32, align: TextAlign, ws: WhiteSpace, children: Vec<Component>| {
        let mut style = rtl(Style {
            display: Display::Block,
            width: Dimension::Px(width),
            text_align: align,
            white_space: ws,
            ..Style::default()
        });
        font(&mut style, 20.0, n1);
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        Component::boxed(style, children)
    };
    let text = |content: &str, ws: WhiteSpace| {
        let mut style = rtl(Style {
            display: Display::Inline,
            white_space: ws,
            ..Style::default()
        });
        font(&mut style, 20.0, n1);
        Component::text(content, style)
    };
    let normal = WhiteSpace::Normal;
    let mut span = rtl(Style {
        display: Display::Inline,
        ..Style::default()
    });
    font(&mut span, 20.0, n1);
    span.padding.left = Spacing::Px(5.0);
    let px = Spacing::Px;
    let zero = px(0.0);
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            Component::boxed(
                rtl(Style {
                    display: Display::Block,
                    width: Dimension::Px(300.0),
                    height: Dimension::Px(10.0),
                    ..Style::default()
                }),
                vec![
                    child(Dimension::Px(100.0), zero, px(20.0), 0.0),
                    child(Dimension::Px(100.0), px(30.0), zero, 0.0),
                    child(Dimension::Px(100.0), px(10.0), Spacing::Auto, 0.0),
                    child(Dimension::Px(400.0), zero, zero, 0.0),
                    child(Dimension::Auto, px(5.0), px(20.0), 2.0),
                ],
            ),
            paragraph(
                300.0,
                TextAlign::Start,
                normal,
                vec![
                    text("AA ", normal),
                    Component::boxed(span, vec![text("BB", normal)]),
                    text(" CC", normal),
                ],
            ),
            paragraph(
                300.0,
                TextAlign::End,
                normal,
                vec![text("AA ", normal), text("BB", normal)],
            ),
            paragraph(
                50.0,
                TextAlign::Start,
                WhiteSpace::NoWrap,
                vec![
                    text("AAAA ", WhiteSpace::NoWrap),
                    text("BB", WhiteSpace::NoWrap),
                ],
            ),
            paragraph(
                100.0,
                TextAlign::Start,
                normal,
                vec![
                    text("AAA ", normal),
                    text("BBB ", normal),
                    text("CC", normal),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 21] = [
        (1, [0.0, 0.0, 300.0, 10.0]),
        (2, [180.0, 0.0, 100.0, 10.0]),
        (3, [200.0, 10.0, 100.0, 10.0]),
        (4, [10.0, 20.0, 100.0, 10.0]),
        (5, [-100.0, 30.0, 400.0, 10.0]),
        (6, [5.0, 40.0, 275.0, 14.0]),
        (7, [0.0, 10.0, 300.0, 20.0]),
        (8, [135.0, 10.0, 60.0, 20.0]),
        (9, [195.0, 10.0, 45.0, 20.0]),
        (10, [200.0, 10.0, 40.0, 20.0]),
        (11, [240.0, 10.0, 60.0, 20.0]),
        (12, [0.0, 30.0, 300.0, 20.0]),
        (13, [0.0, 30.0, 60.0, 20.0]),
        (14, [60.0, 30.0, 40.0, 20.0]),
        (15, [0.0, 50.0, 50.0, 20.0]),
        (16, [-90.0, 50.0, 100.0, 20.0]),
        (17, [10.0, 50.0, 40.0, 20.0]),
        (18, [0.0, 70.0, 100.0, 60.0]),
        (19, [40.0, 70.0, 60.0, 20.0]),
        (20, [40.0, 90.0, 60.0, 20.0]),
        (21, [60.0, 110.0, 40.0, 20.0]),
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

/// vertical-align: middle on text leaves and atomics (Blink
/// ComputeAlignmentBaselineShift kMiddle: the midpoint half the parent's
/// x-height above its baseline).  Rects from Chrome (Ahem, x-height 0.8em).
#[test]
fn middle_aligned_items_shift_their_baseline() {
    use w3cos_std::style::AlignSelf;
    let _ahem = AhemGuard::new();
    let n1 = LineHeight::Number(1.0);
    let container = || {
        let mut style = Style {
            display: Display::Block,
            width: Dimension::Px(300.0),
            ..Style::default()
        };
        font(&mut style, 20.0, n1);
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(inline::IFC_MARKER.into(), "1".into());
        style
    };
    let text = |content: &str, size: f32, display: Display, middle: bool| {
        let mut style = Style {
            display,
            align_self: if middle {
                AlignSelf::Center
            } else {
                AlignSelf::Auto
            },
            ..Style::default()
        };
        font(&mut style, size, n1);
        Component::text(content, style)
    };
    let mut ib = Style {
        display: Display::InlineBlock,
        width: Dimension::Px(20.0),
        height: Dimension::Px(50.0),
        align_self: AlignSelf::Center,
        ..Style::default()
    };
    font(&mut ib, 20.0, n1);
    let inline = Display::Inline;
    let root = Component::boxed(
        Style {
            display: Display::Block,
            ..Style::default()
        },
        vec![
            Component::boxed(
                container(),
                vec![
                    text("AA", 20.0, inline, false),
                    text("B", 40.0, inline, true),
                    Component::boxed(ib, vec![]),
                    text("CC", 20.0, inline, false),
                ],
            ),
            Component::boxed(
                container(),
                vec![
                    text("AA", 20.0, inline, false),
                    text("xy", 10.0, inline, true),
                    text("Q", 30.0, Display::InlineBlock, true),
                ],
            ),
        ],
    );
    let flat = pre_flatten(&root);
    assert_eq!(block::unsupported_reasons(&flat), Vec::<String>::new());
    let ng = layout_tree(&root, &flat, 800.0, 600.0).expect("ng");
    let expected: [(usize, [f32; 4]); 9] = [
        (1, [0.0, 0.0, 300.0, 50.0]),
        (2, [0.0, 17.0, 40.0, 20.0]),
        (3, [40.0, 5.0, 40.0, 40.0]),
        (4, [80.0, 0.0, 20.0, 50.0]),
        (5, [100.0, 17.0, 40.0, 20.0]),
        (6, [0.0, 50.0, 300.0, 30.0]),
        (7, [0.0, 57.0, 40.0, 20.0]),
        (8, [40.0, 60.0, 20.0, 10.0]),
        (9, [60.0, 50.0, 30.0, 30.0]),
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
