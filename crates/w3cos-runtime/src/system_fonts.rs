//! Read-only installed-font lookup for the CSS render cascade.
//! Installed faces are not document-owned @font-face registrations.

use super::{FontFaceStyle, FontKey, FontWeight, LoadedFont};
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

static DATABASE: OnceLock<Mutex<fontdb::Database>> = OnceLock::new();
static CACHE: OnceLock<Mutex<HashMap<FontKey, Option<LoadedFont>>>> = OnceLock::new();
static FACES: OnceLock<Mutex<HashMap<fontdb::ID, LoadedFont>>> = OnceLock::new();
const CACHE_CAPACITY: usize = 2048;

pub(super) fn database() -> MutexGuard<'static, fontdb::Database> {
    DATABASE
        .get_or_init(|| {
            pin_test_fontconfig();
            let mut database = fontdb::Database::new();
            #[cfg(test)]
            database.load_fonts_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/test-fonts"));
            database.load_system_fonts();
            #[cfg(any(target_os = "android", target_env = "ohos"))]
            database.load_fonts_dir("/system/fonts");
            #[cfg(target_os = "ios")]
            for path in [
                "/System/Library/Fonts",
                "/System/Library/Fonts/Core",
                "/System/Library/Fonts/Cache",
            ] {
                database.load_fonts_dir(path);
            }
            Mutex::new(database)
        })
        .lock()
        .unwrap()
}

pub(super) fn resolve(
    family: &str,
    weight: FontWeight,
    style: FontFaceStyle,
) -> Option<LoadedFont> {
    let key = FontKey {
        family: family.to_lowercase(),
        weight,
        style,
    };
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(face) = cache.lock().unwrap().get(&key).cloned() {
        return face;
    }
    let face = load(family, weight, style);
    let mut cache = cache.lock().unwrap();
    if cache.len() >= CACHE_CAPACITY {
        cache.clear();
    }
    cache.insert(key, face.clone());
    face
}

fn load(family: &str, weight: FontWeight, style: FontFaceStyle) -> Option<LoadedFont> {
    let database = database();
    let id = database.query(&fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight: fontdb::Weight(weight.0),
        style: match style {
            FontFaceStyle::Normal => fontdb::Style::Normal,
            FontFaceStyle::Italic => fontdb::Style::Italic,
            FontFaceStyle::Oblique => fontdb::Style::Oblique,
        },
        ..fontdb::Query::default()
    })?;
    drop(database);
    load_by_id(id)
}

fn load_by_id(id: fontdb::ID) -> Option<LoadedFont> {
    let database = database();
    // Different CSS weights/aliases can select the same installed face.
    // Share its bytes and parsed cmap, not one copy per requested weight.
    let faces = FACES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(face) = faces.lock().unwrap().get(&id).cloned() {
        return Some(face);
    }
    let info = database.face(id)?;
    let family = info.families.first()?.0.clone();
    let weight = FontWeight(info.weight.0);
    let style = match info.style {
        fontdb::Style::Normal => FontFaceStyle::Normal,
        fontdb::Style::Italic => FontFaceStyle::Italic,
        fontdb::Style::Oblique => FontFaceStyle::Oblique,
    };
    let is_monospace = info.monospaced;
    let (data, index) =
        database.with_face_data(id, |data, index| (Arc::new(data.to_vec()), index))?;
    drop(database);
    let parsed = Arc::new(
        fontdue::Font::from_bytes(
            data.as_slice(),
            fontdue::FontSettings {
                collection_index: index,
                ..fontdue::FontSettings::default()
            },
        )
        .ok()?,
    );
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    index.hash(&mut hasher);
    family.hash(&mut hasher);
    weight.hash(&mut hasher);
    style.hash(&mut hasher);
    #[cfg(feature = "skia")]
    let skia_typeface = {
        pin_test_fontconfig();
        use skia_safe::{FontMgr, FontStyle, font_style};
        let face = FontMgr::default()
            .match_family_style(
                &family,
                FontStyle::new(
                    (weight.0 as i32).into(),
                    font_style::Width::NORMAL,
                    match style {
                        FontFaceStyle::Normal => font_style::Slant::Upright,
                        FontFaceStyle::Italic => font_style::Slant::Italic,
                        FontFaceStyle::Oblique => font_style::Slant::Oblique,
                    },
                ),
            )
            .or_else(|| FontMgr::default().new_from_data(data.as_slice(), Some(index as usize)));
        let cell = OnceLock::new();
        let _ = cell.set(face);
        Arc::new(cell)
    };
    let loaded = LoadedFont {
        family,
        weight,
        style,
        data,
        is_monospace,
        parsed: Some(parsed),
        unicode_ranges: None,
        cache_key: hasher.finish(),
        collection_index: Some(index),
        #[cfg(feature = "skia")]
        skia_typeface,
    };
    let mut faces = faces.lock().unwrap();
    if faces.len() >= CACHE_CAPACITY {
        faces.clear();
    }
    let loaded = faces.entry(id).or_insert(loaded).clone();
    Some(loaded)
}

