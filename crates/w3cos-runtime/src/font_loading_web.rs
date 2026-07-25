//! Browser-facing CSS Font Loading API backed by the native font registry.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;

use w3cos_core::Value;

use crate::font_face::{FontDisplay, FontFace, FontFaceSet, FontFaceStyle, FontSource, FontWeight};

thread_local! {
    static FONT_FACE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FONT_FACE_SET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FONT_FACE_SET_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FONT_FACES: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static JS_FAMILIES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static URL_WARNING_EMITTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
                        "[w3cos] warning: FontFace network URL loading requires a host fetch \
                         adapter; use ArrayBuffer, local(), file://, or a local path"
                    );
                }
            });
            return Err("NetworkError: FontFace network URL loading is unavailable".into());
        }
        return Ok(FontSource::Path(PathBuf::from(
            url.strip_prefix("file://").unwrap_or(url),
        )));
    }
    Ok(FontSource::Local(source))
}

fn load_face(face: &Value) -> Result<(), String> {
    if face.get_property("status").to_js_string() == "loaded" {
        return Ok(());
    }
    face.set_property("status", Value::string("loading"));
    let source = source_from_value(&face.get_property("__w3cos_source"))?;
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
            Ok(())
        }
        Err(error) => {
            face.set_property("status", Value::string("error"));
            Err(error)
        }
    }
}

pub fn font_face_class() -> Value {
    FONT_FACE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let family = arg(&args, 0).to_js_string();
            JS_FAMILIES.with(|families| {
                families.borrow_mut().insert(family.clone());
            });
            let source = arg(&args, 1);
            let descriptors = arg(&args, 2);
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
                Value::function(move |_, _| match load_face(&face_for_load) {
                    Ok(()) => resolved(face_for_load.clone()),
                    Err(error) => rejected(error),
                }),
            );
            let face_for_loaded = value.clone();
            value.set_property(
                "__w3cos_getter_loaded",
                Value::function(move |_, _| {
                    if face_for_loaded.get_property("status").to_js_string() == "error" {
                        rejected("NetworkError: FontFace failed to load")
                    } else {
                        match load_face(&face_for_loaded) {
                            Ok(()) => resolved(face_for_loaded.clone()),
                            Err(error) => rejected(error),
                        }
                    }
                }),
            );
            w3cos_core::class::set_prototype_of(
                &value,
                &font_face_class().get_property("prototype"),
            );
            value
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
        let set = Value::object(HashMap::new());
        set.set_property("status", Value::string("loaded"));
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
                FONT_FACES.with(|faces| faces.borrow_mut().clear());
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
                let faces = matching_faces(&arg(&args, 0).to_js_string());
                for face in &faces {
                    if let Err(error) = load_face(face) {
                        return rejected(error);
                    }
                }
                resolved(Value::array(faces))
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
        for name in ["addEventListener", "removeEventListener"] {
            set.set_property(name, Value::function(|_, _| Value::Undefined));
        }
        set.set_property("dispatchEvent", Value::function(|_, _| Value::Bool(true)));
        let set_for_ready = set.clone();
        set.set_property(
            "__w3cos_getter_ready",
            Value::function(move |_, _| resolved(set_for_ready.clone())),
        );
        w3cos_core::class::set_prototype_of(&set, &font_face_set_class().get_property("prototype"));
        *slot.borrow_mut() = Some(set.clone());
        set
    })
}

pub fn reset() {
    FONT_FACES.with(|faces| faces.borrow_mut().clear());
    JS_FAMILIES.with(|families| families.borrow_mut().clear());
    if let Some(set) = FONT_FACE_SET_VALUE.with(|slot| slot.borrow().clone()) {
        update_size(&set);
    }
}
