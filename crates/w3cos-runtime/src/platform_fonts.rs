//! Platform font APIs for hosts that do not answer through fontconfig.
//!
//! Android uses `AFontMatcher` (API 29) from `libandroid.so`. Apple platforms
//! use Core Text. Windows reads the UI font face from `NONCLIENTMETRICS`.
//! A missing library or symbol fails soft so the caller can warn or fall
//! through to fontdb.

use std::path::PathBuf;

/// Font file that covers `character`, when the platform matcher can name one.
pub(super) fn font_file_for_character(character: char) -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        return android::match_file(character);
    }
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    {
        return apple::match_file(character);
    }
    #[cfg(not(any(target_os = "android", target_os = "ios", target_os = "macos")))]
    {
        let _ = character;
        None
    }
}

/// File for the platform UI font, when the matcher returns a path.
pub(super) fn ui_font_file() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        return android::match_file('A');
    }
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    {
        return apple::ui_file();
    }
    #[cfg(not(any(target_os = "android", target_os = "ios", target_os = "macos")))]
    {
        None
    }
}

/// Face name from `SystemParametersInfoW(SPI_GETNONCLIENTMETRICS)`.
#[cfg(target_os = "windows")]
pub(super) fn ui_message_font_name() -> Option<String> {
    windows::message_font_name()
}

#[cfg(target_os = "android")]
mod android {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::path::PathBuf;
    use std::sync::OnceLock;

    struct AFontApi {
        create: unsafe extern "C" fn() -> *mut c_void,
        destroy: unsafe extern "C" fn(*mut c_void),
        match_font: unsafe extern "C" fn(
            *const c_void,
            *const c_char,
            *const u16,
            u32,
            *mut u32,
        ) -> *mut c_void,
        close: unsafe extern "C" fn(*mut c_void),
        path: unsafe extern "C" fn(*const c_void) -> *const c_char,
    }

    fn api() -> Option<&'static AFontApi> {
        static LOADED: OnceLock<Option<AFontApi>> = OnceLock::new();
        LOADED.get_or_init(|| unsafe { load() }).as_ref()
    }

    unsafe fn load() -> Option<AFontApi> {
        unsafe {
            let name = CString::new("libandroid.so").ok()?;
            let handle = libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            if handle.is_null() {
                return None;
            }
            let symbol = |symbol: &[u8]| unsafe {
                let name = CString::new(symbol).ok()?;
                let pointer = libc::dlsym(handle, name.as_ptr());
                if pointer.is_null() {
                    None
                } else {
                    Some(pointer)
                }
            };
            Some(AFontApi {
                create: std::mem::transmute(symbol(b"AFontMatcher_create")?),
                destroy: std::mem::transmute(symbol(b"AFontMatcher_destroy")?),
                match_font: std::mem::transmute(symbol(b"AFontMatcher_match")?),
                close: std::mem::transmute(symbol(b"AFont_close")?),
                path: std::mem::transmute(symbol(b"AFont_getFontFilePath")?),
            })
        }
    }

    pub(super) fn match_file(character: char) -> Option<PathBuf> {
        let api = api()?;
        let mut utf16 = [0u16; 2];
        let encoded = character.encode_utf16(&mut utf16);
        let family = CString::new("sans-serif").ok()?;
        unsafe {
            let matcher = (api.create)();
            if matcher.is_null() {
                return None;
            }
            let mut run = 0u32;
            let font = (api.match_font)(
                matcher,
                family.as_ptr(),
                encoded.as_ptr(),
                encoded.len() as u32,
                &mut run,
            );
            let path = if font.is_null() {
                None
            } else {
                let pointer = (api.path)(font);
                let resolved = if pointer.is_null() {
                    None
                } else {
                    CStr::from_ptr(pointer)
                        .to_str()
                        .ok()
                        .map(PathBuf::from)
                        .filter(|path| path.is_file())
                };
                (api.close)(font);
                resolved
            };
            (api.destroy)(matcher);
            path
        }
    }
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
mod apple {
    use std::ffi::c_void;
    use std::path::PathBuf;

