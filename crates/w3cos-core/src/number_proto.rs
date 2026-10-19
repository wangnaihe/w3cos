//! `Number.prototype` / `Boolean.prototype` methods on primitive receivers.

use crate::value::{Value, range_error};

pub(crate) const NUMBER_METHODS: &[&str] = &[
    "toString",
    "toFixed",
    "toPrecision",
    "toExponential",
    "toLocaleString",
    "valueOf",
];

/// A primitive number's method `key`, or `None` when it is not one.
pub(crate) fn number_method(value: f64, key: &str, args: &[Value]) -> Option<Value> {
    let arg = args.first().filter(|arg| !arg.is_undefined());
    Some(match key {
        "toString" => {
            let radix = arg.map_or(10.0, |radix| radix.to_number().trunc());
            if !(2.0..=36.0).contains(&radix) {
                crate::throw_value(range_error("toString() radix must be between 2 and 36"));
            }
            if radix == 10.0 {
                Value::Number(value).to_js_string().into()
            } else {
                to_radix_string(value, radix as u32).into()
            }
        }
        "toFixed" => {
            let digits = arg.map_or(0.0, |digits| digits.to_number()).trunc();
            let digits = if digits.is_nan() { 0.0 } else { digits };
            if !(0.0..=100.0).contains(&digits) {
                crate::throw_value(range_error(
                    "toFixed() digits argument must be between 0 and 100",
                ));
            }
            to_fixed(value, digits as usize).into()
        }
        "toExponential" => {
            let digits = arg.map(|digits| {
                let digits = digits.to_number().trunc();
                if digits.is_nan() { 0.0 } else { digits }
            });
            if !value.is_finite() {
                return Some(Value::Number(value).to_js_string().into());
            }
            if digits.is_some_and(|digits| !(0.0..=100.0).contains(&digits)) {
                crate::throw_value(range_error(
                    "toExponential() argument must be between 0 and 100",
                ));
            }
            to_exponential(value, digits.map(|digits| digits as usize)).into()
        }
        "toPrecision" => {
            let Some(precision) = arg else {
                return Some(Value::Number(value).to_js_string().into());
            };
            let precision = precision.to_number().trunc();
            if !value.is_finite() {
                return Some(Value::Number(value).to_js_string().into());
            }
            if !(1.0..=100.0).contains(&precision) {
                crate::throw_value(range_error(
                    "toPrecision() argument must be between 1 and 100",
                ));
            }
            to_precision(value, precision as usize).into()
        }
        "toLocaleString" => to_locale_string(value).into(),
        "valueOf" => Value::Number(value),
        _ => return None,
    })
}

/// A primitive boolean's method `key`.
pub(crate) fn boolean_method(value: bool, key: &str) -> Option<Value> {
    match key {
        "toString" | "toLocaleString" => Some(Value::from(value.to_string())),
        "valueOf" => Some(Value::Bool(value)),
        _ => None,
    }
}

/// `n.toFixed` read as a value: a native function that re-dispatches on
/// its receiver.
pub(crate) fn primitive_method_value(key: &'static str) -> Value {
    crate::named_function(key, 1, move |this, args| {
        if let Some(flag) = this.as_bool() {
            return boolean_method(flag, key).unwrap_or(Value::Undefined);
        }
        let number = this.as_number().unwrap_or_else(|| this.to_number());
        number_method(number, key, &args).unwrap_or(Value::Undefined)
    })
}

