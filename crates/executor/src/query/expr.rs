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
//! Expression evaluation for a single row.
//!
//! [`evaluate_expression`] is the crate-wide entry point used by DML, index
//! maintenance, CTEs and the join engine; [`evaluate_predicate`] is the
//! boolean specialisation used for `WHERE` and `ON` clauses.

use super::compare::{
    arithmetic_expression, compare_expression, extract_datetime_field, like_match,
};
use super::scan::value_cmp;
use crate::coerce::coerce_comparison_operands;
use crate::error::{SqlError, SqlResult};
use crate::row::values_equal;
use crate::scalar::integer_bitwise;
use crate::util::unqualify;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::Expression;
use plomid_sql::IsBooleanKind;
use plomid_sql::JsonKind;
use plomid_sql::TableSchema;
use plomid_sql::Value;

pub fn evaluate_predicate(
    row: &[Value],
    schema: &TableSchema,
    expr: &Expression,
) -> SqlResult<bool> {
    match expr {
        Expression::Equal(left, right) => {
            let left_val = evaluate_expression(row, schema, left)?;
            let right_val = evaluate_expression(row, schema, right)?;
            let (left_val, right_val) = coerce_comparison_operands(left_val, right_val);
            if left_val.is_null() || right_val.is_null() {
                Ok(false)
            } else {
                Ok(values_equal(&left_val, &right_val))
            }
        }
        other => Ok(matches!(
            evaluate_expression(row, schema, other)?,
            Value::Bool(true)
        )),
    }
}

