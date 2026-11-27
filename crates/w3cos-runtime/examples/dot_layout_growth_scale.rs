// Card-count robustness geometry and warm-mutation fixture; not a renderer benchmark.
use std::time::Instant;
use w3cos_core::Value;
use w3cos_runtime::{dom, font_face, incremental_layout, jsdom};

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
fn prepare(n: usize, pos: usize, long: bool) {
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
    for i in 0..n {
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
        let s = styled("span", p, if i == pos { "target" } else { "" }, &[]);
        dom::append_child(
            s,
            dom::create_text_node(if long && i == pos { LONG } else { SHORT }),
        );
        styled("div", c, &format!("s-{i}"), &[("height", "80px")]);
    }
}
#[derive(Debug, PartialEq)]
struct Snapshot {
    scroll: Rect,
    outside: Rect,
    cards: Vec<Rect>,
    paras: Vec<Rect>,
    spacers: Vec<Rect>,
    lines: Vec<Rect>,
}
fn snapshot(n: usize, pos: usize, long: bool) -> Snapshot {
    let expected_h = if long { 40.0 } else { 20.0 };
    assert_eq!(rect(&find(&format!("p-{pos}")))[3], expected_h);
    let d = Snapshot {
        scroll: rect(&find("scroll")),
        outside: rect(&find("outside")),
        cards: (0..n).map(|i| rect(&find(&format!("c-{i}")))).collect(),
        paras: (0..n).map(|i| rect(&find(&format!("p-{i}")))).collect(),
        spacers: (0..n).map(|i| rect(&find(&format!("s-{i}")))).collect(),
        lines: line_rects(&find("target")),
    };
    assert_eq!(d.scroll, [0.0, 0.0, 160.0, 240.0]);
    assert_eq!(d.outside, [0.0, 240.0, 10.0, 5.0]);
    assert_eq!(d.lines.len(), if long { 2 } else { 1 });
    for i in 0..n {
        let y = 100.0 * i as f64 + if long && i > pos { 20.0 } else { 0.0 };
        let p = if long && i == pos { 40.0 } else { 20.0 };
        assert_eq!(d.cards[i], [0.0, y, 140.0, p + 80.0], "card {i}");
        assert_eq!(d.paras[i], [0.0, y, 140.0, p], "paragraph {i}");
        assert_eq!(d.spacers[i], [0.0, y + p, 140.0, 80.0], "spacer {i}");
    }
    d
}

fn stats(pos: usize) -> serde_json::Value {
    let s = incremental_layout::last_flush_stats();
    assert_eq!(s.kind, "splice", "position {pos} did not splice");
    assert!(s.solved_nodes > 0);
    // Even the last card has its own following spacer. Count native hosts,
    // not just later card roots; do not require a predetermined total.
    assert!(
        s.shifted_nodes > 0,
        "position {pos} did not report its moved spacer"
    );
    serde_json::json!({"solved":s.solved_nodes,"shifted":s.shifted_nodes,"kind":s.kind})
}
fn verify(n: usize, pos: usize) -> serde_json::Value {
    prepare(n, pos, false);
    let cold_short = snapshot(n, pos, false);
    prepare(n, pos, true);
    let cold_long = snapshot(n, pos, true);
    prepare(n, pos, false);
    assert_eq!(snapshot(n, pos, false), cold_short);
    let p = find(&format!("p-{pos}"));
    let text = find("target").get_property("firstChild");
    eprintln!("[growth-phase] grow");
    text.set_property("data", Value::string(LONG));
    assert_eq!(rect(&p)[3], 40.0);
    let grow = stats(pos);
    assert_eq!(snapshot(n, pos, true), cold_long);
    eprintln!("[growth-phase] shrink");
    text.set_property("data", Value::string(SHORT));
    assert_eq!(rect(&p)[3], 20.0);
    let shrink = stats(pos);
    assert_eq!(snapshot(n, pos, false), cold_short);
    serde_json::json!({"count":n,"position":pos,"cold_parity":true,"grow":grow,"shrink":shrink})
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
    let n: usize = args.get(2).ok_or("need card count")?.parse()?;
    let pos: usize = args.get(3).ok_or("need position")?.parse()?;
    assert!([60, 1200].contains(&n) && (pos == 0 || pos == n - 1));
    font_face::FontRegistry::global().register_for_owner(
        u64::MAX - 513,
        font_face::FontFace {
            family: "Ahem".into(),
            src: font_face::FontSource::Bytes(std::fs::read(args.get(1).ok_or("need Ahem.ttf")?)?),
            ..Default::default()
        },
    )?;
    let control = verify(n, pos);
    if args.get(4).map(String::as_str) == Some("check") {
        println!("{control}");
        return Ok(());
    }
    let p = find(&format!("p-{pos}"));
    let text = find("target").get_property("firstChild");
    let short = Value::string(SHORT);
    let long = Value::string(LONG);
    let mut rows = vec![];
    for batch in 0..26 {
        let mut sum = 0.0;
        let mut correct = 0;
        let c = cpu_ns()?;
        let t = Instant::now();
        for i in 0..128 {
            text.set_property(
                "data",
                if i % 2 == 0 {
                    long.clone()
                } else {
                    short.clone()
                },
            );
            let rect = p.call_method("getBoundingClientRect", vec![]);
            let h = rect.get_property("height").to_number();
            sum += h;
            correct += u32::from(h == if i % 2 == 0 { 40.0 } else { 20.0 });
        }
        let elapsed = t.elapsed().as_nanos();
        let cpu = cpu_ns()? - c;
        assert_eq!(sum, 3840.0);
        assert_eq!(correct, 128);
        let end = stats(pos);
        if batch >= 6 {
            rows.push(
                serde_json::json!({"wall_ns":elapsed as u64,"thread_cpu_ns":cpu,"last_flush":end}),
            );
        }
    }
    assert_eq!(snapshot(n, pos, false).cards[n - 1][1], 100.0 * (n - 1) as f64);
    println!(
        "{}",
        serde_json::json!({"check":control,"ops_per_batch":128,"samples":rows})
    );
    Ok(())
}
