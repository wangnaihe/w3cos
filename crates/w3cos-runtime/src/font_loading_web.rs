//! Browser-facing CSS Font Loading API backed by the native font registry.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use w3cos_core::Value;

use crate::font_face::{FontDisplay, FontFace, FontFaceSet, FontFaceStyle, FontSource, FontWeight};

thread_local! {
    static FONT_FACE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FONT_FACE_SET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FONT_FACE_SET_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FONT_FACES: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static STYLESHEET_FONT_FACES: RefCell<HashMap<u64, Vec<Value>>> =
        RefCell::new(HashMap::new());
    static JS_FAMILIES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static ACTIVE_FONT_LOADS: Cell<usize> = const { Cell::new(0) };
    static READY_CYCLE: RefCell<Option<ReadyCycle>> = const { RefCell::new(None) };
    static CYCLE_LOADED_FACES: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static CYCLE_FAILED_FACES: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static URL_WARNING_EMITTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct ReadyCycle {
    promise: Value,
    resolve: Option<Value>,
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn resolved(value: Value) -> Value {
    w3cos_core::promise::resolve(vec![value])
}

fn rejected(message: impl Into<String>) -> Value {
    w3cos_core::promise::reject(vec![Value::string(&message.into())])
}

fn pending_ready_cycle() -> ReadyCycle {
    let resolver = Rc::new(RefCell::new(None));
    let resolver_for_executor = Rc::clone(&resolver);
    let promise = w3cos_core::promise::new(vec![Value::function(move |_, args| {
        *resolver_for_executor.borrow_mut() = Some(arg(&args, 0));
        Value::Undefined
    })]);
    let resolve = resolver
        .borrow_mut()
        .take()
        .expect("Promise executor supplies a ready resolver");
    ReadyCycle {
        promise,
        resolve: Some(resolve),
    }
}

fn ready_promise(set: &Value) -> Value {
    READY_CYCLE.with(|cycle| {
        let mut cycle = cycle.borrow_mut();
        if cycle.is_none() {
            *cycle = Some(if ACTIVE_FONT_LOADS.with(Cell::get) == 0 {
                ReadyCycle {
                    promise: resolved(set.clone()),
                    resolve: None,
                }
            } else {
                pending_ready_cycle()
            });
        }
        cycle
            .as_ref()
            .expect("font ready cycle initialized")
            .promise
            .clone()
    })
}

fn dispatch_loading_event(set: &Value, event_type: &str, faces: Vec<Value>) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(event_type)],
    );
    event.set_property("fontfaces", Value::array(faces));
    set.call_method("dispatchEvent", vec![event]);
}

pub(crate) fn begin_font_loading(faces: Vec<Value>) {
    let first = ACTIVE_FONT_LOADS.with(|active| {
        let previous = active.get();
        active.set(previous.saturating_add(1));
        previous == 0
    });
    if !first {
        return;
    }
    CYCLE_LOADED_FACES.with(|loaded| loaded.borrow_mut().clear());
    CYCLE_FAILED_FACES.with(|failed| failed.borrow_mut().clear());
    READY_CYCLE.with(|cycle| *cycle.borrow_mut() = Some(pending_ready_cycle()));
    if let Some(set) = FONT_FACE_SET_VALUE.with(|slot| slot.borrow().clone()) {
        set.set_property("status", Value::string("loading"));
        dispatch_loading_event(&set, "loading", faces);
    }
}

