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
//! `JSON_TABLE` and JSON record expansion.
//!
//! Turns a JSON document into relational rows: the `json_to_record` /
//! `json_to_recordset` families, and the SQL/JSON `JSON_TABLE` table function
//! (row paths, `NESTED` paths, `FOR ORDINALITY`, `EXISTS`, and
//! `DEFAULT ... ON EMPTY / ON ERROR`).
//!
//! Row production from a document is a document-modality concern, so it lives
//! here rather than in the executor. The one thing it cannot own is *expression
//! evaluation*: `JSON_TABLE`'s context item and path arguments are expressions
//! in the host's language. [`ExpressionEval`] inverts that dependency, so this
//! crate never depends on the parser or the executor.
//!
//! JSON text serialization is *not* reimplemented here: values are rendered
//! through [`JsonbValue::to_text`] and keys through [`crate::ops`].

use plomid_core::{ErrorKind, PlomidError, SqlError, SqlResult};
use plomid_types::{ColumnType, JsonbValue, PgValue as Value};

use crate::ast::{JsonTableColumn, JsonTableColumnDefault, JsonTableColumnKind};
use crate::convert::{cast_json_value, jsonb_value_to_sql};

/// Evaluates one expression of the host's expression language.
///
/// The host (for PLOMID, the SQL executor) implements this over its own
/// expression type and evaluator, which is what lets `JSON_TABLE` live in the
/// document crate without that crate depending on the SQL AST.
pub trait ExpressionEval {
    /// The host's expression representation.
    type Expr;

    /// Evaluates `expr` against the host's current row and session scope.
    fn eval(&mut self, expr: &Self::Expr) -> SqlResult<Value>;
}

/// The document modality has no notion of an unsupported SQL construct; the
/// host's expression evaluator reports those, so this only covers the
/// document-level failures raised here.
fn unsupported(message: impl Into<String>) -> SqlError {
    SqlError::Storage(PlomidError::new(ErrorKind::Unsupported, message))
}

/// Expands a JSON object into a single record row for `json_to_record` /
/// `jsonb_to_record`.
pub fn json_record_rows(
    json_value: &Value,
    col_types: &[Option<ColumnType>],
) -> SqlResult<Vec<Vec<Value>>> {
    let tree = match json_value {
        Value::Json(text) => JsonbValue::parse(text).map_err(|e| {
            SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("invalid JSON: {e}"),
            ))
        })?,
        Value::Jsonb(bytes) => JsonbValue::decode(bytes).map_err(|e| {
            SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("invalid JSONB: {e}"),
            ))
        })?,
        Value::Null => return Ok(Vec::new()),
        other => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("expected JSON argument, got {other:?}"),
            )))
        }
    };
    let JsonbValue::Object(pairs) = &tree else {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            "json_to_record requires a JSON object",
        )));
    };
    let map: std::collections::HashMap<&str, &JsonbValue> =
        pairs.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let mut row = Vec::with_capacity(col_types.len());
    for (i, col_type) in col_types.iter().enumerate() {
        let col_name = pairs.get(i).map(|(k, _)| k.as_str()).unwrap_or("");
        let json_val = map.get(col_name).copied().unwrap_or(&JsonbValue::Null);
        let val = jsonb_value_to_sql(json_val);
        let casted = if let Some(ct) = col_type {
            cast_json_value(&val, ct)?
        } else {
            val
        };
        row.push(casted);
    }
    Ok(vec![row])
}

