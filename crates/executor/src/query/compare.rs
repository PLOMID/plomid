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
//! Comparisons, arithmetic and scalar helpers for expression evaluation.
//!
//! Comparison semantics are PostgreSQL's: `NULL` ordering, integer/float
//! promotion, string comparison and `ANY` / `ALL` quantified subqueries.

use super::expr::evaluate_expression;
use super::scan::value_cmp;
use crate::coerce::coerce_comparison_operands;
use crate::error::{SqlError, SqlResult};
use crate::row::values_equal;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::ComparisonOperator;
use plomid_sql::Expression;
use plomid_sql::TableSchema;
use plomid_sql::Value;

pub(super) fn compare_expression(
    row: &[Value],
    schema: &TableSchema,
    expr: &Expression,
) -> SqlResult<Value> {
    let (left, right) = match expr {
        Expression::Greater(l, r)
        | Expression::GreaterOrEqual(l, r)
        | Expression::Less(l, r)
        | Expression::LessOrEqual(l, r) => (l, r),
        _ => unreachable!(),
    };
    let lv = evaluate_expression(row, schema, left)?;
    let rv = evaluate_expression(row, schema, right)?;
    let (lv, rv) = coerce_comparison_operands(lv, rv);
    if lv.is_null() || rv.is_null() {
        return Ok(Value::Null);
    }
    let Some(order) = value_cmp(&lv, &rv) else {
        return Ok(Value::Null);
    };
    let result = match expr {
        Expression::Greater(_, _) => order == std::cmp::Ordering::Greater,
        Expression::GreaterOrEqual(_, _) => order != std::cmp::Ordering::Less,
        Expression::Less(_, _) => order == std::cmp::Ordering::Less,
        _ => order != std::cmp::Ordering::Greater,
    };
    Ok(Value::Bool(result))
}

pub(super) fn arithmetic_expression(
    row: &[Value],
    schema: &TableSchema,
    expr: &Expression,
) -> SqlResult<Value> {
    let (left, right) = match expr {
        Expression::Add(l, r)
        | Expression::Subtract(l, r)
        | Expression::Multiply(l, r)
        | Expression::Divide(l, r)
        | Expression::Modulo(l, r) => (l, r),
        _ => unreachable!(),
    };
    let left_value = evaluate_expression(row, schema, left)?;
    let right_value = evaluate_expression(row, schema, right)?;
    if left_value.is_null() || right_value.is_null() {
        return Ok(Value::Null);
    }
    // JSONB/JSON `-` operator: key deletion (text), array index deletion
    // (integer) or multi-key deletion (text[]). Resolve by operand type before
    // falling through to arithmetic.
    if matches!(expr, Expression::Subtract(_, _))
        && matches!(left_value, Value::Json(_) | Value::Jsonb(_))
    {
        return crate::json::jsonb_subtract(&left_value, &right_value);
    }
    if let (Value::Date(days), Some(delta)) = (&left_value, query_integer(&right_value)) {
        return Ok(match expr {
            Expression::Add(_, _) => Value::Date(days.saturating_add(delta as i32)),
            Expression::Subtract(_, _) => Value::Date(days.saturating_sub(delta as i32)),
            _ => Value::Null,
        });
    }
    if let (Value::Date(days), Value::Interval(interval)) = (&left_value, &right_value) {
        let base = i64::from(*days) * plomid_types::datetime::USECS_PER_DAY;
        let delta =
            i64::from(interval.days) * plomid_types::datetime::USECS_PER_DAY + interval.micros;
        return Ok(match expr {
            Expression::Add(_, _) => {
                if interval.months != 0 {
                    Value::Timestamp(plomid_types::datetime::add_interval_to_timestamp(
                        base, *interval, false,
                    ))
                } else {
                    Value::Timestamp(base.saturating_add(delta))
                }
            }
            Expression::Subtract(_, _) => {
                if interval.months != 0 {
                    Value::Timestamp(plomid_types::datetime::add_interval_to_timestamp(
                        base, *interval, true,
                    ))
                } else {
                    Value::Timestamp(base.saturating_sub(delta))
                }
            }
            _ => {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "invalid date arithmetic",
                )))
            }
        });
    }
    if let (Value::Timestamp(ts), Value::Interval(interval))
    | (Value::Timestamptz(ts), Value::Interval(interval)) = (&left_value, &right_value)
    {
        return Ok(match expr {
            Expression::Add(_, _) => {
                let out = plomid_types::datetime::add_interval_to_timestamp(*ts, *interval, false);
                if matches!(left_value, Value::Timestamptz(_)) {
                    Value::Timestamptz(out)
                } else {
                    Value::Timestamp(out)
                }
            }
            Expression::Subtract(_, _) => {
                let out = plomid_types::datetime::add_interval_to_timestamp(*ts, *interval, true);
                if matches!(left_value, Value::Timestamptz(_)) {
                    Value::Timestamptz(out)
                } else {
                    Value::Timestamp(out)
                }
            }
            _ => {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "invalid timestamp arithmetic",
                )))
            }
        });
    }
    if let (Value::Interval(interval), Value::Timestamp(ts))
    | (Value::Interval(interval), Value::Timestamptz(ts)) = (&left_value, &right_value)
    {
        if matches!(expr, Expression::Add(_, _)) {
            let out = plomid_types::datetime::add_interval_to_timestamp(*ts, *interval, false);
            return Ok(if matches!(right_value, Value::Timestamptz(_)) {
                Value::Timestamptz(out)
            } else {
                Value::Timestamp(out)
            });
        }
    }
    if let (Value::Date(a), Value::Date(b)) = (&left_value, &right_value) {
        if matches!(expr, Expression::Subtract(_, _)) {
            return Ok(Value::Int4(a - b));
        }
    }
    if let (Value::Timestamp(a), Value::Timestamp(b)) = (&left_value, &right_value) {
        if matches!(expr, Expression::Subtract(_, _)) {
            return Ok(Value::Interval(plomid_types::datetime::Interval {
                micros: a.saturating_sub(*b),
                ..Default::default()
            }));
        }
    }
    if let (Value::Timestamptz(a), Value::Timestamptz(b)) = (&left_value, &right_value) {
        if matches!(expr, Expression::Subtract(_, _)) {
            return Ok(Value::Interval(plomid_types::datetime::Interval {
                micros: a.saturating_sub(*b),
                ..Default::default()
            }));
        }
    }
    let Some((a, b)) = numeric_pair(left_value.clone(), right_value.clone()) else {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            "operator requires numeric operands",
        )));
    };
    let numeric = matches!(
        left_value,
        Value::Numeric(_) | Value::Float4(_) | Value::Float8(_)
    ) || matches!(
        right_value,
        Value::Numeric(_) | Value::Float4(_) | Value::Float8(_)
    );
    if numeric {
        let result = match expr {
            Expression::Add(_, _) => a + b,
            Expression::Subtract(_, _) => a - b,
            Expression::Multiply(_, _) => a * b,
            Expression::Divide(_, _) if b != 0.0 => a / b,
            Expression::Modulo(_, _) if b != 0.0 => a % b,
            _ => {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "division by zero",
                )))
            }
        };
        return Ok(Value::Numeric(plomid_types::Numeric::from_f64(result)));
    }
    let value = match expr {
        Expression::Add(_, _) => a + b,
        Expression::Subtract(_, _) => a - b,
        Expression::Multiply(_, _) => a * b,
        Expression::Divide(_, _) if b != 0.0 => a / b,
        Expression::Modulo(_, _) if b != 0.0 => a % b,
        _ => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "division by zero",
            )))
        }
    };
    Ok(Value::Int8(value as i64))
}

