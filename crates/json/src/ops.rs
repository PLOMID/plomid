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
//! JSON / JSONB document handling.
//!
//! One implementation per concept: [`JsonbValue`] is the single decoded tree
//! (never `serde_json`), so construction, path mutation, serialization and the
//! `->`/`@>`/`jsonb_set`-style operators all share the same representation.
//! The `sql_*`/`json_*`/`jsonb_*` scalar entry points are dispatched from
//! the executor's scalar-function registry; [`json_tree`] is additionally the
//! bridge used by `JSON_TABLE` and subscripted assignment.

use crate::subscript::array_index_value;
use plomid_core::ErrorKind;
use plomid_core::PlomidError;
use plomid_core::{SqlError, SqlResult};
use plomid_types::coerce::cast_value;
use plomid_types::function::{invalid_arg, ordinal_to_ymd, require_args, scalar_text};
use plomid_types::JsonbValue;
use plomid_types::PgValue as Value;

/// True when `name` is one of the JSON object aggregating functions
/// (`json_object_agg`, `jsonb_object_agg`, or the SQL-standard `json_objectagg`
/// form — the underscore-less spelling PostgreSQL's standard JSON_ARRAYAGG/
/// JSON_OBJECTAGG companion uses).
pub fn is_json_object_aggregate(name: &str) -> bool {
    let lname = name.to_ascii_lowercase();
    lname.ends_with("object_agg") || lname == "json_objectagg" || lname == "jsonb_objectagg"
}

/// True when `name` is one of the JSON aggregating functions.
pub fn is_json_aggregate(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "json_agg"
            | "jsonb_agg"
            | "json_object_agg"
            | "jsonb_object_agg"
            | "json_arrayagg"
            | "json_objectagg"
    )
}

pub fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            other => vec![other],
        })
        .collect()
}

pub fn append_json_value(output: &mut String, value: &Value) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Int2(value) => output.push_str(&value.to_string()),
        Value::Int4(value) => output.push_str(&value.to_string()),
        Value::Int8(value) => output.push_str(&value.to_string()),
        Value::Float4(value) => output.push_str(&value.to_string()),
        Value::Float8(value) => output.push_str(&value.to_string()),
        Value::Numeric(value) => output.push_str(&value.to_string()),
        Value::Json(value) => output.push_str(value),
        Value::Jsonb(value) => output.push_str(&String::from_utf8_lossy(value)),
        other => {
            output.push('"');
            output.push_str(&json_escape(&other.to_sql_text()));
            output.push('"');
        }
    }
}

/// Parses a JSON/JSONB argument into a JsonbValue tree.
pub fn parse_jsonb_arg(value: &Value) -> SqlResult<JsonbValue> {
    match value {
        Value::Jsonb(bytes) => JsonbValue::decode(bytes).map_err(|e| invalid_arg("json", &e)),
        Value::Json(text) => JsonbValue::parse(text).map_err(|e| invalid_arg("json", &e)),
        Value::Null => Err(invalid_arg("json", "argument is NULL")),
        other => {
            let text = other.to_sql_text();
            JsonbValue::parse(&text).map_err(|e| invalid_arg("json", &e))
        }
    }
}

/// Parses optional JSONPath variables argument.
pub fn parse_jsonb_vars(arg: &Option<&Value>) -> SqlResult<Vec<(String, JsonbValue)>> {
    match arg {
        Some(v) if !v.is_null() => match parse_jsonb_arg(v)? {
            JsonbValue::Object(pairs) => Ok(pairs),
            _ => Err(invalid_arg("jsonpath", "variables must be a JSON object")),
        },
        _ => Ok(vec![]),
    }
}

/// Extracts one JSON element from `left` (Json/Jsonb) using `right` as a
/// key or numeric index. `as_text` selects the `->>` behaviour (text output).
pub fn json_arrow_value(left: &Value, right: &Value, as_text: bool) -> SqlResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let root = match left {
        // JSON text must be parsed once for this access. JSONB is already in
        // the canonical tree form; avoid the old decode -> text -> parse
        // round-trip and its two temporary allocations.
        Value::Json(s) => JsonbValue::parse(s)
            .map_err(|e| invalid_arg("json accessor", &format!("invalid json: {e}")))?,
        Value::Jsonb(bytes) => JsonbValue::decode(bytes.as_slice())
            .map_err(|e| invalid_arg("json accessor", &format!("invalid jsonb: {e}")))?,
        other => {
            return Err(invalid_arg(
                "json accessor",
                &format!("cannot extract from a non-JSON value ({other:?})"),
            ))
        }
    };
    let key = right.to_sql_text();
    let found = json_lookup(&root, &key);
    let Some(value) = found else {
        return Ok(Value::Null);
    };
    if as_text {
        let rendered = match value {
            JsonbValue::String(s) => s.clone(),
            other => other.to_text(),
        };
        Ok(Value::Text(rendered))
    } else {
        Ok(Value::Jsonb(value.clone().encode()))
    }
}

/// Converts a SQL value into its JSON tree representation. Shared by the
/// SQL/JSON scalar functions and the `JSON_TABLE` executor.
pub fn json_tree(value: &Value) -> SqlResult<JsonbValue> {
    json_operand(value, "json")
}

/// Converts a SQL value into a JSONB document for the containment operators
/// (`@>`, `<@`). String literals arrive as `Value::Text` (the parser produces
/// `Value::Text` for `'...'`), so a literal holding JSON source text such as
/// `'{"brand":"PLOMID"}'` must be parsed as JSON — mirroring PostgreSQL's
/// unknown-type literal coercion to jsonb. Text that is not valid JSON falls
/// back to a JSON string scalar. All other inputs reuse [`json_tree`].
pub fn json_operand(value: &Value, context: &str) -> SqlResult<JsonbValue> {
    match value {
        Value::Text(text) | Value::VarChar(text) | Value::Unknown(text) => {
            match JsonbValue::parse(text) {
                Ok(parsed) => Ok(parsed),
                Err(_) => Ok(JsonbValue::String(text.clone())),
            }
        }
        other => json_tree_inner(other, context),
    }
}

