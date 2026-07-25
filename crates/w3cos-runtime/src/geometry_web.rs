//! Value-backed Geometry Interfaces (`DOMRect`, `DOMPoint`, and `DOMMatrix`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::{JsObject, ProxyBuilder, Value};

thread_local! {
    static CLASSES: RefCell<Option<HashMap<String, Value>>> = const { RefCell::new(None) };
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn number(value: Value, default: f64) -> f64 {
    if value.is_undefined() {
        default
    } else {
        value.to_number()
    }
}

fn plain_number_object(entries: &[(&str, f64)]) -> Value {
    Value::object(
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), Value::Number(*value)))
            .collect(),
    )
}

fn point_value(values: [f64; 4], mutable: bool) -> Value {
    let state = Rc::new(RefCell::new(values));
    let get_state = state.clone();
    let handler = ProxyBuilder::new()
        .get(move |target, key, _| {
            let values = *get_state.borrow();
            match key {
                "x" => Value::Number(values[0]),
                "y" => Value::Number(values[1]),
                "z" => Value::Number(values[2]),
                "w" => Value::Number(values[3]),
                "matrixTransform" => Value::function(move |_, args| {
                    point_value(transform_point(matrix_data(&arg(&args, 0)), values), true)
                }),
                "toJSON" => Value::function(move |_, _| {
                    plain_number_object(&[
                        ("x", values[0]),
                        ("y", values[1]),
                        ("z", values[2]),
                        ("w", values[3]),
                    ])
                }),
                _ => target.get_property(key),
            }
        })
        .set(move |_, key, value, _| {
            if !mutable {
                return false;
            }
            let index = match key {
                "x" => 0,
                "y" => 1,
                "z" => 2,
                "w" => 3,
                _ => return true,
            };
            state.borrow_mut()[index] = value.to_number();
            true
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        HashMap::new(),
        handler,
    ))));
    let class_name = if mutable {
        "DOMPoint"
    } else {
        "DOMPointReadOnly"
    };
    w3cos_core::class::set_prototype_of(&value, &class(class_name).get_property("prototype"));
    value
}

fn point_from_init(init: Value, mutable: bool) -> Value {
    point_value(
        [
            number(init.get_property("x"), 0.0),
            number(init.get_property("y"), 0.0),
            number(init.get_property("z"), 0.0),
            number(init.get_property("w"), 1.0),
        ],
        mutable,
    )
}

fn rect_value(values: [f64; 4], mutable: bool) -> Value {
    let state = Rc::new(RefCell::new(values));
    let get_state = state.clone();
    let handler = ProxyBuilder::new()
        .get(move |target, key, _| {
            let [x, y, width, height] = *get_state.borrow();
            match key {
                "x" => Value::Number(x),
                "y" => Value::Number(y),
                "width" => Value::Number(width),
                "height" => Value::Number(height),
                "top" => Value::Number(y.min(y + height)),
                "right" => Value::Number(x.max(x + width)),
                "bottom" => Value::Number(y.max(y + height)),
                "left" => Value::Number(x.min(x + width)),
                "toJSON" => Value::function(move |_, _| {
                    plain_number_object(&[
                        ("x", x),
                        ("y", y),
                        ("width", width),
                        ("height", height),
                        ("top", y.min(y + height)),
                        ("right", x.max(x + width)),
                        ("bottom", y.max(y + height)),
                        ("left", x.min(x + width)),
                    ])
                }),
                _ => target.get_property(key),
            }
        })
        .set(move |_, key, value, _| {
            if !mutable {
                return false;
            }
            let index = match key {
                "x" => 0,
                "y" => 1,
                "width" => 2,
                "height" => 3,
                _ => return true,
            };
            state.borrow_mut()[index] = value.to_number();
            true
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        HashMap::new(),
        handler,
    ))));
    let class_name = if mutable {
        "DOMRect"
    } else {
        "DOMRectReadOnly"
    };
    w3cos_core::class::set_prototype_of(&value, &class(class_name).get_property("prototype"));
    value
}

fn rect_from_init(init: Value, mutable: bool) -> Value {
    rect_value(
        [
            number(init.get_property("x"), 0.0),
            number(init.get_property("y"), 0.0),
            number(init.get_property("width"), 0.0),
            number(init.get_property("height"), 0.0),
        ],
        mutable,
    )
}

