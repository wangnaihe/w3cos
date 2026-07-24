//! ECMAScript URI encoding and decoding globals.

use std::collections::HashMap;

use w3cos_core::Value;

fn is_component_unescaped(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
        )
}

fn is_uri_reserved(byte: u8) -> bool {
    matches!(
        byte,
        b';' | b',' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b'#'
    )
}

fn encode(input: &str, component: bool) -> String {
    let mut output = String::with_capacity(input.len());
    for byte in input.bytes() {
        if is_component_unescaped(byte) || (!component && is_uri_reserved(byte)) {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push_str(&format!("{byte:02X}"));
        }
    }
    output
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode(input: &str, component: bool) -> Result<String, ()> {
    let input = input.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'%' {
            output.push(input[index]);
            index += 1;
            continue;
        }
        if index + 2 >= input.len() {
            return Err(());
        }
        let high = hex_nibble(input[index + 1]).ok_or(())?;
        let low = hex_nibble(input[index + 2]).ok_or(())?;
        let decoded = high << 4 | low;
        if !component && is_uri_reserved(decoded) {
            output.extend_from_slice(&input[index..index + 3]);
        } else {
            output.push(decoded);
        }
        index += 3;
    }
    String::from_utf8(output).map_err(|_| ())
}

fn uri_error(input: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string("URIError")),
        (
            "message".into(),
            Value::string(&format!("URI malformed: {input}")),
        ),
    ]))
}

pub fn encode_uri(arguments: Vec<Value>) -> Value {
    let input = arguments.first().cloned().unwrap_or(Value::Undefined);
    Value::from(encode(&input.to_js_string(), false))
}

pub fn encode_uri_component(arguments: Vec<Value>) -> Value {
    let input = arguments.first().cloned().unwrap_or(Value::Undefined);
    Value::from(encode(&input.to_js_string(), true))
}

pub fn decode_uri(arguments: Vec<Value>) -> Value {
    let input = arguments.first().cloned().unwrap_or(Value::Undefined);
    let input = input.to_js_string();
    match decode(&input, false) {
        Ok(decoded) => Value::from(decoded),
        Err(()) => w3cos_core::throw_value(uri_error(&input)),
    }
}

pub fn decode_uri_component(arguments: Vec<Value>) -> Value {
    let input = arguments.first().cloned().unwrap_or(Value::Undefined);
    let input = input.to_js_string();
    match decode(&input, true) {
        Ok(decoded) => Value::from(decoded),
        Err(()) => w3cos_core::throw_value(uri_error(&input)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_and_component_encoders_preserve_their_respective_safe_sets() {
        assert_eq!(
            encode("https://例子.test/a b?x=1#片", false),
            "https://%E4%BE%8B%E5%AD%90.test/a%20b?x=1#%E7%89%87"
        );
        assert_eq!(encode("a/b?中 文", true), "a%2Fb%3F%E4%B8%AD%20%E6%96%87");
    }

    #[test]
    fn decoders_handle_utf8_and_decode_uri_preserves_reserved_escapes() {
        assert_eq!(decode("%E4%B8%AD%20x%2Fy", true).unwrap(), "中 x/y");
        assert_eq!(
            decode("https%3A%2F%2Fx.test%2Fa%20b%3Fx%3D1", false).unwrap(),
            "https%3A%2F%2Fx.test%2Fa b%3Fx%3D1"
        );
        assert!(decode("%E4%B8", true).is_err());
        assert!(decode("%GG", true).is_err());
    }

    #[test]
    fn malformed_component_raises_uri_error() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_uri_component(vec![Value::string("%GG")])
        }));
        let payload = result.expect_err("malformed URI component must throw");
        let error = payload
            .downcast_ref::<w3cos_core::PanicValue>()
            .expect("URI error uses the JavaScript exception channel");
        assert_eq!(error.0.get_property("name").to_js_string(), "URIError");
    }
}
