use w3cos_core::Value;
use w3cos_runtime::{dom, incremental_layout, jsdom};

fn add(parent: u32, id: &str, width: &str, height: &str, extra: &[(&str,&str)]) -> u32 {
    let n = dom::create_element("div");
    dom::set_attribute(n,"id",id);
    dom::set_style_property(n,"width",width);
    dom::set_style_property(n,"height",height);
    for &(key,val) in extra { dom::set_style_property(n,key,val); }
    dom::append_child(parent,n);
    n
}
fn find(id: &str) -> Value {
    jsdom::document_value().call_method("getElementById",vec![Value::string(id)])
}
fn r(id: &str, field: &str) -> f64 {
    find(id).call_method("getBoundingClientRect",vec![]).get_property(field).to_number()
}
fn flush() -> &'static str {
    assert!(incremental_layout::flush());
    incremental_layout::last_flush_stats().kind
}
fn setup(width: &str, b: &str, c: &str) -> [u32;3] {
    dom::reset_document();
    jsdom::reset_bridge();
    incremental_layout::invalidate();
    jsdom::set_viewport(640.0,480.0);
    let body = dom::body_id();
    dom::set_style_property(body,"margin","0px");
    let sc = add(body,"sc","240px","200px",&[("overflow","hidden")]);
    let a = add(sc,"a",width,"100px",&[]);
    let b = add(sc,"b","120px",b,&[]);
    let c = add(sc,"c","100px",c,&[]);
    add(sc,"tail","100px","100px",&[]);
    add(body,"guard","16px","24px",&[("margin-left","1px")]);
    assert_eq!(incremental_layout::last_flush_stats().kind,"none");
    assert_eq!(flush(),"rebuild");
    [a,b,c]
}
fn read() -> [f64;5] {
    [r("a","width"),r("b","height"),r("c","height"),r("tail","y")-r("sc","y"),
     find("sc").get_property("scrollHeight").to_number()]
}
fn fresh(width: &'static str,b: &'static str,c: &'static str) -> [f64;5] {
    std::thread::spawn(move || { setup(width,b,c); read() }).join().expect("fresh layout")
}
fn case_direct(fixed: bool) {
    let expected = fresh("120px","140px","100px");
    assert_eq!(expected,[120.0,140.0,100.0,340.0,440.0]);
    let [_a,b,_c] = setup("120px","100px","100px");
    let start = read();
    assert_eq!(start,[120.0,100.0,100.0,300.0,400.0]);
    dom::set_style_property(b,"height","140px");
    let route = flush();
    let after = read();
    println!("[memo-direct] route={route} observed={after:?} expected={expected:?}");
    if fixed {
        assert!(matches!(route,"style-subtree" | "retained"));
        assert_eq!(after,expected);
    } else {
        assert_eq!(route,"memo");
        assert_eq!(after,start);
    }
}
fn case_width(fixed: bool, change_others: bool) {
    let normal = fresh("120px","100px","100px");
    let expected = if change_others { fresh("120px","140px","130px") } else { normal };
    let [a,b,c] = setup("120px","100px","100px");
    assert_eq!(read(),normal);
    dom::set_style_property(a,"width","96px");
    let first = flush();
    assert_eq!(first,"retained");
    assert_eq!(read(),[96.0,100.0,100.0,300.0,400.0]);
    let middle = if change_others {
        dom::set_style_property(b,"height","140px");
        dom::set_style_property(c,"height","130px");
        let route = flush();
        assert_eq!(route,"retained");
        assert_eq!(read(),[96.0,140.0,130.0,370.0,470.0]);
        route
    } else { "none" };
    dom::set_style_property(a,"width","120px");
    let restoration = flush();
    let after = read();
    println!("[memo-roundtrip] change_others={change_others} first={first} middle={middle} restore={restoration} observed={after:?} expected={expected:?}");
    if change_others && fixed {
        assert_eq!(restoration,"retained");
        assert_eq!(after,expected);
    } else {
        assert_eq!(restoration,"memo");
        assert_eq!(after,normal);
    }
}
// Observe legacy engine state after a genuine memo replay, independently
// of whether the earlier width-snapshot guard passed.
fn after_replay() {
    let expected = fresh("120px","140px","130px");
    let [a,b,c] = setup("120px","100px","100px");
    dom::set_style_property(a,"width","96px");
    assert_eq!(flush(),"retained");
    assert_eq!(read(),[96.0,100.0,100.0,300.0,400.0]);
    dom::set_style_property(a,"width","120px");
    assert_eq!(flush(),"memo");
    assert_eq!(read(),[120.0,100.0,100.0,300.0,400.0]);
    dom::set_style_property(b,"height","140px");
    dom::set_style_property(c,"height","130px");
    let final_route = flush();
    let observed = read();
    assert_eq!(final_route,"retained");
    println!("[memo-after-replay] restored=memo current={final_route} observed={observed:?} expected={expected:?} matches={}",observed == expected);
}
fn main() {
    let fixed = std::env::var("DOT_WIDTH_MEMO_FIX").as_deref() == Ok("1");
    match std::env::args().nth(1).as_deref() {
        Some("height") => case_direct(fixed),
        Some("width") => case_width(fixed,false),
        Some("cross") => case_width(fixed,true),
        Some("after-replay") => after_replay(),
        other => panic!("unsupported scenario: {other:?}"),
    }
}
