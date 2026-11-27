// 300-card geometry and warm-mutation fixture; not a renderer benchmark.
use std::time::Instant;
use w3cos_core::Value;
use w3cos_runtime::{dom, font_face, incremental_layout, jsdom};

const N: usize = 300;
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
        let id = match i { 0 => "head", 299 => "tail", _ => "" };
        let wraps = (i == 0 && long[0]) || (i == 299 && long[1]);
        let span = styled("span", p, id, &[]);
        dom::append_child(span, dom::create_text_node(if wraps { LONG } else { SHORT }));
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
    for (i, on) in long.into_iter().enumerate() {
        assert_eq!(d.lines[i].len(), if on { 2 } else { 1 });
    }
    for i in 0..N {
        let y = 100.0 * i as f64 + if long[0] && i > 0 { 20.0 } else { 0.0 };
        let wraps = (i == 0 && long[0]) || (i == 299 && long[1]);
        let h = if wraps { 40.0 } else { 20.0 };
        assert_eq!(d.cards[i], [0.0, y, 140.0, h+80.0], "card {i}");
        assert_eq!(d.paras[i], [0.0, y, 140.0, h], "paragraph {i}");
        assert_eq!(d.spacers[i], [0.0, y+h, 140.0, 80.0], "spacer {i}");
    }
    d
}
fn edit(pos: usize, grow: bool) {
    let node = find(if pos == 0 { "head" } else { "tail" }).get_property("firstChild");
    node.set_property("data", Value::string(if grow { LONG } else { SHORT }));
    assert_eq!(rect(&find(&format!("p-{pos}")))[3], if grow {40.0} else {20.0});
    stats(pos);
}
fn mixed() {
    let states = [[false,false],[false,true],[true,false],[true,true]];
    let cold = states.map(|state| { prepare(state); snapshot(state) });
    prepare([false,false]);
    assert_eq!(snapshot([false,false]), cold[0]);
    let mut last = None;
    let mut rows = Vec::new();
    for (i, (pos, grow, key)) in [
        (299,true,1), (0,true,3), (299,false,2), (0,false,0), (299,true,1), (299,false,0)
    ].into_iter().enumerate() {
        eprintln!("[mixed-phase] edit={i} pos={pos} grow={grow}");
        edit(pos, grow);
        let actual = snapshot(states[key]);
        if i == 1 {
            let shifted: Vec<Rect> = last.take().unwrap_or_else(Vec::new).into_iter().map(|mut r: Rect| {
                r[1] += 20.0;
                r
            }).collect();
            assert_eq!(actual.lines[1], shifted, "both wrapped tail lines must translate together");
        }
        assert_eq!(actual, cold[key], "mixed scenario edit {i}");
        rows.push(serde_json::json!({"edit":i,"pos":pos,"grow":grow,
                                   "tail_y":actual.paras[299][1],"tail_height":actual.paras[299][3]}));
        last=Some(actual.lines[1].clone());
    }
    println!("{}",serde_json::json!({"fresh_parity":true,"edits":rows}));
}
fn stats(pos: usize) -> serde_json::Value {
    let s = incremental_layout::last_flush_stats();
    assert_eq!(s.kind, "splice", "position {pos} did not splice");
    assert_eq!(s.solved_nodes, 2);
    assert_eq!(s.shifted_nodes as usize, 4 * (299 - pos) + 1);
    // Even the last card has its own following spacer. Count native hosts,
    // not just later card roots; do not require a predetermined total.
    assert!(
        s.shifted_nodes > 0,
        "position {pos} did not report its moved spacer"
    );
    serde_json::json!({"solved":s.solved_nodes,"shifted":s.shifted_nodes,"kind":s.kind})
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
    let mode = args.get(2).ok_or("need check, primed or unprimed")?.as_str();
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
    assert!(["primed", "unprimed"].contains(&mode));
    prepare([false,false]);
    let cold = snapshot([false,false]);
    if mode == "primed" {
        edit(0,true);
        edit(0,false);
        assert_eq!(snapshot([false,false]),cold);
    }
    edit(299,true);
    edit(299,false);
    assert_eq!(snapshot([false,false]),cold);
    let pos = 299;
    let p = find("p-299");
    let text = find("tail").get_property("firstChild");
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
    assert_eq!(snapshot([false,false]), cold);
    println!(
        "{}",
        serde_json::json!({"mode":mode,"ops_per_batch":128,"samples":rows})
    );
    Ok(())
}
