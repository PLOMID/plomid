//
// © 2026 PLOMID Technology Solutions
//
// PLOMID
// Platform for Modern Intelligence and Data
//
// Author: Sainath Sapa
// GitHub: https://github.com/sainathsapa
//
// Licensed under the Apache License, Version 2.0;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//! `jsonb` canonical binary representation and JSON parsing/serialization.
//!
//! The binary form is a canonical tree: a type tag byte followed by
//! type-specific payload. Object keys are sorted (shorter-first, then
//! lexicographic) and duplicates removed, matching jsonb semantics.

use std::fmt;

/// A parsed JSON document (jsonb tree).
#[derive(Clone, Debug, PartialEq)]
pub enum JsonbValue {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// Numbers kept as decimal text to preserve precision.
    Number(String),
    /// Strings.
    String(String),
    /// Arrays.
    Array(Vec<JsonbValue>),
    /// Objects with sorted unique keys.
    Object(Vec<(String, JsonbValue)>),
}

impl JsonbValue {
    /// Parses JSON text into a jsonb tree.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut parser = Parser {
            chars: text.chars().collect(),
            pos: 0,
        };
        parser.skip_ws();
        let value = parser.parse_value()?;
        parser.skip_ws();
        if parser.pos != parser.chars.len() {
            return Err("trailing characters after JSON document".into());
        }
        Ok(value)
    }

    /// Encodes to canonical jsonb bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Null => out.push(0),
            Self::Bool(b) => {
                out.push(1);
                out.push(u8::from(*b));
            }
            Self::Number(n) => {
                out.push(2);
                write_len(out, n.len());
                out.extend_from_slice(n.as_bytes());
            }
            Self::String(s) => {
                out.push(3);
                write_len(out, s.len());
                out.extend_from_slice(s.as_bytes());
            }
            Self::Array(items) => {
                out.push(4);
                write_len(out, items.len());
                for item in items {
                    item.encode_into(out);
                }
            }
            Self::Object(pairs) => {
                out.push(5);
                write_len(out, pairs.len());
                for (key, value) in pairs {
                    write_len(out, key.len());
                    out.extend_from_slice(key.as_bytes());
                    value.encode_into(out);
                }
            }
        }
    }

    /// Decodes canonical jsonb bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        Reader { bytes, pos: 0 }.read_value()
    }

    /// Renders the tree as compact JSON text.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write_text(&mut out);
        out
    }

    fn write_text(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(true) => out.push_str("true"),
            Self::Bool(false) => out.push_str("false"),
            Self::Number(n) => out.push_str(n),
            Self::String(s) => write_json_string(s, out),
            Self::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_text(out);
                }
                out.push(']');
            }
            Self::Object(pairs) => {
                out.push('{');
                for (i, (key, value)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(key, out);
                    out.push(':');
                    value.write_text(out);
                }
                out.push('}');
            }
        }
    }

    /// jsonb equality: structural (numbers compared numerically).
    #[must_use]
    pub fn jsonb_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => numeric_eq(a, b),
            _ => self == other,
        }
    }

    /// jsonb containment (`@>`).
    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        match (self, other) {
            (_, Self::Object(pairs)) => pairs.iter().all(|(k, v)| match self {
                Self::Object(mine) => mine
                    .iter()
                    .find(|(mk, _)| mk == k)
                    .is_some_and(|(_, mv)| mv.contains(v)),
                _ => false,
            }),
            (_, Self::Array(items)) => items.iter().all(|item| match self {
                Self::Array(mine) => mine.iter().any(|m| m.contains(item)),
                Self::Object(mine) => mine.iter().any(|(_, mv)| mv.contains(item)),
                other_self => other_self.contains(item),
            }),
            (Self::Array(items), other) => items.iter().any(|m| m.contains(other)),
            (Self::Object(pairs), other) => pairs.iter().any(|(_, mv)| mv.contains(other)),
            (Self::Number(a), Self::Number(b)) => numeric_eq(a, b),
            _ => self == other,
        }
    }
}

