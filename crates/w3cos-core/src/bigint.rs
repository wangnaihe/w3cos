//! ECMAScript BigInt primitives represented as tagged runtime values.

use std::cmp::Ordering;
use std::collections::HashMap;

use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};

use crate::Value;

const VALUE: &str = "__w3cos_bigint_value";

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn parse_text(input: &str) -> Option<BigInt> {
    let input = input.trim();
    let (negative, unsigned) = input
        .strip_prefix('-')
        .map(|rest| (true, rest))
        .unwrap_or((false, input));
    let (radix, digits) = if let Some(rest) = unsigned.strip_prefix("0x") {
        (16, rest)
    } else if let Some(rest) = unsigned.strip_prefix("0X") {
        (16, rest)
    } else if let Some(rest) = unsigned.strip_prefix("0o") {
        (8, rest)
    } else if let Some(rest) = unsigned.strip_prefix("0O") {
        (8, rest)
    } else if let Some(rest) = unsigned.strip_prefix("0b") {
        (2, rest)
    } else if let Some(rest) = unsigned.strip_prefix("0B") {
        (2, rest)
    } else {
        (10, unsigned)
    };
    let value = BigInt::parse_bytes(digits.as_bytes(), radix)?;
    Some(if negative { -value } else { value })
}

pub fn parse(input: &str) -> Value {
    parse_text(input).map(from_bigint).unwrap_or_else(|| {
        crate::throw_value(error("SyntaxError", "Cannot convert value to BigInt"))
    })
}

pub fn get(value: &Value) -> Option<BigInt> {
    let Some(object) = value.as_object() else {
        return None;
    };
    // `from_bigint` stores the encoded text as an *own* property, so look at
    // the object's own properties only. `get` runs on every `===`, SameValueZero
    // and primitive conversion of an object; `get_direct` would walk the whole
    // prototype chain (and check accessors) on each of those misses.
    let encoded = object.borrow().properties.get(VALUE).cloned()?;
    if encoded.is_undefined() {
        None
    } else {
        parse_text(&encoded.to_js_string())
    }
}

fn bigint_to_string(value: &BigInt, args: &[Value]) -> Value {
    let radix = args
        .first()
        .filter(|value| !value.is_undefined())
        .map(Value::to_number)
        .unwrap_or(10.0);
    if !radix.is_finite() || !(2.0..=36.0).contains(&radix) || radix.fract() != 0.0 {
        crate::throw_value(error(
            "RangeError",
            "BigInt radix must be an integer between 2 and 36",
        ));
    }
    Value::from(value.to_str_radix(radix as u32))
}

fn from_bigint(value: BigInt) -> Value {
    let encoded = value.to_string();
    let to_string_value = value.clone();
    let value_of_value = value.clone();
    Value::object(HashMap::from([
        (VALUE.into(), Value::from(encoded)),
        (
            "toString".into(),
            Value::function(move |_, args| bigint_to_string(&to_string_value, &args)),
        ),
        (
            "toLocaleString".into(),
            Value::function({
                let value = value.clone();
                move |_, _| Value::from(value.to_string())
            }),
        ),
        (
            "valueOf".into(),
            Value::function(move |_, _| from_bigint(value_of_value.clone())),
        ),
    ]))
}

fn this_bigint(this: &Value, method: &str) -> BigInt {
    get(this).unwrap_or_else(|| {
        crate::throw_value(crate::error_instance(
            "TypeError",
            vec![Value::from(format!(
                "BigInt.prototype.{method} requires that 'this' be a BigInt"
            ))],
        ))
    })
}

