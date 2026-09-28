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
//! Text input/output for [`PgValue`] — PostgreSQL text format.
//!
//! [`format_value`] renders a value the way PostgreSQL returns it to clients;
//! [`parse_value`] parses a text literal according to a target builtin type.

use crate::datetime::Interval;
use crate::datetime::{
    format_date, format_time, format_timestamp, parts_to_date, parts_to_time, parts_to_timestamp,
    TimeParts,
};
use crate::numeric::Numeric;
use crate::value::{quote_element, PgValue, RangeData, TsLexeme, TsQueryNode};
use crate::PgType;

/// Renders a value in PostgreSQL text output format, respecting the column's
/// declared temporal precision (typmod).  For non-temporal types this is
/// identical to [`format_value`]; for TIME/TIMESTAMP/TIMESTAMPTZ the
/// `precision` argument controls the number of fractional-second digits
/// emitted.
pub fn format_value_with_typmod(value: &PgValue, precision: Option<u16>) -> String {
    if value.is_null() {
        return "NULL".into();
    }
    match value {
        PgValue::Null => "NULL".into(),
        PgValue::Bool(b) => if *b { "t" } else { "f" }.to_string(),
        PgValue::Int2(v) => v.to_string(),
        PgValue::Int4(v) => v.to_string(),
        PgValue::Int8(v) => v.to_string(),
        PgValue::Numeric(n) => n.to_string(),
        PgValue::Float4(v) => fmt_f64(f64::from(*v)),
        PgValue::Float8(v) => fmt_f64(*v),
        PgValue::Money(cents) => format_money(*cents),
        PgValue::BpChar(s)
        | PgValue::VarChar(s)
        | PgValue::Text(s)
        | PgValue::Name(s)
        | PgValue::Xml(s)
        | PgValue::Json(s)
        | PgValue::Cstring(s)
        | PgValue::Unknown(s) => s.clone(),
        PgValue::Bytea(bytes) => format!("\\x{}", hex_encode(bytes)),
        PgValue::Date(days) => format_date(*days),
        PgValue::Time(t) => format_time(*t, precision),
        PgValue::TimeTz {
            micros,
            offset_secs,
        } => format_time_tz(*micros, *offset_secs, precision),
        PgValue::Timestamp(t) => format_timestamp(*t, precision),
        PgValue::Timestamptz(t) => format_timestamp(*t, precision),
        PgValue::Interval(i) => i.to_string(),
        PgValue::Uuid(bytes) => format_uuid(bytes),
        PgValue::Jsonb(bytes) => jsonb_to_text(bytes),
        PgValue::Bit { len, bytes } => format_bit(*len, bytes),
        PgValue::Point { x, y } => format!("({},{})", fmt_f64(*x), fmt_f64(*y)),
        PgValue::Line { a, b, c } => format!("{{{},{},{}}}", fmt_f64(*a), fmt_f64(*b), fmt_f64(*c)),
        PgValue::Lseg { x1, y1, x2, y2 } => {
            format!(
                "({},{}),({},{})",
                fmt_f64(*x1),
                fmt_f64(*y1),
                fmt_f64(*x2),
                fmt_f64(*y2)
            )
        }
        PgValue::Box { x1, y1, x2, y2 } => {
            format!(
                "({},{})",
                format_point(x1.max(*x2), y1.max(*y2)),
                format_point(x1.min(*x2), y1.min(*y2))
            )
        }
        PgValue::Path { closed, points } => format_path(*closed, points),
        PgValue::Polygon(points) => format!("({})", format_point_list(points)),
        PgValue::Circle { x, y, radius } => {
            format!("<({},{})>", format_point(*x, *y), fmt_f64(*radius))
        }
        PgValue::Inet { addr, prefix, .. } => format_inet(addr, *prefix),
        PgValue::Macaddr(m) => format_mac(m),
        PgValue::Macaddr8(m) => format_mac8(m),
        PgValue::TsVector(lexemes) => format_tsvector(lexemes),
        PgValue::TsQuery(node) => format_tsquery(node),
        PgValue::Range { range, .. } => format_range(range),
        PgValue::MultiRange { ranges, .. } => {
            format!(
                "{{{}}}",
                ranges
                    .iter()
                    .map(format_range)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        PgValue::Array { elements, .. } => format_array(elements),
        PgValue::Enum { label, .. } => label.clone(),
        PgValue::Composite { fields, .. } => {
            let body = fields
                .iter()
                .map(|(_, v)| {
                    if v.is_null() {
                        String::new()
                    } else {
                        quote_element(&format_value_with_typmod(v, precision))
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("({body})")
        }
        PgValue::Oid(v) | PgValue::Xid(v) | PgValue::Cid(v) => v.to_string(),
        PgValue::Reg { oid, name } => name.clone().unwrap_or_else(|| oid.to_string()),
        PgValue::Tid { block, offset } => format!("({block},{offset})"),
        PgValue::PgLsn(v) => format!("{:X}/{:X}", v >> 32, v & 0xFFFF_FFFF),
        PgValue::PgSnapshot(s) | PgValue::AclItem(s) => s.clone(),
        PgValue::Int2Vector(v) => {
            format_array(&v.iter().map(|x| PgValue::Int2(*x)).collect::<Vec<_>>())
        }
        PgValue::OidVector(v) => {
            format_array(&v.iter().map(|x| PgValue::Oid(*x)).collect::<Vec<_>>())
        }
    }
}

/// Formats a float the way PostgreSQL does.
#[must_use]
pub fn fmt_f64(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else {
        format!("{v}")
    }
}

fn format_point(x: f64, y: f64) -> String {
    format!("({},{})", fmt_f64(x), fmt_f64(y))
}

fn format_money(cents: i64) -> String {
    let negative = cents < 0;
    let digits = cents.unsigned_abs().to_string();
    let (int_part, frac) = if digits.len() > 2 {
        digits.split_at(digits.len() - 2)
    } else {
        ("0", digits.as_str())
    };
    let sign = if negative { "-" } else { "" };
    format!("{sign}${int_part}.{:0>2}", frac)
}

/// Hex-encodes bytes.
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hex-decodes a string (no `\x` prefix).
pub fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.len() % 2 != 0 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid hexadecimal data".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    let hex = hex_encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn format_time_tz(micros: i64, offset_secs: i32, precision: Option<u16>) -> String {
    let sign = if offset_secs < 0 { "-" } else { "+" };
    let abs = offset_secs.unsigned_abs();
    format!(
        "{}{}{:02}:{:02}",
        format_time(micros, precision),
        sign,
        abs / 3600,
        (abs % 3600) / 60
    )
}

fn format_bit(len: u32, bytes: &[u8]) -> String {
    (0..len)
        .map(|i| {
            let byte = bytes[i as usize / 8];
            if byte & (0x80 >> (i % 8)) != 0 {
                '1'
            } else {
                '0'
            }
        })
        .collect()
}

fn format_path(closed: bool, points: &[(f64, f64)]) -> String {
    let (open, close) = if closed { ('(', ')') } else { ('[', ']') };
    format!("{open}{}{close}", format_point_list(points))
}

fn format_point_list(points: &[(f64, f64)]) -> String {
    points
        .iter()
        .map(|(x, y)| format_point(*x, *y))
        .collect::<Vec<_>>()
        .join(",")
}

fn format_inet(addr: &std::net::IpAddr, prefix: u8) -> String {
    let max = match addr {
        std::net::IpAddr::V4(_) => 32,
        std::net::IpAddr::V6(_) => 128,
    };
    if prefix == max {
        addr.to_string()
    } else {
        format!("{addr}/{prefix}")
    }
}

fn format_mac(m: &[u8]) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn format_mac8(m: &[u8]) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn format_tsvector(lexemes: &[TsLexeme]) -> String {
    lexemes
        .iter()
        .map(|l| {
            let body = format!("'{}'", l.lexeme.replace('\'', "''"));
            if l.positions.is_empty() {
                body
            } else {
                let positions = l
                    .positions
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{body}:{positions}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_tsquery(node: &TsQueryNode) -> String {
    match node {
        TsQueryNode::Term { lexeme, .. } => format!("'{}'", lexeme.replace('\'', "''")),
        TsQueryNode::Not(inner) => format!("!{}", format_tsquery(inner)),
        TsQueryNode::And(a, b) => format!("({} & {})", format_tsquery(a), format_tsquery(b)),
        TsQueryNode::Or(a, b) => format!("({} | {})", format_tsquery(a), format_tsquery(b)),
        TsQueryNode::Phrase(a, b, dist) => {
            format!("({} <{}> {})", format_tsquery(a), dist, format_tsquery(b))
        }
    }
}

/// Formats a range: `[1,3)`, `empty`, `(,]` etc.
#[must_use]
pub fn format_range(range: &RangeData) -> String {
    if range.empty {
        return "empty".into();
    }
    let open = if range.lower_inclusive { '[' } else { '(' };
    let close = if range.upper_inclusive { ']' } else { ')' };
    let lower = range
        .lower
        .as_ref()
        .map(|v| quote_element(&format_value_with_typmod(v, None)))
        .unwrap_or_default();
    let upper = range
        .upper
        .as_ref()
        .map(|v| quote_element(&format_value_with_typmod(v, None)))
        .unwrap_or_default();
    format!("{open}{lower},{upper}{close}")
}

/// Formats a 1-D array: `{1,2,3}` with NULL as `NULL`.
#[must_use]
pub fn format_array(elements: &[PgValue]) -> String {
    let body = elements
        .iter()
        .map(|v| match v {
            PgValue::Null => "NULL".to_string(),
            PgValue::Array { elements, .. } => format_array(elements),
            other => quote_element(&format_value_with_typmod(other, None)),
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// Converts jsonb canonical bytes back to text (formatting is preserved by
/// keeping the raw text alongside the binary form in most deployments; here we
/// pretty-print the canonical tree).
#[must_use]
pub fn jsonb_to_text(bytes: &[u8]) -> String {
    match crate::jsonb::JsonbValue::decode(bytes) {
        Ok(v) => v.to_text(),
        Err(_) => String::new(),
    }
}

/// Parses `YYYY-MM-DD` into days-since-2000-01-01.
pub fn parse_date(text: &str) -> Result<i32, String> {
    let text = text.trim();
    let bad = || "invalid input syntax for type date".to_string();
    if text.is_empty() {
        return Err(bad());
    }
    let (year, rest) = text.split_once('-').ok_or_else(bad)?;
    let (month, day) = rest.split_once('-').ok_or_else(bad)?;
    if year.is_empty() || month.is_empty() || day.is_empty() {
        return Err(bad());
    }
    let year_i32: i32 = year.parse().map_err(|_| bad())?;
    let month_u8: u8 = month.parse().map_err(|_| bad())?;
    let day_u8: u8 = day.parse().map_err(|_| bad())?;
    parts_to_date(crate::datetime::DateParts {
        year: year_i32,
        month: month_u8,
        day: day_u8,
    })
    .ok_or_else(bad)
}

/// Parses `HH:MM[:SS[.ffffff]]` into microseconds-since-midnight.
pub fn parse_time(text: &str) -> Result<i64, String> {
    let text = text.trim();
    let bad = || "invalid input syntax for type time".to_string();
    if text.is_empty() {
        return Err(bad());
    }
    let (hour, rest) = text.split_once(':').ok_or_else(bad)?;
    let (minute, second) = match rest.split_once(':') {
        Some((m, s)) => (m, s),
        None => (rest, "0"),
    };
    if hour.is_empty() || minute.is_empty() || second.is_empty() {
        return Err(bad());
    }
    let (sec_text, frac_text) = match second.split_once('.') {
        Some((s, f)) => (s, f),
        None => (second, ""),
    };
    let mut micros: u32 = 0;
    if !frac_text.is_empty() {
        if !frac_text.chars().all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
        let mut padded = frac_text.to_string();
        padded.truncate(6);
        while padded.len() < 6 {
            padded.push('0');
        }
        micros = padded.parse().map_err(|_| bad())?;
    }
    if !sec_text.is_empty() && !sec_text.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    parts_to_time(TimeParts {
        hour: hour.parse().map_err(|_| bad())?,
        minute: minute.parse().map_err(|_| bad())?,
        second: sec_text.parse().map_err(|_| bad())?,
        micros,
    })
    .ok_or_else(bad)
}

/// Parses a timestamp: `YYYY-MM-DD[ T]HH:MM[:SS[.ffffff]]` (time optional — date-only parses as midnight).
pub fn parse_timestamp(text: &str) -> Result<i64, String> {
    let text = text.trim();
    let bad = || "invalid input syntax for type timestamp".to_string();
    if text.is_empty() {
        return Err(bad());
    }
    let (date_part, time_text) = match text.find(['T', 't', ' ']) {
        Some(split) => {
            let (dp, tp) = text.split_at(split);
            if dp.is_empty() {
                return Err(bad());
            }
            let after_sep = tp.get(1..).unwrap_or("");
            let stripped = after_sep.trim_end_matches(['Z', 'z']);
            let tt = stripped.split(['+', '-']).next().ok_or_else(bad)?;
            (dp, tt)
        }
        None => (text, ""),
    };
    let days = parse_date(date_part).map_err(|_| bad())?;
    let time_micros = if time_text.is_empty() {
        0
    } else {
        parse_time(time_text).map_err(|_| bad())?
    };
    Ok(parts_to_timestamp(days, time_micros))
}

/// Parses a PostgreSQL interval like `1 year 2 mons 3 days 04:05:06.5`.
pub fn parse_interval(text: &str) -> Result<Interval, String> {
    let bad = || "invalid input syntax for type interval".to_string();
    let mut interval = Interval::default();
    let mut words = text.split_whitespace().peekable();
    while let Some(word) = words.next() {
        let lower = word.to_ascii_lowercase();
        match lower.as_str() {
            "year" | "years" => {}
            "mon" | "mons" | "month" | "months" => {}
            "day" | "days" => {}
            "hour" | "hours" | "minute" | "minutes" | "min" | "mins" | "second" | "seconds"
            | "sec" | "secs" => {}
            _ => {
                // Bare time-of-day component like `04:05:06.5`.
                if word.contains(':') {
                    interval.micros += parse_time(word).map_err(|_| bad())?;
                    continue;
                }
                let value: f64 = word.parse().map_err(|_| bad())?;
                let unit = words.next().ok_or_else(bad)?.to_ascii_lowercase();
                match unit.as_str() {
                    "year" | "years" => interval.months += (value * 12.0).round() as i32,
                    "mon" | "mons" | "month" | "months" => {
                        interval.months += value.round() as i32;
                    }
                    "day" | "days" => interval.days += value.round() as i32,
                    "hour" | "hours" => interval.micros += (value * 3_600_000_000.0) as i64,
                    "minute" | "minutes" | "min" | "mins" => {
                        interval.micros += (value * 60_000_000.0) as i64;
                    }
                    "second" | "seconds" | "sec" | "secs" => {
                        interval.micros += (value * 1_000_000.0) as i64;
                    }
                    _ => return Err(bad()),
                }
            }
        }
    }
    // A bare time-of-day component like `04:05:06` appears as one token pair
    // handled above only when preceded by a number; handle a leading time too.
    Ok(interval)
}

/// Parses a text literal into a [`PgValue`] for any PostgreSQL type OID,
/// including array types (`int4[]`), enum labels and range types. This is the
/// authoritative entry point used by storage decoding and assignment casts.
///
/// # Errors
/// Returns a PostgreSQL-style error message when the literal is invalid.
pub fn parse_value_oid(text: &str, oid: crate::TypeOid) -> Result<PgValue, String> {
    // Scalar builtin?
    if let Some(ty) = crate::PgType::by_oid(oid) {
        return parse_value(text, ty);
    }
    // Array type: find the element builtin and parse `{a,b,c}`.
    if let Some(element) = crate::PgType::all().find(|ty| ty.array_oid() == Some(oid)) {
        let inner = text.trim();
        if !(inner.starts_with('{') && inner.ends_with('}')) {
            return Err("malformed array literal: missing braces".into());
        }
        let body = &inner[1..inner.len() - 1];
        let mut elements = Vec::new();
        if !body.is_empty() {
            for part in split_array_body(body) {
                let part = part.trim();
                if part.eq_ignore_ascii_case("NULL") {
                    elements.push(PgValue::Null);
                    continue;
                }
                let unquoted = part
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(part);
                elements.push(parse_value(unquoted, element)?);
            }
        }
        return Ok(PgValue::Array {
            element_oid: element.oid(),
            elements,
        });
    }
    // Enum: the text form is just the label; the label list lives in the
    // user-defined type registry, so accept the label verbatim here.
    Ok(PgValue::Enum {
        type_oid: oid,
        label: text.to_string(),
    })
}

/// Splits an array literal body on top-level commas, honouring double-quoted
/// elements.
fn split_array_body(body: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ',' if !in_quotes => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    parts.push(current);
    parts
}

/// Parses a text literal into a [`PgValue`] for the given builtin type.
///
/// # Errors
/// Returns a PostgreSQL-style error message when the literal is invalid.
pub fn parse_value(text: &str, ty: PgType) -> Result<PgValue, String> {
    let bad = || format!("invalid input syntax for type {}", ty.name());
    if ty.is_pseudo() && ty != PgType::Unknown {
        return Err(format!("cannot accept a value of type {}", ty.name()));
    }
    Ok(match ty {
        PgType::Unknown => PgValue::Unknown(text.to_string()),
        PgType::Bool => PgValue::Bool(parse_bool(text)?),
        PgType::Int2 => PgValue::Int2(text.trim().parse().map_err(|_| bad())?),
        PgType::Int4 => PgValue::Int4(text.trim().parse().map_err(|_| bad())?),
        PgType::Int8 => PgValue::Int8(text.trim().parse().map_err(|_| bad())?),
        PgType::Oid | PgType::Xid | PgType::Cid => {
            PgValue::Oid(text.trim().parse::<u32>().map_err(|_| bad())?)
        }
        PgType::Numeric => PgValue::Numeric(Numeric::parse(text).map_err(|_| bad())?),
        PgType::Float4 => PgValue::Float4(parse_f64(text).map_err(|_| bad())? as f32),
        PgType::Float8 => PgValue::Float8(parse_f64(text).map_err(|_| bad())?),
        PgType::Money => PgValue::Money(parse_money(text).ok_or_else(bad)?),
        PgType::Char => {
            let text = text.trim();
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => PgValue::BpChar(c.to_string()),
                (None, None) => PgValue::BpChar(String::new()),
                _ => return Err("value too long for type character(1)".into()),
            }
        }
        PgType::BpChar | PgType::VarChar => PgValue::VarChar(text.to_string()),
        PgType::Text | PgType::Name | PgType::Xml => PgValue::Text(text.to_string()),
        PgType::Cstring => PgValue::Cstring(text.to_string()),
        PgType::Bytea => {
            let text = text.trim();
            let hex = text.strip_prefix("\\x").ok_or_else(bad)?;
            PgValue::Bytea(hex_decode(hex).map_err(|_| bad())?)
        }
        PgType::Date => PgValue::Date(parse_date(text).map_err(|_| bad())?),
        PgType::Time => PgValue::Time(parse_time(text).map_err(|_| bad())?),
        PgType::Timestamp => PgValue::Timestamp(parse_timestamp(text).map_err(|_| bad())?),
        PgType::Timestamptz => PgValue::Timestamptz(parse_timestamp(text).map_err(|_| bad())?),
        PgType::Interval => PgValue::Interval(parse_interval(text).map_err(|_| bad())?),
        PgType::Uuid => PgValue::Uuid(parse_uuid(text).ok_or_else(bad)?),
        PgType::Json => {
            // Validate JSON syntax before accepting
            crate::jsonb::JsonbValue::parse(text).map_err(|_| bad())?;
            PgValue::Json(text.to_string())
        }
        PgType::Jsonb => {
            let tree = crate::jsonb::JsonbValue::parse(text).map_err(|_| bad())?;
            PgValue::Jsonb(tree.encode())
        }
        PgType::Bit | PgType::VarBit => {
            let text = text.trim();
            if !text.bytes().all(|b| b == b'0' || b == b'1') {
                return Err(bad());
            }
            let len = text.len() as u32;
            let mut bytes = vec![0u8; text.len().div_ceil(8)];
            for (i, c) in text.bytes().enumerate() {
                if c == b'1' {
                    bytes[i / 8] |= 0x80 >> (i % 8);
                }
            }
            PgValue::Bit { len, bytes }
        }
        _ty @ (PgType::RegProc
        | PgType::RegProcedure
        | PgType::RegOper
        | PgType::RegOperator
        | PgType::RegClass
        | PgType::RegType
        | PgType::RegConfig
        | PgType::RegDictionary
        | PgType::RegRole
        | PgType::RegNamespace) => {
            // reg* input accepts either a numeric OID or an object name.
            // Plomid has no global function/namespace OID registry, so a
            // name is kept verbatim (with oid 0) and rendered back as its
            // text; catalog comparisons against Oid(0) placeholders stay
            // consistent and client probes like
            // `typinput='pg_catalog.array_in'::regproc` simply resolve to
            // false instead of erroring.
            let trimmed = text.trim();
            match trimmed.parse::<u32>() {
                Ok(oid) => PgValue::Reg { oid, name: None },
                Err(_) => PgValue::Reg {
                    oid: 0,
                    name: Some(trimmed.to_string()),
                },
            }
        }
        PgType::Inet | PgType::Cidr => {
            // PostgreSQL accepts "addr" or "addr/prefix" for inet/cidr input.
            // The existing `inet_function` in the executor already implements this
            // parsing; here we reproduce the same logic at the text-codec layer so
            // that `'127.0.0.1'::inet` and `'192.168.1.0/24'::cidr` both work
            // through the normal type-resolution path rather than only via the
            // function-call form.
            let text = text.trim();
            let (addr_text, explicit_prefix) = match text.split_once('/') {
                Some((a, p)) => (a, p.trim().parse::<u8>().ok()),
                None => (text, None),
            };
            let addr: std::net::IpAddr = addr_text
                .trim()
                .parse()
                .map_err(|_| format!("invalid input syntax for type {}", ty.name()))?;
            let max_prefix = match addr {
                std::net::IpAddr::V4(_) => 32u8,
                std::net::IpAddr::V6(_) => 128u8,
            };
            let prefix = match explicit_prefix {
                Some(p) if p > max_prefix => {
                    return Err(format!(
                        "invalid input syntax for type {}: invalid netmask",
                        ty.name()
                    ));
                }
                Some(p) => p,
                // No explicit `/prefix`: PostgreSQL defaults inet to a host
                // address (full-width mask for the address family).
                None => max_prefix,
            };
            PgValue::Inet {
                addr,
                prefix,
                cidr: ty == PgType::Cidr,
            }
        }
        _ => {
            return Err(format!(
                "text input for type {} is not yet supported",
                ty.name()
            ))
        }
    })
}

/// Parses PostgreSQL boolean literals.
pub fn parse_bool(text: &str) -> Result<bool, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "t" | "true" | "yes" | "on" | "1" => Ok(true),
        "f" | "false" | "no" | "off" | "0" => Ok(false),
        _ => Err("invalid input syntax for type boolean".into()),
    }
}

/// Parses float text including `NaN`/`Infinity` spellings.
pub fn parse_f64(text: &str) -> Result<f64, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "nan" => Ok(f64::NAN),
        "infinity" | "inf" | "+infinity" | "+inf" => Ok(f64::INFINITY),
        "-infinity" | "-inf" => Ok(f64::NEG_INFINITY),
        other => other.parse::<f64>().map_err(|e| e.to_string()),
    }
}

fn parse_money(text: &str) -> Option<i64> {
    let text = text.trim();
    let negative = text.starts_with('-');
    let digits = text
        .trim_start_matches(['-', '+'])
        .trim_start_matches('$')
        .replace(',', "");
    let (int_part, frac) = match digits.split_once('.') {
        Some((i, f)) => (i.to_string(), format!("{f:0<2}")),
        None => (digits.clone(), "00".to_string()),
    };
    let cents: i64 = format!("{int_part}{}", &frac[..2]).parse().ok()?;
    Some(if negative { -cents } else { cents })
}

/// Parses a UUID (with or without hyphens) into 16 bytes.
pub fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    let hex: String = text.trim().to_ascii_lowercase();
    let hex = hex.replace('-', "");
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{format_value_with_typmod, parse_value};
    use crate::value::PgValue;
    use crate::PgType;

    #[test]
    fn text_roundtrip_scalars() {
        let cases = [
            (PgType::Int4, "42", "42"),
            (PgType::Int8, "-9000000000000000001", "-9000000000000000001"),
            (PgType::Numeric, "3.140", "3.14"),
            (PgType::Bool, "yes", "t"),
            (PgType::Bool, "off", "f"),
            (PgType::Date, "2024-02-29", "2024-02-29"),
            (PgType::Time, "13:30", "13:30:00"),
            (
                PgType::Timestamp,
                "2000-01-01 00:00:00",
                "2000-01-01 00:00:00",
            ),
            (PgType::Interval, "1 year 2 mons", "1 year 2 mons"),
            (PgType::Interval, "04:05:06", "04:05:06"),
            (PgType::Money, "$12.05", "$12.05"),
            (PgType::Text, "hello", "hello"),
            (
                PgType::Uuid,
                "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
                "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
            ),
        ];
        for (ty, input, expected) in cases {
            let value = parse_value(input, ty).expect(input);
            assert_eq!(
                format_value_with_typmod(&value, None),
                expected,
                "{ty:?} from {input}"
            );
        }
    }

    #[test]
    fn invalid_literals_error() {
        assert!(parse_value("nope", PgType::Int4).is_err());
        assert!(parse_value("2023-02-29", PgType::Date).is_err());
        assert!(parse_value("25:00:00", PgType::Time).is_err());
        assert!(parse_value("xx", PgType::Uuid).is_err());
        assert!(parse_value("maybe", PgType::Bool).is_err());
        assert!(parse_value("x", PgType::Trigger).is_err());
    }

    #[test]
    fn special_floats_and_bytea() {
        let nan = parse_value("NaN", PgType::Float8).unwrap();
        assert_eq!(format_value_with_typmod(&nan, None), "NaN");
        let inf = parse_value("-Infinity", PgType::Float8).unwrap();
        assert_eq!(format_value_with_typmod(&inf, None), "-Infinity");
        let bytes = parse_value("\\xdeadbeef", PgType::Bytea).unwrap();
        assert_eq!(format_value_with_typmod(&bytes, None), "\\xdeadbeef");
    }

    #[test]
    fn null_and_unknown() {
        assert_eq!(format_value_with_typmod(&PgValue::Null, None), "NULL");
        let unknown = parse_value("abc", PgType::Unknown).unwrap();
        assert_eq!(format_value_with_typmod(&unknown, None), "abc");
    }

    #[test]
    fn inet_text_input() {
        // IPv4 without prefix — PostgreSQL renders as "127.0.0.1" (prefix=32
        // suppressed when it equals the max for the address family).
        let v = parse_value("127.0.0.1", PgType::Inet).expect("parse inet");
        assert_eq!(format_value_with_typmod(&v, None), "127.0.0.1");
        // IPv4 with /24 prefix.
        let v = parse_value("192.168.1.0/24", PgType::Inet).expect("parse inet/24");
        assert_eq!(format_value_with_typmod(&v, None), "192.168.1.0/24");
        // IPv6 without prefix — rendered as "::1" (prefix=128 suppressed).
        let v = parse_value("::1", PgType::Inet).expect("parse ipv6");
        assert_eq!(format_value_with_typmod(&v, None), "::1");
        // CIDR type: prefix is preserved by construction and rendered the same way.
        let v = parse_value("10.0.0.0/8", PgType::Cidr).expect("parse cidr");
        assert_eq!(format_value_with_typmod(&v, None), "10.0.0.0/8");
    }

    #[test]
    fn inet_invalid_input() {
        assert!(parse_value("not-an-ip", PgType::Inet).is_err());
        // Prefix exceeding the address family width is rejected.
        assert!(parse_value("127.0.0.1/99", PgType::Inet).is_err());
    }

    #[test]
    fn inet_roundtrip_matches_format() {
        let cases = [
            ("127.0.0.1", "127.0.0.1"),
            ("192.168.1.0/24", "192.168.1.0/24"),
            ("::1", "::1"),
            ("2001:db8::/32", "2001:db8::/32"),
        ];
        for (input, expected) in cases {
            let v = parse_value(input, PgType::Inet).expect(input);
            assert_eq!(
                format_value_with_typmod(&v, None),
                expected,
                "inet roundtrip {input}"
            );
        }
    }
}
