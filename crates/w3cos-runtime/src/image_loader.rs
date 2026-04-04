use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read as _;
use std::sync::Arc;

#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub data: Arc<Vec<u8>>,
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Option<DecodedImage>>> = RefCell::new(HashMap::new());
}

pub fn get_or_load(src: &str) -> Option<DecodedImage> {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(entry) = cache.get(src) {
            return entry.clone();
        }
        let result = load_from_source(src);
        cache.insert(src.to_string(), result.clone());
        result
    })
}

fn load_from_source(src: &str) -> Option<DecodedImage> {
    let bytes = if src.starts_with("http://") || src.starts_with("https://") {
        let resp = match ureq::get(src).call() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[W3C OS] Failed to fetch image {src}: {e}");
                return None;
            }
        };
        let mut buf = Vec::new();
        if resp.into_body().as_reader().read_to_end(&mut buf).is_err() {
            eprintln!("[W3C OS] Failed to read image response body for {src}");
            return None;
        }
        buf
    } else {
        match std::fs::read(src) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[W3C OS] Failed to read image file {src}: {e}");
                return None;
            }
        }
    };

    match image::load_from_memory(&bytes) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            Some(DecodedImage {
                width: w,
                height: h,
                data: Arc::new(rgba.into_raw()),
            })
        }
        Err(e) => {
            eprintln!("[W3C OS] Failed to decode image {src}: {e}");
            None
        }
    }
}
