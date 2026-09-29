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
//! Scalar expression helpers shared by evaluation and planning.

use crate::error::{SqlError, SqlResult};
use crate::row::values_equal;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{Expression, QueryResult, Value};

use super::support::unsupported;

/// Text outside single-quoted strings is scanned; quoted content is preserved
/// verbatim.
pub(super) fn substitute_function_parameters(body: &str, values: &[Value]) -> String {
    if values.is_empty() {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            in_string = !in_string;
            out.push(ch);
            continue;
        }
        if ch == '$' && !in_string && chars.peek().is_some_and(char::is_ascii_digit) {
            let mut number = String::new();
            while let Some(&digit) = chars.peek() {
                if digit.is_ascii_digit() {
                    number.push(digit);
                    chars.next();
                } else {
                    break;
                }
            }
            if let Ok(index) = number.parse::<usize>() {
                if index >= 1 && index <= values.len() {
                    out.push_str(&sql_literal_text(&values[index - 1]));
                    continue;
                }
            }
            out.push('$');
            out.push_str(&number);
            continue;
        }
        out.push(ch);
    }
    out
}

/// Renders a value as a SQL literal for function-body substitution.
pub(super) fn sql_literal_text(value: &Value) -> String {
    if value.is_null() {
        return "NULL".to_string();
    }
    match value {
        Value::Text(text)
        | Value::VarChar(text)
        | Value::BpChar(text)
        | Value::Name(text)
        | Value::Json(text) => format!("'{}'", text.replace('\'', "''")),
        other => other.to_sql_text(),
    }
}

/// Reduces a subquery result to its first column for `IN (SELECT ...)`.
pub(super) fn subquery_first_column(result: QueryResult) -> SqlResult<Vec<Value>> {
    let QueryResult::Rows { rows, .. } = result else {
        return Err(unsupported("IN subquery must return rows"));
    };
    let mut column = Vec::with_capacity(rows.len());
    for mut row in rows {
        if row.len() != 1 {
            return Err(unsupported("IN subquery must return exactly one column"));
        }
        column.push(row.remove(0));
    }
    Ok(column)
}

/// SQL three-valued `IN` membership.
pub(super) fn in_membership(
    value: &Value,
    candidates: &[Value],
    negated: bool,
) -> SqlResult<Value> {
    let mut saw_null = value.is_null();
    for candidate in candidates {
        if candidate.is_null() {
            saw_null = true;
            continue;
        }
        if values_equal(value, candidate) {
            return Ok(Value::Bool(!negated));
        }
    }
    if saw_null {
        Ok(Value::Null)
    } else {
        Ok(Value::Bool(negated))
    }
}

/// Arithmetic over numeric values with SQL result typing: integer pairs
/// produce int8, any float operand produces float8, and division or modulo by
/// zero raises the standard error.
pub(super) fn arithmetic(left: &Value, right: &Value, expr: &Expression) -> SqlResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    // JSONB/JSON `-` operator: key deletion (text), array index deletion
    // (integer) or multi-key deletion (text[]). Resolve by operand type before
    // falling through to arithmetic.
    if matches!(expr, Expression::Subtract(_, _))
        && matches!(left, Value::Json(_) | Value::Jsonb(_))
    {
        return crate::json::jsonb_subtract(left, right);
    }
    if let (Value::Timestamp(a), Value::Timestamp(b)) = (left, right) {
        if matches!(expr, Expression::Subtract(_, _)) {
            return Ok(Value::Interval(plomid_types::datetime::Interval {
                micros: a.saturating_sub(*b),
                ..Default::default()
            }));
        }
    }
    if let (Value::Timestamptz(a), Value::Timestamptz(b)) = (left, right) {
        if matches!(expr, Expression::Subtract(_, _)) {
            return Ok(Value::Interval(plomid_types::datetime::Interval {
                micros: a.saturating_sub(*b),
                ..Default::default()
            }));
        }
    }
    if let (Value::Date(days), Some(delta)) = (left, integer_value(right)) {
        return match expr {
            Expression::Add(_, _) => Ok(Value::Date(days.saturating_add(delta as i32))),
            Expression::Subtract(_, _) => Ok(Value::Date(days.saturating_sub(delta as i32))),
            _ => Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "invalid date arithmetic",
            ))),
        };
    }
    if let (Value::Date(days), Value::Interval(interval)) = (left, right) {
        let base = i64::from(*days) * plomid_types::datetime::USECS_PER_DAY;
        let delta =
            i64::from(interval.days) * plomid_types::datetime::USECS_PER_DAY + interval.micros;
        return match expr {
            Expression::Add(_, _) => Ok(Value::Timestamp(base.saturating_add(delta))),
            Expression::Subtract(_, _) => Ok(Value::Timestamp(base.saturating_sub(delta))),
            _ => Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "invalid date arithmetic",
            ))),
        };
    }
    if let (Value::Date(left_days), Value::Date(right_days)) = (left, right) {
        if matches!(expr, Expression::Subtract(_, _)) {
            return Ok(Value::Int4(left_days - right_days));
        }
    }
    if let (Value::Timestamp(micros), Value::Interval(interval)) = (left, right) {
        let delta =
            i64::from(interval.days) * plomid_types::datetime::USECS_PER_DAY + interval.micros;
        return match expr {
            Expression::Add(_, _) => Ok(Value::Timestamp(micros.saturating_add(delta))),
            Expression::Subtract(_, _) => Ok(Value::Timestamp(micros.saturating_sub(delta))),
            _ => Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "invalid timestamp arithmetic",
            ))),
        };
    }
    let (Some(lf), Some(rf)) = (as_f64(left), as_f64(right)) else {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("operator requires numeric operands, got {left:?} and {right:?}"),
        )));
    };
    let both_integer = is_integer(left) && is_integer(right);
    match expr {
        Expression::Add(_, _) => Ok(numeric_result(lf + rf, both_integer)),
        Expression::Subtract(_, _) => Ok(numeric_result(lf - rf, both_integer)),
        Expression::Multiply(_, _) => Ok(numeric_result(lf * rf, both_integer)),
        Expression::Divide(_, _) => {
            if rf == 0.0 {
                Err(zero_division())
            } else if both_integer {
                Ok(Value::Int8((lf as i64) / (rf as i64)))
            } else {
                Ok(Value::Float8(lf / rf))
            }
        }
        Expression::Modulo(_, _) => {
            if rf == 0.0 {
                Err(zero_division())
            } else if both_integer {
                Ok(Value::Int8((lf as i64) % (rf as i64)))
            } else {
                Ok(Value::Float8(lf % rf))
            }
        }
        _ => unreachable!("arithmetic only handles arithmetic expressions"),
    }
}

