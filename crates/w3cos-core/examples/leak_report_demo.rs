//! `cargo run -p w3cos-core --features leak-detector --example leak_report_demo`
//! Builds a parent <-> child cycle plus a retained cache entry and prints the
//! leak detector's text and JSON reports.
use std::collections::HashMap;
use w3cos_core::{Value, leak_detector};

fn main() {
    let realm = Value::object(HashMap::new());
    leak_detector::mark_baseline();
    {
        let editor = Value::object(HashMap::from([(
            "title".to_string(),
            Value::string("a.ts"),
        )]));
        let panel = Value::object(HashMap::new());
        editor.set_property("panel", panel.clone());
        panel.set_property("owner", editor.clone());
    }
    let cache = Value::array(vec![Value::object(HashMap::new())]);
    realm.set_property("cache", cache);
    let report = leak_detector::report();
    print!("{}", report.to_text());
    println!(
        "{}",
        serde_json::to_string_pretty(&report.to_json()).unwrap()
    );
}
