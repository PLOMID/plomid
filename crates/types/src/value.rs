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
//! Runtime typed values (`PgValue`) — PLOMID's in-memory value model.
//!
//! Every PostgreSQL builtin type gets a native typed representation. Text
//! (`to_sql_text`/`parse_text`) and binary wire (`to_binary`/`from_binary`)
//! codecs live in [`crate::text`] and [`crate::binary`].

use crate::datetime::{self, Interval};
use crate::numeric::Numeric;
use crate::oid::TypeOid;
use std::net::IpAddr;

/// A lexeme inside a `tsvector` with its positions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TsLexeme {
    /// Normalized lexeme text.
    pub lexeme: String,
    /// Positions (1-based), if tracked.
    pub positions: Vec<u16>,
}

/// A `tsquery` node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TsQueryNode {
    /// A bare lexeme (optionally weighted/prefix).
    Term {
        lexeme: String,
        weights: Vec<u8>,
        prefix: bool,
    },
    /// Logical NOT of a subquery.
    Not(Box<TsQueryNode>),
    /// AND of two subqueries.
    And(Box<TsQueryNode>, Box<TsQueryNode>),
    /// OR of two subqueries.
    Or(Box<TsQueryNode>, Box<TsQueryNode>),
    /// Phrase (distance) operator.
    Phrase(Box<TsQueryNode>, Box<TsQueryNode>, u32),
}

/// Half-open range bounds. Empty ranges are represented by `empty`.
#[derive(Clone, Debug, PartialEq)]
pub struct RangeData {
    /// True for the empty range.
    pub empty: bool,
    /// Lower bound value (None = unbounded).
    pub lower: Option<Box<PgValue>>,
    /// Upper bound value (None = unbounded).
    pub upper: Option<Box<PgValue>>,
    /// Lower bound inclusivity.
    pub lower_inclusive: bool,
    /// Upper bound inclusivity.
    pub upper_inclusive: bool,
}

impl RangeData {
    /// Creates an empty range.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            empty: true,
            lower: None,
            upper: None,
            lower_inclusive: false,
            upper_inclusive: false,
        }
    }
}

