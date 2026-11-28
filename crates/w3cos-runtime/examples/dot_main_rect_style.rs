use w3cos_core::Value;
use w3cos_runtime::{dom, incremental_layout, jsdom};

fn element(parent: u32, id: &str, css: &[(&str,&str)]) -> u32 {
    let n = dom::create_element("div");
    dom::set_attribute(n, "id", id);
    for &(name, val) in css { dom::set_style_property(n, name, val); }
    dom::append_child(parent,n);
    n
}
fn find(id: &str) -> Value {
    jsdom::document_value().call_method("getElementById",vec![Value::string(id)])
}
fn rect(node: &Value) -> [f64;4] {
    let r = node.call_method("getBoundingClientRect",vec![]);
    ["x","y","width","height"].map(|n| r.get_property(n).to_number())
}
fn setup(tall: bool, narrow: bool) -> (u32, Value, Value, Value, Value) {
    dom::reset_document();
    jsdom::reset_bridge();
    incremental_layout::invalidate();
    jsdom::set_viewport(640.0,480.0);
    let body = dom::body_id();
    dom::set_style_property(body,"margin","0px");
    let sc_numeric = element(body,"sc",&[("width","240px"),("height","200px"),("overflow","hidden")]);
    let lead = element(sc_numeric,"lead",&[("width",if narrow {"100px"} else {"120px"}),("height",if tall {"140px"} else {"100px"})]);
    element(sc_numeric,"tail",&[("width","100px"),("height","300px")]);
    // A separate ordinary block with a nonzero horizontal margin rules out
    // the whole-document definite-box shortcut without moving the scroller.
    element(body,"control",&[("width","16px"),("height","24px"),("margin-left","1px")]);
    (lead,find("sc"),find("lead"),find("tail"),find("control"))
}
#[derive(Debug, PartialEq)]
struct Snapshot {
    boxes: [[f64; 4]; 4],
    total: f64,
    clamped: f64,
}
fn snapshot(sc: &Value, lead: &Value, tail: &Value, control: &Value, height: f64) -> Snapshot {
    let total = sc.get_property("scrollHeight").to_number();
    assert_eq!(total,height+300.0);
    sc.set_property("scrollTop", Value::string("0"));
    let boxes = [rect(sc), rect(lead), rect(tail), rect(control)];
    assert_eq!(sc.get_property("clientHeight").to_number(), 200.0);
    assert_eq!(sc.get_property("clientWidth").to_number(), 240.0);
    assert_eq!(boxes[3][0], 1.0);
    assert_eq!(boxes[2][1] - boxes[0][1], height);
    assert_eq!(boxes[2][3], 300.0);
    sc.set_property("scrollTop", Value::string("10000"));
    let clamped = sc.get_property("scrollTop").to_number();
    assert_eq!(clamped,total-200.0);
    sc.set_property("scrollTop", Value::string("0"));
    assert!(incremental_layout::flush());
    Snapshot { boxes, total, clamped }
}
// Independent, non-gating CSSOM observation; the layout receipt test does
// not reinterpret browser-relative getBoundingClientRect semantics.
fn observe_cssom() {
    let (_,sc,_,tail,_) = setup(false,false);
    assert!(incremental_layout::flush());
    assert_eq!(incremental_layout::last_flush_stats().kind,"rebuild");
    let before = rect(&tail)[1] - rect(&sc)[1];
    let height = sc.get_property("scrollHeight").to_number();
    let page = sc.get_property("clientHeight").to_number();
    sc.set_property("scrollTop",Value::string("10000"));
    let scroll = sc.get_property("scrollTop").to_number();
    assert_eq!((height,page,scroll,before),(400.0,200.0,200.0,100.0));
    let observed = rect(&tail)[1] - rect(&sc)[1];
    let expected = before - scroll;
    println!("[cssom-ancestor-scroll] scroll={scroll} before={before} expected={expected} observed={observed} matches={}",observed == expected);
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--observe-cssom") {
        observe_cssom();
        return;
    }
    // Each oracle and the measured fixture have independent last-flush slots.
    let oracle = [(false,false), (true,false), (false,true)].map(|(tall,narrow)| {
        std::thread::spawn(move || {
            let (_,sc,lead,tail,control) = setup(tall,narrow);
            assert_eq!(incremental_layout::last_flush_stats().kind,"none");
            assert!(incremental_layout::flush());
            assert_eq!(incremental_layout::last_flush_stats().kind,"rebuild");
            snapshot(&sc,&lead,&tail,&control, if tall {140.0} else {100.0})
        }).join().expect("fresh oracle")
    });
    let (id,sc,lead,tail,control) = setup(false,false);
    assert_eq!(incremental_layout::last_flush_stats().kind, "none");
    assert!(incremental_layout::flush());
    let stat = incremental_layout::last_flush_stats();
    assert_eq!(stat.kind, "rebuild");
    assert!(stat.solved_nodes > 0);
    eprintln!("[overflow-start] rebuilt={}",stat.solved_nodes);
    assert_eq!(snapshot(&sc,&lead,&tail,&control,100.0),oracle[0]);
    for (tag,property,value,state,height,shifted) in [
        ("grow","height","140px",1,140.0,1u32),
        ("shrink","height","100px",0,100.0,1),
        ("width","width","100px",2,100.0,0),
    ] {
        eprintln!("[overflow-phase] {tag}");
        dom::set_style_property(id,property,value);
        assert!(incremental_layout::flush());
        let stat = incremental_layout::last_flush_stats();
        assert_eq!(stat.kind, "style-subtree", "{tag} did not use style subtree");
        assert_eq!(stat.shifted_nodes, shifted);
        let observed = snapshot(&sc,&lead,&tail,&control,height);
        assert_eq!(observed,oracle[state],"{tag}");
        assert_eq!(observed.boxes[3],oracle[0].boxes[3],"{tag} moved unrelated sibling");
        eprintln!("[overflow-result] {tag} shifted={} height={} clamp={}",stat.shifted_nodes,observed.total,observed.clamped);
    }
}
