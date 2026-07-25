//! JavaScript constructors layered over the existing Canvas 2D engine.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

use crate::canvas2d::PathOp;

const PATH_ID: &str = "__w3cos_path_2d_id";

thread_local! {
    static NEXT_PATH_ID: Cell<u64> = const { Cell::new(1) };
    static PATHS: RefCell<HashMap<u64, Vec<PathOp>>> = RefCell::new(HashMap::new());
    static PATH_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static OFFSCREEN_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CANVAS_GRADIENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CANVAS_PATTERN_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CANVAS_CONTEXT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static OFFSCREEN_CONTEXT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static IMAGE_BITMAP_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static IMAGE_BITMAP_CONTEXT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CANVAS_CAPTURE_TRACK_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TEXT_METRICS_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn illegal_canvas_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
    methods: &'static [&'static str],
    properties: &'static [&'static str],
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(&format!("Illegal constructor: {name}"))],
            ))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in methods {
            prototype.set_property(
                method,
                Value::function(move |_, _| {
                    static WARNING: Once = Once::new();
                    WARNING.call_once(|| {
                        eprintln!(
                            "[w3cos] warning: CanvasRenderingContext2D.{method}() preserves the \
                             browser call shape but requires renderer support for its visual effect"
                        );
                    });
                    Value::Undefined
                }),
            );
        }
        for property in properties {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
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
    w3cos_core::web::image_data_class()
}

pub fn image_data_value(data: Value, width: u32, height: u32) -> Value {
    w3cos_core::web::image_data_value(data, width, height, "srgb")
}

pub fn canvas_gradient_class() -> Value {
    let class = illegal_canvas_class(
        &CANVAS_GRADIENT_CLASS,
        "CanvasGradient",
        &["addColorStop"],
        &[],
    );
    class.get_property("prototype").set_property(
        "addColorStop",
        Value::function(|this, args| {
            let offset = args.first().map(Value::to_number).unwrap_or(f64::NAN);
            if !(0.0..=1.0).contains(&offset) {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "IndexSizeError",
                    vec![Value::string(
                        "CanvasGradient color-stop offset must be between 0 and 1",
                    )],
                ));
            }
            let color = args.get(1).map(Value::to_js_string).unwrap_or_default();
            let mut stops = this
                .get_property("__w3cos_stops")
                .iter()
                .collect::<Vec<_>>();
            stops.push(Value::object(HashMap::from([
                ("offset".into(), Value::Number(offset)),
                ("color".into(), Value::string(&color)),
            ])));
            stops.sort_by(|left, right| {
                left.get_property("offset")
                    .to_number()
                    .total_cmp(&right.get_property("offset").to_number())
            });
            this.set_property("__w3cos_stops", Value::array(stops));
            Value::Undefined
        }),
    );
    class
}

pub fn canvas_gradient_value(kind: &str, arguments: &[Value]) -> Value {
    let value = Value::object(HashMap::from([
        ("__w3cos_gradient_kind".into(), Value::string(kind)),
        (
            "__w3cos_gradient_arguments".into(),
            Value::array(arguments.to_vec()),
        ),
        ("__w3cos_stops".into(), Value::array(vec![])),
    ]));
    w3cos_core::class::set_prototype_of(&value, &canvas_gradient_class().get_property("prototype"));
    value
}

pub fn canvas_pattern_class() -> Value {
    let class = illegal_canvas_class(
        &CANVAS_PATTERN_CLASS,
        "CanvasPattern",
        &["setTransform"],
        &[],
    );
    class.get_property("prototype").set_property(
        "setTransform",
        Value::function(|this, args| {
            this.set_property(
                "__w3cos_transform",
                args.first().cloned().unwrap_or(Value::Undefined),
            );
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: CanvasPattern.setTransform() retains the transform for \
                     compatibility; patterned raster paint requires renderer integration"
                );
            });
            Value::Undefined
        }),
    );
    class
}

pub fn canvas_pattern_value(source: Value, repetition: &str) -> Value {
    let value = Value::object(HashMap::from([
        ("__w3cos_source".into(), source),
        ("__w3cos_repetition".into(), Value::string(repetition)),
    ]));
    w3cos_core::class::set_prototype_of(&value, &canvas_pattern_class().get_property("prototype"));
    value
}