/// The full runtime value enum.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum PgValue {
    /// SQL NULL.
    #[default]
    Null,
    /// `boolean`.
    Bool(bool),
    /// `smallint`.
    Int2(i16),
    /// `integer`.
    Int4(i32),
    /// `bigint`.
    Int8(i64),
    /// `numeric`.
    Numeric(Numeric),
    /// `real`.
    Float4(f32),
    /// `double precision`.
    Float8(f64),
    /// `money` (cents).
    Money(i64),
    /// `character` (blank-padded, stored trimmed).
    BpChar(String),
    /// `varchar` / `character varying`.
    VarChar(String),
    /// `text`.
    Text(String),
    /// `name` (63-byte limited identifier).
    Name(String),
    /// `xml`.
    Xml(String),
    /// `bytea`.
    Bytea(Vec<u8>),
    /// `date` — days since 2000-01-01.
    Date(i32),
    /// `time` — microseconds since midnight.
    Time(i64),
    /// `time with time zone` — micros + UTC offset seconds.
    TimeTz { micros: i64, offset_secs: i32 },
    /// `timestamp` — micros since 2000-01-01.
    Timestamp(i64),
    /// `timestamptz` — micros since 2000-01-01 UTC.
    Timestamptz(i64),
    /// `interval`.
    Interval(Interval),
    /// `uuid`.
    Uuid([u8; 16]),
    /// `json` (text form).
    Json(String),
    /// `jsonb` (canonicalized tree).
    Jsonb(Vec<u8>),
    /// `bit` / `bit varying`.
    Bit { len: u32, bytes: Vec<u8> },
    /// `point`.
    Point { x: f64, y: f64 },
    /// `line` — ax + by + c = 0.
    Line { a: f64, b: f64, c: f64 },
    /// `lseg`.
    Lseg { x1: f64, y1: f64, x2: f64, y2: f64 },
    /// `box`.
    Box { x1: f64, y1: f64, x2: f64, y2: f64 },
    /// `path`.
    Path {
        closed: bool,
        points: Vec<(f64, f64)>,
    },
    /// `polygon`.
    Polygon(Vec<(f64, f64)>),
    /// `circle`.
    Circle { x: f64, y: f64, radius: f64 },
    /// `inet` / `cidr`.
    Inet {
        addr: IpAddr,
        prefix: u8,
        cidr: bool,
    },
    /// `macaddr`.
    Macaddr([u8; 6]),
    /// `macaddr8`.
    Macaddr8([u8; 8]),
    /// `tsvector`.
    TsVector(Vec<TsLexeme>),
    /// `tsquery`.
    TsQuery(TsQueryNode),
    /// Any range type.
    Range { type_oid: TypeOid, range: RangeData },
    /// Any multirange type.
    MultiRange {
        type_oid: TypeOid,
        ranges: Vec<RangeData>,
    },
    /// Arrays (one-dimensional, possibly nested via `Array` elements).
    Array {
        element_oid: TypeOid,
        elements: Vec<PgValue>,
    },
    /// Enum values.
    Enum { type_oid: TypeOid, label: String },
    /// Composite row values.
    Composite {
        type_oid: TypeOid,
        fields: Vec<(String, PgValue)>,
    },
    /// `oid`.
    Oid(u32),
    /// `regclass`/`regproc`/`regtype`/... with optional resolved name.
    Reg { oid: u32, name: Option<String> },
    /// `tid` — (block, offset).
    Tid { block: u32, offset: u16 },
    /// `xid`.
    Xid(u32),
    /// `cid`.
    Cid(u32),
    /// `pg_lsn`.
    PgLsn(u64),
    /// `pg_snapshot`.
    PgSnapshot(String),
    /// `aclitem`.
    AclItem(String),
    /// `int2vector`.
    Int2Vector(Vec<i16>),
    /// `oidvector`.
    OidVector(Vec<u32>),
    /// `cstring`.
    Cstring(String),
    /// `unknown` (untyped literal).
    Unknown(String),
}

impl PgValue {
    /// True when this is SQL NULL.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Single-`NULL` element array used by `array_agg` over empty input.
    #[must_use]
    pub fn null_array() -> Self {
        Self::Array {
            element_oid: TypeOid::INT2_ARRAY,
            elements: vec![Self::Null],
        }
    }

