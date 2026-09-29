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
//! Row encoding/decoding for SQL values stored in the PLOMID StorageEngine.
//!
//! Each table row is encoded as a binary blob and stored under a deterministic
//! key in the B+Tree. The key format is `{table_namespace}:{row_id}`.
//!
//! Storage row identities are internal monotonically allocated integers. User
//! columns, including primary keys, are enforced independently as constraints.

use plomid_core::PlomidError;
use plomid_sql::Value;
use plomid_storage::{Field, Row};
use plomid_types::TypeOid;
use tracing::trace;

// Row-encoding version is defined once in `plomid_core::constants`.
use plomid_core::ROW_VERSION_EXECUTOR as ROW_VERSION;

/// Encodes a row of SQL values into a binary blob.
///
/// Every value is serialized through the authoritative `plomid-types`
/// PostgreSQL type system: each value is stored as `(type_oid, sql_text)`.
pub fn encode_row(values: &[Value]) -> Result<Vec<u8>, PlomidError> {
    trace!(target: "sql::encoding", "encode_row col_count={}", values.len());
    let mut buf = Vec::new();
    buf.push(ROW_VERSION);
    let col_count = values.len().min(u16::MAX as usize) as u16;
    buf.extend_from_slice(&col_count.to_le_bytes());
    for value in values {
        match value {
            Value::Null => buf.push(0),
            other => {
                buf.push(1);
                // Type OID: scalar builtins come from the registry; composite
                // kinds (arrays, enums, ranges) carry their own OID.
                let oid = match other {
                    Value::Array { element_oid, .. } => plomid_types::PgType::by_oid(*element_oid)
                        .and_then(|ty| ty.array_oid())
                        .unwrap_or(*element_oid),
                    Value::Enum {
                        type_oid: element_oid,
                        ..
                    }
                    | Value::Range {
                        type_oid: element_oid,
                        ..
                    }
                    | Value::MultiRange {
                        type_oid: element_oid,
                        ..
                    } => *element_oid,
                    _ => plomid_sql::value_pg_type(other)
                        .ok_or_else(|| {
                            PlomidError::new(
                                plomid_core::ErrorKind::InvalidArgument,
                                "typed value has no PostgreSQL type",
                            )
                        })?
                        .oid(),
                };
                buf.extend_from_slice(&oid.0.to_le_bytes());
                let text = other.to_sql_text();
                let bytes = text.as_bytes();
                buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(bytes);
            }
        }
    }
    trace!(target: "sql::encoding", "encode_row complete bytes={}", buf.len());
    Ok(buf)
}

/// One column of a stored row, resolved without copying or parsing.
///
/// `raw` borrows the column's stored SQL text exactly as [`decode_row`] would
/// parse it, so a caller that does not need the column never allocates for it.
pub enum RowColumn<'a> {
    /// The column is SQL NULL.
    Null,
    /// The column holds the stored text for `oid`.
    Value { oid: TypeOid, raw: &'a [u8] },
}

/// Builds a corruption error with a stable, operator-facing message.
fn corrupt(message: impl Into<String>) -> PlomidError {
    PlomidError::new(plomid_core::ErrorKind::Corruption, message)
}

/// Validates the row header and returns the stored column count.
fn row_header(bytes: &[u8]) -> Result<usize, PlomidError> {
    if bytes.is_empty() {
        return Err(corrupt("empty row bytes"));
    }
    if bytes[0] != ROW_VERSION {
        return Err(corrupt("unknown row encoding version"));
    }
    if bytes.len() < 3 {
        return Err(corrupt("truncated row column count"));
    }
    Ok(u16::from_le_bytes([bytes[1], bytes[2]]) as usize)
}

/// Parses one non-NULL stored column into a typed SQL value.
fn parse_column_value(oid: TypeOid, raw: &[u8]) -> Result<Value, PlomidError> {
    let text = std::str::from_utf8(raw).map_err(|_| corrupt("invalid UTF-8 in row text value"))?;
    plomid_types::text::parse_value_oid(text, oid).map_err(corrupt)
}

/// Streams a stored row's columns in order, without allocating or parsing.
///
/// This is the single framing walker for the row encoding: [`decode_row`],
/// [`decode_row_selected`] and [`validate_row`] are all built on it, so the
/// layout is described in exactly one place and a partial reader cannot drift
/// from the full one. Returns the row's column count.
///
/// `on_column` receives each column's zero-based position; returning `Err`
/// aborts the walk.
pub fn for_each_row_column<'a>(
    bytes: &'a [u8],
    on_column: &mut dyn FnMut(usize, RowColumn<'a>) -> Result<(), PlomidError>,
) -> Result<usize, PlomidError> {
    let col_count = row_header(bytes)?;
    let mut pos = 3;
    for index in 0..col_count {
        if pos >= bytes.len() {
            return Err(corrupt("truncated row value tag"));
        }
        match bytes[pos] {
            0 => {
                pos += 1;
                on_column(index, RowColumn::Null)?;
            }
            1 => {
                pos += 1;
                if pos + 4 > bytes.len() {
                    return Err(corrupt("truncated row type oid"));
                }
                let oid = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
                pos += 4;
                if pos + 4 > bytes.len() {
                    return Err(corrupt("truncated row text length"));
                }
                let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4;
                if pos + len > bytes.len() {
                    return Err(corrupt("truncated row text bytes"));
                }
                on_column(
                    index,
                    RowColumn::Value {
                        oid: TypeOid(oid),
                        raw: &bytes[pos..pos + len],
                    },
                )?;
                pos += len;
            }
            _ => return Err(corrupt("unknown row value tag")),
        }
    }
    Ok(col_count)
}