pub(super) fn integer_value(value: &Value) -> Option<i64> {
    match value {
        Value::Int2(v) => Some(i64::from(*v)),
        Value::Int4(v) => Some(i64::from(*v)),
        Value::Int8(v) => Some(*v),
        _ => None,
    }
}

pub(super) fn zero_division() -> SqlError {
    SqlError::Storage(PlomidError::new(
        ErrorKind::InvalidArgument,
        "division by zero",
    ))
}

pub(super) fn system_relation_oid(name: &str) -> Option<u32> {
    match name {
        "pg_catalog.pg_class" => Some(1259),
        "pg_catalog.pg_attribute" => Some(1249),
        "pg_catalog.pg_type" => Some(1247),
        "pg_catalog.pg_namespace" => Some(2615),
        "pg_catalog.pg_database" => Some(1262),
        _ => None,
    }
}

pub(super) fn is_integer(value: &Value) -> bool {
    matches!(value, Value::Int2(_) | Value::Int4(_) | Value::Int8(_))
}

pub(super) fn numeric_result(f: f64, both_integer: bool) -> Value {
    if both_integer && f.fract() == 0.0 && f.abs() <= i64::MAX as f64 {
        Value::Int8(f as i64)
    } else {
        Value::Float8(f)
    }
}

pub(crate) fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int2(v) => Some(f64::from(*v)),
        Value::Int4(v) => Some(f64::from(*v)),
        Value::Int8(v) => Some(*v as f64),
        Value::Float4(v) => Some(f64::from(*v)),
        Value::Float8(v) => Some(*v),
        Value::Numeric(n) => Some(n.clone().to_f64()),
        _ => None,
    }
}

/// SQL `LIKE` matching with an optional `ESCAPE` character.
pub(super) fn like_match_with_escape(input: &str, pattern: &str, escape: Option<char>) -> bool {
    let input: Vec<char> = input.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    like_inner(&input, 0, &pattern, 0, escape)
}

fn like_inner(input: &[char], i: usize, pattern: &[char], p: usize, escape: Option<char>) -> bool {
    if p == pattern.len() {
        return i == input.len();
    }
    let c = pattern[p];
    // An escape character makes the following character literal.
    if escape.is_some() && Some(c) == escape {
        return p + 1 < pattern.len()
            && i < input.len()
            && input[i] == pattern[p + 1]
            && like_inner(input, i + 1, pattern, p + 2, escape);
    }
    match c {
        '%' => (i..=input.len()).any(|k| like_inner(input, k, pattern, p + 1, escape)),
        '_' => i < input.len() && like_inner(input, i + 1, pattern, p + 1, escape),
        c => i < input.len() && input[i] == c && like_inner(input, i + 1, pattern, p + 1, escape),
    }
}