/// The global `BigInt` function, one per thread, with `BigInt.prototype`
/// (`toString`/`valueOf`/`toLocaleString`) so uncurried captures (DOMPurify
/// `unapply(BigInt.prototype.toString)`) resolve.
pub fn bigint_class() -> Value {
    thread_local! {
        static BIGINT: std::cell::RefCell<Option<Value>> = const { std::cell::RefCell::new(None) };
    }
    BIGINT.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let prototype = Value::object(HashMap::from([
            (
                "toString".to_string(),
                Value::function(|this, args| {
                    bigint_to_string(&this_bigint(&this, "toString"), &args)
                }),
            ),
            (
                "toLocaleString".to_string(),
                Value::function(|this, _| {
                    Value::from(this_bigint(&this, "toLocaleString").to_string())
                }),
            ),
            (
                "valueOf".to_string(),
                Value::function(|this, _| from_bigint(this_bigint(&this, "valueOf"))),
            ),
        ]));
        let value = Value::function(bigint_call);
        value.set_property("prototype", prototype.clone());
        prototype.set_property("constructor", value.clone());
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

fn bigint_call(_: Value, args: Vec<Value>) -> Value {
    let input = args.first().cloned().unwrap_or(Value::Undefined);
    if let Some(value) = get(&input) {
        return from_bigint(value);
    }
    match input.unpack() {
        crate::value::ValueUnpack::Number(number)
            if number.is_finite() && number.fract() == 0.0 =>
        {
            parse(&format!("{number:.0}"))
        }
        crate::value::ValueUnpack::Number(_) => crate::throw_value(error(
            "RangeError",
            "The number cannot be converted to a BigInt because it is not an integer",
        )),
        crate::value::ValueUnpack::Bool(value) => from_bigint(BigInt::from(u8::from(value))),
        _ => parse(&input.to_js_string()),
    }
}

fn pair(left: &Value, right: &Value) -> Option<(BigInt, BigInt)> {
    match (get(left), get(right)) {
        (None, None) => None,
        (Some(left), Some(right)) => Some((left, right)),
        _ => crate::throw_value(error("TypeError", "Cannot mix BigInt and other types")),
    }
}

pub fn add(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| from_bigint(left + right))
}

pub fn sub(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| from_bigint(left - right))
}

pub fn mul(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| from_bigint(left * right))
}

pub fn div(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| {
        if right.is_zero() {
            crate::throw_value(error("RangeError", "Division by zero"));
        }
        from_bigint(left / right)
    })
}

pub fn rem(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| {
        if right.is_zero() {
            crate::throw_value(error("RangeError", "Division by zero"));
        }
        from_bigint(left % right)
    })
}

pub fn pow(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| {
        let Some(exponent) = right.to_u32() else {
            crate::throw_value(error("RangeError", "BigInt exponent must be non-negative"));
        };
        from_bigint(left.pow(exponent))
    })
}

pub fn bitand(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| from_bigint(left & right))
}

pub fn bitor(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| from_bigint(left | right))
}

pub fn bitxor(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| from_bigint(left ^ right))
}

pub fn shift_left(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| {
        let shift = right.to_i64().unwrap_or_else(|| {
            crate::throw_value(error("RangeError", "BigInt shift count is too large"))
        });
        if shift < 0 {
            from_bigint(left >> shift.unsigned_abs() as usize)
        } else {
            from_bigint(left << shift as usize)
        }
    })
}

pub fn shift_right(left: &Value, right: &Value) -> Option<Value> {
    pair(left, right).map(|(left, right)| {
        let shift = right.to_i64().unwrap_or_else(|| {
            crate::throw_value(error("RangeError", "BigInt shift count is too large"))
        });
        if shift < 0 {
            from_bigint(left << shift.unsigned_abs() as usize)
        } else {
            from_bigint(left >> shift as usize)
        }
    })
}

pub fn neg(value: &Value) -> Option<Value> {
    get(value).map(|value| from_bigint(-value))
}

pub fn bitnot(value: &Value) -> Option<Value> {
    get(value).map(|value| from_bigint(!value))
}

pub fn equals(left: &Value, right: &Value) -> Option<bool> {
    match (get(left), get(right)) {
        (Some(left), Some(right)) => Some(left == right),
        (Some(_), None) | (None, Some(_)) => Some(false),
        (None, None) => None,
    }
}