pub fn evaluate_expression(
    row: &[Value],
    schema: &TableSchema,
    expr: &Expression,
) -> SqlResult<Value> {
    match expr {
        Expression::ColumnRef(name) => {
            // Zero-argument session/clock functions also surface as bare
            // ColumnRefs (e.g. `SELECT CURRENT_TIMESTAMP` parses the keyword
            // as a function-shaped target, but nested uses arrive here).
            // Resolve them from the statement execution context instead of
            // treating them as missing columns.
            if crate::catalog_fn::is_session_function(name) {
                return eval_session_function(name);
            }
            let idx = schema.column_index(unqualify(name))?;
            Ok(row[idx].clone())
        }
        Expression::Literal(value) => Ok(value.clone()),
        Expression::Equal(left, right) => {
            let (l, r) = coerce_comparison_operands(
                evaluate_expression(row, schema, left)?,
                evaluate_expression(row, schema, right)?,
            );
            if l.is_null() || r.is_null() {
                Ok(Value::Null)
            } else {
                Ok(Value::Bool(values_equal(&l, &r)))
            }
        }
        Expression::NotEqual(left, right) => {
            let (l, r) = coerce_comparison_operands(
                evaluate_expression(row, schema, left)?,
                evaluate_expression(row, schema, right)?,
            );
            if l.is_null() || r.is_null() {
                Ok(Value::Null)
            } else {
                Ok(Value::Bool(!values_equal(&l, &r)))
            }
        }
        Expression::IsNull(inner) => Ok(Value::Bool(
            evaluate_expression(row, schema, inner)?.is_null(),
        )),
        Expression::IsNotNull(inner) => Ok(Value::Bool(
            !evaluate_expression(row, schema, inner)?.is_null(),
        )),
        Expression::IsJson {
            expr,
            kind,
            negated,
        } => {
            let value = evaluate_expression(row, schema, expr)?;
            // SQL three-valued logic: NULL input yields NULL result.
            if value.is_null() {
                return Ok(Value::Null);
            }
            let is_json = json_kind_matches(&value, *kind)?;
            Ok(Value::Bool(if *negated { !is_json } else { is_json }))
        }
        Expression::IsBoolean {
            expr,
            kind,
            negated,
        } => {
            let value = evaluate_expression(row, schema, expr)?;
            let result = match (kind, &value) {
                (IsBooleanKind::True, Value::Bool(true)) => true,
                (IsBooleanKind::False, Value::Bool(false)) => true,
                // `IS UNKNOWN` is true only for SQL NULL; `IS TRUE`/`IS FALSE`
                // are false for NULL, NULLs, and non-boolean values alike.
                (IsBooleanKind::Unknown, Value::Null) => true,
                _ => false,
            };
            Ok(Value::Bool(if *negated { !result } else { result }))
        }
        Expression::And(left, right) => Ok(Value::Bool(
            matches!(evaluate_expression(row, schema, left)?, Value::Bool(true))
                && matches!(evaluate_expression(row, schema, right)?, Value::Bool(true)),
        )),
        Expression::Or(left, right) => Ok(Value::Bool(
            matches!(evaluate_expression(row, schema, left)?, Value::Bool(true))
                || matches!(evaluate_expression(row, schema, right)?, Value::Bool(true)),
        )),
        Expression::Not(inner) => {
            // SQL three-valued logic: NOT TRUE is FALSE, NOT FALSE is TRUE,
            // and NOT NULL is NULL (the row is then filtered by the caller).
            // Only the NULL case is special: every non-NULL value keeps the
            // historical behavior bit-for-bit (including non-boolean inputs,
            // which the join engine rejects but this path historically
            // accepts as TRUE).
            let value = evaluate_expression(row, schema, inner)?;
            if value.is_null() {
                Ok(Value::Null)
            } else {
                Ok(Value::Bool(!matches!(value, Value::Bool(true))))
            }
        }
        Expression::Greater(_, _)
        | Expression::GreaterOrEqual(_, _)
        | Expression::Less(_, _)
        | Expression::LessOrEqual(_, _) => compare_expression(row, schema, expr),
        Expression::Add(_, _)
        | Expression::Subtract(_, _)
        | Expression::Multiply(_, _)
        | Expression::Divide(_, _)
        | Expression::Modulo(_, _) => arithmetic_expression(row, schema, expr),
        Expression::Concat(left, right) => {
            let left = evaluate_expression(row, schema, left)?;
            let right = evaluate_expression(row, schema, right)?;
            crate::scalar::concat_operator(&left, &right)
        }
        Expression::Power(left, right) => {
            let l = evaluate_expression(row, schema, left)?;
            let r = evaluate_expression(row, schema, right)?;
            if l.is_null() || r.is_null() {
                Ok(Value::Null)
            } else {
                let args = vec![l, r];
                crate::scalar::scalar_function_value("power", &args)
            }
        }
        Expression::BitAnd(left, right)
        | Expression::BitOr(left, right)
        | Expression::BitXor(left, right)
        | Expression::ShiftLeft(left, right)
        | Expression::ShiftRight(left, right) => {
            let l = evaluate_expression(row, schema, left)?;
            let r = evaluate_expression(row, schema, right)?;
            if l.is_null() || r.is_null() {
                Ok(Value::Null)
            } else {
                integer_bitwise(expr, &l, &r)
            }
        }
        Expression::Negate(inner) => match evaluate_expression(row, schema, inner)? {
            Value::Int4(v) => Ok(Value::Int4(-v)),
            Value::Int8(v) => Ok(Value::Int8(-v)),
            _ => Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "invalid numeric value",
            ))),
        },
        Expression::Star => Ok(Value::Null),
        Expression::TypeCast { expr, type_name } => {
            let value = evaluate_expression(row, schema, expr)?;
            crate::coerce::cast_value(&value, type_name)
        }
        Expression::Cast { expr, type_name } => {
            let value = evaluate_expression(row, schema, expr)?;
            crate::coerce::cast_value(&value, type_name)
        }
        Expression::DateLiteral(text) => {
            plomid_types::text::parse_value(text, plomid_types::PgType::Date)
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
        }
        Expression::TimestampLiteral(text) => {
            plomid_types::text::parse_value(text, plomid_types::PgType::Timestamp)
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
        }
        Expression::TimestamptzLiteral(text) => {
            // TIMESTAMPTZ shares the existing timestamp-with-time-zone value
            // representation and parser; keeping one representation means
            // casts, comparisons, and JSON conversion behave identically for
            // `TIMESTAMPTZ '...'` and `TIMESTAMP WITH TIME ZONE '...'`.
            plomid_types::text::parse_value(text, plomid_types::PgType::Timestamptz)
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
        }
        Expression::TimeLiteral(text) => {
            plomid_types::text::parse_value(text, plomid_types::PgType::Time)
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
        }
        Expression::TypedTimeLiteral { text, precision } => {
            let base = plomid_types::text::parse_value(text, plomid_types::PgType::Time)
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))?;
            plomid_types::apply_typmod(base, i32::from(*precision), plomid_types::PgType::Time)
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
        }
        Expression::TypedTimestampLiteral { text, precision } => {
            let base = plomid_types::text::parse_value(text, plomid_types::PgType::Timestamp)
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))?;
            plomid_types::apply_typmod(base, i32::from(*precision), plomid_types::PgType::Timestamp)
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
        }
        Expression::TypedTimestamptzLiteral { text, precision } => {
            let base = plomid_types::text::parse_value(text, plomid_types::PgType::Timestamptz)
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))?;
            plomid_types::apply_typmod(
                base,
                i32::from(*precision),
                plomid_types::PgType::Timestamptz,
            )
            .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
        }
        Expression::Extract { field, expr } => {
            let value = evaluate_expression(row, schema, expr)?;
            extract_datetime_field(&value, field)
        }
        Expression::JsonArrow {
            left,
            right,
            as_text,
        } => {
            let lhs = evaluate_expression(row, schema, left)?;
            let rhs = evaluate_expression(row, schema, right)?;
            crate::json::json_arrow_value(&lhs, &rhs, *as_text)
        }
        Expression::ArrayIndex { array, index } => {
            let arr = evaluate_expression(row, schema, array)?;
            let idx = evaluate_expression(row, schema, index)?;
            crate::util::array_index_value(&arr, &idx)
        }
        Expression::RowField { expr, field } => {
            let value = evaluate_expression(row, schema, expr)?;
            crate::scalar::row_field_value(&value, field)
        }
        // Zero-argument and scalar function calls share one architectural
        // path: session/clock built-ins resolve against the statement
        // execution context (no SELECT required); pure scalar built-ins
        // evaluate from their already-evaluated arguments; everything else
        // still needs the join-aware query engine (sequences, catalog
        // lookups, aggregates, user functions).
        Expression::FunctionCall {
            name,
            args,
            returning,
            null_handling,
            unique_keys,
            ..
        } => {
            if crate::catalog_fn::is_session_function(name) {
                return eval_session_function(name);
            }
            let values = args
                .iter()
                .map(|arg| evaluate_expression(row, schema, arg))
                .collect::<SqlResult<Vec<_>>>()?;
            if crate::scalar::is_scalar_function(name) {
                return crate::scalar::scalar_function_value_extended(
                    name,
                    &values,
                    returning.clone(),
                    *null_handling,
                    *unique_keys,
                );
            }
            // Keep the historical diagnostic for functions that genuinely
            // need the general query engine.
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Unsupported,
                "function evaluation requires query context",
            )));
        }
        // SQL predicates and conditional expressions.
        Expression::In {
            expr,
            list,
            negated,
            ..
        } => {
            let v = evaluate_expression(row, schema, expr)?;
            let mut matched = false;
            let mut saw_null = false;
            for item in list {
                let item_v = evaluate_expression(row, schema, item)?;
                if item_v.is_null() {
                    saw_null = true;
                    continue;
                }
                if values_equal(&v, &item_v) {
                    matched = true;
                    break;
                }
            }
            if matched {
                Ok(Value::Bool(!negated))
            } else if saw_null {
                Ok(Value::Null)
            } else {
                Ok(Value::Bool(*negated))
            }
        }
        Expression::Between {
            expr,
            low,
            high,
            negated,
        } => {
            let v = evaluate_expression(row, schema, expr)?;
            let lo = evaluate_expression(row, schema, low)?;
            let hi = evaluate_expression(row, schema, high)?;
            if v.is_null() || lo.is_null() || hi.is_null() {
                return Ok(Value::Null);
            }
            let ge_lo = matches!(
                value_cmp(&v, &lo),
                Some(std::cmp::Ordering::Greater) | Some(std::cmp::Ordering::Equal)
            );
            let le_hi = matches!(
                value_cmp(&v, &hi),
                Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal)
            );
            let inside = ge_lo && le_hi;
            Ok(Value::Bool(if *negated { !inside } else { inside }))
        }
        Expression::Like {
            expr,
            pattern,
            negated,
            ..
        } => {
            let v = evaluate_expression(row, schema, expr)?;
            let p = evaluate_expression(row, schema, pattern)?;
            if v.is_null() || p.is_null() {
                return Ok(Value::Null);
            }
            let s = v.to_sql_text();
            let pat = p.to_sql_text();
            let matched = like_match(&s, &pat);
            Ok(Value::Bool(if *negated { !matched } else { matched }))
        }
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            for (cond, val) in whens {
                let test = if let Some(op) = operand {
                    let op_v = evaluate_expression(row, schema, op)?;
                    let cv = evaluate_expression(row, schema, cond)?;
                    if op_v.is_null() || cv.is_null() {
                        Value::Null
                    } else {
                        Value::Bool(values_equal(&op_v, &cv))
                    }
                } else {
                    evaluate_expression(row, schema, cond)?
                };
                if matches!(test, Value::Bool(true)) {
                    return evaluate_expression(row, schema, val);
                }
            }
            if let Some(def) = default {
                evaluate_expression(row, schema, def)
            } else {
                Ok(Value::Null)
            }
        }
        Expression::Coalesce(args) => {
            for arg in args {
                let v = evaluate_expression(row, schema, arg)?;
                if !v.is_null() {
                    return Ok(v);
                }
            }
            Ok(Value::Null)
        }
        Expression::NullIf(a, b) => {
            let av = evaluate_expression(row, schema, a)?;
            let bv = evaluate_expression(row, schema, b)?;
            if av.is_null() || bv.is_null() {
                Ok(av)
            } else if values_equal(&av, &bv) {
                Ok(Value::Null)
            } else {
                Ok(av)
            }
        }
        Expression::IsDistinctFrom(l, r) => {
            let lv = evaluate_expression(row, schema, l)?;
            let rv = evaluate_expression(row, schema, r)?;
            if lv.is_null() && rv.is_null() {
                Ok(Value::Bool(false))
            } else if lv.is_null() || rv.is_null() {
                Ok(Value::Bool(true))
            } else {
                Ok(Value::Bool(!values_equal(&lv, &rv)))
            }
        }
        Expression::Exists(_) | Expression::ScalarSubquery(_) => {
            // These are handled by the join module which has storage engine access.
            // The single-table fast path uses placeholder implementations.
            Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Unsupported,
                "subquery evaluation is not yet supported in this context",
            )))
        }
        Expression::QuantifiedComparison { .. } => {
            // Quantified comparisons are handled by the join module which has
            // storage engine access for executing the subquery.
            Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Unsupported,
                "quantified comparison requires the join module for subquery evaluation",
            )))
        }
        Expression::WindowFunction { .. } => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            "window functions are not supported in this expression context",
        ))),
        Expression::JsonSubscript { array, index } => {
            let arr = evaluate_expression(row, schema, array)?;
            let idx = evaluate_expression(row, schema, index)?;
            crate::json::json_subscript_value(&arr, &idx)
        }
    }
}