static FALLBACK_CACHE: OnceLock<Mutex<HashMap<char, Option<LoadedFont>>>> = OnceLock::new();

/// Point fontconfig at the bundled test faces before the first `FcInit`.
///
/// Layout expectations were measured against Noto Sans / Noto Serif / Arimo /
/// Tinos / Cousine. A GitHub runner's DejaVu fallback changes those metrics.
pub(super) fn pin_test_fontconfig() {
    #[cfg(test)]
    {
        use std::sync::Once;
        static PIN: Once = Once::new();
        PIN.call_once(|| {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/test-fonts");
            let cjk = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/test-fonts-cjk");
            let conf_path = std::env::temp_dir().join("w3cos-test-fonts.conf");
            let conf = format!(
                r#"<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir>{dir}</dir>
  <dir>{cjk}</dir>
  <alias><family>sans-serif</family><prefer><family>Noto Sans</family></prefer></alias>
  <alias><family>serif</family><prefer><family>Noto Serif</family></prefer></alias>
  <alias><family>monospace</family><prefer><family>Cousine</family></prefer></alias>
  <alias><family>Arial</family><prefer><family>Arimo</family></prefer></alias>
  <alias><family>Helvetica</family><prefer><family>Arimo</family></prefer></alias>
  <alias><family>Times</family><prefer><family>Tinos</family></prefer></alias>
  <alias><family>Times New Roman</family><prefer><family>Tinos</family></prefer></alias>
  <alias><family>Courier</family><prefer><family>Cousine</family></prefer></alias>
  <alias><family>Courier New</family><prefer><family>Cousine</family></prefer></alias>
</fontconfig>
"#
            );
            std::fs::write(&conf_path, conf).expect("pinned test fontconfig");
            // The first FontMgr::default() call initializes fontconfig for the
            // process. The variable has to be in place before that call.
            unsafe { std::env::set_var("FONTCONFIG_FILE", &conf_path) };
        });
    }
}

/// Test builds ship a CJK subset so layout checks do not depend on the host.
/// Release linkage probes stay inside the AOT size budget, so production
/// fallback keeps using installed faces.
#[cfg(test)]
fn bundled_cjk_face() -> Option<LoadedFont> {
    static FACE: OnceLock<Option<LoadedFont>> = OnceLock::new();
    FACE.get_or_init(|| {
        let data = Arc::new(include_bytes!("../assets/CJK-Subset.ttf").to_vec());
        let parsed =
            fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default()).ok()?;
        let mut hasher = DefaultHasher::new();
        data.hash(&mut hasher);
        Some(LoadedFont {
            family: "W3COS CJK Subset".into(),
            weight: FontWeight::NORMAL,
            style: FontFaceStyle::Normal,
            data,
            is_monospace: false,
            parsed: Some(Arc::new(parsed)),
            unicode_ranges: None,
            cache_key: hasher.finish(),
            collection_index: Some(0),
            #[cfg(feature = "skia")]
            skia_typeface: Arc::new(OnceLock::new()),
        })
    })
    .clone()
}

/// Face to use when the primary font has no glyph for `character`.
///
/// Order: fontconfig's charset match (when `libfontconfig` is installed),
/// then a named CJK / emoji / symbol chain, then other installed faces.
/// A face is kept only when this rasterizer can actually ink the glyph, so a
/// color-bitmap emoji font does not win and then draw nothing.
pub(super) fn fallback_for_character(character: char) -> Option<LoadedFont> {
    let cache = FALLBACK_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().unwrap().get(&character).cloned() {
        return cached;
    }
    let found = lookup_fallback(character);
    let mut cache = cache.lock().unwrap();
    if cache.len() >= CACHE_CAPACITY {
        cache.clear();
    }
    cache.insert(character, found.clone());
    found
}