fn json_tree_inner(value: &Value, context: &str) -> SqlResult<JsonbValue> {
    match value {
        Value::Null => Ok(JsonbValue::Null),
        Value::Bool(value) => Ok(JsonbValue::Bool(*value)),
        Value::Int2(value) => Ok(JsonbValue::Number(value.to_string())),
        Value::Int4(value) => Ok(JsonbValue::Number(value.to_string())),
        Value::Int8(value) => Ok(JsonbValue::Number(value.to_string())),
        Value::Float4(value) => Ok(JsonbValue::Number(value.to_string())),
        Value::Float8(value) => Ok(JsonbValue::Number(value.to_string())),
        Value::Numeric(value) => Ok(JsonbValue::Number(value.to_string())),
        Value::Array { elements, .. } => Ok(JsonbValue::Array(
            elements
                .iter()
                .map(json_tree)
                .collect::<SqlResult<Vec<_>>>()?,
        )),
        Value::Json(text) => JsonbValue::parse(text).map_err(|e| invalid_arg("json", &e)),
        Value::Jsonb(bytes) => JsonbValue::decode(bytes).map_err(|e| invalid_arg("jsonb", &e)),
        // SQL text values become JSON strings (not parsed as JSON). `Text`
        // and `Unknown` literal payloads are handled by `json_operand` before
        // reaching here; keep the remaining text-like variants as strings.
        Value::Text(text)
        | Value::VarChar(text)
        | Value::Unknown(text)
        | Value::Name(text)
        | Value::Cstring(text)
        | Value::BpChar(text) => Ok(JsonbValue::String(text.clone())),
        Value::Date(_)
        | Value::Time(_)
        | Value::Timestamp(_)
        | Value::Timestamptz(_)
        | Value::TimeTz { .. } => Ok(JsonbValue::String(value.to_sql_text())),
        Value::Interval(_) => Ok(JsonbValue::String(value.to_sql_text())),
        Value::Uuid(_) | Value::Bytea(_) | Value::Money(_) | Value::Xml(_) | Value::Bit { .. } => {
            Ok(JsonbValue::String(value.to_sql_text()))
        }
        Value::Composite { fields, .. } => {
            let pairs = fields
                .iter()
                .map(|(name, v)| {
                    let jv = json_tree(v)?;
                    Ok((name.clone(), jv))
                })
                .collect::<SqlResult<Vec<_>>>()?;
            Ok(JsonbValue::Object(pairs))
        }
        Value::Inet { .. }
        | Value::Macaddr(_)
        | Value::Macaddr8(_)
        | Value::Point { .. }
        | Value::Line { .. }
        | Value::Lseg { .. }
        | Value::Box { .. }
        | Value::Path { .. }
        | Value::Polygon(_)
        | Value::Circle { .. }
        | Value::Range { .. }
        | Value::MultiRange { .. } => Ok(JsonbValue::String(value.to_sql_text())),
        // Enum values are their label text; PostgreSQL renders an enum as a
        // JSON string (the label), not as an object or a re-parsed document.
        Value::Enum { label, .. } => Ok(JsonbValue::String(label.clone())),
        _ => {
            let text = value.to_sql_text();
            JsonbValue::parse(&text).map_err(|e| invalid_arg(context, &e))
        }
    }
}

// json_build_object treats ordinary SQL text values as JSON strings.  JSON
// and JSONB values, on the other hand, retain their parsed JSON structure.
pub fn json_build_value(value: &Value) -> SqlResult<JsonbValue> {
    match value {
        Value::Text(text) | Value::VarChar(text) | Value::Unknown(text) => {
            Ok(JsonbValue::String(text.clone()))
        }
        _ => json_tree(value),
    }
}

pub fn json_type_name(value: &JsonbValue) -> &'static str {
    match value {
        JsonbValue::Null => "null",
        JsonbValue::Bool(_) => "boolean",
        JsonbValue::Number(_) => "number",
        JsonbValue::String(_) => "string",
        JsonbValue::Array(_) => "array",
        JsonbValue::Object(_) => "object",
    }
}

pub fn array_or_single_strings(value: &Value) -> SqlResult<Vec<String>> {
    match value {
        Value::Array { elements, .. } => elements
            .iter()
            .map(|v| {
                if v.is_null() {
                    Err(invalid_arg("json path", "path elements cannot be NULL"))
                } else {
                    Ok(v.to_sql_text())
                }
            })
            .collect(),
        other => {
            let text = other.to_sql_text();
            // PostgreSQL accepts paths written with bare {braces} as a
            // composite/record literal; strip the outer braces and split on
            // commas so '{a,b,c}' becomes ["a","b","c"].
            let trimmed = text.trim();
            if trimmed.starts_with('{') && trimmed.ends_with('}') {
                let inner = &trimmed[1..trimmed.len() - 1];
                if inner.is_empty() {
                    Ok(vec![])
                } else {
                    Ok(inner.split(',').map(|s| s.trim().to_string()).collect())
                }
            } else {
                Ok(vec![text])
            }
        }
    }
}

pub fn json_path_function(base: &Value, path: &[Value], as_text: bool) -> SqlResult<Value> {
    if base.is_null() || path.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let path = if path.len() == 1 {
        match &path[0] {
            Value::Array { elements, .. } => elements.as_slice(),
            _ => path,
        }
    } else {
        path
    };
    let mut current = json_tree(base)?;
    for key in path.iter().map(Value::to_sql_text) {
        let Some(next) = json_lookup(&current, &key).cloned() else {
            return Ok(Value::Null);
        };
        current = next;
    }
    if as_text {
        Ok(Value::Text(match current {
            JsonbValue::String(s) => s,
            other => other.to_text(),
        }))
    } else if matches!(base, Value::Json(_)) {
        Ok(Value::Json(current.to_text()))
    } else {
        Ok(Value::Jsonb(current.encode()))
    }
}

