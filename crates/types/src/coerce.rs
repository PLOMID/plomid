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
//! Type resolution, comparison coercion and casting.
//!
//! Everything that answers "what type is this value" or "how do these two
//! operands meet at a common type" lives here, so the comparison and assignment
//! paths cannot drift apart. It sits in the type registry rather than the
//! executor because every modality shares the same coercion rules.

use crate::value_type::value_pg_type;
use crate::ColumnType;
use crate::PgType;
use crate::PgValue as Value;
use plomid_core::{ErrorKind, PlomidError, SqlError, SqlResult};

/// Projects a numeric SQL value to `f64` for numeric comparison and casting.
///
/// This is the single numeric projection in the SQL layer: equality
/// (`row::values_equal`), duplicate bucketing (`row::equality_signature`),
/// ordering (`query::value_cmp`) and numeric casts all read it, so a value
/// cannot compare equal in one path and unequal in another. Text-like values
/// intentionally return `None` — SQL would coerce them first.
pub fn number_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int2(v) => Some(f64::from(*v)),
        Value::Int4(v) => Some(f64::from(*v)),
        Value::Int8(v) => Some(*v as f64),
        Value::Float4(v) => Some(f64::from(*v)),
        Value::Float8(v) => Some(*v),
        Value::Numeric(v) => Some(v.clone().to_f64()),
        _ => None,
    }
}

pub fn value_type(value: &Value) -> Option<ColumnType> {
    value_column_type(value)
}

/// PostgreSQL `format_type` display name for a built-in type.
///
/// `format_type` uses the SQL-facing name (e.g. `character varying`), not the
/// internal `pg_type.typname` (`varchar`).
#[must_use]
pub fn format_type_display_name(ty: crate::PgType) -> &'static str {
    match ty {
        crate::PgType::Int2 => "smallint",
        crate::PgType::Int4 => "integer",
        crate::PgType::Int8 => "bigint",
        crate::PgType::Float4 => "real",
        crate::PgType::Float8 => "double precision",
        crate::PgType::Bool => "boolean",
        crate::PgType::VarChar => "character varying",
        crate::PgType::BpChar => "character",
        crate::PgType::Time => "time without time zone",
        crate::PgType::TimeTz => "time with time zone",
        crate::PgType::Timestamp => "timestamp without time zone",
        crate::PgType::Timestamptz => "timestamp with time zone",
        crate::PgType::VarBit => "bit varying",
        _ => ty.name(),
    }
}

/// PostgreSQL `regtype` textual form for a type OID: display name for plain
/// types, `name[]` for implicit array OIDs, and the numeric OID itself when
/// the type is unknown.
#[must_use]
fn regtype_name(oid: u32) -> String {
    if let Some(element) =
        crate::PgType::all().find(|ty| ty.array_oid() == Some(crate::TypeOid(oid)))
    {
        return format!("{}[]", format_type_display_name(element));
    }
    crate::PgType::by_oid(crate::TypeOid(oid))
        .map(format_type_display_name)
        .map_or_else(|| oid.to_string(), str::to_string)
}

/// Renders a typmod suffix `(p)` / `(p,s)` the way `format_type` does.
///
/// Conventions (matching `crates/types` typmod encoding):
/// * `numeric`: typmod = `(precision << 16) | scale`; a scale equal to
///   [`NUMERIC_MAX_SCALE`] means "precision only".
/// * `varchar`/`char`: typmod = `length + 4`.
/// * `bit`/`varbit`: typmod = `length` directly.
/// * datetime types: typmod = fractional-second precision `0..=6`.
#[must_use]
pub fn format_type_typmod_suffix(ty: crate::PgType, typmod: i32) -> Option<String> {
    if typmod == crate::typmod::NO_TYPEMOD {
        return None;
    }
    match ty {
        crate::PgType::Numeric => {
            let precision = (typmod >> 16) as u16;
            let scale = (typmod & 0xFFFF) as u16;
            if scale == crate::typmod::NUMERIC_MAX_SCALE {
                Some(format!("({precision})"))
            } else {
                Some(format!("({precision},{scale})"))
            }
        }
        crate::PgType::VarChar | crate::PgType::BpChar | crate::PgType::Char => {
            Some(format!("({})", (typmod - 4).max(0)))
        }
        crate::PgType::Bit | crate::PgType::VarBit => Some(format!("({})", typmod.max(0))),
        crate::PgType::Time
        | crate::PgType::TimeTz
        | crate::PgType::Timestamp
        | crate::PgType::Timestamptz
        | crate::PgType::Interval => {
            if typmod < 0 || typmod > i32::from(crate::typmod::MAX_TIME_PRECISION) {
                None
            } else {
                Some(format!("({typmod})"))
            }
        }
        _ => None,
    }
}

