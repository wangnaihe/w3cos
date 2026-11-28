use std::time::Instant;
use w3cos_core::Value;
use w3cos_runtime::{dom,incremental_layout,jsdom};

fn div(p: u32,t: &str, id: &str, styles: &[(&str,&str)]) -> u32 {
    let n=dom::create_element(t);
    dom::set_attribute(n,"id",id);
    for &(k,v) in styles { dom::set_style_property(n,k,v); }
    dom::append_child(p,n);
    n
}
fn node(id: &str) -> Value {
    jsdom::document_value().call_method("getElementById",vec![Value::string(id)])
}
fn setup(w: &str,b: &str,c: &str) -> [u32;3] {
    dom::reset_document();
    jsdom::reset_bridge();
    incremental_layout::invalidate();
    jsdom::set_viewport(1100.0,760.0);
    let body=dom::body_id();
    dom::set_style_property(body,"margin","0px");
    let sc=div(body,"div","sc",&[("width","720px"),("height","320px"),("overflow","hidden")]);
    let a=div(sc,"div","a",&[("width",w),("height","5400px"),("font-family","sans-serif"),("font-size","16px"),("line-height","20px")]);
    for i in 0..80 {
        let p=div(a,"p",&format!("p{i}"),&[("margin","0px"),("padding","0px")]);
        let text=dom::create_text_node("A line of ordinary text in a project overview wraps when the reading pane gets narrower in an editing session.");
        dom::append_child(p,text);
    }
    let b=div(sc,"div","b",&[("width","500px"),("height",b)]);
    let c=div(sc,"div","c",&[("width","500px"),("height",c)]);
    div(sc,"div","tail",&[("width","500px"),("height","100px")]);
    div(body,"div","guard",&[("width","16px"),("height","24px"),("margin-left","1px")]);
    assert!(incremental_layout::flush());
    assert_eq!(incremental_layout::last_flush_stats().kind,"rebuild");
    [a,b,c]
}
fn rect(id: &str,k: &str) -> u64 {
    node(id).call_method("getBoundingClientRect",vec![]).get_property(k).to_number().to_bits()
}
fn observe() -> [u64;8] {
    [rect("a","width"),rect("b","height"),rect("c","height"),rect("tail","y"),
     node("sc").get_property("scrollHeight").to_number().to_bits(),
     rect("p0","height"),rect("p39","y"),rect("p79","y")]
}
fn fresh(w: &'static str,b: &'static str,c: &'static str) -> [u64;8] {
    std::thread::spawn(move||{ setup(w,b,c); observe() }).join().expect("fresh oracle")
}
fn cpu_ns() -> u64 {
    let mut t=libc::timespec {tv_sec:0,tv_nsec:0};
    assert_eq!(unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID,&mut t) },0);
    t.tv_sec as u64*1_000_000_000+t.tv_nsec as u64
}
fn timed(edit: impl FnOnce()) -> ([u64;2], &'static str) {
    let w=Instant::now();
    let c=cpu_ns();
    edit();
    let ok=incremental_layout::flush();
    let cpu=cpu_ns()-c;
    let wall=w.elapsed().as_nanos() as u64;
    assert!(ok);
    ([wall,cpu],incremental_layout::last_flush_stats().kind)
}
fn stats(samples: &[[u64;2]]) -> [u64;4] {
    let pick=|col,percent| {
        let mut values=samples.iter().map(|v|v[col]).collect::<Vec<_>>();
        values.sort_unstable();
        values[(values.len()-1)*percent/100]
    };
    [pick(0,50),pick(0,95),pick(1,50),pick(1,95)]
}
fn main() {
    let trial=std::env::args().nth(1).expect("width or mixed");
    let flag=std::env::var("DOT_WIDTH_MEMO_FIX").as_deref()==Ok("1");
    let nomemo=std::env::var("DOT_WIDTH_MEMO_DISABLE").as_deref()==Ok("1");
    let iterations=24;
    let warm=6;
    let wide=fresh("640px","100px","100px");
    let narrow=fresh("360px","100px","100px");
    assert_ne!(wide[7],narrow[7],"actual paragraph wrapping");
    let mut samples=vec![vec![];4];
    if trial=="width" {
        let [a,_,_]=setup("640px","100px","100px");
        assert_eq!(observe(),wide);
        let (_,route)=timed(||dom::set_style_property(a,"width","360px"));
        assert_eq!(route,"retained");
        assert_eq!(observe(),narrow);
        for i in 0..(warm+iterations)*2 {
            let (width,expected)=if i%2==0 {("640px",wide)} else {("360px",narrow)};
            let (slice,route)=timed(||dom::set_style_property(a,"width",width));
            assert_eq!(route,if nomemo {"retained"} else {"memo"});
            assert_eq!(observe(),expected);
            if i>=warm*2 { samples[0].push(slice); }
        }
    } else {
        assert_eq!(trial,"mixed");
        assert!(flag,"stock is invalid for this mixed comparison");
        let fresh_first=fresh("640px","140px","130px");
        let fresh_second=fresh("640px","125px","110px");
        for i in 0..warm+iterations {
            let [a,b,c]=setup("640px","100px","100px");
            assert_eq!(observe(),wide);
            let (_,route)=timed(||dom::set_style_property(a,"width","360px"));
            assert_eq!(route,"retained");
            assert_eq!(observe(),narrow);
            let (restore,route)=timed(||dom::set_style_property(a,"width","640px"));
            assert_eq!(route,if nomemo {"retained"} else {"memo"});
            assert_eq!(observe(),wide);
            let (first,route)=timed(||{
                dom::set_style_property(b,"height","140px");
                dom::set_style_property(c,"height","130px");
            });
            assert_eq!(route,"retained");
            assert_eq!(observe(),fresh_first);
            let (second,route)=timed(||{
                dom::set_style_property(b,"height","125px");
                dom::set_style_property(c,"height","110px");
            });
            assert_eq!(route,"retained");
            assert_eq!(observe(),fresh_second);
            if i>=warm {
                for (index,value) in [restore,first,second,[restore[0]+first[0]+second[0],restore[1]+first[1]+second[1]]].into_iter().enumerate() {
                    samples[index].push(value);
                }
            }
        }
    }
    println!("{}",serde_json::json!({"trial":trial,"guard":flag,"nomemo":nomemo,"width_control_wraps":true,
        "counts":samples.iter().map(Vec::len).collect::<Vec<_>>(),"ns_wall50_wall95_cpu50_cpu95":samples.iter().filter(|x|!x.is_empty()).map(|x|stats(x)).collect::<Vec<_>>()}));
}