/// Compares two values for use in quantified comparisons (ANY/ALL/SOME).
/// Returns a Value::Bool or Value::Null following SQL three-valued logic.
pub(crate) fn compare_values_for_any_all(
    left: Value,
    operator: ComparisonOperator,
    right: Value,
) -> SqlResult<Value> {
    // Handle NULL comparisons - in SQL, comparisons with NULL return NULL
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }

    // coerce to compatible types
    let (left, right) = coerce_comparison_operands(left, right);

    // Use value_cmp for ordering comparisons
    let result = match operator {
        ComparisonOperator::Equal => {
            if values_equal(&left, &right) {
                Value::Bool(true)
            } else {
                Value::Bool(false)
            }
        }
        ComparisonOperator::NotEqual => {
            if values_equal(&left, &right) {
                Value::Bool(false)
            } else {
                Value::Bool(true)
            }
        }
        ComparisonOperator::Less
        | ComparisonOperator::LessOrEqual
        | ComparisonOperator::Greater
        | ComparisonOperator::GreaterOrEqual => {
            let Some(order) = value_cmp(&left, &right) else {
                return Ok(Value::Null);
            };
            match operator {
                ComparisonOperator::Less => {
                    if order == std::cmp::Ordering::Less {
                        Value::Bool(true)
                    } else {
                        Value::Bool(false)
                    }
                }
                ComparisonOperator::LessOrEqual => {
                    if order != std::cmp::Ordering::Greater {
                        Value::Bool(true)
                    } else {
                        Value::Bool(false)
                    }
                }
                ComparisonOperator::Greater => {
                    if order == std::cmp::Ordering::Greater {
                        Value::Bool(true)
                    } else {
                        Value::Bool(false)
                    }
                }
                ComparisonOperator::GreaterOrEqual => {
                    if order != std::cmp::Ordering::Less {
                        Value::Bool(true)
                    } else {
                        Value::Bool(false)
                    }
                }
                _ => unreachable!(),
            }
        }
    };

    Ok(result)
}