/// Applies PostgreSQL-style implicit casts when the two comparison operands
/// carry different types (e.g. `bool = 'false'` casts the text to boolean,
/// `int = '5'` casts the text to integer). When no common type can be found the
/// original pair is returned unchanged and the comparison falls back to the
/// per-value equality/ordering logic.
#[must_use]
pub fn coerce_comparison_operands(left: Value, right: Value) -> (Value, Value) {
    if left.is_null()
        || right.is_null()
        || std::mem::discriminant(&left) == std::mem::discriminant(&right)
    {
        return (left, right);
    }
    if let Some(cast) = coerce_to_common_type(&left, &right) {
        return cast;
    }
    (left, right)
}

fn coerce_to_common_type(left: &Value, right: &Value) -> Option<(Value, Value)> {
    let (text_side, other, swap) = if is_text_value(left) {
        (left, right, false)
    } else if is_text_value(right) {
        (right, left, true)
    } else {
        return None;
    };
    let target = value_builtin_type(other)?;
    let src_ty = PgType::Text.oid();
    let can_cast =
        |ctx: crate::CastContext| crate::builtin_casts().allows(src_ty, target.oid(), ctx);
    // Comparison operators follow PostgreSQL's coercion sequence: try an
    // implicit cast first, then fall back to an assignment cast.
    if !can_cast(crate::CastContext::Implicit) && !can_cast(crate::CastContext::Assignment) {
        return None;
    }
    let coerced =
        crate::apply_cast(text_side.clone(), src_ty, target.oid(), crate::NO_TYPEMOD).ok()?;
    if swap {
        Some((other.clone(), coerced))
    } else {
        Some((coerced, other.clone()))
    }
}

fn is_text_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Text(_) | Value::VarChar(_) | Value::Name(_) | Value::Unknown(_)
    )
}

fn value_builtin_type(value: &Value) -> Option<PgType> {
    match value {
        Value::Bool(_) => Some(PgType::Bool),
        Value::Int2(_) => Some(PgType::Int2),
        Value::Int4(_) => Some(PgType::Int4),
        Value::Int8(_) => Some(PgType::Int8),
        Value::Float4(_) => Some(PgType::Float4),
        Value::Float8(_) => Some(PgType::Float8),
        Value::Numeric(_) => Some(PgType::Numeric),
        Value::Date(_) => Some(PgType::Date),
        Value::Time(_) => Some(PgType::Time),
        Value::TimeTz { .. } => Some(PgType::TimeTz),
        Value::Timestamp(_) => Some(PgType::Timestamp),
        Value::Timestamptz(_) => Some(PgType::Timestamptz),
        Value::Uuid(_) => Some(PgType::Uuid),
        _ => None,
    }
}

pub fn value_column_type(value: &Value) -> Option<ColumnType> {
    if let Value::Array { element_oid, .. } = value {
        let array_oid = crate::PgType::by_oid(*element_oid)?.array_oid()?;
        return Some(ColumnType {
            type_oid: array_oid,
            typmod: -1,
            serial: false,
        });
    }
    value_pg_type(value).map(|ty| ColumnType {
        type_oid: ty.oid(),
        typmod: -1,
        serial: false,
    })
}

