//! C entry points of the system runtime cdylib.
//!
//! AOT apps for this slice are JSON component bundles. The host passes the
//! path to [`w3cos_runtime_run_bundle`], which paints with the same native
//! `run_app_static` path as a statically linked binary. Web pages enter
//! through [`w3cos_runtime_page_text`] / [`w3cos_runtime_run_page`] and run on
//! the W3VM hosted by this library. Those page symbols return an error unless
//! `dynamic-js` is on.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Mutex;

static LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);
static PAGE_TEXT: Mutex<Option<CString>> = Mutex::new(None);
static PAGE_HTML: Mutex<Option<String>> = Mutex::new(None);

fn set_error(message: impl Into<String>) {
    let text = message.into().replace('\0', "");
    if let Ok(mut slot) = LAST_ERROR.lock() {
        *slot = CString::new(text).ok();
    }
}

fn store_text(text: String) -> *const c_char {
    let Ok(cstring) = CString::new(text.replace('\0', "")) else {
        return std::ptr::null();
    };
    let ptr = cstring.as_c_str().as_ptr();
    if let Ok(mut slot) = PAGE_TEXT.lock() {
        *slot = Some(cstring);
        ptr
    } else {
        std::ptr::null()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_runtime_abi_version() -> u32 {
    w3cos_abi::W3COS_RUNTIME_ABI
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_runtime_last_error() -> *const c_char {
    LAST_ERROR
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|text| text.as_c_str().as_ptr()))
        .unwrap_or(std::ptr::null())
}

/// Load `html` on the shared W3VM and return the body's descendant text.
/// The pointer stays valid until the next call. Null means failure; read
/// [`w3cos_runtime_last_error`].
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_runtime_page_text(html: *const c_char) -> *const c_char {
    #[cfg(not(feature = "dynamic-js"))]
    {
        let _ = html;
        set_error("this libw3cos_runtime.so was built without the dynamic-js VM");
        std::ptr::null()
    }
    #[cfg(feature = "dynamic-js")]
    {
        if html.is_null() {
            set_error("HTML pointer is null");
            return std::ptr::null();
        }
        let html = unsafe { CStr::from_ptr(html) }.to_string_lossy();
        match crate::dynamic_script::load_html_on_shared_vm(
            &html,
            "https://shared.w3cos.local/page.html",
        ) {
            Ok(()) => {
                let text = crate::dom::get_descendant_text_content(crate::dom::body_id());
                store_text(text)
            }
            Err(error) => {
                set_error(error.to_string());
                std::ptr::null()
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct SharedBundle {
    abi: u32,
    component: w3cos_std::Component,
}

/// Load a static AOT UI bundle and present it with this process's runtime.
/// The bundle does not contain layout, rendering, or the VM.
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_runtime_run_bundle(path: *const c_char) -> i32 {
    if path.is_null() {
        set_error("bundle path is null");
        return 2;
    }
    let path = unsafe { CStr::from_ptr(path) }.to_string_lossy().into_owned();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            set_error(format!("could not read {path}: {error}"));
            return 2;
        }
    };
    let bundle: SharedBundle = match serde_json::from_str(&text) {
        Ok(bundle) => bundle,
        Err(error) => {
            set_error(format!("bundle is not a w3cos component tree: {error}"));
            return 2;
        }
    };
    if !w3cos_abi::abi_compatible(w3cos_abi::W3COS_RUNTIME_ABI, bundle.abi) {
        set_error(format!(
            "ABI mismatch: runtime {:#x} cannot load bundle {:#x}",
            w3cos_abi::W3COS_RUNTIME_ABI, bundle.abi
        ));
        return 2;
    }
    match crate::run_app_static(bundle.component) {
        Ok(()) => 0,
        Err(error) => {
            set_error(error.to_string());
            1
        }
    }
}

/// Open a native window for the HTML file at `path`, executed on the shared VM.
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_runtime_run_page(path: *const c_char) -> i32 {
    #[cfg(not(feature = "dynamic-js"))]
    {
        let _ = path;
        set_error("this libw3cos_runtime.so was built without the dynamic-js VM");
        3
    }
    #[cfg(feature = "dynamic-js")]
    {
        if path.is_null() {
            set_error("page path is null");
            return 3;
        }
        let path = unsafe { CStr::from_ptr(path) }.to_string_lossy().into_owned();
        match std::fs::read_to_string(&path) {
            Ok(html) => {
                if let Ok(mut slot) = PAGE_HTML.lock() {
                    *slot = Some(html);
                }
            }
            Err(error) => {
                set_error(format!("could not read {path}: {error}"));
                return 3;
            }
        }
        match crate::run_app_dom(setup_shared_page) {
            Ok(()) => 0,
            Err(error) => {
                set_error(error.to_string());
                1
            }
        }
    }
}

#[cfg(feature = "dynamic-js")]
fn setup_shared_page() {
    let html = PAGE_HTML
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_default();
    if let Err(error) = crate::dynamic_script::load_html_on_shared_vm(
        &html,
        "https://shared.w3cos.local/page.html",
    ) {
        set_error(error.to_string());
        eprintln!("W3COS_PAGE_ERROR {error:#}");
    }
}
