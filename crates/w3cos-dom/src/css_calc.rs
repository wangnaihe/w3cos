//! `calc()` length parsing.
//!
//! Lengths are linear in px, percentage, em and viewport units, so an
//! expression folds into a [`CalcLength`] at parse time. Products of two
//! lengths, division by a length and mixing a bare number with a length are
//! invalid CSS and are rejected, leaving the declaration ignored.

use w3cos_std::style::{CalcLength, Dimension};

#[derive(Clone, Copy)]
enum Value {
    Number(f32),
    Length(CalcLength),
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    depth: u32,
}

/// Parse a `calc(...)` value (nested `calc()` and bare parentheses allowed).
pub(crate) fn parse_calc_length(value: &str) -> Option<CalcLength> {
    let value = value.trim();
    let lower_prefix = value.get(..5)?;
    if !lower_prefix.eq_ignore_ascii_case("calc(") {
        return None;
    }
    let mut parser = Parser {
        input: value.as_bytes(),
        pos: 0,
        depth: 0,
    };
    let result = parser.factor()?;
    parser.skip_ws();
    if parser.pos != parser.input.len() {
        return None;
    }
    match result {
        Value::Length(length)
            if [length.px, length.percent, length.em, length.vw, length.vh]
                .iter()
                .all(|c| c.is_finite()) =>
        {
            Some(length)
        }
        _ => None,
    }
}

