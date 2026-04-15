use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// JavaScript-compatible dynamic value type.
///
/// Maps the full ECMAScript value space into Rust with reference-counted
/// sharing for heap types (Array, Object, Function).
#[derive(Clone, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<RefCell<crate::JsObject>>),
    Function(JsFunction),
}

/// A callable JS function stored as a reference-counted closure.
#[derive(Clone)]
pub struct JsFunction {
    inner: Rc<dyn Fn(Value, Vec<Value>) -> Value>,
}

impl JsFunction {
    pub fn new(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Self {
        Self { inner: Rc::new(f) }
    }

    pub fn call(&self, this: Value, args: Vec<Value>) -> Value {
        (self.inner)(this, args)
    }
}

impl fmt::Debug for JsFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[Function]")
    }
}

// ── Type coercion ──────────────────────────────────────────────────────

impl Value {
    /// ECMAScript `typeof` operator.
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) | Value::Object(_) => "object",
            Value::Function(_) => "function",
        }
    }

    /// ECMAScript `ToBoolean`.
    pub fn to_bool(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::String(s) => !s.is_empty(),
            Value::Array(_) | Value::Object(_) | Value::Function(_) => true,
        }
    }

    /// ECMAScript `ToNumber`.
    pub fn to_number(&self) -> f64 {
        match self {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => if *b { 1.0 } else { 0.0 },
            Value::Number(n) => *n,
            Value::String(s) => s.parse::<f64>().unwrap_or(f64::NAN),
            _ => f64::NAN,
        }
    }

    /// ECMAScript `ToString`.
    pub fn to_js_string(&self) -> String {
        match self {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => {
                if n.is_nan() {
                    "NaN".into()
                } else if n.is_infinite() {
                    if *n > 0.0 { "Infinity".into() } else { "-Infinity".into() }
                } else if *n == 0.0 {
                    "0".into()
                } else if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                    format!("{}", *n as i64)
                } else {
                    format!("{}", n)
                }
            }
            Value::String(s) => s.clone(),
            Value::Array(arr) => {
                let elems: Vec<String> = arr.borrow().iter().map(|v| v.to_js_string()).collect();
                elems.join(",")
            }
            Value::Object(_) => "[object Object]".into(),
            Value::Function(_) => "function() { [native code] }".into(),
        }
    }

    pub fn is_undefined(&self) -> bool { matches!(self, Value::Undefined) }
    pub fn is_null(&self) -> bool { matches!(self, Value::Null) }
    pub fn is_nullish(&self) -> bool { matches!(self, Value::Undefined | Value::Null) }

    pub fn is_number(&self) -> bool { matches!(self, Value::Number(_)) }
    pub fn is_string(&self) -> bool { matches!(self, Value::String(_)) }
    pub fn is_bool(&self) -> bool { matches!(self, Value::Bool(_)) }
    pub fn is_object(&self) -> bool { matches!(self, Value::Object(_)) }
    pub fn is_array(&self) -> bool { matches!(self, Value::Array(_)) }
    pub fn is_function(&self) -> bool { matches!(self, Value::Function(_)) }

    /// ECMAScript `ToInt32`.
    pub fn to_i32(&self) -> i32 {
        let n = self.to_number();
        if n.is_nan() || n.is_infinite() || n == 0.0 {
            return 0;
        }
        let i = n.trunc() as i64;
        (i % (1i64 << 32)) as i32
    }

    /// ECMAScript `ToUint32`.
    pub fn to_u32(&self) -> u32 {
        self.to_i32() as u32
    }

    /// ECMAScript `in` operator: `key in obj`.
    pub fn js_in(&self, obj: &Value) -> Value {
        let key = self.to_js_string();
        match obj {
            Value::Object(o) => Value::Bool(o.borrow().has(&key)),
            Value::Array(arr) => {
                if let Ok(idx) = key.parse::<usize>() {
                    Value::Bool(idx < arr.borrow().len())
                } else {
                    Value::Bool(false)
                }
            }
            _ => Value::Bool(false),
        }
    }

    /// Property access: `obj[key]` or `obj.key`.
    pub fn get_property(&self, key: &str) -> Value {
        match self {
            Value::Object(o) => o.borrow().get(key, self).clone(),
            Value::Array(arr) => {
                if let Ok(idx) = key.parse::<usize>() {
                    arr.borrow().get(idx).cloned().unwrap_or(Value::Undefined)
                } else if key == "length" {
                    Value::Number(arr.borrow().len() as f64)
                } else {
                    Value::Undefined
                }
            }
            Value::String(s) => {
                if let Ok(idx) = key.parse::<usize>() {
                    s.chars().nth(idx).map(|c| Value::String(c.to_string())).unwrap_or(Value::Undefined)
                } else if key == "length" {
                    Value::Number(s.len() as f64)
                } else {
                    Value::Undefined
                }
            }
            _ => Value::Undefined,
        }
    }

    /// Property assignment: `obj[key] = value`.
    pub fn set_property(&self, key: &str, value: Value) {
        match self {
            Value::Object(o) => { o.borrow_mut().set(key, value, &Value::Undefined); }
            Value::Array(arr) => {
                if let Ok(idx) = key.parse::<usize>() {
                    let mut a = arr.borrow_mut();
                    if idx >= a.len() {
                        a.resize(idx + 1, Value::Undefined);
                    }
                    a[idx] = value;
                }
            }
            _ => {}
        }
    }
}