fn quad_value(points: [Value; 4]) -> Value {
    let value = Value::object(HashMap::from([
        ("p1".to_string(), points[0].clone()),
        ("p2".to_string(), points[1].clone()),
        ("p3".to_string(), points[2].clone()),
        ("p4".to_string(), points[3].clone()),
    ]));
    let points_for_bounds = points.clone();
    value.set_property(
        "getBounds",
        Value::function(move |_, _| {
            let xs = points_for_bounds
                .iter()
                .map(|point| point.get_property("x").to_number())
                .collect::<Vec<_>>();
            let ys = points_for_bounds
                .iter()
                .map(|point| point.get_property("y").to_number())
                .collect::<Vec<_>>();
            let left = xs.iter().copied().fold(f64::INFINITY, f64::min);
            let right = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let top = ys.iter().copied().fold(f64::INFINITY, f64::min);
            let bottom = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            rect_value([left, top, right - left, bottom - top], true)
        }),
    );
    let points_for_json = points;
    value.set_property(
        "toJSON",
        Value::function(move |_, _| {
            Value::object(HashMap::from([
                (
                    "p1".to_string(),
                    points_for_json[0].call_method("toJSON", vec![]),
                ),
                (
                    "p2".to_string(),
                    points_for_json[1].call_method("toJSON", vec![]),
                ),
                (
                    "p3".to_string(),
                    points_for_json[2].call_method("toJSON", vec![]),
                ),
                (
                    "p4".to_string(),
                    points_for_json[3].call_method("toJSON", vec![]),
                ),
            ]))
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &class("DOMQuad").get_property("prototype"));
    value
}

fn quad_from_init(init: Value) -> Value {
    quad_value([
        point_from_init(init.get_property("p1"), true),
        point_from_init(init.get_property("p2"), true),
        point_from_init(init.get_property("p3"), true),
        point_from_init(init.get_property("p4"), true),
    ])
}

fn quad_from_rect(init: Value) -> Value {
    let x = number(init.get_property("x"), 0.0);
    let y = number(init.get_property("y"), 0.0);
    let width = number(init.get_property("width"), 0.0);
    let height = number(init.get_property("height"), 0.0);
    quad_value([
        point_value([x, y, 0.0, 1.0], true),
        point_value([x + width, y, 0.0, 1.0], true),
        point_value([x + width, y + height, 0.0, 1.0], true),
        point_value([x, y + height, 0.0, 1.0], true),
    ])
}

pub fn rect_list(rects: Vec<Value>) -> Value {
    let rects = Rc::new(rects);
    let values = Rc::clone(&rects);
    let handler = ProxyBuilder::new()
        .get(move |target, key, _| match key {
            "length" => Value::Number(values.len() as f64),
            "item" => {
                let values = Rc::clone(&values);
                Value::function(move |_, args| {
                    let index = number(arg(&args, 0), 0.0);
                    if !index.is_finite() || index < 0.0 {
                        return Value::Null;
                    }
                    values
                        .get(index.trunc() as usize)
                        .cloned()
                        .unwrap_or(Value::Null)
                })
            }
            _ => key
                .parse::<usize>()
                .ok()
                .and_then(|index| values.get(index).cloned())
                .unwrap_or_else(|| target.get_property(key)),
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        HashMap::new(),
        handler,
    ))));
    w3cos_core::class::set_prototype_of(&value, &class("DOMRectList").get_property("prototype"));
    value
}

fn identity() -> [f64; 16] {
    let mut matrix = [0.0; 16];
    for index in 0..4 {
        matrix[index * 5] = 1.0;
    }
    matrix
}

fn matrix_index(column: usize, row: usize) -> usize {
    column * 4 + row
}

fn multiply(left: [f64; 16], right: [f64; 16]) -> [f64; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[matrix_index(column, row)] = (0..4)
                .map(|k| left[matrix_index(k, row)] * right[matrix_index(column, k)])
                .sum();
        }
    }
    result
}

fn transform_point(matrix: [f64; 16], point: [f64; 4]) -> [f64; 4] {
    let mut result = [0.0; 4];
    for row in 0..4 {
        result[row] = (0..4)
            .map(|column| matrix[matrix_index(column, row)] * point[column])
            .sum();
    }
    result
}

fn is_2d(matrix: &[f64; 16]) -> bool {
    matrix[2] == 0.0
        && matrix[3] == 0.0
        && matrix[6] == 0.0
        && matrix[7] == 0.0
        && matrix[8] == 0.0
        && matrix[9] == 0.0
        && matrix[10] == 1.0
        && matrix[11] == 0.0
        && matrix[14] == 0.0
        && matrix[15] == 1.0
}