pub(crate) fn finish_font_loading(loaded_faces: Vec<Value>, failed_faces: Vec<Value>) {
    CYCLE_LOADED_FACES.with(|collected| {
        let mut collected = collected.borrow_mut();
        for face in loaded_faces {
            if !collected.iter().any(|existing| existing == &face) {
                collected.push(face);
            }
        }
    });
    CYCLE_FAILED_FACES.with(|collected| {
        let mut collected = collected.borrow_mut();
        for face in failed_faces {
            if !collected.iter().any(|existing| existing == &face) {
                collected.push(face);
            }
        }
    });
    let finished = ACTIVE_FONT_LOADS.with(|active| {
        let previous = active.get();
        if previous == 0 {
            return false;
        }
        active.set(previous.saturating_sub(1));
        previous == 1
    });
    if !finished {
        return;
    }
    let loaded_faces = CYCLE_LOADED_FACES.with(|faces| std::mem::take(&mut *faces.borrow_mut()));
    let failed_faces = CYCLE_FAILED_FACES.with(|faces| std::mem::take(&mut *faces.borrow_mut()));
    let set = FONT_FACE_SET_VALUE.with(|slot| slot.borrow().clone());
    if let Some(set) = &set {
        set.set_property("status", Value::string("loaded"));
    }
    READY_CYCLE.with(|cycle| {
        let resolve = cycle
            .borrow_mut()
            .as_mut()
            .and_then(|cycle| cycle.resolve.take());
        if let (Some(resolve), Some(set)) = (resolve, set.clone()) {
            resolve.call(Value::Undefined, vec![set]);
        }
    });
    if let Some(set) = set {
        dispatch_loading_event(&set, "loadingdone", loaded_faces);
        if !failed_faces.is_empty() {
            dispatch_loading_event(&set, "loadingerror", failed_faces);
        }
    }
}

pub(crate) fn cancel_font_loading(faces: Vec<Value>) {
    for face in &faces {
        if face.get_property("status").to_js_string() == "loading" {
            face.set_property("status", Value::string("error"));
        }
    }
    finish_font_loading(Vec::new(), faces);
}

fn display_from_str(value: &str) -> FontDisplay {
    match value {
        "block" => FontDisplay::Block,
        "swap" => FontDisplay::Swap,
        "fallback" => FontDisplay::Fallback,
        "optional" => FontDisplay::Optional,
        _ => FontDisplay::Auto,
    }
}

fn source_from_value(value: &Value) -> Result<FontSource, String> {
    if let Some(bytes) = w3cos_core::binary::bytes_of(value) {
        return Ok(FontSource::Bytes(bytes));
    }
    let source = value.to_js_string();
    if let Some(local) = source.strip_prefix("local(") {
        return Ok(FontSource::Local(
            local
                .trim_end_matches(')')
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        ));
    }
    if let Some(url) = source.strip_prefix("url(") {
        let url = url
            .trim_end_matches(')')
            .trim()
            .trim_matches('"')
            .trim_matches('\'');
        if url.starts_with("http://") || url.starts_with("https://") {
            URL_WARNING_EMITTED.with(|warned| {
                if !warned.replace(true) {
                    eprintln!(
                        "[w3cos] warning: FontFace network URL loading requires an active \
                         dynamic Browser document; use ArrayBuffer, local(), file://, or a \
                         local path in ordinary AOT"
                    );
                }
            });
            return Err(
                "NetworkError: FontFace network URL loading requires an active Browser document"
                    .into(),
            );
        }
        return Ok(FontSource::Path(PathBuf::from(
            url.strip_prefix("file://").unwrap_or(url),
        )));
    }
    Ok(FontSource::Local(source))
}

#[cfg(feature = "dynamic-js")]
fn url_source(value: &Value) -> Option<String> {
    let source = value.to_js_string();
    let source = source.trim().strip_prefix("url(")?;
    let closing = source.find(')')?;
    Some(
        source[..closing]
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string(),
    )
}