pub fn array_dimension_length(elements: &[Value], dimension: usize) -> Option<usize> {
    if dimension == 0 {
        return None;
    }
    if dimension == 1 {
        return Some(elements.len());
    }
    match elements.first() {
        Some(Value::Array { elements, .. }) => array_dimension_length(elements, dimension - 1),
        _ => None,
    }
}

pub fn array_cardinality(elements: &[Value]) -> usize {
    elements
        .iter()
        .map(|value| match value {
            Value::Array { elements, .. } => array_cardinality(elements),
            _ => 1,
        })
        .sum()
}

pub fn json_set_path(
    root: &mut JsonbValue,
    path: &[String],
    replacement: JsonbValue,
    create_missing: bool,
) -> bool {
    if path.is_empty() {
        *root = replacement;
        return true;
    }
    match root {
        JsonbValue::Object(pairs) => {
            if path.len() == 1 {
                if let Some((_, value)) = pairs.iter_mut().find(|(key, _)| key == &path[0]) {
                    *value = replacement;
                } else {
                    if !create_missing {
                        return false;
                    }
                    pairs.push((path[0].clone(), replacement));
                }
                true
            } else if let Some((_, value)) = pairs.iter_mut().find(|(key, _)| key == &path[0]) {
                json_set_path(value, &path[1..], replacement, create_missing)
            } else if create_missing {
                let mut child = JsonbValue::Object(Vec::new());
                let changed = json_set_path(&mut child, &path[1..], replacement, true);
                if changed {
                    pairs.push((path[0].clone(), child));
                }
                changed
            } else {
                false
            }
        }
        JsonbValue::Array(items) => {
            let Ok(index) = path[0].parse::<usize>() else {
                return false;
            };
            if index >= items.len() {
                if !create_missing {
                    return false;
                }
                items.resize(index + 1, JsonbValue::Null);
            }
            if path.len() == 1 {
                items[index] = replacement;
                true
            } else {
                if matches!(items[index], JsonbValue::Null) && create_missing {
                    items[index] = JsonbValue::Object(Vec::new());
                }
                json_set_path(&mut items[index], &path[1..], replacement, create_missing)
            }
        }
        _ if create_missing => {
            *root = JsonbValue::Object(Vec::new());
            json_set_path(root, path, replacement, create_missing)
        }
        _ => false,
    }
}

/// Deletes the key/path segment at `path[0]` from `root`. When path has more
/// segments, recurses into the nested container. Returns true if a deletion
/// occurred.
// `depth` threads through recursion unused today; it is kept (not removed)
// as the hook for a hostile-input depth limit and to keep the public
// signature stable across its call sites.
#[allow(clippy::only_used_in_recursion)]
pub fn json_delete_path(root: &mut JsonbValue, path: &[String], depth: usize) -> SqlResult<bool> {
    if path.is_empty() {
        return Ok(false);
    }
    match root {
        JsonbValue::Object(pairs) => {
            if let Some(slot) = pairs.iter_mut().position(|(key, _)| key == &path[0]) {
                if path.len() == 1 {
                    pairs.remove(slot);
                    // Re-sort keys to keep jsonb canonical order.
                    pairs.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
                    Ok(true)
                } else {
                    json_delete_path(&mut pairs[slot].1, &path[1..], depth + 1)
                }
            } else {
                Ok(false)
            }
        }
        JsonbValue::Array(items) => {
            let Ok(index) = path[0].parse::<usize>() else {
                return Ok(false);
            };
            if index >= items.len() {
                return Ok(false);
            }
            if path.len() == 1 {
                items.remove(index);
                Ok(true)
            } else {
                json_delete_path(&mut items[index], &path[1..], depth + 1)
            }
        }
        _ => Ok(false),
    }
}