fn lookup_fallback(character: char) -> Option<LoadedFont> {
    if let Some(font) = fontconfig_fallback(character)
        && inks(&font, character)
    {
        return Some(font);
    }
    #[cfg(test)]
    if is_cjk(character)
        && let Some(font) = bundled_cjk_face()
        && inks(&font, character)
    {
        return Some(font);
    }
    for family in preferred_families(character) {
        if let Some(font) = resolve(family, FontWeight::NORMAL, FontFaceStyle::Normal)
            && inks(&font, character)
        {
            return Some(font);
        }
    }
    scan_installed(character)
}

fn inks(font: &LoadedFont, character: char) -> bool {
    let Some(parsed) = font.parsed() else {
        return false;
    };
    if parsed.lookup_glyph_index(character) == 0 {
        return false;
    }
    let (metrics, bitmap) = parsed.rasterize(character, 16.0);
    metrics.width > 0 && bitmap.iter().any(|coverage| *coverage > 0)
}

fn preferred_families(character: char) -> &'static [&'static str] {
    if is_cjk(character) {
        &[
            "Noto Sans CJK SC",
            "Noto Sans SC",
            "Noto Sans CJK TC",
            "Noto Sans CJK JP",
            "Noto Sans CJK KR",
            "Source Han Sans SC",
            "Source Han Sans CN",
            "WenQuanYi Micro Hei",
            "WenQuanYi Zen Hei",
            "Droid Sans Fallback",
            "Noto Serif CJK SC",
            "PingFang SC",
            "Hiragino Sans GB",
            "Microsoft YaHei",
            "Heiti SC",
        ]
    } else if is_emoji(character) {
        &[
            "Noto Emoji",
            "Noto Sans Symbols 2",
            "Noto Sans Symbols",
            "Segoe UI Emoji",
            "Segoe UI Symbol",
            "Apple Color Emoji",
            "Noto Color Emoji",
        ]
    } else {
        &[
            "Noto Sans Symbols 2",
            "Noto Sans Symbols",
            "DejaVu Sans",
            "Noto Sans",
            "Segoe UI Symbol",
            "Droid Sans Fallback",
        ]
    }
}

fn is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{2E80}'..='\u{2FDF}'
            | '\u{3000}'..='\u{303F}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3100}'..='\u{312F}'
            | '\u{31C0}'..='\u{31EF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{AC00}'..='\u{D7AF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF00}'..='\u{FFEF}'
            | '\u{20000}'..='\u{2A6DF}'
    )
}

fn is_emoji(character: char) -> bool {
    matches!(
        character,
        '\u{1F000}'..='\u{1FAFF}' | '\u{2600}'..='\u{27BF}' | '\u{FE0F}'
    )
}

fn scan_installed(character: char) -> Option<LoadedFont> {
    let mut names: Vec<String> = {
        let database = database();
        database
            .faces()
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect()
    };
    names.sort_by_key(|name| (family_rank(name), name.clone()));
    names.dedup();
    for name in names.into_iter().take(64) {
        if let Some(font) = resolve(&name, FontWeight::NORMAL, FontFaceStyle::Normal)
            && inks(&font, character)
        {
            return Some(font);
        }
    }
    None
}

fn family_rank(name: &str) -> u8 {
    let name = name.to_ascii_lowercase();
    if name.contains("cjk")
        || name.contains("source han")
        || name.contains("wenquanyi")
        || name.contains("droid sans fallback")
        || name.contains("noto sans sc")
    {
        0
    } else if name.contains("emoji") || name.contains("symbol") {
        1
    } else if name.contains("noto sans") || name.contains("dejavu") {
        2
    } else {
        3
    }
}

/// Named families tried before the generic sans-serif fallback.
/// The order is the host UI query; do not reorder it.
const HOST_UI_NAMED_FAMILIES: &[&str] = &[
    "PingFang SC",
    "Microsoft YaHei",
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Noto Sans",
    "Roboto",
];

pub(super) fn host_ui_families() -> [fontdb::Family<'static>; 7] {
    [
        fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[0]),
        fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[1]),
        fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[2]),
        fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[3]),
        fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[4]),
        fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[5]),
        fontdb::Family::SansSerif,
    ]
}

