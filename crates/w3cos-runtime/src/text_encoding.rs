/// W3C Encoding API — TextEncoder / TextDecoder.
///
/// TextEncoder always encodes to UTF-8 (per spec).
/// TextDecoder supports UTF-8 (default) and common single-byte encodings.

/// TextEncoder — encodes a string into a UTF-8 byte array.
pub struct TextEncoder;

impl TextEncoder {
    pub fn new() -> Self {
        Self
    }

    pub fn encoding(&self) -> &'static str {
        "utf-8"
    }

    /// `TextEncoder.encode(string)` → `Vec<u8>` (Uint8Array equivalent).
    pub fn encode(&self, input: &str) -> Vec<u8> {
        input.as_bytes().to_vec()
    }

    /// `TextEncoder.encodeInto(string, destination)` → `EncodeResult`.
    pub fn encode_into(&self, input: &str, dest: &mut [u8]) -> EncodeResult {
        let bytes = input.as_bytes();
        let mut read_chars = 0;
        let mut written = 0;

        for ch in input.chars() {
            let len = ch.len_utf8();
            if written + len > dest.len() {
                break;
            }
            ch.encode_utf8(&mut dest[written..]);
            written += len;
            read_chars += 1;
        }

        // For surrogate pair handling, count UTF-16 code units
        let read = input.chars().take(read_chars).map(|c| c.len_utf16()).sum();

        EncodeResult { read, written }
    }
}

