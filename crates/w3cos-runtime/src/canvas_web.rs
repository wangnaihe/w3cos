//! JavaScript constructors layered over the existing Canvas 2D engine.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

use crate::canvas2d::PathOp;

const PATH_ID: &str = "__w3cos_path_2d_id";

thread_local! {
    static NEXT_PATH_ID: Cell<u64> = const { Cell::new(1) };
    static PATHS: RefCell<HashMap<u64, Vec<PathOp>>> = RefCell::new(HashMap::new());
    static IMAGE_DATA_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PATH_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static OFFSCREEN_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn path_id(value: &Value) -> Option<u64> {
    let Value::Object(object) = value else {
        return None;
    };
    match object.borrow().get_direct(PATH_ID) {
        Value::Number(id) => Some(id as u64),
        _ => None,
    }
}

pub fn path_ops(value: &Value) -> Option<Vec<PathOp>> {
    PATHS.with(|paths| paths.borrow().get(&path_id(value)?).cloned())
}

fn push_path_op(value: &Value, op: PathOp) {
    if let Some(id) = path_id(value) {
        PATHS.with(|paths| paths.borrow_mut().entry(id).or_default().push(op));
    }
}

pub fn image_data_class() -> Value {
    IMAGE_DATA_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let first = args.first().cloned().unwrap_or_default();
            let (data, width, height) = if w3cos_core::binary::is_typed_array(&first) {
                let width = args.get(1).map(Value::to_u32).unwrap_or(0);
                let bytes = w3cos_core::binary::bytes_of(&first).unwrap_or_default();
                let height = args
                    .get(2)
                    .map(Value::to_u32)
                    .unwrap_or_else(|| (bytes.len() as u32 / 4) / width.max(1));
                (first, width, height)
            } else {
                let width = first.to_u32();
                let height = args.get(1).map(Value::to_u32).unwrap_or(0);
                let data = w3cos_core::class::construct(
                    &w3cos_core::binary::typed_array_class("Uint8ClampedArray"),
                    vec![Value::Number((width * height * 4) as f64)],
                );
                (data, width, height)
            };
            image_data_value(data, width, height)
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn image_data_value(data: Value, width: u32, height: u32) -> Value {
    let value = Value::object(HashMap::from([
        ("data".to_string(), data),
        ("width".to_string(), Value::Number(width as f64)),
        ("height".to_string(), Value::Number(height as f64)),
        ("colorSpace".to_string(), Value::string("srgb")),
    ]));
    w3cos_core::class::set_prototype_of(&value, &image_data_class().get_property("prototype"));
    value
}

pub fn path_2d_class() -> Value {
    PATH_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let id = NEXT_PATH_ID.with(|next| {
                let id = next.get();
                next.set(id + 1);
                id
            });
            let initial = args.first().and_then(path_ops).unwrap_or_default();
            PATHS.with(|paths| paths.borrow_mut().insert(id, initial));
            let value = Value::object(HashMap::from([(
                PATH_ID.to_string(),
                Value::Number(id as f64),
            )]));
            w3cos_core::class::set_prototype_of(&value, &path_2d_class().get_property("prototype"));
            for name in [
                "moveTo",
                "lineTo",
                "rect",
                "arc",
                "quadraticCurveTo",
                "bezierCurveTo",
            ] {
                value.set_property(
                    name,
                    Value::function(move |this, args| {
                        let n = |index: usize| {
                            args.get(index).map(Value::to_number).unwrap_or(0.0) as f32
                        };
                        let op = match name {
                            "moveTo" => PathOp::MoveTo(n(0), n(1)),
                            "lineTo" => PathOp::LineTo(n(0), n(1)),
                            "rect" => PathOp::Rect(n(0), n(1), n(2), n(3)),
                            "arc" => PathOp::Arc(
                                n(0),
                                n(1),
                                n(2),
                                n(3),
                                n(4),
                                args.get(5).map(Value::to_bool).unwrap_or(false),
                            ),
                            "quadraticCurveTo" => PathOp::QuadraticCurveTo(n(0), n(1), n(2), n(3)),
                            _ => PathOp::BezierCurveTo(n(0), n(1), n(2), n(3), n(4), n(5)),
                        };
                        push_path_op(&this, op);
                        Value::Undefined
                    }),
                );
            }
            value.set_property(
                "closePath",
                Value::function(|this, _| {
                    push_path_op(&this, PathOp::ClosePath);
                    Value::Undefined
                }),
            );
            value.set_property(
                "addPath",
                Value::function(|this, args| {
                    if let Some(ops) = args.first().and_then(path_ops) {
                        for op in ops {
                            push_path_op(&this, op);
                        }
                    }
                    Value::Undefined
                }),
            );
            value
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn offscreen_canvas_class() -> Value {
    OFFSCREEN_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let width = args.first().map(Value::to_u32).unwrap_or(0);
            let height = args.get(1).map(Value::to_u32).unwrap_or(0);
            let canvas = crate::jsdom::document_value()
                .call_method("createElement", vec![Value::string("canvas")]);
            canvas.set_property("width", Value::Number(width as f64));
            canvas.set_property("height", Value::Number(height as f64));
            let value = Value::object(HashMap::from([
                ("width".to_string(), Value::Number(width as f64)),
                ("height".to_string(), Value::Number(height as f64)),
            ]));
            let context_canvas = canvas.clone();
            value.set_property(
                "getContext",
                Value::function(move |_, args| {
                    context_canvas.call_method(
                        "getContext",
                        vec![args.first().cloned().unwrap_or_default()],
                    )
                }),
            );
            value.set_property(
                "transferToImageBitmap",
                Value::function(move |_, _| {
                    Value::object(HashMap::from([
                        ("width".to_string(), Value::Number(width as f64)),
                        ("height".to_string(), Value::Number(height as f64)),
                        (
                            "close".to_string(),
                            Value::function(|_, _| Value::Undefined),
                        ),
                    ]))
                }),
            );
            value.set_property(
                "convertToBlob",
                Value::function(move |_, args| {
                    let options = args.first().cloned().unwrap_or_default();
                    let type_name = if options.get_property("type").is_undefined() {
                        Value::string("image/png")
                    } else {
                        options.get_property("type")
                    };
                    w3cos_core::class::construct(
                        &crate::files::blob_class(),
                        vec![
                            Value::array(vec![]),
                            Value::object(HashMap::from([("type".to_string(), type_name)])),
                        ],
                    )
                }),
            );
            w3cos_core::class::set_prototype_of(
                &value,
                &offscreen_canvas_class().get_property("prototype"),
            );
            value
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}
