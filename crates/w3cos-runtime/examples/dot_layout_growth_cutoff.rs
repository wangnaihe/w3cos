// Difference between 63 and 64 measured moved layout boxes in the final card.
use w3cos_core::Value;
use w3cos_runtime::{dom, font_face, incremental_layout, jsdom};

const N: usize = 300;
const LAST: usize = N - 1;
const SHORT: &str = "XXXXXXXXXX";
const LONG: &str = "XXXXXXXXXX XXXXXXXXXX";
type Rect = [f64; 4];

fn styled(tag: &str, parent: u32, id: &str, styles: &[(&str, &str)]) -> u32 {
    let n = dom::create_element(tag);
    if !id.is_empty() {
        dom::set_attribute(n, "id", id);
    }
    for (key, value) in styles {
        dom::set_style_property(n, key, value);
    }
    dom::append_child(parent, n);
    n
}
fn find(id: &str) -> Value {
    jsdom::document_value().call_method("getElementById", vec![Value::string(id)])
}
fn rect(v: &Value) -> Rect {
    let r = v.call_method("getBoundingClientRect", vec![]);
    ["x", "y", "width", "height"].map(|k| r.get_property(k).to_number())
}
fn line_rects(v: &Value) -> Vec<Rect> {
    let rs = v.call_method("getClientRects", vec![]);
    let n = rs.get_property("length").to_u32() as usize;
    assert!(n <= 4);
    (0..n)
        .map(|i| {
            let r = rs.get_property(&i.to_string());
            ["x", "y", "width", "height"].map(|k| r.get_property(k).to_number())
        })
        .collect()
}
fn prepare(long: [bool; 2], count: usize) {
    dom::reset_document();
    jsdom::reset_bridge();
    incremental_layout::invalidate();
    jsdom::set_viewport(160.0, 240.0);
    let body = dom::body_id();
    dom::set_style_property(body, "margin", "0px");
    dom::set_style_property(body, "padding", "0px");
    let sc = styled(
        "div",
        body,
        "scroll",
        &[
            ("width", "160px"),
            ("height", "240px"),
            ("overflow", "hidden"),
        ],
    );
    styled(
        "div",
        body,
        "outside",
        &[("width", "10px"), ("height", "5px")],
    );
    for i in 0..N {
        let c = styled("div", sc, &format!("c-{i}"), &[("width", "140px")]);
        let p = styled(
            "p",
            c,
            &format!("p-{i}"),
            &[
                ("margin", "0px"),
                ("padding", "0px"),
                ("font-family", "Ahem"),
                ("font-size", "10px"),
                ("line-height", "20px"),
                ("white-space", "normal"),
            ],
        );
        let id = match i { 0 => "head", LAST => "tail", _ => "" };
        let text = if (i == 0 && long[0]) || (i == LAST && long[1]) { LONG } else { SHORT };
        let span = styled("span", p, id, &[]);
        dom::append_child(span, dom::create_text_node(text));
        if i == LAST {
            for j in 0..count {
                let height = if j + 1 == count { (81 - count) as f64 } else { 1.0 };
                styled("div", c, &format!("s-{i}-{j}"), &[("height", &format!("{height}px"))]);
            }
        } else {
            styled("div", c, &format!("s-{i}"), &[("height", "80px")]);
        }
    }
}
#[derive(Debug, PartialEq)]
struct Snapshot {
    cards: Vec<Rect>,
    paras: Vec<Rect>,
    spacers: Vec<Rect>,
    tail_spacers: Vec<Rect>,
    lines: [Vec<Rect>; 2],
}
fn snapshot(long: [bool; 2], count: usize) -> Snapshot {
    assert_eq!(rect(&find("scroll")), [0.0, 0.0, 160.0, 240.0]);
    assert_eq!(rect(&find("outside")), [0.0, 240.0, 10.0, 5.0]);
    let d = Snapshot {
        cards: (0..N).map(|i| rect(&find(&format!("c-{i}")))).collect(),
        paras: (0..N).map(|i| rect(&find(&format!("p-{i}")))).collect(),
        spacers: (0..LAST).map(|i| rect(&find(&format!("s-{i}")))).collect(),
        tail_spacers: (0..count).map(|j| rect(&find(&format!("s-{LAST}-{j}")))).collect(),
        lines: [line_rects(&find("head")), line_rects(&find("tail"))],
    };
    assert_eq!(d.lines[0].len(), if long[0] { 2 } else { 1 });
    assert_eq!(d.lines[1].len(), if long[1] { 2 } else { 1 });
    for i in 0..N {
        let y = 100.0 * i as f64 + if long[0] && i > 0 { 20.0 } else { 0.0 };
        let wraps = (i == 0 && long[0]) || (i == LAST && long[1]);
        let h = if wraps { 40.0 } else { 20.0 };
        assert_eq!(d.cards[i], [0.0, y, 140.0, h+80.0], "card {i}");
        assert_eq!(d.paras[i], [0.0, y, 140.0, h], "paragraph {i}");
        if i == LAST {
            for (j, actual) in d.tail_spacers.iter().enumerate() {
                let height = if j + 1 == count { (81 - count) as f64 } else { 1.0 };
                assert_eq!(*actual, [0.0, y + h + j as f64, 140.0, height], "tail spacer {j}");
            }
        } else {
            assert_eq!(d.spacers[i], [0.0, y+h, 140.0, 80.0], "spacer {i}");
        }
    }
    d
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let count: usize = args.get(2).ok_or("need spacer count")?.parse()?;
    assert!([63, 64].contains(&count));
    font_face::FontRegistry::global().register_for_owner(
        u64::MAX - 513,
        font_face::FontFace {
            family: "Ahem".into(),
            src: font_face::FontSource::Bytes(std::fs::read(args.get(1).ok_or("need font")?)?),
            ..Default::default()
        },
    )?;
    let states = [[false, false], [false, true], [true, false], [true, true]];
    let cold = states.map(|state| { prepare(state, count); snapshot(state, count) });
    prepare([false, false], count);
    assert_eq!(snapshot(states[0], count), cold[0]);
    let head = find("head").get_property("firstChild");
    let tail = find("tail").get_property("firstChild");
    for (i, (node, grow, key)) in [
        (&tail, true, 1), (&tail, false, 0), (&head, true, 2),
        (&tail, true, 3), (&tail, false, 2), (&head, false, 0),
    ].into_iter().enumerate() {
        eprintln!("[boundary-phase] edit={i}");
        node.set_property("data", Value::string(if grow { LONG } else { SHORT }));
        assert_eq!(snapshot(states[key], count), cold[key], "fresh layout parity at phase {i}");
    }
    println!("{}", serde_json::json!({"fresh_parity":true,"count":count,"edits":6}));
    Ok(())
}