pub fn compare(left: &Value, right: &Value) -> Option<Ordering> {
    match (get(left), get(right)) {
        (Some(left), Some(right)) => Some(left.cmp(&right)),
        _ => None,
    }
}

pub fn is_zero(value: &Value) -> Option<bool> {
    get(value).map(|value| value.is_zero())
}

pub(crate) fn to_i64(value: &Value) -> Option<i64> {
    Some(to_u64_wrapping(get(value)?) as i64)
}

pub(crate) fn to_u64(value: &Value) -> Option<u64> {
    Some(to_u64_wrapping(get(value)?))
}

fn to_u64_wrapping(value: BigInt) -> u64 {
    let modulus: BigInt = BigInt::from(1_u8) << 64_usize;
    let wrapped: BigInt = ((value % &modulus) + &modulus) % &modulus;
    wrapped
        .to_u64()
        .expect("value reduced modulo 2^64 always fits u64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_own_encoded_objects_are_bigints() {
        let plain = Value::object(HashMap::from([("a".into(), Value::Number(1.0))]));
        assert!(get(&plain).is_none());
        assert_eq!(equals(&plain, &plain.clone()), None);
        assert_eq!(equals(&plain, &parse("1")), Some(false));
        assert_eq!(equals(&parse("12"), &parse("12")), Some(true));
        assert_eq!(equals(&parse("12"), &parse("13")), Some(false));
        assert!(get(&Value::Number(1.0)).is_none());
        assert_eq!(get(&parse("-7")).map(|v| v.to_string()), Some("-7".into()));
    }

    #[test]
    fn parses_arbitrary_precision_and_formats_radices() {
        let value = parse("9007199254740993");
        assert_eq!(value.type_of(), "bigint");
        assert_eq!(value.to_js_string(), "9007199254740993");
        assert_eq!(
            value
                .call_method("toString", vec![Value::Number(16.0)])
                .to_js_string(),
            "20000000000001"
        );
        assert!(value.to_bool());
        assert!(!parse("0").to_bool());
    }

    #[test]
    fn bigint_function_exposes_an_uncurryable_prototype() {
        let class = bigint_class();
        assert!(class.strict_eq(&bigint_class()));
        let to_string = class.get_property("prototype").get_property("toString");
        assert_eq!(
            to_string
                .call(parse("255"), vec![Value::Number(16.0)])
                .to_js_string(),
            "ff"
        );
        assert!(crate::value::catch_js(|| to_string.call(Value::Number(1.0), Vec::new())).is_err());
        assert_eq!(
            class
                .call(Value::Undefined, vec![Value::Number(12.0)])
                .to_js_string(),
            "12"
        );
    }

    #[test]
    fn arithmetic_bitwise_and_comparison_keep_bigint_precision() {
        let base = parse("9007199254740993");
        let computed = base.js_add(&parse("7")).js_mul(&parse("2"));
        assert_eq!(computed.to_js_string(), "18014398509482000");
        assert_eq!(parse("2").js_pow(&parse("10")).to_js_string(), "1024");
        assert_eq!(
            parse("5")
                .js_shl(&parse("2"))
                .js_bitor(&parse("1"))
                .to_js_string(),
            "21"
        );
        assert_eq!(parse("-5").js_div(&parse("2")).to_js_string(), "-2");
        assert!(base.strict_eq(&parse("9007199254740993")));
        assert!(parse("2").js_lt(&parse("10")));
    }

    #[test]
    fn mixing_number_and_bigint_throws_type_error() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            parse("1").js_add(&Value::Number(1.0))
        }));
        let payload = result.expect_err("mixed numeric domains must throw");
        let error = payload
            .downcast_ref::<crate::PanicValue>()
            .expect("BigInt uses the JavaScript exception channel");
        assert_eq!(error.0.get_property("name").to_js_string(), "TypeError");
    }
}