fn numeric_eq(a: &str, b: &str) -> bool {
    // Use the existing exact decimal implementation first. Falling back to
    // textual comparison is conservative for values outside its supported
    // mantissa range and never introduces f64 rounding errors.
    match (crate::Numeric::parse(a), crate::Numeric::parse(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => canonical_number(a) == canonical_number(b),
    }
}

/// Canonicalizes arbitrarily large JSON numbers without converting through a
/// binary float. This fallback handles integer/decimal/exponent forms that
/// exceed the bounded SQL numeric mantissa.
fn canonical_number(input: &str) -> String {
    let text = input.trim();
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, 0i32), |(m, e)| (m, e.parse().unwrap_or(0)));
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let first = digits.find(|c| c != '0').unwrap_or(digits.len());
    if first == digits.len() {
        return "0".into();
    }
    let digits = &digits[first..];
    let decimal_position = whole.len() as i32 + exponent - first as i32;
    let rendered = if decimal_position <= 0 {
        format!("0.{}{}", "0".repeat((-decimal_position) as usize), digits)
    } else if decimal_position as usize >= digits.len() {
        format!(
            "{}{}",
            digits,
            "0".repeat(decimal_position as usize - digits.len())
        )
    } else {
        let split = decimal_position as usize;
        format!("{}.{}", &digits[..split], &digits[split..])
    };
    if negative {
        format!("-{rendered}")
    } else {
        rendered
    }
}

fn write_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&(len as u32).to_be_bytes());
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

impl fmt::Display for JsonbValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{c}'"))
        }
    }

    fn parse_value(&mut self) -> Result<JsonbValue, String> {
        self.skip_ws();
        match self.peek() {
            Some('n') => self.parse_lit("null", JsonbValue::Null),
            Some('t') => self.parse_lit("true", JsonbValue::Bool(true)),
            Some('f') => self.parse_lit("false", JsonbValue::Bool(false)),
            Some('"') => self.parse_string().map(JsonbValue::String),
            Some('[') => self.parse_array(),
            Some('{') => self.parse_object(),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            _ => Err("unexpected character in JSON".into()),
        }
    }

    fn parse_lit(&mut self, lit: &str, value: JsonbValue) -> Result<JsonbValue, String> {
        if self.chars[self.pos..]
            .iter()
            .take(lit.len())
            .collect::<String>()
            == lit
        {
            self.pos += lit.len();
            Ok(value)
        } else {
            Err(format!("invalid literal, expected {lit}"))
        }
    }

    fn next_char(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.next_char() {
                Some('"') => return Ok(out),
                Some('\\') => match self.next_char() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let code = self.read_hex4()?;
                        if (0xD800..0xDC00).contains(&code) {
                            self.expect('\\')?;
                            self.expect('u')?;
                            let low = self.read_hex4()?;
                            let combined = 0x1_0000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            out.push(char::from_u32(combined).ok_or("invalid surrogate pair")?);
                        } else {
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                    }
                    _ => return Err("invalid escape".into()),
                },
                Some(c) => out.push(c),
                None => return Err("unterminated string".into()),
            }
        }
    }

    fn read_hex4(&mut self) -> Result<u32, String> {
        let text: String = (0..4)
            .map(|_| self.next_char().ok_or("truncated \\u escape"))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect();
        u32::from_str_radix(&text, 16).map_err(|_| "invalid \\u escape".into())
    }

    fn parse_number(&mut self) -> Result<JsonbValue, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        let digits_start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return Err("invalid number".into());
        }
        if self.peek() == Some('.') {
            self.pos += 1;
            let frac_start = self.pos;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == frac_start {
                return Err("invalid number".into());
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.pos += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.pos += 1;
            }
            let exponent_start = self.pos;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == exponent_start {
                return Err("invalid number exponent".into());
            }
        }
        Ok(JsonbValue::Number(
            self.chars[start..self.pos].iter().collect(),
        ))
    }

    fn parse_array(&mut self) -> Result<JsonbValue, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(JsonbValue::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {
                    self.pos += 1;
                    return Ok(JsonbValue::Array(items));
                }
                _ => return Err("expected ',' or ']' in array".into()),
            }
        }
    }
}