/// Implements `jsonb_insert(jsonb, text[], jsonb [, boolean])` with
/// PostgreSQL-compatible path semantics.
pub fn jsonb_insert_path(
    root: &mut JsonbValue,
    path: &[String],
    new_value: JsonbValue,
    insert_after: bool,
) -> SqlResult<()> {
    if path.is_empty() {
        return Ok(());
    }
    match root {
        JsonbValue::Object(pairs) => {
            if path.len() == 1 {
                let key = path[0].clone();
                if let Some(slot) = pairs.iter_mut().position(|(k, _)| *k == key) {
                    pairs[slot].1 = new_value;
                } else {
                    pairs.push((key, new_value));
                    pairs.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
                }
            } else if let Some(slot) = pairs.iter_mut().position(|(k, _)| k == &path[0]) {
                jsonb_insert_path(&mut pairs[slot].1, &path[1..], new_value, insert_after)?;
            }
        }
        JsonbValue::Array(items) => {
            // Array path components are interpreted as integer indexes;
            // object path components remain JSON object keys.
            // PostgreSQL accepts negative indexes (counting from the end)
            // and inserts at the end when the index is beyond the array length.
            let idx_text = &path[0];
            // Try parsing as i64 first to handle negative indexes.
            let resolved_index: isize = match idx_text.parse::<i64>() {
                Ok(i) => i as isize,
                // If parsing as i64 fails, the path element is not a valid
                // integer; reject it with the appropriate error.
                Err(_) => {
                    return Err(invalid_arg(
                        "jsonb_insert",
                        "array index path elements must be integers",
                    ));
                }
            };

            // Negative indexes count from the end (PostgreSQL semantics).
            let adjusted_index = if resolved_index < 0 {
                // PostgreSQL treats -1 as "after the last element" for
                // jsonb_insert, and negative indexes count from the end.
                let len = items.len() as isize;
                if resolved_index < -len {
                    // Index too negative; clamp to 0 (insert at beginning).
                    0
                } else {
                    (len + resolved_index) as usize
                }
            } else {
                resolved_index as usize
            };

            if path.len() == 1 {
                // Insert at the resolved position.
                let pos = if adjusted_index >= items.len() || insert_after {
                    items.len().min(adjusted_index + 1)
                } else {
                    adjusted_index
                };
                items.insert(pos, new_value);
            } else if adjusted_index < items.len() {
                jsonb_insert_path(
                    &mut items[adjusted_index],
                    &path[1..],
                    new_value,
                    insert_after,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// PostgreSQL-compatible `jsonb_pretty` / `json_pretty` formatter.
pub fn json_pretty(value: &JsonbValue, indent: usize) -> String {
    let pad = "    ".repeat(indent);
    let inner = "    ".repeat(indent + 1);
    match value {
        JsonbValue::Object(pairs) => {
            if pairs.is_empty() {
                return "{}".into();
            }
            let mut out = String::from("{\n");
            for (i, (key, val)) in pairs.iter().enumerate() {
                out.push_str(&inner);
                write_json_key(&mut out, key);
                out.push_str(": ");
                if matches!(val, JsonbValue::Object(_) | JsonbValue::Array(_)) {
                    out.push_str(&json_pretty(val, indent + 1));
                } else {
                    out.push_str(&scalar_json_text(val));
                }
                if i + 1 < pairs.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push('}');
            out
        }
        JsonbValue::Array(items) => {
            if items.is_empty() {
                return "[]".into();
            }
            let mut out = String::from("[\n");
            for (i, val) in items.iter().enumerate() {
                out.push_str(&inner);
                if matches!(val, JsonbValue::Object(_) | JsonbValue::Array(_)) {
                    out.push_str(&json_pretty(val, indent + 1));
                } else {
                    out.push_str(&scalar_json_text(val));
                }
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push(']');
            out
        }
        other => other.to_text(),
    }
}

fn scalar_json_text(value: &JsonbValue) -> String {
    match value {
        JsonbValue::String(s) => {
            let mut out = String::new();
            write_json_key(&mut out, s);
            out
        }
        other => other.to_text(),
    }
}

fn write_json_key(out: &mut String, key: &str) {
    out.push('"');
    for ch in key.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
}

pub fn strip_json_nulls(value: JsonbValue) -> JsonbValue {
    match value {
        JsonbValue::Object(pairs) => JsonbValue::Object(
            pairs
                .into_iter()
                .filter_map(|(key, value)| {
                    if matches!(value, JsonbValue::Null) {
                        None
                    } else {
                        Some((key, strip_json_nulls(value)))
                    }
                })
                .collect(),
        ),
        JsonbValue::Array(items) => {
            JsonbValue::Array(items.into_iter().map(strip_json_nulls).collect())
        }
        other => other,
    }
}

/// Implements `jsonb - text` (key deletion), `jsonb - integer` (array index
/// deletion at 0-based index) and `jsonb - text[]` (multiple key deletion).
pub fn jsonb_subtract(left: &Value, right: &Value) -> SqlResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let is_json = matches!(left, Value::Json(_));
    let mut root = json_tree(left)?;
    match right {
        Value::Array { elements, .. } => {
            for element in elements {
                if element.is_null() {
                    continue;
                }
                let key = element.to_sql_text();
                json_delete_path(&mut root, &[key], 0)?;
            }
        }
        Value::Int2(v) => {
            let idx = i64::from(*v) as usize;
            json_delete_array_index(&mut root, idx)?;
        }
        Value::Int4(v) => json_delete_array_index(&mut root, *v as usize)?,
        Value::Int8(v) => json_delete_array_index(&mut root, *v as usize)?,
        _ => {
            let key = right.to_sql_text();
            json_delete_path(&mut root, &[key], 0)?;
        }
    }
    Ok(if is_json {
        Value::Json(root.to_text())
    } else {
        Value::Jsonb(root.encode())
    })
}

/// Deletes the element at the (0-based) array index within a top-level array.
fn json_delete_array_index(root: &mut JsonbValue, index: usize) -> SqlResult<()> {
    let JsonbValue::Array(items) = root else {
        return Ok(());
    };
    if index < items.len() {
        items.remove(index);
    }
    Ok(())
}

/// Finalises a JSON aggregate (`json_agg` / `jsonb_agg`): each accumulated SQL
/// value converts with PostgreSQL JSON semantics, preserving SQL NULL as JSON
/// `null`. Empty input yields NULL like PostgreSQL's json_agg.
pub fn json_aggregate_result(name: &str, values: &[Value]) -> SqlResult<Value> {
    let lname = name.to_ascii_lowercase();
    if lname == "json_agg" || lname == "json_arrayagg" {
        if values.is_empty() {
            return Ok(Value::Null);
        }
        let elements = values
            .iter()
            .map(json_build_value)
            .collect::<SqlResult<Vec<JsonbValue>>>()?;
        Ok(Value::Json(JsonbValue::Array(elements).to_text()))
    } else {
        if values.is_empty() {
            return Ok(Value::Null);
        }
        let elements = values
            .iter()
            .map(json_build_value)
            .collect::<SqlResult<Vec<JsonbValue>>>()?;
        Ok(Value::Jsonb(JsonbValue::Array(elements).encode()))
    }
}

/// Finalises a JSON object aggregate (`json_object_agg` / `jsonb_object_agg`):
/// each pair of accumulated (key, value) values becomes one object member.
pub fn json_object_aggregate_result(name: &str, values: &[(Value, Value)]) -> SqlResult<Value> {
    let lname = name.to_ascii_lowercase();
    // PostgreSQL's json_object_agg/jsonb_object_agg final function returns SQL
    // NULL when the input produced no (key, value) pair — i.e. zero rows were
    // aggregated. The aggregate state only becomes a JSON object once the first
    // transition runs, so an empty pair list must finalize to NULL rather than
    // an empty `{}` object.
    if values.is_empty() {
        return Ok(Value::Null);
    }
    let mut pairs = Vec::with_capacity(values.len());
    for (key, value) in values {
        if key.is_null() {
            return Err(invalid_arg(&lname, "object keys cannot be NULL"));
        }
        pairs.push((scalar_text(key), json_build_value(value)?));
    }
    if lname == "json_object_agg" || lname == "json_objectagg" {
        Ok(Value::Json(JsonbValue::Object(pairs).to_text()))
    } else {
        Ok(Value::Jsonb(JsonbValue::Object(pairs).encode()))
    }
}

/// Implements PostgreSQL's `jsonb || jsonb` concatenation. Two objects merge
/// (right wins on duplicate keys); otherwise each operand is normalised to an
/// array (wrapping scalars/objects) and the arrays are concatenated.
pub fn jsonb_concat(left: &Value, right: &Value) -> SqlResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let is_json = matches!(left, Value::Json(_)) && matches!(right, Value::Json(_));
    let l = json_tree(left)?;
    let r = json_tree(right)?;
    let result = match (l, r) {
        (JsonbValue::Object(lpairs), JsonbValue::Object(rpairs)) => {
            let mut pairs = lpairs.clone();
            for (k, v) in rpairs {
                if let Some(slot) = pairs.iter_mut().position(|(key, _)| *key == k) {
                    pairs[slot].1 = v;
                } else {
                    pairs.push((k, v));
                }
            }
            pairs.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
            JsonbValue::Object(pairs)
        }
        (JsonbValue::Array(li), JsonbValue::Array(ri)) => {
            let mut items = li.clone();
            items.extend(ri.iter().cloned());
            JsonbValue::Array(items)
        }
        (JsonbValue::Array(items), other) => {
            let mut merged = items.clone();
            merged.push(other);
            JsonbValue::Array(merged)
        }
        (other, JsonbValue::Array(items)) => {
            let mut merged = Vec::new();
            merged.push(other);
            merged.extend(items.iter().cloned());
            JsonbValue::Array(merged)
        }
        (l, r) => JsonbValue::Array(vec![l, r]),
    };
    Ok(if is_json {
        Value::Json(result.to_text())
    } else {
        Value::Jsonb(result.encode())
    })
}

/// Resolves a single JSON field or array element lookup.
pub fn json_lookup<'a>(root: &'a JsonbValue, key: &str) -> Option<&'a JsonbValue> {
    match root {
        JsonbValue::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        JsonbValue::Array(items) => key.parse::<usize>().ok().and_then(|i| items.get(i)),
        _ => None,
    }
}

/// Reads `array[index]` with SQL 1-based indexing. NULL index or out-of-range
/// positions (including index 0) produce NULL like PostgreSQL.
pub fn jsonb_array_index(doc: &JsonbValue, idx: i64) -> Option<JsonbValue> {
    match doc {
        JsonbValue::Array(items) => {
            // PostgreSQL uses 0-based indexing for JSON arrays in subscripting.
            if idx < 0 {
                let len = items.len() as i64;
                let i = len + idx;
                if i >= 0 && i < len {
                    items.get(i as usize).cloned()
                } else {
                    None
                }
            } else if idx < items.len() as i64 {
                items.get(idx as usize).cloned()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Converts a scalar SQL value into compact JSON text for JSON_SCALAR(x).
/// NULL maps to the JSON `null` literal, strings are JSON-escaped and quoted,
/// dates/timestamps serialize to ISO-8601 strings (quoted), and UUIDs to their
/// canonical hex form (quoted).
pub fn json_scalar_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Int2(n) => n.to_string(),
        Value::Int4(n) => n.to_string(),
        Value::Int8(n) => n.to_string(),
        Value::Float4(n) => n.to_string(),
        Value::Float8(n) => n.to_string(),
        Value::Numeric(n) => n.to_string(),
        Value::Oid(n) => n.to_string(),
        Value::Json(_) | Value::Jsonb(_) => {
            // JSON/JSONB wrap their textual form; preserve structure exactly.
            value.to_sql_text()
        }
        Value::Text(t)
        | Value::VarChar(t)
        | Value::Name(t)
        | Value::Unknown(t)
        | Value::Cstring(t)
        | Value::BpChar(t)
        | Value::Xml(t) => {
            let mut out = String::with_capacity(t.len() + 2);
            out.push('"');
            out.push_str(&json_escape(t));
            out.push('"');
            out
        }
        Value::Date(days) => {
            let mut out = String::with_capacity(12);
            out.push('"');
            out.push_str(&ordinal_to_ymd(*days));
            out.push('"');
            out
        }
        Value::Time(micros) => {
            let parts = plomid_types::datetime::time_to_parts(*micros);
            let text = if parts.micros != 0 {
                format!(
                    "{:02}:{:02}:{:02}.{:06}",
                    parts.hour, parts.minute, parts.second, parts.micros
                )
            } else {
                format!("{:02}:{:02}:{:02}", parts.hour, parts.minute, parts.second)
            };
            let mut out = String::from("\"");
            out.push_str(&text);
            out.push('"');
            out
        }
        Value::TimeTz {
            micros,
            offset_secs,
        } => {
            let parts = plomid_types::datetime::time_to_parts(*micros);
            let sign = if *offset_secs < 0 { "-" } else { "+" };
            let abs = (*offset_secs as i64).abs();
            let tz_hours = abs / 3600;
            let tz_min = (abs % 3600) / 60;
            let mut text = String::new();
            if parts.micros != 0 {
                text.push_str(&format!(
                    "{:02}:{:02}:{:02}.{:06}",
                    parts.hour, parts.minute, parts.second, parts.micros
                ));
            } else {
                text.push_str(&format!(
                    "{:02}:{:02}:{:02}",
                    parts.hour, parts.minute, parts.second
                ));
            }
            text.push_str(&format!("{sign}{:02}:{:02}", tz_hours, tz_min));
            let mut out = String::from("\"");
            out.push_str(&text);
            out.push('"');
            out
        }
        Value::Timestamp(ts) | Value::Timestamptz(ts) => {
            let parts = plomid_types::datetime::timestamp_to_parts(*ts);
            let (d, t) = parts;
            let text = if t.micros != 0 {
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}",
                    d.year, d.month, d.day, t.hour, t.minute, t.second, t.micros
                )
            } else {
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    d.year, d.month, d.day, t.hour, t.minute, t.second
                )
            };
            let mut out = String::from("\"");
            out.push_str(&text);
            out.push('"');
            out
        }
        Value::Uuid(_) => {
            let mut out = String::from("\"");
            out.push_str(&value.to_sql_text());
            out.push('"');
            out
        }
        Value::Interval(_) => {
            let mut out = String::from("\"");
            out.push_str(&value.to_sql_text());
            out.push('"');
            out
        }
        Value::Composite { fields, .. } => {
            let mut out = String::from("{");
            for (index, (name, field)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(&json_escape(name));
                out.push_str("\":");
                out.push_str(&json_scalar_value(field));
            }
            out.push('}');
            out
        }
        Value::Array { elements, .. } => {
            let mut out = String::from("[");
            for (index, element) in elements.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&json_scalar_value(element));
            }
            out.push(']');
            out
        }
        other => {
            let mut out = String::from("\"");
            out.push_str(&json_escape(&other.to_sql_text()));
            out.push('"');
            out
        }
    }
}