/// Port of V8's `DoubleToRadixCString`: shortest digits that round-trip.
pub(crate) fn to_radix_string(value: f64, radix: u32) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if value == 0.0 {
        return "0".into();
    }
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let negative = value < 0.0;
    let value = value.abs();
    let radix_f = f64::from(radix);
    let mut integer = value.floor();
    let mut fraction = value - integer;
    let next = f64::from_bits(value.to_bits() + 1);
    let mut delta = (0.5 * (next - value)).max(f64::from_bits(1));
    let mut fraction_digits: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= radix_f;
            delta *= radix_f;
            let digit = fraction as u32;
            fraction_digits.push(CHARS[digit as usize]);
            fraction -= f64::from(digit);
            if (fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1)) && fraction + delta > 1.0 {
                // Round up, propagating carries.
                loop {
                    let Some(last) = fraction_digits.pop() else {
                        integer += 1.0;
                        break;
                    };
                    let digit = if last > b'9' {
                        u32::from(last - b'a') + 10
                    } else {
                        u32::from(last - b'0')
                    };
                    if digit + 1 < radix {
                        fraction_digits.push(CHARS[(digit + 1) as usize]);
                        break;
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    let mut integer_digits: Vec<u8> = Vec::new();
    // Beyond 2^53 the low digits carry no information.
    while integer / radix_f >= 9_007_199_254_740_992.0 {
        integer /= radix_f;
        integer_digits.push(b'0');
    }
    loop {
        let remainder = integer % radix_f;
        integer_digits.push(CHARS[remainder as usize]);
        integer = (integer - remainder) / radix_f;
        if integer <= 0.0 {
            break;
        }
    }
    integer_digits.reverse();
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(std::str::from_utf8(&integer_digits).expect("ascii"));
    if !fraction_digits.is_empty() {
        out.push('.');
        out.push_str(std::str::from_utf8(&fraction_digits).expect("ascii"));
    }
    out
}

/// Exact decimal digits of a finite positive double plus the decimal
/// exponent of the first digit (`d.ddd × 10^exp`).
fn exact_digits(value: f64) -> (Vec<u8>, i32) {
    let text = format!("{value:.800e}");
    let (mantissa, exponent) = text.split_once('e').expect("exponent");
    let mut digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    (digits, exponent.parse().expect("exponent"))
}

/// Round `digits` to `count` significant digits, ties away from zero (the
/// spec's "larger n"). Returns the digits and the adjusted exponent.
fn round_significant(mut digits: Vec<u8>, mut exponent: i32, count: usize) -> (Vec<u8>, i32) {
    if digits.len() > count {
        let round_up = digits[count] >= b'5';
        digits.truncate(count);
        if round_up {
            let mut index = count;
            loop {
                if index == 0 {
                    digits.insert(0, b'1');
                    digits.truncate(count);
                    exponent += 1;
                    break;
                }
                index -= 1;
                if digits[index] == b'9' {
                    digits[index] = b'0';
                } else {
                    digits[index] += 1;
                    break;
                }
            }
        }
    }
    while digits.len() < count {
        digits.push(b'0');
    }
    (digits, exponent)
}

fn exponent_suffix(exponent: i32) -> String {
    if exponent < 0 {
        format!("e{exponent}")
    } else {
        format!("e+{exponent}")
    }
}

pub(crate) fn to_fixed(value: f64, digits: usize) -> String {
    if !value.is_finite() || value.abs() >= 1e21 {
        return Value::Number(value).to_js_string();
    }
    let negative = value < 0.0;
    let magnitude = value.abs();
    let text = if magnitude == 0.0 {
        if digits == 0 {
            "0".to_string()
        } else {
            format!("0.{}", "0".repeat(digits))
        }
    } else {
        let (all, exponent) = exact_digits(magnitude);
        // Significant digits needed to reach `digits` places after the point.
        let count = exponent + 1 + digits as i32;
        if count <= 0 {
            // Rounds to 0 or to one unit in the last place.
            let half = count == 0 && all[0] >= b'5';
            let unit = if half { "1" } else { "0" };
            if digits == 0 {
                unit.to_string()
            } else {
                format!("0.{}{}", "0".repeat(digits - 1), unit)
            }
        } else {
            let (rounded, exponent) = round_significant(all, exponent, count as usize);
            // A carry may add a leading digit: keep exactly `digits` decimals.
            let int_len = (exponent + 1).max(0) as usize;
            let mut body = String::from_utf8(rounded).expect("ascii");
            let wanted = int_len + digits;
            while body.len() < wanted {
                body.push('0');
            }
            body.truncate(wanted);
            let (int_part, frac_part) = body.split_at(int_len);
            let int_part = if int_part.is_empty() { "0" } else { int_part };
            if digits == 0 {
                int_part.to_string()
            } else {
                format!("{int_part}.{frac_part}")
            }
        }
    };
    if negative
        && text
            .bytes()
            .any(|byte| byte.is_ascii_digit() && byte != b'0')
    {
        format!("-{text}")
    } else if negative && value != 0.0 {
        // (-0.0001).toFixed(2) is "-0.00".
        format!("-{text}")
    } else {
        text
    }
}

pub(crate) fn to_exponential(value: f64, digits: Option<usize>) -> String {
    let negative = value < 0.0;
    let magnitude = value.abs();
    let (digits_out, exponent) = if magnitude == 0.0 {
        (vec![b'0'; digits.unwrap_or(0) + 1], 0)
    } else if let Some(digits) = digits {
        let (all, exponent) = exact_digits(magnitude);
        round_significant(all, exponent, digits + 1)
    } else {
        // Shortest round-trip digits.
        let text = format!("{magnitude:e}");
        let (mantissa, exponent) = text.split_once('e').expect("exponent");
        (
            mantissa.bytes().filter(u8::is_ascii_digit).collect(),
            exponent.parse().expect("exponent"),
        )
    };
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push(digits_out[0] as char);
    if digits_out.len() > 1 {
        out.push('.');
        out.push_str(std::str::from_utf8(&digits_out[1..]).expect("ascii"));
    }
    out.push_str(&exponent_suffix(exponent));
    out
}

pub(crate) fn to_precision(value: f64, precision: usize) -> String {
    let negative = value < 0.0;
    let magnitude = value.abs();
    let (digits, exponent) = if magnitude == 0.0 {
        (vec![b'0'; precision], 0)
    } else {
        let (all, exponent) = exact_digits(magnitude);
        round_significant(all, exponent, precision)
    };
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    let digits = String::from_utf8(digits).expect("ascii");
    if exponent < -6 || exponent >= precision as i32 {
        out.push_str(&digits[..1]);
        if precision > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push_str(&exponent_suffix(exponent));
    } else if exponent >= 0 {
        let split = exponent as usize + 1;
        out.push_str(&digits[..split]);
        if split < precision {
            out.push('.');
            out.push_str(&digits[split..]);
        }
    } else {
        out.push_str("0.");
        out.push_str(&"0".repeat((-exponent - 1) as usize));
        out.push_str(&digits);
    }
    out
}

/// `en-US` formatting: grouping, at most three fraction digits.
pub(crate) fn to_locale_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "∞".into()
        } else {
            "-∞".into()
        };
    }
    let fixed = to_fixed(value.abs(), 3);
    let (int_part, frac_part) = fixed.split_once('.').unwrap_or((&fixed, ""));
    let frac_part = frac_part.trim_end_matches('0');
    let mut grouped = String::new();
    for (index, ch) in int_part.chars().enumerate() {
        if index > 0 && (int_part.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let sign = if value < 0.0 && (int_part != "0" || !frac_part.is_empty()) {
        "-"
    } else {
        ""
    };
    if frac_part.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{frac_part}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radix_strings_match_v8() {
        assert_eq!(to_radix_string(255.0, 16), "ff");
        assert_eq!(to_radix_string(-255.0, 36), "-73");
        assert_eq!(to_radix_string(0.5, 2), "0.1");
        assert_eq!(
            to_radix_string(0.1, 3),
            "0.0022002200220022002200220022002201"
        );
        assert_eq!(to_radix_string(3.75, 16), "3.c");
        assert_eq!(to_radix_string(1e21, 16), "3635c9adc5dea00000");
    }

    #[test]
    fn fixed_precision_exponential() {
        assert_eq!(to_fixed(255.5, 2), "255.50");
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(0.000001, 2), "0.00");
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(-1.5, 0), "-2");
        assert_eq!(to_fixed(-0.0001, 2), "-0.00");
        assert_eq!(to_fixed(99.99, 1), "100.0");
        assert_eq!(to_fixed(0.05, 1), "0.1");
        assert_eq!(to_fixed(123.456, 1), "123.5");
        assert_eq!(to_precision(255.5, 4), "255.5");
        assert_eq!(to_precision(123456.0, 2), "1.2e+5");
        assert_eq!(to_precision(0.00001234, 2), "0.000012");
        assert_eq!(to_precision(0.0000001234, 2), "1.2e-7");
        assert_eq!(to_precision(0.0, 3), "0.00");
        assert_eq!(to_exponential(1e21, Some(2)), "1.00e+21");
        assert_eq!(to_exponential(123.456, None), "1.23456e+2");
        assert_eq!(to_exponential(0.00015, Some(1)), "1.5e-4");
        assert_eq!(to_exponential(0.0, None), "0e+0");
        assert_eq!(to_locale_string(1234.5678), "1,234.568");
        assert_eq!(to_locale_string(-1234567.0), "-1,234,567");
    }
}