/// Casts a runtime value to the PostgreSQL type named `type_name` using the
/// authoritative plomid-types text codecs. Returns NULL for NULL input.
pub fn cast_value(value: &Value, type_name: &str) -> SqlResult<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let mut name = type_name.trim();
    // Accept the abbreviated spellings emitted by older prepared-statement
    // metadata paths. They are not SQL spellings, but normalizing them here
    // prevents a malformed Describe result from turning a valid JSON cast
    // into an unrelated "type does not exist" error.
    if name.eq_ignore_ascii_case("js") {
        name = "json";
    } else if name.eq_ignore_ascii_case("jso") {
        name = "jsonb";
    }
    let typmod = name.find('(').map(|start| {
        let end = name.rfind(')').unwrap_or(name.len());
        let modifier = &name[start..=end.min(name.len().saturating_sub(1))];
        name = name[..start].trim();
        modifier.to_string()
    });
    // Array parameters arrive as PostgreSQL text literals, but the declared
    // type still carries the authoritative `[]` suffix.
    // Do not infer array-ness from a leading `{`: JSON objects use the same
    // character.  The parser/parameter binder preserves the declared `[]`
    // suffix, which is the unambiguous source of this information.
    let is_array = name.ends_with("[]");
    if is_array {
        if name.ends_with("[]") {
            name = name[..name.len() - 2].trim();
        }
    }
    // Handle user-defined types (schema-qualified names like `schema.typename`).
    // These are composite types, enums, or domains created via CREATE TYPE/DOMAIN.
    if name.contains('.') {
        return cast_to_user_defined_type(value, name);
    }
    let Some(ty) = PgType::by_name(name) else {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            format!("type \"{name}\" does not exist"),
        )));
    };
    if ty == PgType::RegType {
        // `oid::regtype` renders the type's display name (e.g. `bigint`), and
        // regtype values are output as text on the wire.
        let oid = match value {
            Value::Oid(o) => Some(*o),
            Value::Reg { oid, .. } => Some(*oid),
            Value::Int2(v) => Some(i32::from(*v).max(0) as u32),
            Value::Int4(v) => Some((*v).max(0) as u32),
            Value::Int8(v) => Some((*v).max(0) as u32),
            Value::Text(t) | Value::VarChar(t) | Value::Name(t) => t.trim().parse::<u32>().ok(),
            _ => None,
        };
        if let Some(oid) = oid {
            return Ok(Value::Text(regtype_name(oid)));
        }
    }
    if ty == PgType::Oid {
        if let Value::Reg { oid, .. } = value {
            return Ok(Value::Oid(*oid));
        }
    }
    let dst_typmod = if let Some(modifier) = typmod {
        let spec = crate::TypmodSpec::parse(&modifier).map_err(|error| {
            SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, error))
        })?;
        crate::encode_typmod(ty, spec).map_err(|error| {
            SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, error))
        })?
    } else {
        crate::NO_TYPEMOD
    };
    // Delegate casts the registry knows to the authoritative CastRegistry singleton so that
    // varchar/char length, numeric p/s and time/timestamp precision semantics are all
    // applied consistently with the type registry.  Unregistered (e.g. array) casts fall
    // through to the text-codec path below.

    if !is_array {
        if let Some(src_ty) = value_pg_type(value) {
            if crate::builtin_casts().allows(src_ty.oid(), ty.oid(), crate::CastContext::Explicit) {
                return crate::apply_cast(value.clone(), src_ty.oid(), ty.oid(), dst_typmod)
                    .map_err(SqlError::Storage);
            }
        }
    }
    let text = value.to_sql_text();
    if is_array {
        let inner = text
            .strip_prefix('{')
            .and_then(|text| text.strip_suffix('}'))
            .ok_or_else(|| {
                SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    format!("invalid input syntax for type {name}[]"),
                ))
            })?;
        let mut elements = Vec::new();
        if !inner.is_empty() {
            for item in inner.split(',') {
                let item = item.trim();
                let item_value = if item.eq_ignore_ascii_case("null") {
                    Value::Null
                } else {
                    cast_value(&Value::Text(item.trim_matches('"').to_string()), name)?
                };
                elements.push(item_value);
            }
        }
        return Ok(Value::Array {
            element_oid: ty.oid(),
            elements,
        });
    }
    match crate::text::parse_value(&text, ty) {
        Ok(parsed) => Ok(parsed),
        Err(e) => {
            // parse_value's builtin codecs already emit the
            // "invalid input syntax for type <name>" prefix (e.g. JSON);
            // re-wrapping it here would duplicate it. Reuse it verbatim.
            let prefix = format!("invalid input syntax for type {name}");
            let detail = if e.starts_with(&prefix) {
                e
            } else {
                format!("{prefix}: {e}")
            };
            Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                detail,
            )))
        }
    }
}

/// Casts a value to a user-defined type (schema-qualified name like
/// `schema.typename`). These are composite types, enums, or domains.
/// Since `util::cast_value` doesn't have catalog access, we use the
/// deterministic OID from `custom_type_oid` to tag the value.
fn cast_to_user_defined_type(value: &Value, type_name: &str) -> SqlResult<Value> {
    let oid = crate::name::custom_type_oid(type_name);
    match value {
        Value::Composite { fields, .. } => Ok(Value::Composite {
            type_oid: crate::TypeOid(oid),
            fields: fields.clone(),
        }),
        Value::Null => Ok(Value::Null),
        other => {
            // For enum labels or domain values, wrap as a single-field composite.
            let bare = type_name.rsplit('.').next().unwrap_or(type_name);
            Ok(Value::Composite {
                type_oid: crate::TypeOid(oid),
                fields: vec![(bare.to_string(), other.clone())],
            })
        }
    }
}