    /// Best-effort scalar extraction for sorting/comparison: returns a
    /// comparison key. Arrays/ranges/composites are not comparable here.
    #[must_use]
    pub fn sort_key(&self) -> Option<SortKey<'_>> {
        Some(match self {
            Self::Null => return None,
            Self::Bool(b) => SortKey::Bool(*b),
            Self::Int2(v) => SortKey::Int(i128::from(*v)),
            Self::Int4(v) => SortKey::Int(i128::from(*v)),
            Self::Int8(v) => SortKey::Int(i128::from(*v)),
            Self::Numeric(n) => SortKey::Num(n.clone()),
            Self::Float4(v) => SortKey::Float(F64Key(f64::from(*v))),
            Self::Float8(v) => SortKey::Float(F64Key(*v)),
            Self::Money(v) => SortKey::Int(i128::from(*v)),
            Self::BpChar(s)
            | Self::VarChar(s)
            | Self::Text(s)
            | Self::Name(s)
            | Self::Xml(s)
            | Self::Json(s)
            | Self::Cstring(s)
            | Self::Unknown(s)
            | Self::PgSnapshot(s)
            | Self::AclItem(s) => SortKey::Str(s),
            Self::Bytea(b) | Self::Jsonb(b) => SortKey::Bytes(b),
            Self::Date(d) => SortKey::Int(i128::from(*d)),
            Self::Time(t) | Self::TimeTz { micros: t, .. } => SortKey::Int(i128::from(*t)),
            Self::Timestamp(t) | Self::Timestamptz(t) => SortKey::Int(i128::from(*t)),
            Self::Interval(i) => SortKey::Interval(*i),
            Self::Uuid(u) => SortKey::Bytes(u),
            Self::Enum { type_oid, label } => SortKey::Composite(*type_oid, label),
            Self::Oid(v) | Self::Xid(v) | Self::Cid(v) | Self::Reg { oid: v, .. } => {
                SortKey::Int(i128::from(*v))
            }
            Self::Tid { block, offset } => {
                SortKey::Int((i128::from(*block) << 16) | i128::from(*offset))
            }
            Self::PgLsn(v) => SortKey::Int(i128::from(*v)),
            _ => SortKey::Text(self.to_sql_text()),
        })
    }

    /// Total order matching PostgreSQL's btree semantics per type.
    #[must_use]
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        match (self.sort_key(), other.sort_key()) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(&b),
        }
    }

    /// SQL text representation (what `SELECT` sends back).
    #[must_use]
    pub fn to_sql_text(&self) -> String {
        text::format_value_with_typmod(self, None)
    }

    /// SQL text representation respecting the column's declared temporal
    /// precision (typmod).  For non-temporal types this is identical to
    /// [`to_sql_text`]; for TIME/TIMESTAMP/TIMESTAMPTZ the `precision`
    /// argument controls the number of fractional-second digits emitted.
    pub fn to_sql_text_with_precision(&self, precision: Option<u16>) -> String {
        match self {
            Self::Time(micros) => datetime::format_time(*micros, precision),
            Self::TimeTz {
                micros,
                offset_secs,
            } => {
                let sign = if *offset_secs < 0 { "-" } else { "+" };
                let abs = offset_secs.unsigned_abs();
                format!(
                    "{}{}{:02}:{:02}",
                    datetime::format_time(*micros, precision),
                    sign,
                    abs / 3600,
                    (abs % 3600) / 60
                )
            }
            Self::Timestamp(micros) => datetime::format_timestamp(*micros, precision),
            Self::Timestamptz(micros) => datetime::format_timestamp(*micros, precision),
            _ => self.to_sql_text(),
        }
    }
}

/// An `f64` wrapper with a total order where NaN is greater than +inf.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct F64Key(pub f64);

impl Eq for F64Key {}

impl PartialOrd for F64Key {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for F64Key {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.0.is_nan(), other.0.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => self.0.partial_cmp(&other.0).expect("no NaN"),
        }
    }
}

/// A canonical comparison key for [`PgValue::compare`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortKey<'a> {
    /// Boolean key (false < true).
    Bool(bool),
    /// Integer key.
    Int(i128),
    /// Numeric key.
    Num(Numeric),
    /// Floating key (NaN sorts above every finite value, like PostgreSQL).
    Float(F64Key),
    /// String key.
    Str(&'a str),
    /// Bytes key.
    Bytes(&'a [u8]),
    /// Interval key (months, days, micros lexicographic).
    Interval(Interval),
    /// Enum key: type oid then label order.
    Composite(TypeOid, &'a str),
    /// Fallback: text representation.
    Text(String),
}

/// Formats a boolean the way PostgreSQL does.
#[must_use]
pub fn format_bool(b: bool) -> &'static str {
    if b {
        "t"
    } else {
        "f"
    }
}

/// Wraps a text value in single quotes with PostgreSQL escaping rules.
#[must_use]
pub fn quote_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        match c {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Doubles single quotes inside an array/range element body.
#[must_use]
pub fn quote_element(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    if text.is_empty()
        || text.starts_with('"')
        || text.starts_with('{')
        || text.starts_with('[')
        || text.starts_with('(')
        || text.contains(',')
        || text.contains(' ')
    {
        out.push('"');
        for c in text.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
    } else {
        out.push_str(text);
    }
    out
}

use crate::text;