/// Collapse a calc length to the simplest `Dimension`.
pub(crate) fn calc_to_dimension(length: CalcLength) -> Dimension {
    let only = |px: bool, percent: bool, em: bool, vw: bool, vh: bool| {
        (length.px == 0.0 || px)
            && (length.percent == 0.0 || percent)
            && (length.em == 0.0 || em)
            && (length.vw == 0.0 || vw)
            && (length.vh == 0.0 || vh)
    };
    if only(true, false, false, false, false) {
        Dimension::Px(length.px)
    } else if only(false, true, false, false, false) {
        Dimension::Percent(length.percent)
    } else if only(false, false, true, false, false) {
        Dimension::Em(length.em)
    } else if only(false, false, false, true, false) {
        Dimension::Vw(length.vw)
    } else if only(false, false, false, false, true) {
        Dimension::Vh(length.vh)
    } else {
        Dimension::Calc(length)
    }
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self
            .input
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn expr(&mut self) -> Option<Value> {
        let mut left = self.term()?;
        loop {
            let before = self.pos;
            self.skip_ws();
            let op = match self.peek() {
                Some(b'+') => 1.0,
                Some(b'-') => -1.0,
                _ => {
                    self.pos = before;
                    return Some(left);
                }
            };
            // `+` and `-` must be surrounded by whitespace in calc().
            if self.pos == before {
                return None;
            }
            self.pos += 1;
            if !self
                .input
                .get(self.pos)
                .is_some_and(u8::is_ascii_whitespace)
            {
                return None;
            }
            let right = self.term()?;
            left = match (left, right) {
                (Value::Number(a), Value::Number(b)) => Value::Number(a + op * b),
                (Value::Length(a), Value::Length(b)) => Value::Length(a.add(b.scale(op))),
                _ => return None,
            };
        }
    }

    fn term(&mut self) -> Option<Value> {
        let mut left = self.factor()?;
        loop {
            let before = self.pos;
            self.skip_ws();
            let op = match self.peek() {
                Some(b'*') => b'*',
                Some(b'/') => b'/',
                _ => {
                    self.pos = before;
                    return Some(left);
                }
            };
            self.pos += 1;
            let right = self.factor()?;
            left = match (op, left, right) {
                (b'*', Value::Number(a), Value::Number(b)) => Value::Number(a * b),
                (b'*', Value::Number(a), Value::Length(b))
                | (b'*', Value::Length(b), Value::Number(a)) => Value::Length(b.scale(a)),
                (b'/', Value::Number(a), Value::Number(b)) if b != 0.0 => Value::Number(a / b),
                (b'/', Value::Length(a), Value::Number(b)) if b != 0.0 => {
                    Value::Length(a.scale(1.0 / b))
                }
                _ => return None,
            };
        }
    }

    fn factor(&mut self) -> Option<Value> {
        self.skip_ws();
        let rest = &self.input[self.pos..];
        if rest.len() >= 5 && rest[..5].eq_ignore_ascii_case(b"calc(") {
            self.pos += 5;
            return self.group();
        }
        if self.peek() == Some(b'(') {
            self.pos += 1;
            return self.group();
        }
        self.dimension()
    }

    fn group(&mut self) -> Option<Value> {
        self.depth += 1;
        if self.depth > 32 {
            return None;
        }
        self.skip_ws();
        let value = self.expr()?;
        self.skip_ws();
        if self.peek() != Some(b')') {
            return None;
        }
        self.pos += 1;
        self.depth -= 1;
        Some(value)
    }

    fn dimension(&mut self) -> Option<Value> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let digits_start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'.') {
            self.pos += 1;
        }
        // Exponent (`1e3`) only when followed by digits, so `1em` stays a unit.
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let mut look = self.pos + 1;
            if matches!(self.input.get(look), Some(b'+' | b'-')) {
                look += 1;
            }
            if self.input.get(look).is_some_and(u8::is_ascii_digit) {
                self.pos = look;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        if self.pos == digits_start {
            return None;
        }
        let number: f32 = std::str::from_utf8(&self.input[start..self.pos])
            .ok()?
            .parse()
            .ok()?;
        let unit_start = self.pos;
        if self.peek() == Some(b'%') {
            self.pos += 1;
        } else {
            while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                self.pos += 1;
            }
        }
        let unit = std::str::from_utf8(&self.input[unit_start..self.pos])
            .ok()?
            .to_ascii_lowercase();
        let mut length = CalcLength::default();
        match unit.as_str() {
            "" => return Some(Value::Number(number)),
            "px" => length.px = number,
            "%" => length.percent = number,
            "em" => length.em = number,
            "rem" => length.px = number * 16.0,
            "vw" | "dvw" | "svw" | "lvw" => length.vw = number,
            "vh" | "dvh" | "svh" | "lvh" => length.vh = number,
            "ch" | "ex" => length.em = number * 0.5,
            other => {
                length.px = w3cos_std::style::parse_absolute_length_px(&format!("{number}{other}"))?
            }
        }
        Some(Value::Length(length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(value: &str) -> Option<CalcLength> {
        parse_calc_length(value)
    }

    #[test]
    fn folds_linear_expressions() {
        let c = calc("calc(100% - 32px)").unwrap();
        assert_eq!((c.percent, c.px), (100.0, -32.0));
        let c = calc("calc(2 * (10px + 5px))").unwrap();
        assert_eq!(c.px, 30.0);
        let c = calc("calc(60px*2)").unwrap();
        assert_eq!(c.px, 120.0);
        let c = calc("calc(-1 * 4px)").unwrap();
        assert_eq!(c.px, -4.0);
        let c = calc("calc(1rem + 8px)").unwrap();
        assert_eq!(c.px, 24.0);
        let c = calc("calc(50% / 2 + 1em)").unwrap();
        assert_eq!((c.percent, c.em), (25.0, 1.0));
        let c = calc("calc(calc(10px + 2px) * 2)").unwrap();
        assert_eq!(c.px, 24.0);
        let c = calc("calc(100vh - 2vw)").unwrap();
        assert_eq!((c.vh, c.vw), (100.0, -2.0));
    }

    #[test]
    fn rejects_invalid_expressions() {
        assert!(calc("calc(10px * 2px)").is_none());
        assert!(calc("calc(10px + 2)").is_none());
        assert!(calc("calc(10px / 0)").is_none());
        assert!(calc("calc(10px+2px)").is_none());
        assert!(calc("calc(10px +)").is_none());
        assert!(calc("calc(10px").is_none());
        assert!(calc("calc(10px) 5px").is_none());
        assert!(calc("10px").is_none());
    }

    #[test]
    fn collapses_to_simple_dimensions() {
        let px = |v: &str| calc_to_dimension(calc(v).unwrap());
        assert_eq!(px("calc(60px * 2)"), Dimension::Px(120.0));
        assert_eq!(px("calc(100% / 4)"), Dimension::Percent(25.0));
        assert_eq!(px("calc(2em + 1em)"), Dimension::Em(3.0));
        assert!(matches!(px("calc(100% - 8px)"), Dimension::Calc(_)));
    }
}
