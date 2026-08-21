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
    let Value::Object(object) = value else {
        return None;
    };
    let encoded = object.borrow().get_direct(VALUE);
    if encoded.is_undefined() {
        None
    } else {
        parse_text(&encoded.to_js_string())
    }
}

fn from_bigint(value: BigInt) -> Value {
    let encoded = value.to_string();
    let to_string_value = value.clone();
    let value_of_value = value.clone();
    Value::object(HashMap::from([
        (VALUE.into(), Value::from(encoded)),
        (
            "toString".into(),
            Value::function(move |_, args| {
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
                Value::from(to_string_value.to_str_radix(radix as u32))
            }),
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

pub fn bigint_class() -> Value {
    Value::function(|_, args| {
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
    })
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
