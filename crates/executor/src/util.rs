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
//! Shared SQL plumbing.
//!
//! Only genuinely cross-cutting helpers remain here: identifier normalization
//! ([`unqualify`]) plus subscripted `UPDATE` assignment, which is the one piece
//! shared by JSON and array access in the DML paths. Value/type logic is in
//! [`crate::coerce`] (`plomid-types`), JSON documents in [`crate::json`]
//! (`plomid-json`), the scalar function registry in [`crate::scalar`], and
//! catalog/session introspection in [`crate::catalog_fn`].

use crate::error::SqlResult;
use crate::json::{json_set_path, json_tree};
use plomid_sql::Expression;
use plomid_sql::Value;
use plomid_types::function::scalar_text;
use plomid_types::JsonbValue;

// JSON / array subscripting lives in the document crate; keep the historic
// `crate::util::array_index_value` path for the executor's expression paths.
pub(crate) use plomid_json::subscript::array_index_value;

pub(crate) fn unqualify(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Given an assignment target from an UPDATE SET clause, returns the base column
/// name (the initial ColumnRef) and the ordered list of subscript index
/// expressions (ArrayIndex / JsonSubscript wrappers), outermost first).
///
/// For example:
/// - `col`           -> `("col", [])
/// - `col['k']`     -> `("col", [Literal("k")])`
/// - `col['a'][0]`    -> `("col", [Literal("a"), Literal(0)])`
pub(crate) fn column_and_subscripts(target: &Expression) -> Option<(&str, Vec<&Expression>)> {
    let mut indices: Vec<&Expression> = Vec::new();
    let mut cur = target;
    loop {
        match cur {
            Expression::ColumnRef(name) => return Some((name.as_str(), indices)),
            Expression::ArrayIndex { array, index }
            | Expression::JsonSubscript { array, index } => {
                indices.insert(0, index.as_ref());
                cur = array.as_ref();
            }
            _ => return None,
        }
    }
}

/// Apply a subscripted UPDATE assignment: given the current column value and the
/// new scalar/new_value and a vector of evaluated subscript keys and the new value.
/// Returns the new top-level column value (to be stored back into the row.
pub(crate) fn apply_subscripted_assignment(
    current_col: &Value,
    indices: &[Value],
    new_value: &Value,
) -> SqlResult<Value> {
    // If no subscripts this the new value.
    if indices.is_empty() {
        return Ok(new_value.clone());
    }
    // Parse current_col into a JsonbValue tree; NULL becomes an empty object
    // so missing keys can be created.
    let mut root = if current_col.is_null() {
        JsonbValue::Object(Vec::new())
    } else {
        json_tree(current_col)?
    };
    let path: Vec<String> = indices
        .iter()
        .map(|v| match v {
            Value::Text(t) => t.clone(),
            Value::VarChar(t) | Value::Unknown(t) | Value::Name(t) | Value::BpChar(t) => t.clone(),
            Value::Int2(n) => format!("{}", *n as i32 - 1),
            Value::Int4(n) => format!("{}", n - 1),
            Value::Int8(n) => format!("{}", n - 1),
            other => scalar_text(other),
        })
        .collect();
    let replacement = if new_value.is_null() {
        JsonbValue::Null
    } else {
        json_tree(new_value)?
    };
    let _ = json_set_path(&mut root, &path, replacement, true);
    Ok(match current_col {
        Value::Json(_) | Value::Null => Value::Json(root.to_text()),
        _ => Value::Jsonb(root.encode()),
    })
}