// ── Constructors ───────────────────────────────────────────────────────

impl Value {
    pub fn from_f64(n: f64) -> Self { Value::Number(n) }
    pub fn from_bool(b: bool) -> Self { Value::Bool(b) }
    pub fn string(s: &str) -> Self { Value::String(s.to_string()) }

    pub fn array(items: Vec<Value>) -> Self {
        Value::Array(Rc::new(RefCell::new(items)))
    }

    pub fn object(props: HashMap<String, Value>) -> Self {
        Value::Object(Rc::new(RefCell::new(crate::JsObject::from_map(props))))
    }

    pub fn function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Self {
        Value::Function(JsFunction::new(f))
    }
}

// ── Arithmetic / comparison operators ──────────────────────────────────

impl Value {
    /// ECMAScript `+` (addition or string concatenation).
    pub fn js_add(&self, other: &Value) -> Value {
        if self.is_string() || other.is_string() {
            Value::String(format!("{}{}", self.to_js_string(), other.to_js_string()))
        } else {
            Value::Number(self.to_number() + other.to_number())
        }
    }

    pub fn js_sub(&self, other: &Value) -> Value {
        Value::Number(self.to_number() - other.to_number())
    }

    pub fn js_mul(&self, other: &Value) -> Value {
        Value::Number(self.to_number() * other.to_number())
    }

    pub fn js_div(&self, other: &Value) -> Value {
        Value::Number(self.to_number() / other.to_number())
    }

    pub fn js_rem(&self, other: &Value) -> Value {
        Value::Number(self.to_number() % other.to_number())
    }

    pub fn js_neg(&self) -> Value {
        Value::Number(-self.to_number())
    }

    pub fn js_pow(&self, other: &Value) -> Value {
        Value::Number(self.to_number().powf(other.to_number()))
    }

    // ── Bitwise operators ──

    pub fn js_bitor(&self, other: &Value) -> Value {
        Value::Number((self.to_i32() | other.to_i32()) as f64)
    }

    pub fn js_bitand(&self, other: &Value) -> Value {
        Value::Number((self.to_i32() & other.to_i32()) as f64)
    }

    pub fn js_bitxor(&self, other: &Value) -> Value {
        Value::Number((self.to_i32() ^ other.to_i32()) as f64)
    }

    pub fn js_bitnot(&self) -> Value {
        Value::Number((!self.to_i32()) as f64)
    }