#[cfg(feature = "dynamic-js")]
fn browser_network_source(value: &Value) -> Result<Option<FontSource>, String> {
    let Some(requested_url) = url_source(value) else {
        return Ok(None);
    };
    let resolved_url = crate::fetch::resolve_page_fetch_url(&requested_url)?;
    let parsed = url::Url::parse(&resolved_url)
        .map_err(|error| format!("NetworkError: invalid FontFace URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Ok(None);
    }
    let Some((allow_network, max_source_bytes)) =
        crate::dynamic_script::active_document_font_fetch_limits()
    else {
        return Err(
            "NetworkError: FontFace network URL loading requires an active Browser document".into(),
        );
    };
    if !allow_network {
        return Err("SecurityError: network loading is disabled by ScriptPolicy".into());
    }
    let (bytes, content_type, final_url) =
        crate::fetch::fetch_page_font_bytes(&resolved_url, max_source_bytes)
            .map_err(|error| format!("NetworkError: {error}"))?;
    if !crate::dynamic_script::is_supported_font_mime_type(&content_type) {
        return Err(format!(
            "NetworkError: FontFace MIME check failed for {}: received {}",
            final_url,
            if content_type.is_empty() || content_type == "null" {
                "<missing>"
            } else {
                &content_type
            }
        ));
    }
    crate::font_face::normalize_font_bytes_with_limit(&bytes, max_source_bytes)
        .map(FontSource::Bytes)
        .map(Some)
        .map_err(|error| format!("NetworkError: FontFace decode failed: {error}"))
}

fn load_face(face: &Value) -> Result<(), String> {
    if face.get_property("status").to_js_string() == "loaded" {
        return Ok(());
    }
    begin_font_loading(vec![face.clone()]);
    FontFaceSet::global().mark_loading();
    face.set_property("status", Value::string("loading"));
    let source_value = face.get_property("__w3cos_source");
    #[cfg(feature = "dynamic-js")]
    let source = match browser_network_source(&source_value)
        .and_then(|network| network.map_or_else(|| source_from_value(&source_value), Ok))
    {
        Ok(source) => source,
        Err(error) => {
            face.set_property("status", Value::string("error"));
            FontFaceSet::global().mark_ready();
            finish_font_loading(Vec::new(), vec![face.clone()]);
            return Err(error);
        }
    };
    #[cfg(not(feature = "dynamic-js"))]
    let source = match source_from_value(&source_value) {
        Ok(source) => source,
        Err(error) => {
            face.set_property("status", Value::string("error"));
            FontFaceSet::global().mark_ready();
            finish_font_loading(Vec::new(), vec![face.clone()]);
            return Err(error);
        }
    };
    let native = FontFace {
        family: face.get_property("family").to_js_string(),
        src: source,
        weight: FontWeight::from_str(&face.get_property("weight").to_js_string()),
        style: FontFaceStyle::from_str(&face.get_property("style").to_js_string()),
        display: display_from_str(&face.get_property("display").to_js_string()),
        unicode_range: Some(face.get_property("unicodeRange").to_js_string())
            .filter(|value| !value.is_empty()),
    };
    match FontFaceSet::global().add(native) {
        Ok(()) => {
            face.set_property("status", Value::string("loaded"));
            FontFaceSet::global().mark_ready();
            finish_font_loading(vec![face.clone()], Vec::new());
            Ok(())
        }
        Err(error) => {
            face.set_property("status", Value::string("error"));
            FontFaceSet::global().mark_ready();
            finish_font_loading(Vec::new(), vec![face.clone()]);
            Err(error)
        }
    }
}

fn stylesheet_face_loaded_promise(face: &Value) -> Value {
    match face.get_property("status").to_js_string().as_str() {
        "loaded" => resolved(face.clone()),
        "error" => rejected("NetworkError: FontFace failed to load"),
        _ => {
            let face = face.clone();
            font_face_set_value().get_property("ready").call_method(
                "then",
                vec![Value::function(move |_, _| {
                    if face.get_property("status").to_js_string() == "loaded" {
                        face.clone()
                    } else {
                        rejected("NetworkError: FontFace failed to load")
                    }
                })],
            )
        }
    }
}

fn create_font_face_value(
    family: String,
    source: Value,
    descriptors: Value,
    stylesheet_connected: bool,
) -> Value {
    JS_FAMILIES.with(|families| {
        families.borrow_mut().insert(family.clone());
    });
    let value = Value::object(HashMap::from([
        ("family".to_string(), Value::string(&family)),
        ("style".to_string(), Value::string("normal")),
        ("weight".to_string(), Value::string("normal")),
        ("stretch".to_string(), Value::string("normal")),
        ("unicodeRange".to_string(), Value::string("U+0-10FFFF")),
        ("featureSettings".to_string(), Value::string("normal")),
        ("variationSettings".to_string(), Value::string("normal")),
        ("display".to_string(), Value::string("auto")),
        ("status".to_string(), Value::string("unloaded")),
        ("__w3cos_source".to_string(), source),
        (
            "__w3cos_stylesheet_connected".to_string(),
            Value::Bool(stylesheet_connected),
        ),
    ]));
    for name in [
        "style",
        "weight",
        "stretch",
        "unicodeRange",
        "featureSettings",
        "variationSettings",
        "display",
    ] {
        let descriptor = descriptors.get_property(name);
        if !descriptor.is_undefined() {
            value.set_property(name, descriptor);
        }
    }
    let face_for_load = value.clone();
    value.set_property(
        "load",
        Value::function(move |_, _| {
            if stylesheet_connected {
                #[cfg(feature = "dynamic-js")]
                crate::dynamic_script::request_stylesheet_font_faces(
                    std::slice::from_ref(&face_for_load),
                    "",
                );
                stylesheet_face_loaded_promise(&face_for_load)
            } else {
                match load_face(&face_for_load) {
                    Ok(()) => resolved(face_for_load.clone()),
                    Err(error) => rejected(error),
                }
            }
        }),
    );
    let face_for_loaded = value.clone();
    value.set_property(
        "__w3cos_getter_loaded",
        Value::function(move |_, _| {
            if stylesheet_connected {
                stylesheet_face_loaded_promise(&face_for_loaded)
            } else if face_for_loaded.get_property("status").to_js_string() == "error" {
                rejected("NetworkError: FontFace failed to load")
            } else {
                match load_face(&face_for_loaded) {
                    Ok(()) => resolved(face_for_loaded.clone()),
                    Err(error) => rejected(error),
                }
            }
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &font_face_class().get_property("prototype"));
    value
}

pub(crate) fn register_stylesheet_font_face(
    owner: u64,
    family: &str,
    style: Option<&str>,
    weight: Option<&str>,
    display: Option<&str>,
    unicode_range: Option<&str>,
) -> Value {
    let descriptors = Value::object(HashMap::new());
    for (name, descriptor) in [
        ("style", style),
        ("weight", weight),
        ("display", display),
        ("unicodeRange", unicode_range),
    ] {
        if let Some(descriptor) = descriptor {
            descriptors.set_property(name, Value::string(descriptor));
        }
    }
    let face = create_font_face_value(family.to_string(), Value::Undefined, descriptors, true);
    FONT_FACES.with(|faces| faces.borrow_mut().push(face.clone()));
    STYLESHEET_FONT_FACES.with(|faces| {
        faces
            .borrow_mut()
            .entry(owner)
            .or_default()
            .push(face.clone());
    });
    if let Some(set) = FONT_FACE_SET_VALUE.with(|slot| slot.borrow().clone()) {
        update_size(&set);
    }
    face
}

pub(crate) fn clear_stylesheet_font_owner(owner: u64) {
    let removed = STYLESHEET_FONT_FACES.with(|faces| faces.borrow_mut().remove(&owner));
    let Some(removed) = removed else {
        return;
    };
    FONT_FACES.with(|faces| {
        faces
            .borrow_mut()
            .retain(|face| !removed.iter().any(|removed| removed == face));
    });
    JS_FAMILIES.with(|families| {
        let remaining = FONT_FACES.with(|faces| {
            faces
                .borrow()
                .iter()
                .map(|face| face.get_property("family").to_js_string())
                .collect()
        });
        *families.borrow_mut() = remaining;
    });
    if let Some(set) = FONT_FACE_SET_VALUE.with(|slot| slot.borrow().clone()) {
        update_size(&set);
    }
}

pub fn font_face_class() -> Value {
    FONT_FACE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let family = arg(&args, 0).to_js_string();
            let source = arg(&args, 1);
            let descriptors = arg(&args, 2);
            create_font_face_value(family, source, descriptors, false)
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "ascentOverride",
            "descentOverride",
            "display",
            "family",
            "featureSettings",
            "lineGapOverride",
            "load",
            "loaded",
            "sizeAdjust",
            "status",
            "stretch",
            "style",
            "unicodeRange",
            "variant",
            "variationSettings",
            "weight",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn css_font_query(value: &str) -> (String, FontWeight, FontFaceStyle) {
    let tokens: Vec<&str> = value.split_whitespace().collect();
    let size_index = tokens
        .iter()
        .position(|token| token.contains("px") || token.contains("pt") || token.contains("em"))
        .unwrap_or(0);
    let family = tokens
        .iter()
        .skip(size_index + 1)
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string();
    let prefix = &tokens[..size_index];
    let weight = prefix
        .iter()
        .find(|token| {
            token.parse::<u16>().is_ok()
                || matches!(
                    **token,
                    "normal"
                        | "bold"
                        | "bolder"
                        | "lighter"
                        | "thin"
                        | "light"
                        | "medium"
                        | "semibold"
                        | "semi-bold"
                        | "black"
                )
        })
        .copied()
        .unwrap_or("normal");
    let style = prefix
        .iter()
        .find(|token| matches!(**token, "italic" | "oblique"))
        .copied()
        .unwrap_or("normal");
    (
        family,
        FontWeight::from_str(weight),
        FontFaceStyle::from_str(style),
    )
}

fn matching_faces(font: &str) -> Vec<Value> {
    let (family, weight, style) = css_font_query(font);
    FONT_FACES.with(|faces| {
        faces
            .borrow()
            .iter()
            .filter(|face| {
                face.get_property("family").to_js_string() == family
                    && FontWeight::from_str(&face.get_property("weight").to_js_string()) == weight
                    && FontFaceStyle::from_str(&face.get_property("style").to_js_string()) == style
            })
            .cloned()
            .collect()
    })
}

fn matching_faces_for_text(font: &str, text: &str) -> Vec<Value> {
    if text.is_empty() {
        return Vec::new();
    }
    let (family, weight, style) = css_font_query(font);
    FONT_FACES.with(|faces| {
        let faces = faces.borrow();
        let mut selected = Vec::new();
        for character in text.chars() {
            let mut encoded = [0; 4];
            let character = character.encode_utf8(&mut encoded);
            let best = faces
                .iter()
                .filter_map(|face| font_face_value_score(face, &family, weight, style, character))
                .min();
            let Some(best) = best else {
                continue;
            };
            for face in faces.iter().filter(|face| {
                font_face_value_score(face, &family, weight, style, character) == Some(best)
            }) {
                if !selected.iter().any(|existing| existing == face) {
                    selected.push(face.clone());
                }
            }
        }
        selected
    })
}

fn font_face_value_score(
    face: &Value,
    family: &str,
    weight: FontWeight,
    style: FontFaceStyle,
    text: &str,
) -> Option<(u8, u16, u16)> {
    if !face
        .get_property("family")
        .to_js_string()
        .eq_ignore_ascii_case(family)
        || !crate::font_face::unicode_range_matches_text(
            Some(&face.get_property("unicodeRange").to_js_string()),
            text,
        )
    {
        return None;
    }
    let candidate_weight = FontWeight::from_str(&face.get_property("weight").to_js_string());
    let candidate_style = FontFaceStyle::from_str(&face.get_property("style").to_js_string());
    Some((
        u8::from(candidate_style != style),
        candidate_weight.0.abs_diff(weight.0),
        candidate_weight.0,
    ))
}

fn update_size(set: &Value) {
    let size = FONT_FACES.with(|faces| faces.borrow().len());
    set.set_property("size", Value::Number(size as f64));
}

pub fn font_face_set_class() -> Value {
    FONT_FACE_SET_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| font_face_set_value());
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn font_face_set_value() -> Value {
    FONT_FACE_SET_VALUE.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            update_size(&value);
            return value;
        }
        let set = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
        set.set_property(
            "status",
            Value::string(if ACTIVE_FONT_LOADS.with(Cell::get) == 0 {
                "loaded"
            } else {
                "loading"
            }),
        );
        set.set_property("size", Value::Number(0.0));

        let set_for_add = set.clone();
        set.set_property(
            "add",
            Value::function(move |_, args| {
                let face = arg(&args, 0);
                if !w3cos_core::class::instance_of(&face, &font_face_class()) {
                    return set_for_add.clone();
                }
                FONT_FACES.with(|faces| {
                    if !faces.borrow().iter().any(|existing| existing == &face) {
                        faces.borrow_mut().push(face);
                    }
                });
                update_size(&set_for_add);
                set_for_add.clone()
            }),
        );
        let set_for_delete = set.clone();
        set.set_property(
            "delete",
            Value::function(move |_, args| {
                let face = arg(&args, 0);
                if face.get_property("__w3cos_stylesheet_connected").to_bool() {
                    return Value::Bool(false);
                }
                let removed = FONT_FACES.with(|faces| {
                    let mut faces = faces.borrow_mut();
                    let before = faces.len();
                    faces.retain(|existing| existing != &face);
                    before != faces.len()
                });
                update_size(&set_for_delete);
                Value::Bool(removed)
            }),
        );
        let set_for_clear = set.clone();
        set.set_property(
            "clear",
            Value::function(move |_, _| {
                FONT_FACES.with(|faces| {
                    faces
                        .borrow_mut()
                        .retain(|face| face.get_property("__w3cos_stylesheet_connected").to_bool());
                });
                update_size(&set_for_clear);
                Value::Undefined
            }),
        );
        set.set_property(
            "has",
            Value::function(|_, args| {
                let face = arg(&args, 0);
                Value::Bool(
                    FONT_FACES
                        .with(|faces| faces.borrow().iter().any(|existing| existing == &face)),
                )
            }),
        );
        set.set_property(
            "check",
            Value::function(|_, args| {
                let query = arg(&args, 0).to_js_string();
                let (family, weight, style) = css_font_query(&query);
                let loaded_match = matching_faces(&query)
                    .iter()
                    .any(|face| face.get_property("status").to_js_string() == "loaded");
                let js_family = JS_FAMILIES.with(|families| families.borrow().contains(&family));
                Value::Bool(if js_family {
                    loaded_match
                } else {
                    crate::font_face::FontRegistry::global()
                        .resolve(&family, weight, style)
                        .is_some()
                })
            }),
        );
        set.set_property(
            "load",
            Value::function(|_, args| {
                let query = arg(&args, 0).to_js_string();
                let text = args
                    .get(1)
                    .map(Value::to_js_string)
                    .unwrap_or_else(|| " ".to_string());
                let faces = matching_faces_for_text(&query, &text);
                #[cfg(feature = "dynamic-js")]
                crate::dynamic_script::request_stylesheet_font_faces(&faces, &text);
                let mut promises = Vec::with_capacity(faces.len());
                for face in &faces {
                    if face.get_property("__w3cos_stylesheet_connected").to_bool() {
                        promises.push(stylesheet_face_loaded_promise(face));
                    } else {
                        match load_face(face) {
                            Ok(()) => promises.push(resolved(face.clone())),
                            Err(error) => return rejected(error),
                        }
                    }
                }
                w3cos_core::promise::all(vec![Value::array(promises)])
            }),
        );
        for name in ["values", "keys"] {
            set.set_property(
                name,
                Value::function(|_, _| {
                    Value::array(FONT_FACES.with(|faces| faces.borrow().clone()))
                }),
            );
        }
        set.set_property(
            "entries",
            Value::function(|_, _| {
                Value::array(FONT_FACES.with(|faces| {
                    faces
                        .borrow()
                        .iter()
                        .map(|face| Value::array(vec![face.clone(), face.clone()]))
                        .collect()
                }))
            }),
        );
        let set_for_each = set.clone();
        set.set_property(
            "forEach",
            Value::function(move |_, args| {
                let callback = arg(&args, 0);
                let this_arg = arg(&args, 1);
                for face in FONT_FACES.with(|faces| faces.borrow().clone()) {
                    callback.call(
                        this_arg.clone(),
                        vec![face.clone(), face, set_for_each.clone()],
                    );
                }
                Value::Undefined
            }),
        );
        let set_for_ready = set.clone();
        set.set_property(
            "__w3cos_getter_ready",
            Value::function(move |_, _| ready_promise(&set_for_ready)),
        );
        for name in ["onloading", "onloadingdone", "onloadingerror"] {
            set.set_property(name, Value::Null);
        }
        w3cos_core::class::set_prototype_of(&set, &font_face_set_class().get_property("prototype"));
        *slot.borrow_mut() = Some(set.clone());
        set
    })
}

pub fn reset() {
    FONT_FACES.with(|faces| faces.borrow_mut().clear());
    STYLESHEET_FONT_FACES.with(|faces| faces.borrow_mut().clear());
    JS_FAMILIES.with(|families| families.borrow_mut().clear());
    ACTIVE_FONT_LOADS.with(|active| active.set(0));
    READY_CYCLE.with(|cycle| cycle.borrow_mut().take());
    CYCLE_LOADED_FACES.with(|faces| faces.borrow_mut().clear());
    CYCLE_FAILED_FACES.with(|faces| faces.borrow_mut().clear());
    if let Some(set) = FONT_FACE_SET_VALUE.with(|slot| slot.borrow().clone()) {
        set.set_property("status", Value::string("loaded"));
        update_size(&set);
    }
}