fn matrix_property_index(key: &str) -> Option<usize> {
    match key {
        "a" => Some(matrix_index(0, 0)),
        "b" => Some(matrix_index(0, 1)),
        "c" => Some(matrix_index(1, 0)),
        "d" => Some(matrix_index(1, 1)),
        "e" => Some(matrix_index(3, 0)),
        "f" => Some(matrix_index(3, 1)),
        _ if key.len() == 3 && key.starts_with('m') => {
            let bytes = key.as_bytes();
            let column = bytes[1].checked_sub(b'1')? as usize;
            let row = bytes[2].checked_sub(b'1')? as usize;
            (column < 4 && row < 4).then(|| matrix_index(column, row))
        }
        _ => None,
    }
}

fn matrix_data(value: &Value) -> [f64; 16] {
    let mut result = identity();
    for column in 0..4 {
        for row in 0..4 {
            let candidate = value.get_property(&format!("m{}{}", column + 1, row + 1));
            if !candidate.is_undefined() {
                result[matrix_index(column, row)] = candidate.to_number();
            }
        }
    }
    for (key, column, row) in [
        ("a", 0, 0),
        ("b", 0, 1),
        ("c", 1, 0),
        ("d", 1, 1),
        ("e", 3, 0),
        ("f", 3, 1),
    ] {
        let candidate = value.get_property(key);
        if !candidate.is_undefined() {
            result[matrix_index(column, row)] = candidate.to_number();
        }
    }
    result
}

fn matrix_from_init(init: Value) -> [f64; 16] {
    if init.is_nullish() {
        return identity();
    }
    if matches!(init, Value::String(_)) {
        let text = init.to_js_string();
        if let Some(body) = text
            .strip_prefix("matrix(")
            .and_then(|value| value.strip_suffix(')'))
        {
            let values = body
                .split(|ch: char| ch == ',' || ch.is_ascii_whitespace())
                .filter(|part| !part.is_empty())
                .filter_map(|part| part.parse::<f64>().ok())
                .collect::<Vec<_>>();
            if values.len() == 6 {
                return matrix_from_sequence(&values);
            }
        }
    }
    let length = init.get_property("length");
    if length.is_number() {
        let values = (0..length.to_u32() as usize)
            .map(|index| init.get_property(&index.to_string()).to_number())
            .collect::<Vec<_>>();
        if values.len() == 6 {
            return matrix_from_sequence(&values);
        }
        if values.len() == 16 {
            return values.try_into().unwrap_or_else(|_| identity());
        }
    }
    matrix_data(&init)
}

fn matrix_from_sequence(values: &[f64]) -> [f64; 16] {
    let mut matrix = identity();
    for (value, (column, row)) in
        values
            .iter()
            .copied()
            .zip([(0, 0), (0, 1), (1, 0), (1, 1), (3, 0), (3, 1)])
    {
        matrix[matrix_index(column, row)] = value;
    }
    matrix
}

fn translation(tx: f64, ty: f64, tz: f64) -> [f64; 16] {
    let mut matrix = identity();
    matrix[12] = tx;
    matrix[13] = ty;
    matrix[14] = tz;
    matrix
}

fn scaling(sx: f64, sy: f64, sz: f64) -> [f64; 16] {
    let mut matrix = identity();
    matrix[0] = sx;
    matrix[5] = sy;
    matrix[10] = sz;
    matrix
}

fn rotation_z(degrees: f64) -> [f64; 16] {
    let radians = degrees.to_radians();
    let mut matrix = identity();
    matrix[0] = radians.cos();
    matrix[1] = radians.sin();
    matrix[4] = -radians.sin();
    matrix[5] = radians.cos();
    matrix
}