pub fn canvas_rendering_context_2d_class() -> Value {
    illegal_canvas_class(
        &CANVAS_CONTEXT_CLASS,
        "CanvasRenderingContext2D",
        &[
            "arc",
            "arcTo",
            "beginPath",
            "bezierCurveTo",
            "clearRect",
            "clip",
            "closePath",
            "createConicGradient",
            "createImageData",
            "createLinearGradient",
            "createPattern",
            "createRadialGradient",
            "drawFocusIfNeeded",
            "drawImage",
            "ellipse",
            "fill",
            "fillRect",
            "fillText",
            "getContextAttributes",
            "getImageData",
            "getLineDash",
            "getTransform",
            "isContextLost",
            "isPointInPath",
            "isPointInStroke",
            "lineTo",
            "measureText",
            "moveTo",
            "putImageData",
            "quadraticCurveTo",
            "rect",
            "reset",
            "resetTransform",
            "restore",
            "rotate",
            "roundRect",
            "save",
            "scale",
            "setLineDash",
            "setTransform",
            "stroke",
            "strokeRect",
            "strokeText",
            "transform",
            "translate",
        ],
        &[
            "canvas",
            "direction",
            "fillStyle",
            "filter",
            "font",
            "fontKerning",
            "fontStretch",
            "fontVariantCaps",
            "globalAlpha",
            "globalCompositeOperation",
            "imageSmoothingEnabled",
            "imageSmoothingQuality",
            "lang",
            "letterSpacing",
            "lineCap",
            "lineDashOffset",
            "lineJoin",
            "lineWidth",
            "miterLimit",
            "shadowBlur",
            "shadowColor",
            "shadowOffsetX",
            "shadowOffsetY",
            "strokeStyle",
            "textAlign",
            "textBaseline",
            "textRendering",
            "wordSpacing",
        ],
    )
}

