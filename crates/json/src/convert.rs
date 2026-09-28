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
//! Conversions between the decoded JSON tree and SQL values.
//!
//! Used by `JSON_TABLE` / `json_to_record` expansion, where a document subtree
//! has to become a typed SQL value. Objects round-trip as JSON text; arrays
//! keep an explicit element type so a JSON array is not silently retyped as a
//! SQL text array.

use plomid_core::{ErrorKind, PlomidError};
use plomid_core::{SqlError, SqlResult};
use plomid_types::jsonb::JsonbValue;
use plomid_types::{ColumnType, PgValue as Value};

/// Converts a `JsonbValue` to the corresponding SQL `Value`.
pub(crate) fn jsonb_value_to_sql(value: &JsonbValue) -> Value {
    match value {
        JsonbValue::Null => Value::Null,
        JsonbValue::Bool(b) => Value::Bool(*b),
        JsonbValue::Number(n) => {
            if let Ok(i) = n.parse::<i64>() {
                Value::Int8(i)
            } else if let Ok(f) = n.parse::<f64>() {
                Value::Float8(f)
            } else {
                Value::Text(n.clone())
            }
        }
        JsonbValue::String(s) => Value::Text(s.clone()),
        JsonbValue::Array(arr) => Value::Array {
            element_oid: plomid_types::TypeOid::TEXT,
            elements: arr.iter().map(jsonb_value_to_sql).collect(),
        },
        // Objects serialize through the canonical `JsonbValue` writer so that
        // JSON produced here is byte-identical to every other JSON path.
        JsonbValue::Object(_) => Value::Json(value.to_text()),
    }
}

/// Casts a SQL value to the target column type for JSON record expansion.
pub(crate) fn cast_json_value(value: &Value, col_type: &ColumnType) -> SqlResult<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    use plomid_types::TypeOid;
    match col_type.type_oid {
        TypeOid::INT2 | TypeOid::INT4 | TypeOid::INT8 => match value {
            Value::Int8(i) => Ok(Value::Int4(*i as i32)),
            Value::Text(s) => s.parse::<i32>().map(Value::Int4).map_err(|_| {
                SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    format!("cannot cast '{s}' to integer"),
                ))
            }),
            _ => Ok(value.clone()),
        },
        TypeOid::BOOL => match value {
            Value::Text(s) => match s.to_lowercase().as_str() {
                "true" | "t" | "yes" | "y" | "1" => Ok(Value::Bool(true)),
                "false" | "f" | "no" | "n" | "0" => Ok(Value::Bool(false)),
                _ => Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    format!("cannot cast '{s}' to boolean"),
                ))),
            },
            _ => Ok(value.clone()),
        },
        TypeOid::TEXT | TypeOid::VARCHAR | TypeOid::BPCHAR | TypeOid::NAME => {
            Ok(Value::Text(value.to_sql_text()))
        }
        TypeOid::JSON => match value {
            Value::Json(_) => Ok(value.clone()),
            other => Ok(Value::Json(other.to_sql_text())),
        },
        TypeOid::JSONB => match value {
            Value::Jsonb(_) => Ok(value.clone()),
            other => {
                let text = other.to_sql_text();
                match JsonbValue::parse(&text) {
                    Ok(tree) => Ok(Value::Jsonb(tree.encode())),
                    Err(_) => Ok(Value::Json(text)),
                }
            }
        },
        _ => Ok(value.clone()),
    }
}