/// Renders the accumulated object pairs as either JSON text or JSONB bytes.
fn finish_json_object(lname: &str, pairs: Vec<(String, JsonbValue)>) -> Value {
    if lname == "jsonb_object" {
        Value::Jsonb(JsonbValue::Object(pairs).encode())
    } else {
        Value::Json(JsonbValue::Object(pairs).to_text())
    }
}

/// Extended json_array honoring NULL ON NULL / ABSENT ON NULL + RETURNING.
pub fn json_array_extended(
    lname: &str,
    args: &[Value],
    returning: Option<String>,
    null_handling: Option<crate::ast::NullHandling>,
) -> SqlResult<Value> {
    let absent = matches!(null_handling, Some(crate::ast::NullHandling::AbsentOnNull));
    let mut elements = Vec::with_capacity(args.len());
    for v in args {
        if v.is_null() && absent {
            continue;
        }
        elements.push(json_build_value(v)?);
    }
    let base = if lname == "jsonb_array" {
        Value::Jsonb(JsonbValue::Array(elements).encode())
    } else {
        Value::Json(JsonbValue::Array(elements).to_text())
    };
    match returning {
        Some(ty) => cast_value(&base, &ty),
        None => Ok(base),
    }
}

/// Extended json_serialize honoring RETURNING <type>.
pub fn json_serialize_extended(
    lname: &str,
    args: &[Value],
    returning: Option<String>,
) -> SqlResult<Value> {
    require_args(lname, args, 1, 1)?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    // JSON literal prefix `JSON '...'` arrives as Value::Json already;
    // serialize it by emitting the raw text — RETURNING target types only
    // control the SQL type tag of the result, never re-quote the payload.
    let text = match &args[0] {
        Value::Json(t) => t.clone(),
        Value::Jsonb(b) => plomid_types::JsonbValue::decode(b)
            .map_err(|e| invalid_arg(lname, &e))?
            .to_text(),
        other => json_tree(other)?.to_text(),
    };
    match returning {
        Some(ty) => {
            // Preserve exact JSON text for TEXT/VARCHAR/CHAR style targets
            // (Postgres JSON_SERIALIZE(... RETURNING TEXT) returns the raw
            // JSON document). Other targets (json/jsonb/...) go through the
            // normal cast machinery. A bare `RETURNING varchar` without a
            // length arrives as `character varying` from the parser, so it
            // must stay TEXT rather than being varchar-cast (which would
            // JSON-quote the payload).
            let norm = ty.trim().trim_end_matches("[]").trim().to_ascii_lowercase();
            let base_norm = norm.split('(').next().unwrap_or(&norm).trim();
            if base_norm.contains("character varying") || base_norm == "varchar" {
                Ok(Value::VarChar(text))
            } else if base_norm == "text" {
                Ok(Value::Text(text))
            } else {
                cast_value(&Value::Text(text), &ty)
            }
        }
        None => Ok(Value::Text(text)),
    }
}