pub fn offscreen_canvas_rendering_context_2d_class() -> Value {
    OFFSCREEN_CONTEXT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let base = canvas_rendering_context_2d_class();
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(
                    "Illegal constructor: OffscreenCanvasRenderingContext2D",
                )],
            ))
        });
        class.set_property("name", Value::string("OffscreenCanvasRenderingContext2D"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        if let Value::Object(object) = base.get_property("prototype") {
            for key in object.borrow().keys() {
                if !matches!(key.as_str(), "constructor" | "drawFocusIfNeeded") {
                    prototype.set_property(&key, Value::Undefined);
                }
            }
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn text_metrics_class() -> Value {
    illegal_canvas_class(
        &TEXT_METRICS_CLASS,
        "TextMetrics",
        &[],
        &[
            "actualBoundingBoxAscent",
            "actualBoundingBoxDescent",
            "actualBoundingBoxLeft",
            "actualBoundingBoxRight",
            "alphabeticBaseline",
            "fontBoundingBoxAscent",
            "fontBoundingBoxDescent",
            "hangingBaseline",
            "ideographicBaseline",
            "width",
        ],
    )
}

pub fn text_metrics_value(properties: HashMap<String, Value>) -> Value {
    let value = Value::object(properties);
    for property in [
        "actualBoundingBoxAscent",
        "actualBoundingBoxDescent",
        "actualBoundingBoxLeft",
        "actualBoundingBoxRight",
        "alphabeticBaseline",
        "fontBoundingBoxAscent",
        "fontBoundingBoxDescent",
        "hangingBaseline",
        "ideographicBaseline",
        "width",
    ] {
        if value.get_property(property).is_undefined() {
            value.set_property(property, Value::Number(0.0));
        }
    }
    w3cos_core::class::set_prototype_of(&value, &text_metrics_class().get_property("prototype"));
    value
}

pub fn image_bitmap_class() -> Value {
    let class = illegal_canvas_class(
        &IMAGE_BITMAP_CLASS,
        "ImageBitmap",
        &["close"],
        &["height", "width"],
    );
    class.get_property("prototype").set_property(
        "close",
        Value::function(|this, _| {
            this.set_property("__w3cos_closed", Value::Bool(true));
            Value::Undefined
        }),
    );
    class
}

pub fn image_bitmap_value(width: u32, height: u32, canvas: Option<Value>) -> Value {
    let value = Value::object(HashMap::from([
        ("width".into(), Value::Number(width as f64)),
        ("height".into(), Value::Number(height as f64)),
        ("__w3cos_closed".into(), Value::Bool(false)),
    ]));
    if let Some(canvas) = canvas {
        value.set_property("__w3cos_canvas", canvas);
    }
    w3cos_core::class::set_prototype_of(&value, &image_bitmap_class().get_property("prototype"));
    value
}

pub fn image_bitmap_rendering_context_class() -> Value {
    illegal_canvas_class(
        &IMAGE_BITMAP_CONTEXT_CLASS,
        "ImageBitmapRenderingContext",
        &["transferFromImageBitmap"],
        &["canvas"],
    )
}

pub fn image_bitmap_rendering_context_value(canvas: Value) -> Value {
    let value = Value::object(HashMap::from([("canvas".into(), canvas.clone())]));
    value.set_property(
        "transferFromImageBitmap",
        Value::function(move |_, args| {
            let bitmap = args.first().cloned().unwrap_or(Value::Null);
            let context = canvas.call_method("getContext", vec![Value::string("2d")]);
            if bitmap.is_null() {
                context.call_method(
                    "clearRect",
                    vec![
                        Value::Number(0.0),
                        Value::Number(0.0),
                        canvas.get_property("width"),
                        canvas.get_property("height"),
                    ],
                );
            } else {
                context.call_method(
                    "drawImage",
                    vec![bitmap.clone(), Value::Number(0.0), Value::Number(0.0)],
                );
                bitmap.call_method("close", vec![]);
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &image_bitmap_rendering_context_class().get_property("prototype"),
    );
    value
}

pub fn canvas_capture_media_stream_track_class() -> Value {
    let class = illegal_canvas_class(
        &CANVAS_CAPTURE_TRACK_CLASS,
        "CanvasCaptureMediaStreamTrack",
        &["requestFrame"],
        &["canvas"],
    );
    w3cos_core::class::set_prototype_of(
        &class.get_property("prototype"),
        &crate::media_devices_web::media_stream_track_class().get_property("prototype"),
    );
    class
}

pub fn canvas_capture_media_stream_track_value(canvas: Value) -> Value {
    let value = crate::media_devices_web::track_value("video", "Canvas");
    value.set_property("canvas", canvas);
    value.set_property(
        "requestFrame",
        Value::function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: CanvasCaptureMediaStreamTrack.requestFrame() preserves \
                     capture scheduling semantics, but encoded-frame delivery requires a media \
                     host adapter"
                );
            });
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &canvas_capture_media_stream_track_class().get_property("prototype"),
    );
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
        for member in [
            "addPath",
            "arc",
            "arcTo",
            "bezierCurveTo",
            "closePath",
            "ellipse",
            "lineTo",
            "moveTo",
            "quadraticCurveTo",
            "rect",
            "roundRect",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
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
                ("__w3cos_canvas".to_string(), canvas.clone()),
            ]));
            let context_canvas = canvas.clone();
            let offscreen = value.clone();
            value.set_property(
                "getContext",
                Value::function(move |_, args| {
                    let context = context_canvas.call_method(
                        "getContext",
                        vec![args.first().cloned().unwrap_or_default()],
                    );
                    if args.first().map(Value::to_js_string).as_deref() == Some("2d") {
                        context.set_property("canvas", offscreen.clone());
                        w3cos_core::class::set_prototype_of(
                            &context,
                            &offscreen_canvas_rendering_context_2d_class()
                                .get_property("prototype"),
                        );
                    }
                    context
                }),
            );
            value.set_property(
                "transferToImageBitmap",
                Value::function(move |_, _| {
                    image_bitmap_value(width, height, Some(canvas.clone()))
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
        for member in [
            "convertToBlob",
            "getContext",
            "height",
            "oncontextlost",
            "oncontextrestored",
            "transferToImageBitmap",
            "width",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}