/// Evaluates PostgreSQL's common date/time EXTRACT fields.
pub(crate) fn extract_datetime_field(value: &Value, field: &str) -> SqlResult<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    use plomid_types::datetime::{date_to_parts, time_to_parts, timestamp_to_parts, USECS_PER_DAY};
    let (date, time) = match value {
        Value::Date(days) => (date_to_parts(*days), None),
        Value::Timestamp(micros) | Value::Timestamptz(micros) => {
            let (date, time) = timestamp_to_parts(*micros);
            (date, Some(time))
        }
        Value::Time(micros) | Value::TimeTz { micros, .. } => {
            (date_to_parts(0), Some(time_to_parts(*micros)))
        }
        _ => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "EXTRACT requires date/time value",
            )))
        }
    };
    let days = match value {
        Value::Date(days) => *days,
        Value::Timestamp(micros) | Value::Timestamptz(micros) => {
            micros.div_euclid(USECS_PER_DAY) as i32
        }
        _ => 0,
    };
    let lower = field.to_ascii_lowercase();
    let n = match lower.as_str() {
        "year" => date.year as f64,
        "month" => f64::from(date.month),
        "day" => f64::from(date.day),
        "quarter" => f64::from((date.month - 1) / 3 + 1),
        "dow" => f64::from((days + 6).rem_euclid(7)),
        "isodow" => f64::from((days + 6).rem_euclid(7) + 1),
        "doy" => f64::from(
            (1..date.month)
                .map(|m| plomid_types::datetime::days_in_month(date.year, m))
                .sum::<i32>()
                + i32::from(date.day),
        ),
        "hour" => f64::from(time.map_or(0, |t| t.hour)),
        "minute" => f64::from(time.map_or(0, |t| t.minute)),
        "second" => time.map_or(0.0, |t| {
            f64::from(t.second) + f64::from(t.micros) / 1_000_000.0
        }),
        "milliseconds" => time.map_or(0.0, |t| {
            f64::from(t.second) * 1_000.0 + f64::from(t.micros) / 1_000.0
        }),
        "microseconds" => time.map_or(0.0, |t| {
            f64::from(t.second) * 1_000_000.0 + f64::from(t.micros)
        }),
        "century" => ((date.year - 1).div_euclid(100) + 1) as f64,
        "decade" => (date.year.div_euclid(10)) as f64,
        _ => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("unsupported EXTRACT field \"{field}\""),
            )))
        }
    };
    // PostgreSQL exposes EXTRACT/date_part as numeric. Integral fields are
    // exact integers; use from_i64 so scale stays 0 and text output is "12"
    // rather than "12.00000000000000". Fractional fields keep from_f64 scale.
    let numeric = if n.fract() == 0.0
        && n.is_finite()
        && n >= -9_000_000_000_000_000.0
        && n <= 9_000_000_000_000_000.0
    {
        plomid_types::Numeric::from_i64(n as i64)
    } else {
        plomid_types::Numeric::from_f64(n)
    };
    Ok(Value::Numeric(numeric))
}

fn query_integer(value: &Value) -> Option<i64> {
    match value {
        Value::Int2(v) => Some(i64::from(*v)),
        Value::Int4(v) => Some(i64::from(*v)),
        Value::Int8(v) => Some(*v),
        _ => None,
    }
}

fn numeric_pair(left: Value, right: Value) -> Option<(f64, f64)> {
    fn as_f64(value: &Value) -> Option<f64> {
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
    Some((as_f64(&left)?, as_f64(&right)?))
}

/// SQL `LIKE` / `NOT LIKE` pattern matcher with `%` (any) and `_` (one char).
///
/// This is a small recursive matcher that supports the standard SQL `LIKE`
/// semantics used by PostgreSQL when no `ESCAPE` clause is given. The
/// implementation is intentionally simple and operates on UTF-8 strings; for
/// the small set of patterns that real-world clients use (and our regression
/// tests cover) the asymptotic cost is irrelevant.
pub(super) fn like_match(input: &str, pattern: &str) -> bool {
    let input: Vec<char> = input.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    like_match_inner(&input, 0, &pattern, 0)
}

fn like_match_inner(input: &[char], i: usize, pattern: &[char], p: usize) -> bool {
    if p == pattern.len() {
        return i == input.len();
    }
    match pattern[p] {
        '%' => {
            // Try matching `%` against every possible suffix.
            for k in i..=input.len() {
                if like_match_inner(input, k, pattern, p + 1) {
                    return true;
                }
            }
            false
        }
        '_' => {
            if i < input.len() {
                like_match_inner(input, i + 1, pattern, p + 1)
            } else {
                false
            }
        }
        c => i < input.len() && input[i] == c && like_match_inner(input, i + 1, pattern, p + 1),
    }
}