fn inverse(matrix: [f64; 16]) -> [f64; 16] {
    let mut augmented = [[0.0; 8]; 4];
    for row in 0..4 {
        for column in 0..4 {
            augmented[row][column] = matrix[matrix_index(column, row)];
        }
        augmented[row][row + 4] = 1.0;
    }

    for column in 0..4 {
        let pivot_row = (column..4)
            .max_by(|left, right| {
                augmented[*left][column]
                    .abs()
                    .total_cmp(&augmented[*right][column].abs())
            })
            .unwrap_or(column);
        let pivot = augmented[pivot_row][column];
        if !pivot.is_finite() || pivot.abs() <= f64::EPSILON {
            return [f64::NAN; 16];
        }
        augmented.swap(column, pivot_row);
        for entry in &mut augmented[column] {
            *entry /= pivot;
        }
        for row in 0..4 {
            if row == column {
                continue;
            }
            let factor = augmented[row][column];
            for index in 0..8 {
                augmented[row][index] -= factor * augmented[column][index];
            }
        }
    }

    let mut result = [0.0; 16];
    for row in 0..4 {
        for column in 0..4 {
            result[matrix_index(column, row)] = augmented[row][column + 4];
        }
    }
    result
}

fn matrix_json(matrix: [f64; 16]) -> Value {
    let mut entries = HashMap::new();
    for column in 0..4 {
        for row in 0..4 {
            entries.insert(
                format!("m{}{}", column + 1, row + 1),
                Value::Number(matrix[matrix_index(column, row)]),
            );
        }
    }
    for (key, index) in [("a", 0), ("b", 1), ("c", 4), ("d", 5), ("e", 12), ("f", 13)] {
        entries.insert(key.to_string(), Value::Number(matrix[index]));
    }
    entries.insert("is2D".to_string(), Value::Bool(is_2d(&matrix)));
    Value::object(entries)
}