/// Extended json_object honoring SQL/JSON constructor clauses.
pub fn json_object_extended(
    lname: &str,
    args: &[Value],
    returning: Option<String>,
    null_handling: Option<crate::ast::NullHandling>,
    unique_keys: Option<bool>,
) -> SqlResult<Value> {
    let absent = matches!(null_handling, Some(crate::ast::NullHandling::AbsentOnNull));
    let mut raw: Vec<(String, JsonbValue)> = Vec::new();
    let mut push_pair = |key: &Value, val: &Value| -> SqlResult<()> {
        if key.is_null() {
            return Err(invalid_arg(lname, "object keys cannot be NULL"));
        }
        if val.is_null() && absent {
            return Ok(());
        }
        raw.push((scalar_text(key), json_build_value(val)?));
        Ok(())
    };
    if args.len() == 1 {
        if let Value::Array { elements, .. } = &args[0] {
            if elements.len() % 2 != 0 {
                return Err(invalid_arg(
                    lname,
                    "array form requires an even number of elements",
                ));
            }
            for chunk in elements.chunks(2) {
                push_pair(&chunk[0], &chunk[1])?;
            }
            return finish_json_object_extended(lname, raw, returning, unique_keys);
        }
    }
    if args.len() % 2 != 0 {
        return Err(invalid_arg(lname, "requires an even number of arguments"));
    }
    for chunk in args.chunks(2) {
        push_pair(&chunk[0], &chunk[1])?;
    }
    finish_json_object_extended(lname, raw, returning, unique_keys)
}

