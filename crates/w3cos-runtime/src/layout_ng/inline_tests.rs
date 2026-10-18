//! S2a inline formatting context: generated IFC corpus judged against
//! Chrome (tools/layout-ng/chrome_oracle.py, Ahem so glyph advances and
//! font metrics are exact in both engines).

use super::*;
use crate::layout::pre_flatten;
use w3cos_std::style::{Dimension, Display, Edges, Spacing, Style, TextAlign, WhiteSpace};

const AHEM_OWNER: u64 = 0x1a70_75ce_a4e1;

/// Registers the pinned WPT Ahem face for the guard's lifetime.
pub(super) struct AhemGuard;

impl AhemGuard {
    pub(super) fn new() -> Self {
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
        AhemGuard
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
