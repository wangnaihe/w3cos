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
    DATABASE.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        #[cfg(any(target_os = "android", target_env = "ohos"))]
        database.load_fonts_dir("/system/fonts");
        #[cfg(target_os = "ios")]
        for path in ["/System/Library/Fonts", "/System/Library/Fonts/Core", "/System/Library/Fonts/Cache"] {
            database.load_fonts_dir(path);
        }
        Mutex::new(database)
    }).lock().unwrap()
}

pub(super) fn resolve(family: &str, weight: FontWeight, style: FontFaceStyle) -> Option<LoadedFont> {
    let key = FontKey { family: family.to_lowercase(), weight, style };
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(face) = cache.lock().unwrap().get(&key).cloned() { return face; }
    let face = load(family, weight, style);
    let mut cache = cache.lock().unwrap();
    if cache.len() >= CACHE_CAPACITY { cache.clear(); }
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
    // Different CSS weights/aliases can select the same installed face.
    // Share its bytes and parsed cmap, not one copy per requested weight.
    let faces = FACES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(face) = faces.lock().unwrap().get(&id).cloned() { return Some(face); }
    let info = database.face(id)?;
    let family = info.families.first()?.0.clone();
    let weight = FontWeight(info.weight.0);
    let style = match info.style {
        fontdb::Style::Normal => FontFaceStyle::Normal,
        fontdb::Style::Italic => FontFaceStyle::Italic,
        fontdb::Style::Oblique => FontFaceStyle::Oblique,
    };
    let is_monospace = info.monospaced;
    let (data, index) = database.with_face_data(id, |data, index| (Arc::new(data.to_vec()), index))?;
    drop(database);
    let parsed = Arc::new(fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings {
        collection_index: index, ..fontdue::FontSettings::default()
    }).ok()?);
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    index.hash(&mut hasher);
    family.hash(&mut hasher);
    weight.hash(&mut hasher);
    style.hash(&mut hasher);
    #[cfg(feature = "skia")]
    let skia_typeface = {
        use skia_safe::{FontMgr, FontStyle, font_style};
        let face = FontMgr::default().match_family_style(&family, FontStyle::new(
            (weight.0 as i32).into(), font_style::Width::NORMAL,
            match style { FontFaceStyle::Normal => font_style::Slant::Upright,
                FontFaceStyle::Italic => font_style::Slant::Italic,
                FontFaceStyle::Oblique => font_style::Slant::Oblique },
        )).or_else(|| FontMgr::default().new_from_data(data.as_slice(), Some(index as usize)));
        let cell = OnceLock::new();
        let _ = cell.set(face);
        Arc::new(cell)
    };
    let loaded = LoadedFont {
        family, weight, style, data, is_monospace, parsed: Some(parsed),
        unicode_ranges: None, cache_key: hasher.finish(), collection_index: Some(index),
        #[cfg(feature = "skia")]
        skia_typeface,
    };
    let mut faces = faces.lock().unwrap();
    if faces.len() >= CACHE_CAPACITY { faces.clear(); }
    let loaded = faces.entry(id).or_insert(loaded).clone();
    Some(loaded)
}
