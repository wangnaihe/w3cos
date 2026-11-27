// Exercise CSSOM owner maintenance across real-wrap and same-height text mutations.
use std::time::Instant;
use w3cos_core::Value;
use w3cos_runtime::{dom, font_face, incremental_layout, jsdom};

const N: usize = 1200;
const LAST: usize = N - 1;
const WIDE: &str = "XXXXXXXXXXX";
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
fn prepare(long: [bool; 2]) {
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
        let text = if i == 0 && long[0] { LONG } else if i == LAST && long[1] { WIDE } else { SHORT };
        let span = styled("span", p, id, &[]);
        dom::append_child(span, dom::create_text_node(text));
        styled("div", c, &format!("s-{i}"), &[("height", "80px")]);
    }
}
#[derive(Debug, PartialEq)]
struct Snapshot {
    cards: Vec<Rect>,
    paras: Vec<Rect>,
    spacers: Vec<Rect>,
    lines: [Vec<Rect>; 2],
}
fn snapshot(long: [bool; 2]) -> Snapshot {
    assert_eq!(rect(&find("scroll")), [0.0, 0.0, 160.0, 240.0]);
    assert_eq!(rect(&find("outside")), [0.0, 240.0, 10.0, 5.0]);
    let d = Snapshot {
        cards: (0..N).map(|i| rect(&find(&format!("c-{i}")))).collect(),
        paras: (0..N).map(|i| rect(&find(&format!("p-{i}")))).collect(),
        spacers: (0..N).map(|i| rect(&find(&format!("s-{i}")))).collect(),
        lines: [line_rects(&find("head")), line_rects(&find("tail"))],
    };
    assert_eq!(d.lines[0].len(), if long[0] { 2 } else { 1 });
    assert_eq!(d.lines[1].len(), 1);
    assert_eq!(d.lines[1][0][2], if long[1] { 110.0 } else { 100.0 });
    for i in 0..N {
        let y = 100.0 * i as f64 + if long[0] && i > 0 { 20.0 } else { 0.0 };
        let wraps = i == 0 && long[0];
        let h = if wraps { 40.0 } else { 20.0 };
        assert_eq!(d.cards[i], [0.0, y, 140.0, h+80.0], "card {i}");
        assert_eq!(d.paras[i], [0.0, y, 140.0, h], "paragraph {i}");
        assert_eq!(d.spacers[i], [0.0, y+h, 140.0, 80.0], "spacer {i}");
    }
    d
}
fn measured_edit(pos: usize, grow: bool) -> (u64, i64) {
    let span = find(if pos == 0 { "head" } else { "tail" });
    let node = span.get_property("firstChild");
    let measured = if pos == 0 { find("p-0") } else { span };
    let text = Value::string(if !grow { SHORT } else if pos == 0 { LONG } else { WIDE });
    let prop = if pos == 0 { "height" } else { "width" };
    let expected = if pos == 0 { if grow { 40.0 } else { 20.0 } } else if grow { 110.0 } else { 100.0 };
    let cpu = cpu_ns().expect("thread clock");
    let t = Instant::now();
    node.set_property("data", text);
    let actual = measured.call_method("getBoundingClientRect", vec![]).get_property(prop).to_number();
    let w = t.elapsed().as_nanos() as u64;
    let c = cpu_ns().expect("thread clock") - cpu;
    assert_eq!(actual, expected);
    (w, c)
}

fn mixed() {
    let states = [[false, false], [false, true], [true, false], [true, true]];
    let cold = states.map(|state| { prepare(state); snapshot(state) });
    prepare([false, false]);
    assert_eq!(snapshot([false, false]), cold[0]);
    let mut last = cold[0].lines[1][0];
    let mut records = Vec::new();
    for (i, (pos, grow, key)) in [
        (LAST, true, 1), (0, true, 3), (LAST, false, 2),
        (0, false, 0), (LAST, true, 1), (LAST, false, 0),
    ].into_iter().enumerate() {
        eprintln!("[mixed-phase] edit={i}");
        measured_edit(pos, grow);
        let actual = snapshot(states[key]);
        if i == 1 || i == 3 {
            last[1] += if i == 1 { 20.0 } else { -20.0 };
            assert_eq!(actual.lines[1][0], last, "the untouched tail client rect must translate");
        }
        assert_eq!(actual, cold[key], "mixed scenario edit {i}");
        last = actual.lines[1][0];
        records.push(serde_json::json!({"edit":i,"position":pos,"grow":grow,
                                      "tail_rect":last}));
    }
    println!("{}", serde_json::json!({"fresh_parity":true,"edits":records}));
}

fn cpu_ns() -> std::io::Result<i64> {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: t is initialized writable stack storage for the host ABI.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(t.tv_sec * 1_000_000_000 + t.tv_nsec)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(2).ok_or("need a mode")?.as_str();
    font_face::FontRegistry::global().register_for_owner(
        u64::MAX - 513,
        font_face::FontFace {
            family: "Ahem".into(),
            src: font_face::FontSource::Bytes(std::fs::read(args.get(1).ok_or("need Ahem.ttf")?)?),
            ..Default::default()
        },
    )?;
    if mode == "check" {
        mixed();
        return Ok(());
    }
    assert!(["cold-tail", "primed-tail", "wide"].contains(&mode));
    prepare([false, false]);
    let cold = snapshot([false, false]);
    let first_wide = if mode != "cold-tail" {
        let first = measured_edit(0, true);
        measured_edit(0, false);
        assert_eq!(snapshot([false, false]), cold);
        Some(first)
    } else {
        None
    };
    let wide = mode == "wide";
    let span = find(if wide { "head" } else { "tail" });
    let target = if wide { find("p-0") } else { span.clone() };
    let prop = if wide { "height" } else { "width" };
    let (short_size, long_size) = if wide { (20.0, 40.0) } else { (100.0, 110.0) };
    let node = span.get_property("firstChild");
    let short = Value::string(SHORT);
    let long = Value::string(if wide { LONG } else { WIDE });
    let ops = if wide { 32 } else { 128 };
    let mut rows = Vec::new();
    for batch in 0..26 {
        let mut correct = 0;
        let c = cpu_ns()?;
        let t = Instant::now();
        for i in 0..ops {
            let grow = i % 2 == 0;
            node.set_property("data", if grow { long.clone() } else { short.clone() });
            let v = target.call_method("getBoundingClientRect", vec![]).get_property(prop).to_number();
            correct += u32::from(v == if grow { long_size } else { short_size });
        }
        let elapsed = t.elapsed().as_nanos();
        let cpu = cpu_ns()? - c;
        assert_eq!(correct, ops);
        if batch >= 6 {
            rows.push(serde_json::json!({"wall_ns":elapsed as u64,"thread_cpu_ns":cpu}));
        }
    }
    assert_eq!(snapshot([false, false]), cold);
    println!("{}", serde_json::json!({"mode":mode,"ops_per_batch":ops,"first_wide":first_wide,"samples":rows}));
    Ok(())
}