/// Renders object pairs with UNIQUE enforcement + RETURNING cast.
fn finish_json_object_extended(
    lname: &str,
    pairs: Vec<(String, JsonbValue)>,
    returning: Option<String>,
    unique_keys: Option<bool>,
) -> SqlResult<Value> {
    if matches!(unique_keys, Some(true)) {
        let mut seen = std::collections::HashSet::new();
        for (k, _) in &pairs {
            if !seen.insert(k.clone()) {
                return Err(invalid_arg(lname, &format!("duplicate object key '{}'", k)));
            }
        }
    }
    let mut dedup: Vec<(String, JsonbValue)> = Vec::with_capacity(pairs.len());
    for (k, v) in pairs {
        if let Some(slot) = dedup.iter_mut().find(|(ek, _)| ek == &k) {
            slot.1 = v;
        } else {
            dedup.push((k, v));
        }
    }
    let base = finish_json_object(lname, dedup);
    match returning {
        Some(ty) => cast_value(&base, &ty),
        None => Ok(base),
    }
}

/// Implements `json_value(context, path, ...)`: evaluates a SQL/JSON path
/// expression and returns the first matched value as a SQL scalar. Missing
/// paths return SQL NULL. An optional `RETURNING <type>` argument is honoured
/// by casting the result.
pub fn json_value_function(lname: &str, args: &[Value]) -> SqlResult<Value> {
    if args.is_empty() {
        return Err(invalid_arg(lname, "requires at least a context argument"));
    }
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let doc = json_tree(&args[0])?;
    let path_str = if args.len() >= 2 && !args[1].is_null() {
        scalar_text(&args[1])
    } else {
        String::from("$")
    };
    let path = crate::path::parse(&path_str).map_err(|e| invalid_arg(lname, &e))?;
    let ctx = crate::path::JsonpathContext {
        root: &doc,
        outer: None,
        vars: &Vec::<(String, JsonbValue)>::new(),
        mode: crate::path::JsonpathMode::Lax,
    };
    let results =
        crate::path::evaluate(&path, &doc, &ctx).map_err(|e| invalid_arg(lname, &e.to_string()))?;
    if results.is_empty() {
        return Ok(Value::Null);
    }
    let value = match &results[0] {
        JsonbValue::String(ref s) => Some(Value::Text(s.clone())),
        JsonbValue::Bool(ref b) => Some(Value::Bool(*b)),
        JsonbValue::Number(ref n) => match n.parse::<i64>().ok() {
            Some(i) => Some(Value::Int8(i)),
            None => match n.parse::<f64>().ok() {
                Some(f) => Some(Value::Float8(f)),
                None => Some(Value::Text(n.clone())),
            },
        },
        JsonbValue::Null => None,
        ref other => Some(Value::Json(other.to_text())),
    };
    Ok(value.unwrap_or(Value::Null))
}

/// Implements `json_query(context, path, ...)`: evaluates a SQL/JSON path and
/// returns the JSON result(s). When the path targets an array of elements the
/// result is wrapped into a JSON array by default.
pub fn json_query_function(lname: &str, args: &[Value]) -> SqlResult<Value> {
    if args.is_empty() {
        return Err(invalid_arg(lname, "requires at least a context argument"));
    }
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let doc = json_tree(&args[0])?;
    let path_str = if args.len() >= 2 && !args[1].is_null() {
        scalar_text(&args[1])
    } else {
        String::from("$")
    };
    let with_wrapper = args.iter().skip(2).any(|v| matches!(&v, Value::Bool(true)));
    let path = crate::path::parse(&path_str).map_err(|e| invalid_arg(lname, &e))?;
    let ctx = crate::path::JsonpathContext {
        root: &doc,
        outer: None,
        vars: &Vec::<(String, JsonbValue)>::new(),
        mode: crate::path::JsonpathMode::Lax,
    };
    let results =
        crate::path::evaluate(&path, &doc, &ctx).map_err(|e| invalid_arg(lname, &e.to_string()))?;
    let is_json = matches!(&args[0], Value::Json(_));
    if results.len() > 1 || with_wrapper {
        let array = JsonbValue::Array(results.clone());
        Ok(if is_json {
            Value::Json(array.to_text())
        } else {
            Value::Jsonb(array.encode())
        })
    } else if let Some(value) = results.first() {
        Ok(if is_json {
            Value::Json(value.to_text())
        } else {
            Value::Jsonb(value.encode())
        })
    } else {
        Ok(Value::Null)
    }
}

/// Implements `jsonb_set_lax(target, path, value, create_missing, null_treatment)`.
/// Returns the modified jsonb document. NULL values honour the `null_treatment`
/// argument: `use_json_null` (default) writes JSON null, `delete_key` deletes
/// the key, `return_target` leaves the document unchanged, and `raise_exception`
/// raises an error.
pub fn jsonb_set_lax_function(lname: &str, args: &[Value]) -> SqlResult<Value> {
    require_args(lname, args, 3, 5)?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let mut root = json_tree(&args[0])?;
    let path = array_or_single_strings(&args[1])?;
    let create_missing = args
        .get(3)
        .filter(|v| !v.is_null())
        .map(|v| match v {
            Value::Bool(flag) => Ok(*flag),
            _ => Err(invalid_arg(lname, "create_missing must be boolean")),
        })
        .transpose()?
        .unwrap_or(true);
    let null_treatment = args
        .get(4)
        .filter(|v| !v.is_null())
        .map(scalar_text)
        .unwrap_or_else(|| "use_json_null".to_string());
    if args[2].is_null() {
        match null_treatment.to_ascii_lowercase().as_str() {
            "delete_key" => {
                let _ = json_delete_path(&mut root, &path, 0)?;
                return Ok(Value::Jsonb(root.encode()));
            }
            "return_target" => {
                return Ok(Value::Jsonb(root.encode()));
            }
            "raise_exception" => {
                return Err(invalid_arg(
                    lname,
                    "null_value_treatment cannot be 'raise_exception' when new_value is NULL",
                ));
            }
            _ => {
                // use_json_null (default): fall through and set JSON null.
            }
        }
    }
    let replacement = if args[2].is_null() {
        JsonbValue::Null
    } else {
        json_tree(&args[2])?
    };
    let _ = json_set_path(&mut root, &path, replacement, create_missing);
    Ok(Value::Jsonb(root.encode()))
}