/// Implements `expr IS [NOT] JSON [VALUE|OBJECT|ARRAY|SCALAR]`.
///
/// PostgreSQL semantics (9.16.2 SQL/JSON predicates):
/// - `json`/`text` inputs are parsed; syntactically invalid input is simply
///   *not JSON* (predicate false), not an error.
/// - `jsonb` inputs are already-parsed trees, so the kind test is structural.
/// - `VALUE` is the default kind: any valid JSON document.
/// - `OBJECT`/`ARRAY` restrict the test to a top-level object/array.
/// - `SCALAR` excludes top-level objects and arrays (numbers, strings,
///   booleans and `null` all count as scalars).
pub fn json_kind_matches(value: &Value, kind: JsonKind) -> SqlResult<bool> {
    // Resolve the top-level `JsonbValue` for the input, mirroring how
    // PostgreSQL classifies `json` (parse the text) vs `jsonb` (inspect the
    // tree). Non-JSON values are never JSON.
    let top = match value {
        Value::Jsonb(bytes) => plomid_types::jsonb::JsonbValue::decode(bytes).ok(),
        Value::Json(text) | Value::Text(text) => {
            // Invalid JSON text: the predicate is false, not an error.
            plomid_types::jsonb::JsonbValue::parse(text).ok()
        }
        _ => None,
    };
    let matches = match (&top, kind) {
        (None, _) => false,
        // `VALUE` (and bare `IS JSON`) accept any valid JSON document.
        (Some(_), JsonKind::Any) | (Some(_), JsonKind::Value) => true,
        (Some(tree), JsonKind::Object) => {
            matches!(tree, plomid_types::jsonb::JsonbValue::Object(_))
        }
        (Some(tree), JsonKind::Array) => matches!(tree, plomid_types::jsonb::JsonbValue::Array(_)),
        (Some(tree), JsonKind::Scalar) => !matches!(
            tree,
            plomid_types::jsonb::JsonbValue::Object(_) | plomid_types::jsonb::JsonbValue::Array(_)
        ),
    };
    Ok(matches)
}

/// Resolves a zero-argument session/clock built-in against the statement
/// Resolves a zero-argument session/clock built-in against the statement
/// execution context. Non-clock session functions still use the session
/// database/user; CURRENT_* values come from the cached statement clock so
/// every reference in one statement observes the same timestamp.
fn eval_session_function(name: &str) -> SqlResult<Value> {
    let lname = crate::util::unqualify(name).to_ascii_lowercase();
    match lname.as_str() {
        "current_timestamp" | "now" => crate::context::statement_timestamp(),
        "current_date" => crate::context::statement_date(),
        "current_time" => crate::context::statement_time(),
        _ => {
            let (db, user) = crate::context::current_ids();
            crate::catalog_fn::function_value(name, &db, &user)
        }
    }
}