    #[repr(C)]
    struct CFRange {
        location: isize,
        length: isize,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontCreateUIFontForLanguage(
            ui_type: u32,
            size: f64,
            language: *const c_void,
        ) -> *mut c_void;
        fn CTFontCreateForString(
            current: *mut c_void,
            string: *const c_void,
            range: CFRange,
        ) -> *mut c_void;
        fn CTFontCopyAttribute(font: *mut c_void, attribute: *const c_void) -> *const c_void;
        fn CFStringCreateWithCharacters(
            alloc: *const c_void,
            chars: *const u16,
            num: isize,
        ) -> *const c_void;
        fn CFURLGetFileSystemRepresentation(
            url: *const c_void,
            resolve: u8,
            buffer: *mut u8,
            max: isize,
        ) -> u8;
        fn CFRelease(value: *const c_void);
        static kCTFontURLAttribute: *const c_void;
    }

    /// `kCTFontUIFontSystem`.
    const UI_FONT_SYSTEM: u32 = 2;

    pub(super) fn match_file(character: char) -> Option<PathBuf> {
        let mut utf16 = [0u16; 2];
        let encoded = character.encode_utf16(&mut utf16);
        unsafe {
            let base = CTFontCreateUIFontForLanguage(UI_FONT_SYSTEM, 16.0, std::ptr::null());
            if base.is_null() {
                return None;
            }
            let string = CFStringCreateWithCharacters(
                std::ptr::null(),
                encoded.as_ptr(),
                encoded.len() as isize,
            );
            if string.is_null() {
                CFRelease(base);
                return None;
            }
            let font = CTFontCreateForString(
                base,
                string,
                CFRange {
                    location: 0,
                    length: encoded.len() as isize,
                },
            );
            CFRelease(base);
            CFRelease(string);
            if font.is_null() {
                return None;
            }
            let path = file_url(font);
            CFRelease(font);
            path
        }
    }

    pub(super) fn ui_file() -> Option<PathBuf> {
        unsafe {
            let font = CTFontCreateUIFontForLanguage(UI_FONT_SYSTEM, 16.0, std::ptr::null());
            if font.is_null() {
                return None;
            }
            let path = file_url(font);
            CFRelease(font);
            path
        }
    }

    unsafe fn file_url(font: *mut c_void) -> Option<PathBuf> {
        unsafe {
            let url = CTFontCopyAttribute(font, kCTFontURLAttribute);
            if url.is_null() {
                return None;
            }
            let mut buffer = [0u8; 1024];
            let ok = CFURLGetFileSystemRepresentation(
                url,
                1,
                buffer.as_mut_ptr(),
                buffer.len() as isize,
            );
            CFRelease(url);
            if ok == 0 {
                return None;
            }
            let end = buffer
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(buffer.len());
            let path = PathBuf::from(String::from_utf8_lossy(&buffer[..end]).as_ref());
            path.is_file().then_some(path)
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::ffi::c_void;

    #[repr(C)]
    struct LogFontW {
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u8,
        underline: u8,
        strike_out: u8,
        char_set: u8,
        out_precision: u8,
        clip_precision: u8,
        quality: u8,
        pitch_and_family: u8,
        face_name: [u16; 32],
    }

    #[repr(C)]
    struct NonClientMetricsW {
        cb_size: u32,
        border_width: i32,
        scroll_width: i32,
        scroll_height: i32,
        caption_width: i32,
        caption_height: i32,
        caption_font: LogFontW,
        sm_caption_width: i32,
        sm_caption_height: i32,
        sm_caption_font: LogFontW,
        menu_width: i32,
        menu_height: i32,
        menu_font: LogFontW,
        status_font: LogFontW,
        message_font: LogFontW,
        padded_border_width: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SystemParametersInfoW(action: u32, param: u32, pv: *mut c_void, win_ini: u32) -> i32;
    }

    const SPI_GETNONCLIENTMETRICS: u32 = 0x0029;

    pub(super) fn message_font_name() -> Option<String> {
        unsafe {
            let mut metrics: NonClientMetricsW = std::mem::zeroed();
            metrics.cb_size = std::mem::size_of::<NonClientMetricsW>() as u32;
            let ok = SystemParametersInfoW(
                SPI_GETNONCLIENTMETRICS,
                metrics.cb_size,
                &mut metrics as *mut NonClientMetricsW as *mut c_void,
                0,
            );
            if ok == 0 {
                return None;
            }
            let len = metrics
                .message_font
                .face_name
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(metrics.message_font.face_name.len());
            let name = String::from_utf16(&metrics.message_font.face_name[..len]).ok()?;
            (!name.is_empty()).then_some(name)
        }
    }
}