/// Implements `json_populate_record(template, json)`: creates a single composite
/// value whose fields mirror the JSON object members. The `template` argument
/// (typically a typed NULL cast like `NULL::person`) supplies the field names
/// and value types; JSON member values are cast into those types when present.
/// A NULL template yields an anonymous composite keyed by the JSON members.
pub fn json_populate_record_function(lname: &str, args: &[Value]) -> SqlResult<Value> {
    require_args(lname, args, 2, 2)?;
    if args[1].is_null() {
        return Ok(Value::Null);
    }
    let doc = json_tree(&args[1])?;
    let JsonbValue::Object(members) = doc else {
        return Err(invalid_arg(
            lname,
            "json_populate_record expects a JSON object",
        ));
    };
    let (template_fields, record_oid) = match &args[0] {
        Value::Composite { fields, type_oid } => (fields.clone(), *type_oid),
        _ => (Vec::<(String, Value)>::new(), plomid_types::TypeOid::RECORD),
    };
    let mut fields = Vec::<(String, Value)>::new();
    if template_fields.is_empty() {
        for (name, member_value) in members {
            fields.push((name.clone(), convert_jsonb_to_value(&member_value)?));
        }
    } else {
        for (name, template_value) in template_fields {
            let Some(member) = members.iter().find(|(k, _)| *k == name) else {
                continue;
            };
            let value = convert_jsonb_to_value(&member.1)?;
            let cast = if !template_value.is_null() {
                cast_value(&value, template_value.to_sql_text().as_str())
            } else {
                Ok(value)
            };
            fields.push((name.clone(), cast?));
        }
    }
    Ok(Value::Composite {
        type_oid: record_oid,
        fields,
    })
}

/// Implements `json_populate_recordset(template, json_array)`: returns an array
/// of composite values, one per element of the input JSON array. Each element
/// is populated using the same typed field resolution as `json_populate_record`.
pub fn json_populate_recordset_function(lname: &str, args: &[Value]) -> SqlResult<Value> {
    require_args(lname, args, 2, 2)?;
    if args[1].is_null() {
        return Ok(Value::Null);
    }
    let doc = json_tree(&args[1])?;
    let JsonbValue::Array(elements) = doc else {
        return Err(invalid_arg(
            lname,
            "json_populate_recordset expects a JSON array",
        ));
    };
    let record_oid = match &args[0] {
        Value::Composite { type_oid, .. } => *type_oid,
        _ => plomid_types::TypeOid::RECORD,
    };
    let template_fields = match &args[0] {
        Value::Composite { fields, .. } => Some(fields.clone()),
        _ => None,
    };
    let mut composites = Vec::with_capacity(elements.len());
    for element in elements {
        let JsonbValue::Object(members) = element else {
            return Err(invalid_arg(
                lname,
                "recordset elements must be JSON objects",
            ));
        };
        let mut fields = Vec::with_capacity(members.len());
        if let Some(ref template) = template_fields {
            for (name, template_value) in template {
                let Some(member) = members.iter().find(|(k, _)| k == name) else {
                    continue;
                };
                let value = convert_jsonb_to_value(&member.1)?;
                let cast = if !template_value.is_null() {
                    cast_value(&value, template_value.to_sql_text().as_str())
                } else {
                    Ok(value)
                };
                fields.push((name.clone(), cast?));
            }
        } else {
            for (name, member_value) in members {
                fields.push((name.clone(), convert_jsonb_to_value(&member_value)?));
            }
        }
        composites.push(Value::Composite {
            type_oid: record_oid,
            fields,
        });
    }
    Ok(Value::Array {
        element_oid: plomid_types::TypeOid::RECORD,
        elements: composites,
    })
}

/// Converts a parsed JSON value into the closest PLOMID SQL `Value` form.
fn convert_jsonb_to_value(value: &JsonbValue) -> SqlResult<Value> {
    match value {
        JsonbValue::Null => Ok(Value::Null),
        JsonbValue::Bool(b) => Ok(Value::Bool(*b)),
        JsonbValue::Number(n) => match n.parse::<i64>().ok() {
            Some(i) => Ok(Value::Int8(i)),
            None => match n.parse::<f64>().ok() {
                Some(f) => Ok(Value::Float8(f)),
                None => Ok(Value::Text(n.clone())),
            },
        },
        JsonbValue::String(s) => Ok(Value::Text(s.clone())),
        JsonbValue::Array(items) => {
            let elements = items
                .iter()
                .map(convert_jsonb_to_value)
                .collect::<SqlResult<Vec<Value>>>()?;
            Ok(Value::Array {
                element_oid: plomid_types::TypeOid::JSONB,
                elements,
            })
        }
        JsonbValue::Object(_) => Ok(Value::Json(value.to_text())),
    }
}

/// JSON subscripting used by the parser's `jsonb['key']` / `jsonb[0]` syntax.
/// Text subscripts on objects behave like `->` (return JSONB), integer subscripts
/// behave like array index access on JSON arrays.
pub fn json_subscript_value(array: &Value, index: &Value) -> SqlResult<Value> {
    if array.is_null() || index.is_null() {
        return Ok(Value::Null);
    }
    match array {
        Value::Json(_) | Value::Jsonb(_) => array_index_value(array, index),
        other => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("cannot subscript non-JSON value: {other:?}"),
        ))),
    }
}