    pub fn js_shl(&self, other: &Value) -> Value {
        let shift = (other.to_i32() as u32) & 0x1f;
        Value::Number((self.to_i32() << shift) as f64)
    }

    pub fn js_shr(&self, other: &Value) -> Value {
        let shift = (other.to_i32() as u32) & 0x1f;
        Value::Number((self.to_i32() >> shift) as f64)
    }

    pub fn js_ushr(&self, other: &Value) -> Value {
        let shift = (other.to_i32() as u32) & 0x1f;
        Value::Number(((self.to_i32() as u32) >> shift) as f64)
    }

    /// ECMAScript `===` (strict equality).
    pub fn strict_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) => true,
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => {
                if a.is_nan() || b.is_nan() { false } else { a == b }
            }
            (Value::String(a), Value::String(b)) => a == b,
            _ => false,
        }
    }

    /// ECMAScript `==` (abstract equality — simplified).
    pub fn abstract_eq(&self, other: &Value) -> bool {
        if std::mem::discriminant(self) == std::mem::discriminant(other) {
            return self.strict_eq(other);
        }
        match (self, other) {
            (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => true,
            (Value::Number(_), Value::String(_)) => {
                self.strict_eq(&Value::Number(other.to_number()))
            }
            (Value::String(_), Value::Number(_)) => {
                Value::Number(self.to_number()).strict_eq(other)
            }
            (Value::Bool(_), _) => Value::Number(self.to_number()).abstract_eq(other),
            (_, Value::Bool(_)) => self.abstract_eq(&Value::Number(other.to_number())),
            _ => false,
        }
    }

    pub fn js_lt(&self, other: &Value) -> bool {
        self.to_number() < other.to_number()
    }

    pub fn js_gt(&self, other: &Value) -> bool {
        self.to_number() > other.to_number()
    }

    pub fn js_le(&self, other: &Value) -> bool {
        self.to_number() <= other.to_number()
    }

    pub fn js_ge(&self, other: &Value) -> bool {
        self.to_number() >= other.to_number()
    }

    pub fn js_not(&self) -> Value {
        Value::Bool(!self.to_bool())
    }
}

// ── Display ────────────────────────────────────────────────────────────

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_js_string())
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => write!(f, "{n}"),
            Value::String(s) => write!(f, "{s:?}"),
            Value::Array(arr) => write!(f, "{:?}", arr.borrow()),
            Value::Object(_) => write!(f, "{{...}}"),
            Value::Function(_) => write!(f, "[Function]"),
        }
    }
}