fn matrix_value(matrix: [f64; 16], mutable: bool) -> Value {
    let state = Rc::new(RefCell::new(matrix));
    let get_state = state.clone();
    let self_state = state.clone();
    let handler = ProxyBuilder::new()
        .get(move |target, key, _| {
            let matrix = *get_state.borrow();
            if let Some(index) = matrix_property_index(key) {
                return Value::Number(matrix[index]);
            }
            match key {
                "is2D" => Value::Bool(is_2d(&matrix)),
                "isIdentity" => Value::Bool(matrix == identity()),
                "translate" => Value::function(move |_, args| {
                    matrix_value(
                        multiply(
                            matrix,
                            translation(
                                number(arg(&args, 0), 0.0),
                                number(arg(&args, 1), 0.0),
                                number(arg(&args, 2), 0.0),
                            ),
                        ),
                        true,
                    )
                }),
                "scale" => Value::function(move |_, args| {
                    let sx = number(arg(&args, 0), 1.0);
                    matrix_value(
                        multiply(
                            matrix,
                            scaling(sx, number(arg(&args, 1), sx), number(arg(&args, 2), 1.0)),
                        ),
                        true,
                    )
                }),
                "rotate" => Value::function(move |_, args| {
                    let degrees = if args.len() >= 3 {
                        number(arg(&args, 2), 0.0)
                    } else {
                        number(arg(&args, 0), 0.0)
                    };
                    matrix_value(multiply(matrix, rotation_z(degrees)), true)
                }),
                "multiply" => Value::function(move |_, args| {
                    matrix_value(multiply(matrix, matrix_data(&arg(&args, 0))), true)
                }),
                "inverse" => Value::function(move |_, _| matrix_value(inverse(matrix), true)),
                "transformPoint" => Value::function(move |_, args| {
                    let point = point_from_init(arg(&args, 0), true);
                    point_value(
                        transform_point(
                            matrix,
                            [
                                point.get_property("x").to_number(),
                                point.get_property("y").to_number(),
                                point.get_property("z").to_number(),
                                point.get_property("w").to_number(),
                            ],
                        ),
                        true,
                    )
                }),
                "toFloat32Array" | "toFloat64Array" => Value::function(move |_, _| {
                    Value::array(matrix.into_iter().map(Value::Number).collect())
                }),
                "toJSON" => Value::function(move |_, _| matrix_json(matrix)),
                "toString" => Value::function(move |_, _| {
                    if is_2d(&matrix) {
                        Value::string(&format!(
                            "matrix({}, {}, {}, {}, {}, {})",
                            matrix[0], matrix[1], matrix[4], matrix[5], matrix[12], matrix[13]
                        ))
                    } else {
                        Value::string(&format!(
                            "matrix3d({})",
                            matrix
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                }),
                "translateSelf" if mutable => {
                    let state = self_state.clone();
                    Value::function(move |this, args| {
                        let current = *state.borrow();
                        *state.borrow_mut() = multiply(
                            current,
                            translation(
                                number(arg(&args, 0), 0.0),
                                number(arg(&args, 1), 0.0),
                                number(arg(&args, 2), 0.0),
                            ),
                        );
                        this
                    })
                }
                "scaleSelf" if mutable => {
                    let state = self_state.clone();
                    Value::function(move |this, args| {
                        let sx = number(arg(&args, 0), 1.0);
                        let current = *state.borrow();
                        *state.borrow_mut() = multiply(
                            current,
                            scaling(sx, number(arg(&args, 1), sx), number(arg(&args, 2), 1.0)),
                        );
                        this
                    })
                }
                "rotateSelf" if mutable => {
                    let state = self_state.clone();
                    Value::function(move |this, args| {
                        let degrees = if args.len() >= 3 {
                            number(arg(&args, 2), 0.0)
                        } else {
                            number(arg(&args, 0), 0.0)
                        };
                        let current = *state.borrow();
                        *state.borrow_mut() = multiply(current, rotation_z(degrees));
                        this
                    })
                }
                "multiplySelf" if mutable => {
                    let state = self_state.clone();
                    Value::function(move |this, args| {
                        let current = *state.borrow();
                        *state.borrow_mut() = multiply(current, matrix_data(&arg(&args, 0)));
                        this
                    })
                }
                "preMultiplySelf" if mutable => {
                    let state = self_state.clone();
                    Value::function(move |this, args| {
                        let current = *state.borrow();
                        *state.borrow_mut() = multiply(matrix_data(&arg(&args, 0)), current);
                        this
                    })
                }
                "invertSelf" if mutable => {
                    let state = self_state.clone();
                    Value::function(move |this, _| {
                        let current = *state.borrow();
                        *state.borrow_mut() = inverse(current);
                        this
                    })
                }
                _ => target.get_property(key),
            }
        })
        .set(move |_, key, value, _| {
            if mutable {
                if let Some(index) = matrix_property_index(key) {
                    state.borrow_mut()[index] = value.to_number();
                }
            }
            mutable
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        HashMap::new(),
        handler,
    ))));
    let class_name = if mutable {
        "DOMMatrix"
    } else {
        "DOMMatrixReadOnly"
    };
    w3cos_core::class::set_prototype_of(&value, &class(class_name).get_property("prototype"));
    value
}

fn build_classes() -> HashMap<String, Value> {
    let mut classes = HashMap::new();
    for name in [
        "DOMPointReadOnly",
        "DOMPoint",
        "DOMRectReadOnly",
        "DOMRect",
        "DOMMatrixReadOnly",
        "DOMMatrix",
    ] {
        let mutable = !name.ends_with("ReadOnly");
        let class = match name {
            "DOMPointReadOnly" | "DOMPoint" => Value::function(move |_, args| {
                point_value(
                    [
                        number(arg(&args, 0), 0.0),
                        number(arg(&args, 1), 0.0),
                        number(arg(&args, 2), 0.0),
                        number(arg(&args, 3), 1.0),
                    ],
                    mutable,
                )
            }),
            "DOMRectReadOnly" | "DOMRect" => Value::function(move |_, args| {
                rect_value(
                    [
                        number(arg(&args, 0), 0.0),
                        number(arg(&args, 1), 0.0),
                        number(arg(&args, 2), 0.0),
                        number(arg(&args, 3), 0.0),
                    ],
                    mutable,
                )
            }),
            _ => Value::function(move |_, args| {
                matrix_value(matrix_from_init(arg(&args, 0)), mutable)
            }),
        };
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        classes.insert(name.to_string(), class);
    }
    for (child, parent) in [
        ("DOMPoint", "DOMPointReadOnly"),
        ("DOMRect", "DOMRectReadOnly"),
        ("DOMMatrix", "DOMMatrixReadOnly"),
    ] {
        w3cos_core::class::set_prototype_of(
            &classes[child].get_property("prototype"),
            &classes[parent].get_property("prototype"),
        );
    }
    for property in ["matrixTransform", "toJSON", "w", "x", "y", "z"] {
        classes["DOMPointReadOnly"]
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    for property in ["w", "x", "y", "z"] {
        classes["DOMPoint"]
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    for property in [
        "bottom", "height", "left", "right", "toJSON", "top", "width", "x", "y",
    ] {
        classes["DOMRectReadOnly"]
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    for property in ["height", "width", "x", "y"] {
        classes["DOMRect"]
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    for property in [
        "a",
        "b",
        "c",
        "d",
        "e",
        "f",
        "flipX",
        "flipY",
        "inverse",
        "is2D",
        "isIdentity",
        "m11",
        "m12",
        "m13",
        "m14",
        "m21",
        "m22",
        "m23",
        "m24",
        "m31",
        "m32",
        "m33",
        "m34",
        "m41",
        "m42",
        "m43",
        "m44",
        "multiply",
        "rotate",
        "rotateAxisAngle",
        "rotateFromVector",
        "scale",
        "scale3d",
        "scaleNonUniform",
        "skewX",
        "skewY",
        "toFloat32Array",
        "toFloat64Array",
        "toJSON",
        "toString",
        "transformPoint",
        "translate",
    ] {
        classes["DOMMatrixReadOnly"]
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    for property in [
        "a",
        "b",
        "c",
        "d",
        "e",
        "f",
        "invertSelf",
        "m11",
        "m12",
        "m13",
        "m14",
        "m21",
        "m22",
        "m23",
        "m24",
        "m31",
        "m32",
        "m33",
        "m34",
        "m41",
        "m42",
        "m43",
        "m44",
        "multiplySelf",
        "preMultiplySelf",
        "rotateAxisAngleSelf",
        "rotateFromVectorSelf",
        "rotateSelf",
        "scale3dSelf",
        "scaleSelf",
        "setMatrixValue",
        "skewXSelf",
        "skewYSelf",
        "translateSelf",
    ] {
        classes["DOMMatrix"]
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    for (name, method) in [
        ("DOMPoint", "fromPoint"),
        ("DOMPointReadOnly", "fromPoint"),
        ("DOMRect", "fromRect"),
        ("DOMRectReadOnly", "fromRect"),
        ("DOMMatrix", "fromMatrix"),
        ("DOMMatrixReadOnly", "fromMatrix"),
    ] {
        let mutable = !name.ends_with("ReadOnly");
        let function = match method {
            "fromPoint" => Value::function(move |_, args| point_from_init(arg(&args, 0), mutable)),
            "fromRect" => Value::function(move |_, args| rect_from_init(arg(&args, 0), mutable)),
            _ => Value::function(move |_, args| {
                matrix_value(matrix_from_init(arg(&args, 0)), mutable)
            }),
        };
        classes[name].set_property(method, function);
    }
    for name in ["DOMMatrix", "DOMMatrixReadOnly"] {
        let mutable = name == "DOMMatrix";
        for method in ["fromFloat32Array", "fromFloat64Array"] {
            classes[name].set_property(
                method,
                Value::function(move |_, args| {
                    matrix_value(matrix_from_init(arg(&args, 0)), mutable)
                }),
            );
        }
    }
    let quad = Value::function(|_, args| {
        quad_value([
            point_from_init(arg(&args, 0), true),
            point_from_init(arg(&args, 1), true),
            point_from_init(arg(&args, 2), true),
            point_from_init(arg(&args, 3), true),
        ])
    });
    quad.set_property("name", Value::string("DOMQuad"));
    quad.set_property(
        "fromQuad",
        Value::function(|_, args| quad_from_init(arg(&args, 0))),
    );
    quad.set_property(
        "fromRect",
        Value::function(|_, args| quad_from_rect(arg(&args, 0))),
    );
    let quad_prototype = Value::object(HashMap::new());
    quad_prototype.set_property("constructor", quad.clone());
    for property in ["getBounds", "p1", "p2", "p3", "p4", "toJSON"] {
        quad_prototype.set_property(property, Value::Undefined);
    }
    quad.set_property("prototype", quad_prototype);
    classes.insert("DOMQuad".to_string(), quad);
    let rect_list_class = Value::function(|_, _| rect_list(Vec::new()));
    rect_list_class.set_property("name", Value::string("DOMRectList"));
    let rect_list_prototype = Value::object(HashMap::new());
    rect_list_prototype.set_property("constructor", rect_list_class.clone());
    rect_list_prototype.set_property("item", Value::function(|_, _| Value::Null));
    rect_list_prototype.set_property("length", Value::Undefined);
    rect_list_class.set_property("prototype", rect_list_prototype);
    classes.insert("DOMRectList".to_string(), rect_list_class);
    classes
}

pub fn class(name: &str) -> Value {
    CLASSES.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(build_classes());
        }
        slot.borrow()
            .as_ref()
            .and_then(|classes| classes.get(name))
            .cloned()
            .unwrap_or(Value::Undefined)
    })
}

pub fn rect(x: f64, y: f64, width: f64, height: f64) -> Value {
    rect_value([x, y, width, height], true)
}

pub fn point(x: f64, y: f64, z: f64, w: f64) -> Value {
    point_value([x, y, z, w], true)
}

pub fn identity_matrix() -> Value {
    matrix_value(identity(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_edges_point_transform_and_matrix_chain_match_geometry_api() {
        let rect = w3cos_core::class::construct(
            &class("DOMRect"),
            vec![
                Value::Number(10.0),
                Value::Number(20.0),
                Value::Number(-5.0),
                Value::Number(-8.0),
            ],
        );
        assert_eq!(rect.get_property("left").to_number(), 5.0);
        assert_eq!(rect.get_property("top").to_number(), 12.0);
        assert!(w3cos_core::class::instance_of(&rect, &class("DOMRect")));
        assert!(w3cos_core::class::instance_of(
            &rect,
            &class("DOMRectReadOnly")
        ));

        let matrix = w3cos_core::class::construct(&class("DOMMatrix"), vec![])
            .call_method("translate", vec![Value::Number(5.0), Value::Number(7.0)])
            .call_method("scale", vec![Value::Number(2.0), Value::Number(3.0)]);
        let point = w3cos_core::class::construct(
            &class("DOMPoint"),
            vec![Value::Number(4.0), Value::Number(6.0)],
        )
        .call_method("matrixTransform", vec![matrix]);
        assert_eq!(point.get_property("x").to_number(), 13.0);
        assert_eq!(point.get_property("y").to_number(), 25.0);

        let matrix_3d = matrix_value(
            multiply(translation(5.0, 7.0, 11.0), scaling(2.0, 3.0, 4.0)),
            true,
        );
        assert!(!matrix_3d.get_property("is2D").to_bool());
        let transformed = matrix_3d.call_method(
            "transformPoint",
            vec![point_value([2.0, 3.0, 5.0, 1.0], true)],
        );
        let restored = matrix_3d
            .call_method("inverse", vec![])
            .call_method("transformPoint", vec![transformed]);
        assert!((restored.get_property("x").to_number() - 2.0).abs() < 1e-9);
        assert!((restored.get_property("y").to_number() - 3.0).abs() < 1e-9);
        assert!((restored.get_property("z").to_number() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn quad_construction_statics_bounds_and_json_match_geometry_api() {
        let quad = class("DOMQuad").call_method(
            "fromRect",
            vec![Value::object(HashMap::from([
                ("x".into(), Value::Number(10.0)),
                ("y".into(), Value::Number(20.0)),
                ("width".into(), Value::Number(-5.0)),
                ("height".into(), Value::Number(8.0)),
            ]))],
        );
        assert!(w3cos_core::class::instance_of(&quad, &class("DOMQuad")));
        assert!(w3cos_core::class::instance_of(
            &quad.get_property("p1"),
            &class("DOMPoint")
        ));
        let bounds = quad.call_method("getBounds", vec![]);
        assert_eq!(bounds.get_property("x"), Value::Number(5.0));
        assert_eq!(bounds.get_property("y"), Value::Number(20.0));
        assert_eq!(bounds.get_property("width"), Value::Number(5.0));
        assert_eq!(bounds.get_property("height"), Value::Number(8.0));
        assert_eq!(
            quad.call_method("toJSON", vec![])
                .get_property("p3")
                .get_property("x"),
            Value::Number(5.0)
        );
        let copy = class("DOMQuad").call_method("fromQuad", vec![quad]);
        assert_eq!(
            copy.get_property("p4").get_property("y"),
            Value::Number(28.0)
        );
    }

    #[test]
    fn rect_list_has_index_item_length_and_constructor_identity() {
        let first = rect(1.0, 2.0, 3.0, 4.0);
        let list = rect_list(vec![first.clone()]);
        assert!(w3cos_core::class::instance_of(&list, &class("DOMRectList")));
        assert_eq!(list.get_property("length"), Value::Number(1.0));
        assert_eq!(list.get_property("0"), first);
        assert_eq!(
            list.call_method("item", vec![Value::Number(0.0)]),
            list.get_property("0")
        );
        assert_eq!(
            list.call_method("item", vec![Value::Number(1.0)]),
            Value::Null
        );
        let empty = w3cos_core::class::construct(&class("DOMRectList"), vec![]);
        assert_eq!(empty.get_property("length"), Value::Number(0.0));
    }
}