/// Decodes a binary blob into a row of SQL values.
pub fn decode_row(bytes: &[u8]) -> Result<Vec<Value>, PlomidError> {
    trace!(target: "sql::encoding", "decode_row bytes={}", bytes.len());
    let mut values = Vec::with_capacity(row_header(bytes)?);
    for_each_row_column(bytes, &mut |_, column| {
        values.push(match column {
            RowColumn::Null => Value::Null,
            RowColumn::Value { oid, raw } => parse_column_value(oid, raw)?,
        });
        Ok(())
    })?;
    trace!(target: "sql::encoding", "decode_row complete col_count={}", values.len());
    Ok(values)
}

/// Decodes only the columns marked `true` in `selected`.
///
/// Every unselected position is filled with [`Value::Null`] and its stored text
/// is stepped over without a `String` allocation or a type parse. Callers must
/// therefore never read an unselected column; the masks this is used with come
/// from [`crate::projection`], whose analysis guarantees that (and fails closed
/// to a full decode when it cannot).
///
/// When `selected` does not match the stored row's width the complete row is
/// decoded instead, so a width mismatch can never silently drop a value.
pub fn decode_row_selected(bytes: &[u8], selected: &[bool]) -> Result<Vec<Value>, PlomidError> {
    if row_header(bytes)? != selected.len() {
        return decode_row(bytes);
    }
    let mut values = Vec::with_capacity(selected.len());
    for_each_row_column(bytes, &mut |index, column| {
        if !selected[index] {
            values.push(Value::Null);
            return Ok(());
        }
        values.push(match column {
            RowColumn::Null => Value::Null,
            RowColumn::Value { oid, raw } => parse_column_value(oid, raw)?,
        });
        Ok(())
    })?;
    Ok(values)
}

/// Validates a stored row's framing without decoding or parsing any value.
///
/// Test and diagnostic helper: checks version, value tags and every length
/// prefix so structural corruption is reported exactly as [`decode_row`]
/// would, while the per-column text parse is deferred to a statement that
/// actually reads the column.
#[cfg(test)]
pub fn validate_row(bytes: &[u8]) -> Result<(), PlomidError> {
    for_each_row_column(bytes, &mut |_, _| Ok(()))?;
    Ok(())
}

/// Maps one decoded SQL row into the columnar field sequence.
///
/// This is the bridge between the authoritative SQL value encoding and the
/// columnar materialization input type. It reuses the existing decoder output
/// rather than introducing a second SQL row format: a row is read with
/// [`decode_row`] and mapped field by field.
///
/// Mappings follow the columnar field contract:
///
/// ```text
/// SQL NULL                     → Field::Null
/// smallint / integer / bigint  → Field::Integer
/// bytea                        → Field::Bytes
/// text-like types              → Field::String
/// every other type             → Field::String of its canonical SQL text
/// ```
///
/// Types with no native columnar field (numeric, temporal, uuid, jsonb, ...)
/// are carried as their canonical SQL text, which is lossless for the value and
/// keeps every row present. Coercing them to a different native field would
/// silently change the value, and dropping them would lose data.
#[must_use]
pub fn row_to_fields(values: &[Value]) -> Vec<Field> {
    values.iter().map(value_to_field).collect()
}

/// Maps one SQL value onto its columnar field.
fn value_to_field(value: &Value) -> Field {
    match value {
        Value::Null => Field::Null,
        Value::Int2(number) => Field::Integer(i64::from(*number)),
        Value::Int4(number) => Field::Integer(i64::from(*number)),
        Value::Int8(number) => Field::Integer(*number),
        Value::Bytea(bytes) => Field::Bytes(bytes.clone()),
        Value::BpChar(text)
        | Value::VarChar(text)
        | Value::Text(text)
        | Value::Name(text)
        | Value::Xml(text)
        | Value::Json(text) => Field::String(text.clone()),
        // Types without a native columnar field keep their canonical SQL text
        // so the value survives materialization unchanged.
        other => Field::String(other.to_sql_text()),
    }
}

/// Builds the storage row of a table row for columnar materialization.
#[must_use]
pub fn storage_row(values: &[Value]) -> Row {
    Row::new(row_to_fields(values))
}

/// Builds the storage key for a row in a table.
///
/// Builds the internal storage key for a row.
pub fn row_key(table_name: &str, row_id: i64) -> Vec<u8> {
    format!("{}:{row_id}", table_name).into_bytes()
}

/// Builds the table-scoped key range used for SQL materialization.
///
/// SQL row keys are formatted as `"{table}:{row_id}"`. The table
/// identity is the catalog's authoritative qualified name (e.g.
/// `analytics.events`). The range bounds are byte-identical to the
/// prefixes the SQL executor already uses for INSERT/UPDATE/DELETE/SELECT,
/// so exactly the rows of one logical table are selected — never rows of
/// another table.
///
/// The end bound uses `'\u{10FFFF}'` (the highest Unicode scalar), matching
/// the existing scan bounds in `dml.rs` and `query.rs`.
#[must_use]
pub fn sql_table_key_range(table_name: &str) -> (Vec<u8>, Vec<u8>) {
    let start = format!("{table_name}:").into_bytes();
    let end = format!("{table_name}:\u{10FFFF}").into_bytes();
    (start, end)
}
