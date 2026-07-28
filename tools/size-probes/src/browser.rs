use w3cos_core::Value;

fn main() {
    w3cos_runtime::dom::reset_document();
    w3cos_runtime::jsdom::reset_bridge();
    let compiled_cache_dir =
        std::env::var_os("W3COS_SIZE_PROBE_CACHE_DIR").map(std::path::PathBuf::from);
    let policy = w3cos_runtime::dynamic_script::ScriptPolicy {
        compiled_cache_dir,
        ..Default::default()
    };
    if let Ok(url) = std::env::var("W3COS_SIZE_PROBE_FETCH_URL") {
        let mut document_loader =
            w3cos_runtime::dynamic_script::DocumentLoader::new(policy, Default::default());
        document_loader
            .navigate(&url)
            .expect("dynamic size probe URL must start document navigation");
        loop {
            match document_loader.poll() {
                w3cos_runtime::dynamic_script::DocumentLoadProgress::Complete => break,
                w3cos_runtime::dynamic_script::DocumentLoadProgress::Failed(error) => {
                    panic!("dynamic size probe document navigation failed: {error}")
                }
                w3cos_runtime::dynamic_script::DocumentLoadProgress::Cancelled => {
                    panic!("dynamic size probe document navigation was cancelled")
                }
                _ => std::thread::yield_now(),
            }
        }
    } else {
        let loader = w3cos_runtime::dynamic_script::ScriptLoader::new(policy);
        let mut parser = w3cos_runtime::dynamic_script::StreamingDocumentParser::new(
            loader.clone(),
            "https://size-probe.invalid/document.html",
        )
        .expect("dynamic size probe document parser must initialize");
        let html = std::env::var("W3COS_SIZE_PROBE_HTML").unwrap_or_else(|_| {
            r#"<html><body><script>document.body.setAttribute("data-size-probe", "dynamic-w3vm");</script></body></html>"#
                .to_string()
        });
        parser
            .write(&html)
            .expect("dynamic size probe must tokenize and build the live DOM");
        parser
            .finish()
            .expect("dynamic size probe must finish parser lifecycle");
        w3cos_runtime::jsdom::drain_microtasks();
    }
    let body = w3cos_runtime::jsdom::document_value().get_property("body");
    println!(
        "{}",
        body.call_method("getAttribute", vec![Value::string("data-size-probe")])
            .to_js_string()
    );
}
