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
//! JSON / array subscripting (`jsonb['key']`, `jsonb[0]`, `array[i]`).
//!
//! Text subscripts on objects behave like `->` (yielding JSONB); integer
//! subscripts behave like array index access. This is the single subscript
//! implementation, shared by the executor's expression evaluator and the JSON
//! scalar functions.

use plomid_core::{ErrorKind, PlomidError};
use plomid_core::{SqlError, SqlResult};
use plomid_types::function::invalid_arg;
use plomid_types::JsonbValue;
use plomid_types::PgValue as Value;

use crate::ops::jsonb_array_index;

pub fn array_index_value(array: &Value, index: &Value) -> SqlResult<Value> {
    if array.is_null() || index.is_null() {
        return Ok(Value::Null);
    }
    // JSON/JSONB subscripting: jsonb['key'] or jsonb[1]
    if matches!(array, Value::Json(_) | Value::Jsonb(_)) {
        let doc = match array {
            Value::Jsonb(bytes) => {
                JsonbValue::decode(bytes).map_err(|e| invalid_arg("jsonb subscript", &e))?
            }
            Value::Json(text) => {
                JsonbValue::parse(text).map_err(|e| invalid_arg("json subscript", &e))?
            }
            _ => unreachable!(),
        };
        let result = match index {
            Value::Text(s) | Value::VarChar(s) | Value::Name(s) | Value::Unknown(s) => match &doc {
                JsonbValue::Object(pairs) => {
                    pairs.iter().find(|(k, _)| k == s).map(|(_, v)| v.clone())
                }
                _ => None,
            },
            Value::Int2(v) => jsonb_array_index(&doc, i64::from(*v)),
            Value::Int4(v) => jsonb_array_index(&doc, i64::from(*v)),
            Value::Int8(v) => jsonb_array_index(&doc, *v),
            other => {
                let text = other.to_sql_text();
                match &doc {
                    JsonbValue::Object(pairs) => pairs
                        .iter()
                        .find(|(k, _)| k == &text)
                        .map(|(_, v)| v.clone()),
                    _ => None,
                }
            }
        };
        return Ok(match result {
            Some(v) => Value::Jsonb(v.encode()),
            None => Value::Null,
        });
    }
    let Value::Array { elements, .. } = array else {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            "cannot subscript a non-array value",
        )));
    };
    let position = match index {
        Value::Int2(v) => i64::from(*v),
        Value::Int4(v) => i64::from(*v),
        Value::Int8(v) => *v,
        _ => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "array subscript must be an integer",
            )))
        }
    };
    if position < 1 {
        return Ok(Value::Null);
    }
    Ok(elements
        .get((position - 1) as usize)
        .cloned()
        .unwrap_or(Value::Null))
}
