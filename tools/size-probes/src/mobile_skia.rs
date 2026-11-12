use w3cos_core::Value;

fn main() {
    w3cos_runtime::dom::reset_document();
    let document = w3cos_runtime::jsdom::document_value();
    let body = document.get_property("body");
    body.call_method(
        "setAttribute",
        vec![
            Value::string("data-size-probe"),
            Value::string("mobile-skia"),
        ],
    );
    let readback = body
        .call_method("getAttribute", vec![Value::string("data-size-probe")])
        .to_js_string();
    let anchor = std::hint::black_box(w3cos_runtime::distribution_linkage_anchor());
    let _ = std::hint::black_box(readback);
    println!("mobile-skia");
    let _ = anchor;
}