/// Expands a JSON array of objects into record rows for `json_to_recordset`.
pub fn json_recordset_rows(
    json_value: &Value,
    col_types: &[Option<ColumnType>],
) -> SqlResult<Vec<Vec<Value>>> {
    let tree = match json_value {
        Value::Json(text) => JsonbValue::parse(text).map_err(|e| {
            SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("invalid JSON: {e}"),
            ))
        })?,
        Value::Jsonb(bytes) => JsonbValue::decode(bytes).map_err(|e| {
            SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("invalid JSONB: {e}"),
            ))
        })?,
        Value::Null => return Ok(Vec::new()),
        other => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("expected JSON argument, got {other:?}"),
            )))
        }
    };
    let JsonbValue::Array(elements) = &tree else {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            "json_to_recordset requires a JSON array",
        )));
    };
    let mut rows = Vec::with_capacity(elements.len());
    for element in elements {
        let JsonbValue::Object(pairs) = element else {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "json_to_recordset requires an array of JSON objects",
            )));
        };
        let map: std::collections::HashMap<&str, &JsonbValue> =
            pairs.iter().map(|(k, v)| (k.as_str(), v)).collect();
        let mut row = Vec::with_capacity(col_types.len());
        for (i, col_type) in col_types.iter().enumerate() {
            let col_name = pairs.get(i).map(|(k, _)| k.as_str()).unwrap_or("");
            let json_val = map.get(col_name).copied().unwrap_or(&JsonbValue::Null);
            let val = jsonb_value_to_sql(json_val);
            let casted = if let Some(ct) = col_type {
                cast_json_value(&val, ct)?
            } else {
                val
            };
            row.push(casted);
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Builds the column-name / column-type list for a `JSON_TABLE` `COLUMNS` spec.
/// Flattens nested `NESTED` columns into the top-level column list, matching
/// PostgreSQL's behavior of producing a flat row type from nested paths.
pub fn json_table_schema(json_columns: &[JsonTableColumn]) -> (Vec<String>, Vec<ColumnType>) {
    let mut names: Vec<String> = Vec::new();
    let mut types: Vec<ColumnType> = Vec::new();
    for col in json_columns {
        collect_json_table_columns(col, &mut names, &mut types);
    }
    (names, types)
}

/// Recursively collects column names and types from a JSON_TABLE column spec,
/// flattening NESTED columns.
fn collect_json_table_columns(
    col: &JsonTableColumn,
    names: &mut Vec<String>,
    types: &mut Vec<ColumnType>,
) {
    match &col.kind {
        JsonTableColumnKind::Nested { columns } => {
            for nested in columns {
                collect_json_table_columns(nested, names, types);
            }
        }
        JsonTableColumnKind::Ordinality => {
            if let Some(name) = &col.name {
                names.push(name.clone());
                types.push(ColumnType::bigint());
            }
        }
        JsonTableColumnKind::Regular | JsonTableColumnKind::Exists => {
            if let Some(name) = &col.name {
                names.push(name.clone());
                let ty = col
                    .type_name
                    .as_ref()
                    .and_then(|t| ColumnType::from_type_name(t))
                    .unwrap_or_else(ColumnType::text);
                types.push(ty);
            }
        }
    }
}

/// Evaluates a `JSON_TABLE(...)` table function call, producing rows by
/// applying the row path against the JSON document and extracting each
/// declared column from every result element.
///
/// The context item and path arguments are host expressions, so they are
/// evaluated through [`ExpressionEval`] rather than by reaching into the host's
/// engine.
pub fn json_table_rows<V: ExpressionEval>(
    evaluator: &mut V,
    args: &[V::Expr],
    json_columns: &[JsonTableColumn],
) -> SqlResult<Vec<Vec<Value>>> {
    if args.is_empty() {
        return Err(unsupported("json_table requires a JSON document argument"));
    }
    let json_value = evaluator.eval(&args[0])?;
    if json_value.is_null() {
        return Ok(Vec::new());
    }
    // The context item is implicitly cast to jsonb (PostgreSQL semantics), so
    // a text/document literal is parsed as JSON rather than treated as a
    // scalar JSON string. Value::Json/Jsonb are already JSON documents.
    let doc = match &json_value {
        Value::Json(text) => JsonbValue::parse(text)
            .map_err(|e| unsupported(format!("invalid JSON_TABLE context item: {e}")))?,
        Value::Jsonb(bytes) => JsonbValue::decode(bytes)
            .map_err(|e| unsupported(format!("invalid JSON_TABLE context item: {e}")))?,
        other => JsonbValue::parse(&other.to_sql_text())
            .map_err(|e| unsupported(format!("invalid JSON_TABLE context item: {e}")))?,
    };

    let path_str = match args.get(1) {
        Some(path_expr) => {
            let value = evaluator.eval(path_expr)?;
            if value.is_null() {
                String::from("$")
            } else {
                value.to_sql_text()
            }
        }
        None => String::from("$"),
    };
    let path = crate::path::parse(&path_str)
        .map_err(|e| unsupported(format!("invalid JSON_TABLE path: {e}")))?;
    let ctx = crate::path::JsonpathContext {
        root: &doc,
        outer: None,
        vars: &Vec::<(String, JsonbValue)>::new(),
        mode: crate::path::JsonpathMode::Lax,
    };

    // Expand the row path into the set of JSON row elements.
    let row_elements = crate::path::evaluate(&path, &doc, &ctx)
        .map_err(|e| unsupported(format!("JSON_TABLE path evaluation: {e}")))?;

    if json_columns.is_empty() {
        // No COLUMNS clause: one row per element in a single JSON column.
        return Ok(row_elements
            .iter()
            .map(jsonb_value_to_sql)
            .map(|v| vec![v])
            .collect());
    }

    let mut rows = Vec::new();
    for (index, element) in row_elements.iter().enumerate() {
        rows.extend(flatten_json_table_row(
            element,
            json_columns,
            index as i64 + 1,
            &ctx,
        )?);
    }
    Ok(rows)
}

/// Flattens one JSON row element against a JSON_TABLE column list into output
/// rows. `ordinal` is the 1-based counter of this element at its level, used
/// by FOR ORDINALITY columns. NESTED columns multiply the row set (PostgreSQL
/// lateral semantics); a nested path with no matches yields no rows, which is
/// PostgreSQL's default INNER behavior for NESTED PATH.
fn flatten_json_table_row(
    element: &JsonbValue,
    columns: &[JsonTableColumn],
    ordinal: i64,
    parent_ctx: &crate::path::JsonpathContext<'_>,
) -> SqlResult<Vec<Vec<Value>>> {
    // Column and NESTED paths are relative to the current row element: within
    // a column path, `$` denotes the element itself (PostgreSQL semantics),
    // so the path context is rooted at the element rather than the document.
    let ctx = crate::path::JsonpathContext {
        root: element,
        outer: None,
        vars: parent_ctx.vars,
        mode: parent_ctx.mode,
    };
    let mut partial: Vec<Vec<Value>> = vec![Vec::new()];
    for col in columns {
        match &col.kind {
            JsonTableColumnKind::Ordinality => {
                for row in &mut partial {
                    row.push(Value::Int8(ordinal));
                }
            }
            JsonTableColumnKind::Nested { columns: nested } => {
                let nested_path = col
                    .path
                    .as_deref()
                    .ok_or_else(|| unsupported("NESTED column requires a PATH"))?;
                let path = crate::path::parse(nested_path)
                    .map_err(|e| unsupported(format!("invalid JSON_TABLE path: {e}")))?;
                let elements = crate::path::evaluate(&path, element, &ctx)
                    .map_err(|e| unsupported(format!("JSON_TABLE path evaluation: {e}")))?;
                let mut next = Vec::new();
                for (nested_index, nested_element) in elements.iter().enumerate() {
                    for nested_row in flatten_json_table_row(
                        nested_element,
                        nested,
                        nested_index as i64 + 1,
                        &ctx,
                    )? {
                        for row in &partial {
                            let mut combined = row.clone();
                            combined.extend(nested_row.iter().cloned());
                            next.push(combined);
                        }
                    }
                }
                partial = next;
            }
            JsonTableColumnKind::Exists => {
                let value = match &col.path {
                    Some(col_path) => {
                        let path = crate::path::parse(col_path)
                            .map_err(|e| unsupported(format!("invalid JSON_TABLE path: {e}")))?;
                        let results = crate::path::evaluate(&path, element, &ctx)
                            .map_err(|e| unsupported(format!("JSON_TABLE path evaluation: {e}")))?;
                        Value::Bool(!results.is_empty())
                    }
                    None => Value::Bool(false),
                };
                for row in &mut partial {
                    row.push(value.clone());
                }
            }
            JsonTableColumnKind::Regular => {
                let value = json_table_column_value(col, element, &ctx)?;
                for row in &mut partial {
                    row.push(value.clone());
                }
            }
        }
    }
    Ok(partial)
}

/// Evaluates one regular JSON_TABLE column: applies the column path to the
/// row element, applies DEFAULT ... ON EMPTY / ON ERROR when declared, and
/// casts the result to the declared SQL type.
fn json_table_column_value(
    col: &JsonTableColumn,
    element: &JsonbValue,
    ctx: &crate::path::JsonpathContext<'_>,
) -> SqlResult<Value> {
    let Some(col_path) = &col.path else {
        return Ok(Value::Null);
    };
    let path = crate::path::parse(col_path)
        .map_err(|e| unsupported(format!("invalid JSON_TABLE path: {e}")))?;
    let results = crate::path::evaluate(&path, element, ctx)
        .map_err(|e| unsupported(format!("JSON_TABLE path evaluation: {e}")))?;
    let col_type = col
        .type_name
        .as_deref()
        .and_then(ColumnType::from_type_name);
    if results.is_empty() {
        return match col.default_mode {
            JsonTableColumnDefault::Empty | JsonTableColumnDefault::EmptyOrError => {
                json_table_apply_default(&col.default_value, col_type.as_ref())
            }
            JsonTableColumnDefault::Error => Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("JSON_TABLE path {col_path} not found and ON ERROR requested"),
            ))),
            JsonTableColumnDefault::None => Ok(Value::Null),
        };
    }
    let Some(col_type) = &col_type else {
        return Ok(jsonb_value_to_sql(&results[0]));
    };
    // Declared JSON / JSONB columns (including `... FORMAT JSON`) keep the
    // extracted value's JSON representation: arrays and objects must
    // round-trip as JSON documents (PostgreSQL FORMAT JSON semantics) rather
    // than being converted through the SQL array / text value layer.
    if matches!(
        col_type.type_oid,
        plomid_types::TypeOid::JSON | plomid_types::TypeOid::JSONB
    ) {
        return Ok(if col_type.type_oid == plomid_types::TypeOid::JSONB {
            Value::Jsonb(results[0].encode())
        } else {
            Value::Json(results[0].to_text())
        });
    }
    // A value exists but fails conversion to the declared type: this is a
    // conversion error (PostgreSQL `ON ERROR` target), distinct from a
    // missing path (the `ON EMPTY` target above).
    match cast_json_value(&jsonb_value_to_sql(&results[0]), col_type) {
        Ok(value) => Ok(value),
        Err(cast_error) => {
            let applies_error_default = match (&col.error_default_mode, &col.error_default_value) {
                (JsonTableColumnDefault::Error | JsonTableColumnDefault::EmptyOrError, Some(_)) => {
                    true
                }
                _ => matches!(
                    col.default_mode,
                    JsonTableColumnDefault::Error | JsonTableColumnDefault::EmptyOrError
                ),
            };
            if !applies_error_default {
                return Err(cast_error);
            }
            let default_value = match (&col.error_default_mode, &col.error_default_value) {
                (JsonTableColumnDefault::Error | JsonTableColumnDefault::EmptyOrError, Some(_)) => {
                    &col.error_default_value
                }
                _ => &col.default_value,
            };
            json_table_apply_default(default_value, Some(col_type))
        }
    }
}

/// Applies a declared JSON_TABLE DEFAULT value, casting it to the declared
/// column type (PostgreSQL casts the DEFAULT literal to the column type).
fn json_table_apply_default(
    default_value: &Option<String>,
    col_type: Option<&ColumnType>,
) -> SqlResult<Value> {
    match default_value {
        Some(default_text) => {
            let value = json_value_from_jsonb(default_text)?;
            match col_type {
                Some(col_type) => cast_json_value(&value, col_type),
                None => Ok(value),
            }
        }
        None => Ok(Value::Null),
    }
}

/// Parses a JSON text literal (a DEFAULT value) into a SQL value.
fn json_value_from_jsonb(text: &str) -> SqlResult<Value> {
    match JsonbValue::parse(text) {
        Ok(tree) => Ok(jsonb_value_to_sql(&tree)),
        Err(e) => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("invalid JSON default value: {e}"),
        ))),
    }
}