pub(super) struct PreferredHostFace {
    pub bytes: Arc<Vec<u8>>,
    pub index: u32,
    pub family: String,
}

/// Fontconfig match for the host UI list, accepted only when a one-file
/// fontdb query selects a regular face of one of those families.
///
/// A full `load_system_fonts` scan stays the fallback when this file would
/// not be the database's choice (different weight, or only the generic
/// sans-serif alias).
pub(super) fn preferred_host_face() -> Option<PreferredHostFace> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        pin_test_fontconfig();
        let (path, _) = fontconfig_match_named(
            "PingFang SC,Microsoft YaHei,Noto Sans CJK SC,Noto Sans SC,Noto Sans,Roboto,sans-serif:weight=regular:slant=0",
        )?;
        return single_file_host_face(&[path]);
    }
    #[cfg(target_os = "macos")]
    {
        return macos_pingfang_files().and_then(|files| single_file_host_face(&files));
    }
    #[cfg(not(any(
        all(target_os = "linux", not(target_env = "ohos")),
        target_os = "macos"
    )))]
    {
        None
    }
}

/// macOS ships PingFang SC, the first host UI family, as a downloadable
/// asset. fontdb's `load_system_fonts` walks `/Library/Fonts`,
/// `/System/Library/Fonts`, every `AssetsV2/com_apple_MobileAsset_Font*`
/// and `~/Library/Fonts`; its query picks the first listed family that has
/// faces. When PingFang only lives in the asset directories, loading just
/// those files selects the same face without parsing every installed font.
#[cfg(target_os = "macos")]
fn macos_pingfang_files() -> Option<Vec<std::path::PathBuf>> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, any_pingfang: &mut bool) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out, any_pingfang);
            } else if path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("pingfang")
            }) {
                *any_pingfang = true;
                out.push(path);
            }
        }
    }
    // A PingFang file in a directory fontdb scans first would win ties.
    let home_fonts = std::env::var("HOME")
        .map(|home| std::path::Path::new(&home).join("Library/Fonts"))
        .ok();
    for dir in [
        "/Library/Fonts",
        "/System/Library/Fonts",
        "/Network/Library/Fonts",
    ]
    .into_iter()
    .map(std::path::PathBuf::from)
    .chain(home_fonts)
    {
        let mut found = Vec::new();
        let mut any = false;
        walk(&dir, &mut found, &mut any);
        if any {
            return None;
        }
    }
    let mut files = Vec::new();
    let mut any = false;
    for entry in std::fs::read_dir("/System/Library/AssetsV2")
        .ok()?
        .flatten()
    {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("com_apple_MobileAsset_Font")
        {
            walk(&entry.path(), &mut files, &mut any);
        }
    }
    (!files.is_empty()).then_some(files)
}

/// Accept a one-file match only when its query selects a regular face of a
/// named host UI family — the same face the full database query returns.
#[cfg(any(
    all(target_os = "linux", not(target_env = "ohos")),
    target_os = "macos"
))]
fn single_file_host_face(paths: &[std::path::PathBuf]) -> Option<PreferredHostFace> {
    {
        let mut database = fontdb::Database::new();
        for path in paths {
            database.load_font_file(path).ok()?;
        }
        let families = [
            fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[0]),
            fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[1]),
            fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[2]),
            fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[3]),
            fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[4]),
            fontdb::Family::Name(HOST_UI_NAMED_FAMILIES[5]),
        ];
        let id = database.query(&fontdb::Query {
            families: &families,
            weight: fontdb::Weight::NORMAL,
            ..fontdb::Query::default()
        })?;
        let face = database.face(id)?;
        let weight = face.weight.0;
        if !(350..=450).contains(&weight) {
            return None;
        }
        let family = face
            .families
            .iter()
            .find(|(name, _)| {
                HOST_UI_NAMED_FAMILIES
                    .iter()
                    .any(|wanted| wanted.eq_ignore_ascii_case(name))
            })?
            .0
            .clone();
        let index = face.index;
        let bytes = database.with_face_data(id, |data, _| Arc::new(data.to_vec()))?;
        Some(PreferredHostFace {
            bytes,
            index,
            family,
        })
    }
}