impl Default for TextEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of `TextEncoder.encodeInto()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeResult {
    /// Number of UTF-16 code units read from the source string.
    pub read: usize,
    /// Number of bytes written to the destination buffer.
    pub written: usize,
}

/// TextDecoder — decodes bytes into a string.
pub struct TextDecoder {
    encoding: String,
    fatal: bool,
    ignore_bom: bool,
}

impl TextDecoder {
    pub fn new(encoding: &str) -> Self {
        Self {
            encoding: normalize_encoding(encoding),
            fatal: false,
            ignore_bom: false,
        }
    }

    pub fn with_options(encoding: &str, fatal: bool, ignore_bom: bool) -> Self {
        Self {
            encoding: normalize_encoding(encoding),
            fatal,
            ignore_bom,
        }
    }

    pub fn encoding(&self) -> &str {
        &self.encoding
    }

    pub fn fatal(&self) -> bool {
        self.fatal
    }

    pub fn ignore_bom(&self) -> bool {
        self.ignore_bom
    }

    /// `TextDecoder.decode(bytes)` → `Result<String, DecodingError>`.
    pub fn decode(&self, input: &[u8]) -> Result<String, DecodingError> {
        let data = if !self.ignore_bom {
            strip_bom(input, &self.encoding)
        } else {
            input
        };

        match self.encoding.as_str() {
            "utf-8" => {
                if self.fatal {
                    std::str::from_utf8(data)
                        .map(|s| s.to_string())
                        .map_err(|e| DecodingError {
                            message: format!("invalid UTF-8 at byte {}", e.valid_up_to()),
                        })
                } else {
                    Ok(String::from_utf8_lossy(data).into_owned())
                }
            }
            "ascii" | "us-ascii" => {
                if self.fatal && data.iter().any(|&b| b > 127) {
                    Err(DecodingError {
                        message: "non-ASCII byte encountered".to_string(),
                    })
                } else {
                    Ok(data.iter().map(|&b| {
                        if b <= 127 { b as char } else { '\u{FFFD}' }
                    }).collect())
                }
            }
            "utf-16le" => Ok(decode_utf16le(data)),
            "utf-16be" => Ok(decode_utf16be(data)),
            _ => {
                if self.fatal {
                    Err(DecodingError {
                        message: format!("unsupported encoding: {}", self.encoding),
                    })
                } else {
                    Ok(String::from_utf8_lossy(data).into_owned())
                }
            }
        }
    }
}

impl Default for TextDecoder {
    fn default() -> Self {
        Self::new("utf-8")
    }
}

#[derive(Debug, Clone)]
pub struct DecodingError {
    pub message: String,
}

impl std::fmt::Display for DecodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DecodingError: {}", self.message)
    }
}

impl std::error::Error for DecodingError {}

fn normalize_encoding(label: &str) -> String {
    match label.trim().to_ascii_lowercase().as_str() {
        "utf8" | "utf-8" | "" => "utf-8".to_string(),
        "ascii" | "us-ascii" => "ascii".to_string(),
        "utf-16" | "utf-16le" | "utf16le" => "utf-16le".to_string(),
        "utf-16be" | "utf16be" => "utf-16be".to_string(),
        "latin1" | "iso-8859-1" | "windows-1252" => "utf-8".to_string(),
        other => other.to_string(),
    }
}

fn strip_bom<'a>(data: &'a [u8], encoding: &str) -> &'a [u8] {
    match encoding {
        "utf-8" if data.starts_with(&[0xEF, 0xBB, 0xBF]) => &data[3..],
        "utf-16le" if data.starts_with(&[0xFF, 0xFE]) => &data[2..],
        "utf-16be" if data.starts_with(&[0xFE, 0xFF]) => &data[2..],
        _ => data,
    }
}

fn decode_utf16le(data: &[u8]) -> String {
    let iter = data.chunks(2).map(|chunk| {
        if chunk.len() == 2 {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            0xFFFD
        }
    });
    char::decode_utf16(iter)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

fn decode_utf16be(data: &[u8]) -> String {
    let iter = data.chunks(2).map(|chunk| {
        if chunk.len() == 2 {
            u16::from_be_bytes([chunk[0], chunk[1]])
        } else {
            0xFFFD
        }
    });
    char::decode_utf16(iter)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_basic() {
        let enc = TextEncoder::new();
        assert_eq!(enc.encoding(), "utf-8");
        assert_eq!(enc.encode("hello"), b"hello");
        assert_eq!(enc.encode("日本語"), "日本語".as_bytes());
    }

    #[test]
    fn encoder_encode_into() {
        let enc = TextEncoder::new();
        let mut buf = [0u8; 5];
        let result = enc.encode_into("hello world", &mut buf);
        assert_eq!(result.written, 5);
        assert_eq!(&buf, b"hello");
    }

    #[test]
    fn decoder_utf8() {
        let dec = TextDecoder::new("utf-8");
        assert_eq!(dec.decode(b"hello").unwrap(), "hello");
        assert_eq!(dec.decode("日本語".as_bytes()).unwrap(), "日本語");
    }

    #[test]
    fn decoder_utf8_bom() {
        let dec = TextDecoder::new("utf-8");
        let with_bom = [0xEF, 0xBB, 0xBF, b'h', b'i'];
        assert_eq!(dec.decode(&with_bom).unwrap(), "hi");
    }

    #[test]
    fn decoder_fatal_invalid_utf8() {
        let dec = TextDecoder::with_options("utf-8", true, false);
        assert!(dec.decode(&[0xFF, 0xFE]).is_err());
    }

    #[test]
    fn decoder_lossy_invalid_utf8() {
        let dec = TextDecoder::new("utf-8");
        let result = dec.decode(&[0xFF, 0xFE]).unwrap();
        assert!(result.contains('\u{FFFD}'));
    }

    #[test]
    fn decoder_utf16le() {
        let dec = TextDecoder::new("utf-16le");
        let data = [0x68, 0x00, 0x69, 0x00]; // "hi" in UTF-16LE
        assert_eq!(dec.decode(&data).unwrap(), "hi");
    }

    #[test]
    fn decoder_ascii() {
        let dec = TextDecoder::new("ascii");
        assert_eq!(dec.decode(b"hello").unwrap(), "hello");
    }

    #[test]
    fn decoder_ascii_fatal() {
        let dec = TextDecoder::with_options("ascii", true, false);
        assert!(dec.decode(&[0x80]).is_err());
    }
}
