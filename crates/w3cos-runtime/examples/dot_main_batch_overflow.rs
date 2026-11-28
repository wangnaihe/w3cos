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
fn setup(tall: bool, narrow: bool) -> (u32, Value, Value, Value) {
    dom::reset_document();
    jsdom::reset_bridge();
    incremental_layout::invalidate();
    jsdom::set_viewport(640.0,480.0);
    let body = dom::body_id();
    dom::set_style_property(body,"margin","0px");
    let sc_numeric = element(body,"sc",&[("width","240px"),("height","200px"),("overflow","hidden")]);
    let sc = find("sc");
    let lead = element(sc_numeric,"lead",&[("width",if narrow {"100px"} else {"120px"}),("height",if tall {"140px"} else {"100px"})]);
    element(sc_numeric,"tail",&[("width","100px"),("height","300px")]);
    (lead,sc,find("lead"),find("tail"))
}
fn snapshot(sc: &Value, lead: &Value, tail: &Value, height: f64) -> ([f64;4],[f64;4],f64,f64) {
    let total = sc.get_property("scrollHeight").to_number();
    assert_eq!(total,height+300.0);
    sc.set_property("scrollTop", Value::string("0"));
    let lr = rect(lead);
    let tr = rect(tail);
    sc.set_property("scrollTop", Value::string("10000"));
    let clamped = sc.get_property("scrollTop").to_number();
    assert_eq!(clamped,total-200.0);
    sc.set_property("scrollTop", Value::string("0"));
    (lr,tr,total,clamped)
}
fn main() {
    let make = |tall,narrow| {
        let (_,sc, lead,tail) = setup(tall,narrow);
        snapshot(&sc,&lead,&tail, if tall {140.0} else {100.0})
    };
    let oracle = [make(false,false),make(true,false),make(false,true)];
    let (id,sc,lead,tail) = setup(false,false);
    assert_eq!(snapshot(&sc,&lead,&tail,100.0),oracle[0]);
    for (tag,property,value,state,height,shifted) in [
        ("grow","height","140px",1,140.0,1u32),
        ("shrink","height","100px",0,100.0,1),
        ("width","width","100px",2,100.0,0),
    ] {
        eprintln!("[overflow-phase] {tag}");
        dom::set_style_property(id,property,value);
        assert_eq!(sc.get_property("scrollHeight").to_number(),height+300.0);
        let stat = incremental_layout::last_flush_stats();
        assert_eq!(stat.kind, "style-subtree", "{tag} did not use style subtree");
        assert_eq!(stat.shifted_nodes, shifted);
        assert_eq!(snapshot(&sc,&lead,&tail,height),oracle[state],"{tag}");
        eprintln!("[overflow-result] {tag} shifted={} height={}",stat.shifted_nodes,height+300.0);
    }
}