impl Parser {
    fn parse_object(&mut self) -> Result<JsonbValue, String> {
        self.expect('{')?;
        let mut pairs: Vec<(String, JsonbValue)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(JsonbValue::Object(pairs));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let value = self.parse_value()?;
            // Unique keys: last wins, like jsonb.
            if let Some(slot) = pairs.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = value;
            } else {
                pairs.push((key, value));
            }
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {
                    self.pos += 1;
                    pairs.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
                    return Ok(JsonbValue::Object(pairs));
                }
                _ => return Err("expected ',' or '}' in object".into()),
            }
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if self.pos + n > self.bytes.len() {
            return Err("truncated jsonb".into());
        }
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn read_len(&mut self) -> Result<usize, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes(bytes.try_into().expect("4 bytes")) as usize)
    }

    fn read_string(&mut self) -> Result<String, String> {
        let len = self.read_len()?;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| "bad utf8".into())
    }

    fn read_value(&mut self) -> Result<JsonbValue, String> {
        let tag = self.take(1)?[0];
        Ok(match tag {
            0 => JsonbValue::Null,
            1 => JsonbValue::Bool(self.take(1)?[0] != 0),
            2 => JsonbValue::Number(self.read_string()?),
            3 => JsonbValue::String(self.read_string()?),
            4 => {
                let count = self.read_len()?;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.read_value()?);
                }
                JsonbValue::Array(items)
            }
            5 => {
                let count = self.read_len()?;
                let mut pairs = Vec::with_capacity(count);
                for _ in 0..count {
                    let key = self.read_string()?;
                    let value = self.read_value()?;
                    pairs.push((key, value));
                }
                JsonbValue::Object(pairs)
            }
            _ => return Err("invalid jsonb tag".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::JsonbValue;

    #[test]
    fn parse_encode_decode_roundtrip() {
        let text = r#"{"b": [1, 2.5, null, true], "a": "x\"y", "b": 3}"#;
        let tree = JsonbValue::parse(text).unwrap();
        // Duplicate key: last wins. Keys sorted by length then lexicographic.
        assert_eq!(tree.to_text(), r#"{"a":"x\"y","b":3}"#);
        let bytes = tree.encode();
        assert_eq!(JsonbValue::decode(&bytes).unwrap(), tree);
        let a = JsonbValue::parse("1.0").unwrap();
        let b = JsonbValue::parse("1").unwrap();
        assert!(a.jsonb_eq(&b));
    }

    #[test]
    fn containment() {
        let doc = JsonbValue::parse(r#"{"a": 1, "b": [2, 3]}"#).unwrap();
        assert!(doc.contains(&JsonbValue::parse(r#"{"a": 1}"#).unwrap()));
        assert!(!doc.contains(&JsonbValue::parse(r#"{"a": 2}"#).unwrap()));
        assert!(doc.contains(&JsonbValue::parse(r#"[3]"#).unwrap()));
    }

    #[test]
    fn parse_errors() {
        assert!(JsonbValue::parse("{").is_err());
        assert!(JsonbValue::parse("[1,]").is_err());
        assert!(JsonbValue::parse("1 2").is_err());
        assert!(JsonbValue::parse("1e").is_err());
        assert!(JsonbValue::parse("tru").is_err());
    }

    #[test]
    fn number_equality_does_not_round_large_values_through_f64() {
        let one = JsonbValue::parse("123456789012345678901234567890").unwrap();
        let two = JsonbValue::parse("123456789012345678901234567891").unwrap();
        let equivalent = JsonbValue::parse("1.23456789012345678901234567890e29").unwrap();
        assert!(!one.jsonb_eq(&two));
        assert!(one.jsonb_eq(&equivalent));
    }
}