fn fontconfig_fallback(character: char) -> Option<LoadedFont> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        let (path, index) = fontconfig_match(character)?;
        let database = database();
        let id = database.faces().find_map(|face| {
            let fontdb::Source::File(file) = &face.source else {
                return None;
            };
            (file == &path && face.index == index).then_some(face.id)
        });
        drop(database);
        if let Some(id) = id {
            return load_by_id(id);
        }
        let _ = (path, index);
        None
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        let _ = character;
        None
    }
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn fontconfig_match_named(pattern: &str) -> Option<(std::path::PathBuf, u32)> {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::sync::OnceLock;

    struct Fc {
        init: unsafe extern "C" fn() -> *mut c_void,
        name_parse: unsafe extern "C" fn(*const u8) -> *mut c_void,
        pattern_destroy: unsafe extern "C" fn(*mut c_void),
        config_substitute: unsafe extern "C" fn(*mut c_void, *mut c_void, c_int) -> c_int,
        default_substitute: unsafe extern "C" fn(*mut c_void),
        font_match: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_int) -> *mut c_void,
        pattern_get_string:
            unsafe extern "C" fn(*const c_void, *const c_char, c_int, *mut *mut u8) -> c_int,
        pattern_get_integer:
            unsafe extern "C" fn(*const c_void, *const c_char, c_int, *mut c_int) -> c_int,
    }

    fn library() -> Option<&'static Fc> {
        static LOADED: OnceLock<Option<Fc>> = OnceLock::new();
        LOADED.get_or_init(|| unsafe { load_fontconfig() }).as_ref()
    }

    unsafe fn load_fontconfig() -> Option<Fc> {
        unsafe {
            let name = CString::new("libfontconfig.so.1").ok()?;
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
            Some(Fc {
                init: std::mem::transmute(symbol(b"FcInitLoadConfigAndFonts")?),
                name_parse: std::mem::transmute(symbol(b"FcNameParse")?),
                pattern_destroy: std::mem::transmute(symbol(b"FcPatternDestroy")?),
                config_substitute: std::mem::transmute(symbol(b"FcConfigSubstitute")?),
                default_substitute: std::mem::transmute(symbol(b"FcDefaultSubstitute")?),
                font_match: std::mem::transmute(symbol(b"FcFontMatch")?),
                pattern_get_string: std::mem::transmute(symbol(b"FcPatternGetString")?),
                pattern_get_integer: std::mem::transmute(symbol(b"FcPatternGetInteger")?),
            })
        }
    }

    let fc = library()?;
    let pattern_name = CString::new(pattern).ok()?;
    unsafe {
        if (fc.init)().is_null() {
            return None;
        }
        let pattern = (fc.name_parse)(pattern_name.as_ptr() as *const u8);
        if pattern.is_null() {
            return None;
        }
        let file_key = c"file".as_ptr();
        let index_key = c"index".as_ptr();
        (fc.config_substitute)(std::ptr::null_mut(), pattern, 0);
        (fc.default_substitute)(pattern);
        let mut result = 0;
        let matched = (fc.font_match)(std::ptr::null_mut(), pattern, &mut result);
        let resolved = if matched.is_null() || result != 0 {
            None
        } else {
            let mut file_ptr: *mut u8 = std::ptr::null_mut();
            let mut index = 0;
            let file_ok = (fc.pattern_get_string)(matched, file_key, 0, &mut file_ptr) == 0
                && !file_ptr.is_null();
            let _ = (fc.pattern_get_integer)(matched, index_key, 0, &mut index);
            if file_ok {
                CStr::from_ptr(file_ptr as *const c_char)
                    .to_str()
                    .ok()
                    .map(|file| (std::path::PathBuf::from(file), index.max(0) as u32))
            } else {
                None
            }
        };
        if !matched.is_null() {
            (fc.pattern_destroy)(matched);
        }
        (fc.pattern_destroy)(pattern);
        resolved
    }
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn fontconfig_match(character: char) -> Option<(std::path::PathBuf, u32)> {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::sync::OnceLock;

    struct Fc {
        init: unsafe extern "C" fn() -> *mut c_void,
        pattern_create: unsafe extern "C" fn() -> *mut c_void,
        pattern_destroy: unsafe extern "C" fn(*mut c_void),
        charset_create: unsafe extern "C" fn() -> *mut c_void,
        charset_destroy: unsafe extern "C" fn(*mut c_void),
        charset_add: unsafe extern "C" fn(*mut c_void, u32) -> c_int,
        pattern_add_charset:
            unsafe extern "C" fn(*mut c_void, *const c_char, *const c_void) -> c_int,
        pattern_add_bool: unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int,
        config_substitute: unsafe extern "C" fn(*mut c_void, *mut c_void, c_int) -> c_int,
        default_substitute: unsafe extern "C" fn(*mut c_void),
        font_match: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_int) -> *mut c_void,
        pattern_get_string:
            unsafe extern "C" fn(*const c_void, *const c_char, c_int, *mut *mut u8) -> c_int,
        pattern_get_integer:
            unsafe extern "C" fn(*const c_void, *const c_char, c_int, *mut c_int) -> c_int,
    }

    fn library() -> Option<&'static Fc> {
        static LOADED: OnceLock<Option<Fc>> = OnceLock::new();
        LOADED.get_or_init(|| unsafe { load_fontconfig() }).as_ref()
    }

    unsafe fn load_fontconfig() -> Option<Fc> {
        unsafe {
            let name = CString::new("libfontconfig.so.1").ok()?;
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
            Some(Fc {
                init: std::mem::transmute(symbol(b"FcInitLoadConfigAndFonts")?),
                pattern_create: std::mem::transmute(symbol(b"FcPatternCreate")?),
                pattern_destroy: std::mem::transmute(symbol(b"FcPatternDestroy")?),
                charset_create: std::mem::transmute(symbol(b"FcCharSetCreate")?),
                charset_destroy: std::mem::transmute(symbol(b"FcCharSetDestroy")?),
                charset_add: std::mem::transmute(symbol(b"FcCharSetAddChar")?),
                pattern_add_charset: std::mem::transmute(symbol(b"FcPatternAddCharSet")?),
                pattern_add_bool: std::mem::transmute(symbol(b"FcPatternAddBool")?),
                config_substitute: std::mem::transmute(symbol(b"FcConfigSubstitute")?),
                default_substitute: std::mem::transmute(symbol(b"FcDefaultSubstitute")?),
                font_match: std::mem::transmute(symbol(b"FcFontMatch")?),
                pattern_get_string: std::mem::transmute(symbol(b"FcPatternGetString")?),
                pattern_get_integer: std::mem::transmute(symbol(b"FcPatternGetInteger")?),
            })
        }
    }

    pin_test_fontconfig();
    let fc = library()?;
    unsafe {
        if (fc.init)().is_null() {
            return None;
        }
        let pattern = (fc.pattern_create)();
        let charset = (fc.charset_create)();
        if pattern.is_null() || charset.is_null() {
            if !charset.is_null() {
                (fc.charset_destroy)(charset);
            }
            if !pattern.is_null() {
                (fc.pattern_destroy)(pattern);
            }
            return None;
        }
        let file_key = c"file".as_ptr();
        let index_key = c"index".as_ptr();
        let charset_key = c"charset".as_ptr();
        let scalable_key = c"scalable".as_ptr();
        (fc.charset_add)(charset, character as u32);
        (fc.pattern_add_charset)(pattern, charset_key, charset);
        // Outline faces first. Color-bitmap emoji is tried later by name.
        (fc.pattern_add_bool)(pattern, scalable_key, 1);
        (fc.config_substitute)(std::ptr::null_mut(), pattern, 0);
        (fc.default_substitute)(pattern);
        let mut result = 0;
        let matched = (fc.font_match)(std::ptr::null_mut(), pattern, &mut result);
        let resolved = if matched.is_null() || result != 0 {
            None
        } else {
            let mut file_ptr: *mut u8 = std::ptr::null_mut();
            let mut index = 0;
            let file_ok = (fc.pattern_get_string)(matched, file_key, 0, &mut file_ptr) == 0
                && !file_ptr.is_null();
            let _ = (fc.pattern_get_integer)(matched, index_key, 0, &mut index);
            if file_ok {
                CStr::from_ptr(file_ptr as *const c_char)
                    .to_str()
                    .ok()
                    .map(|file| (std::path::PathBuf::from(file), index.max(0) as u32))
            } else {
                None
            }
        };
        if !matched.is_null() {
            (fc.pattern_destroy)(matched);
        }
        (fc.charset_destroy)(charset);
        (fc.pattern_destroy)(pattern);
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_single_file_host_face_matches_the_full_database_query() {
        let Some(files) = macos_pingfang_files() else {
            return;
        };
        let fast = single_file_host_face(&files).expect("PingFang regular face");
        let database = database();
        let id = database
            .query(&fontdb::Query {
                families: &host_ui_families(),
                ..fontdb::Query::default()
            })
            .expect("full query");
        let face = database.face(id).unwrap();
        let family = &face.families.first().unwrap().0;
        assert_eq!(family, &fast.family);
        assert_eq!(face.index, fast.index);
        let bytes = database.with_face_data(id, |data, _| data.len()).unwrap();
        assert_eq!(bytes, fast.bytes.len());
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn preferred_host_face_is_a_regular_named_family() {
        let fast = preferred_host_face().expect("fontconfig host face");
        assert!(!fast.bytes.is_empty());
        assert!(
            HOST_UI_NAMED_FAMILIES
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&fast.family)),
            "fast face family {}",
            fast.family
        );
        let mut lone = fontdb::Database::new();
        let path = std::env::temp_dir().join("w3cos-preferred-host-face.ttf");
        std::fs::write(&path, fast.bytes.as_slice()).unwrap();
        lone.load_font_file(&path).unwrap();
        let id = lone
            .query(&fontdb::Query {
                families: &HOST_UI_NAMED_FAMILIES
                    .iter()
                    .copied()
                    .map(fontdb::Family::Name)
                    .collect::<Vec<_>>(),
                weight: fontdb::Weight::NORMAL,
                ..fontdb::Query::default()
            })
            .expect("named regular face");
        let face = lone.face(id).unwrap();
        assert!((350..=450).contains(&face.weight.0));
        assert_eq!(face.index, fast.index);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn host_ui_font_matches_the_database_query() {
        let host = crate::font_face::host_ui_font();
        let database = database();
        let id = database
            .query(&fontdb::Query {
                families: &host_ui_families(),
                weight: fontdb::Weight::NORMAL,
                ..fontdb::Query::default()
            })
            .or_else(|| database.faces().next().map(|face| face.id))
            .expect("database host face");
        let (bytes, index) = database
            .with_face_data(id, |data, index| (data.to_vec(), index))
            .expect("database face bytes");
        assert_eq!(host.index, index);
        assert_eq!(host.data.as_slice(), bytes.as_slice());
    }

    #[test]
    fn latin_face_without_cjk_falls_back_to_an_installed_cjk_face() {
        let latin = resolve("Noto Sans", FontWeight::NORMAL, FontFaceStyle::Normal)
            .or_else(|| resolve("DejaVu Sans", FontWeight::NORMAL, FontFaceStyle::Normal))
            .expect("a latin system face");
        assert!(
            !latin.supports_character('文'),
            "the primary latin face is the one that drew tofu"
        );
        let fallback = fallback_for_character('文').expect("cjk fallback face");
        assert!(fallback.supports_character('文'));
        assert_ne!(fallback.family, latin.family);
        let parsed = fallback.parsed().expect("parsed fallback");
        let (metrics, bitmap) = parsed.rasterize('文', 32.0);
        assert!(metrics.width > 0 && bitmap.iter().any(|coverage| *coverage > 0));
    }

    #[test]
    fn check_mark_resolves_to_an_outline_symbol_face() {
        let fallback = fallback_for_character('✓').expect("symbol fallback");
        assert!(fallback.supports_character('✓'));
        let (metrics, bitmap) = fallback.parsed().unwrap().rasterize('✓', 32.0);
        assert!(metrics.width > 0 && bitmap.iter().any(|coverage| *coverage > 0));
    }

    #[test]
    fn emoji_fallback_rasterizes_when_an_outline_face_exists() {
        match fallback_for_character('😀') {
            Some(font) => {
                let (metrics, bitmap) = font.parsed().unwrap().rasterize('😀', 32.0);
                assert!(
                    metrics.width > 0 && bitmap.iter().any(|coverage| *coverage > 0),
                    "selected {} but it does not ink",
                    font.family
                );
            }
            None => {
                // Noto Color Emoji is a CBDT bitmap. The CPU rasterizer cannot
                // ink it, and DejaVu does not cover U+1F600, so no face is a
                // valid fallback. That is reported rather than drawn as tofu.
            }
        }
    }
}