/// Standalone `type_of` function matching the generated code's `type_of(expr)` calls.
pub fn type_of(val: &Value) -> Value {
    Value::String(val.type_of().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_coercion() {
        assert_eq!(Value::Undefined.to_bool(), false);
        assert_eq!(Value::Null.to_bool(), false);
        assert_eq!(Value::Bool(false).to_bool(), false);
        assert_eq!(Value::Number(0.0).to_bool(), false);
        assert_eq!(Value::String("".into()).to_bool(), false);
        assert_eq!(Value::Number(1.0).to_bool(), true);
        assert_eq!(Value::String("x".into()).to_bool(), true);
    }

    #[test]
    fn type_of_values() {
        assert_eq!(Value::Undefined.type_of(), "undefined");
        assert_eq!(Value::Null.type_of(), "object");
        assert_eq!(Value::Number(42.0).type_of(), "number");
        assert_eq!(Value::String("hi".into()).type_of(), "string");
        assert_eq!(Value::Bool(true).type_of(), "boolean");
    }

    #[test]
    fn arithmetic() {
        let a = Value::Number(10.0);
        let b = Value::Number(3.0);
        assert_eq!(a.js_add(&b).to_number(), 13.0);
        assert_eq!(a.js_sub(&b).to_number(), 7.0);
        assert_eq!(a.js_mul(&b).to_number(), 30.0);
    }

    #[test]
    fn string_concat() {
        let a = Value::String("hello".into());
        let b = Value::Number(42.0);
        assert_eq!(a.js_add(&b).to_js_string(), "hello42");
    }

    #[test]
    fn strict_equality() {
        assert!(Value::Number(1.0).strict_eq(&Value::Number(1.0)));
        assert!(!Value::Number(f64::NAN).strict_eq(&Value::Number(f64::NAN)));
        assert!(Value::String("a".into()).strict_eq(&Value::String("a".into())));
        assert!(!Value::Number(1.0).strict_eq(&Value::String("1".into())));
    }

    #[test]
    fn abstract_equality() {
        assert!(Value::Null.abstract_eq(&Value::Undefined));
        assert!(Value::Number(1.0).abstract_eq(&Value::String("1".into())));
        assert!(Value::Bool(true).abstract_eq(&Value::Number(1.0)));
    }

    #[test]
    fn to_js_string() {
        assert_eq!(Value::Undefined.to_js_string(), "undefined");
        assert_eq!(Value::Null.to_js_string(), "null");
        assert_eq!(Value::Number(42.0).to_js_string(), "42");
        assert_eq!(Value::Number(3.14).to_js_string(), "3.14");
        assert_eq!(Value::Bool(true).to_js_string(), "true");
    }

    #[test]
    fn bitwise_operations() {
        let a = Value::Number(5.0);
        let b = Value::Number(3.0);
        assert_eq!(a.js_bitor(&b).to_number(), 7.0);
        assert_eq!(a.js_bitand(&b).to_number(), 1.0);
        assert_eq!(a.js_bitxor(&b).to_number(), 6.0);
        assert_eq!(a.js_shl(&Value::Number(1.0)).to_number(), 10.0);
        assert_eq!(a.js_shr(&Value::Number(1.0)).to_number(), 2.0);
    }

    #[test]
    fn power_operator() {
        assert_eq!(Value::Number(2.0).js_pow(&Value::Number(10.0)).to_number(), 1024.0);
        assert_eq!(Value::Number(9.0).js_pow(&Value::Number(0.5)).to_number(), 3.0);
    }

    #[test]
    fn to_i32_conversion() {
        assert_eq!(Value::Number(42.7).to_i32(), 42);
        assert_eq!(Value::Number(-3.9).to_i32(), -3);
        assert_eq!(Value::Number(f64::NAN).to_i32(), 0);
        assert_eq!(Value::Number(f64::INFINITY).to_i32(), 0);
    }

    #[test]
    fn in_operator() {
        let mut props = HashMap::new();
        props.insert("name".to_string(), Value::String("test".into()));
        let obj = Value::object(props);
        assert!(Value::String("name".into()).js_in(&obj).to_bool());
        assert!(!Value::String("age".into()).js_in(&obj).to_bool());

        let arr = Value::array(vec![Value::Number(10.0), Value::Number(20.0)]);
        assert!(Value::Number(0.0).js_in(&arr).to_bool());
        assert!(Value::Number(1.0).js_in(&arr).to_bool());
        assert!(!Value::Number(2.0).js_in(&arr).to_bool());
    }

    #[test]
    fn property_access() {
        let mut props = HashMap::new();
        props.insert("x".to_string(), Value::Number(42.0));
        let obj = Value::object(props);
        assert_eq!(obj.get_property("x").to_number(), 42.0);
        assert!(obj.get_property("y").is_undefined());

        let arr = Value::array(vec![Value::String("a".into()), Value::String("b".into())]);
        assert_eq!(arr.get_property("0").to_js_string(), "a");
        assert_eq!(arr.get_property("length").to_number(), 2.0);

        let s = Value::String("hello".into());
        assert_eq!(s.get_property("length").to_number(), 5.0);
        assert_eq!(s.get_property("0").to_js_string(), "h");
    }

    #[test]
    fn property_set() {
        let obj = Value::object(HashMap::new());
        obj.set_property("key", Value::Number(99.0));
        assert_eq!(obj.get_property("key").to_number(), 99.0);
    }
}
