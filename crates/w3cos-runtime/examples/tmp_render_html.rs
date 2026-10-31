use w3cos_runtime::{dom, dynamic_script, headless};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let html = std::fs::read_to_string(&a[1]).unwrap();
    let w: u32 = a.get(3).and_then(|v| v.parse().ok()).unwrap_or(1280);
    let h: u32 = a.get(4).and_then(|v| v.parse().ok()).unwrap_or(800);
    let url = std::env::var("URL").unwrap_or_else(|_| format!("file://{}", std::fs::canonicalize(&a[1]).unwrap().display()));
    dynamic_script::load_html_on_shared_vm(&html, &url).unwrap();
    let f = headless::render_document_rgba_cpu(w, h).unwrap();
    headless::write_frame_png(std::path::Path::new(&a[2]), &f).unwrap();
    // rects of elements with id
    let ids: Vec<String> = std::env::var("IDS").unwrap_or_default().split(',').map(String::from).collect();
    for id in ids.iter().filter(|i| !i.is_empty()).flat_map(|i| dom::query_selector_all(&format!("#{i}"))) {
        let r = dom::bounding_rect(id);
        println!("#{} rect=({:.0},{:.0},{:.0}x{:.0})", dom::get_attribute(id,"id").unwrap_or_default(), r.x, r.y, r.width, r.height);
    }
    println!("ids done");
    std::process::exit(0);
}
