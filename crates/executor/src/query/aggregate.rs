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
//! Aggregate accumulators and grouped-expression evaluation.
//!
//! [`AggregateAccumulator`] is the state machine behind every aggregate
//! (`COUNT`, `SUM`, `ARRAY_AGG`, `JSON_AGG`, ...); the join engine reuses it
//! through `crate::query`, so it stays `pub(crate)`. This module also collects
//! the aggregate slots a query needs and evaluates the expressions of one
//! finished group.

use super::expr::evaluate_expression;
use super::scan::{number, value_cmp};
use super::select::expression_has_aggregate;
use crate::catalog_fn::is_aggregate_function;
use crate::coerce::number_f64;
use crate::error::{SqlError, SqlResult};
use crate::row::values_equal;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::Expression;
use plomid_sql::SelectTarget;
use plomid_sql::TableSchema;
use plomid_sql::Value;
use std::collections::HashSet;

#[derive(Clone)]
pub(crate) enum AggregateAccumulator {
    CountStar(i64),
    Count(i64),
    SumN(i64, bool),
    SumF(f64, bool),
    SumNumeric(Option<plomid_types::Numeric>, bool),
    AvgN {
        sum: i64,
        count: i64,
    },
    AvgF {
        sum: f64,
        count: i64,
    },
    AvgNumeric {
        sum: plomid_types::Numeric,
        count: i64,
    },
    Min(Option<Value>),
    Max(Option<Value>),
    // JSON aggregates accumulate the full per-row value list (not just
    // numeric state) so json_agg/jsonb_agg and the object aggregates can
    // finalize through the shared json_aggregate_result helpers. The
    // single-table fast path previously fell back to CountStar state for
    // these names, which produced wrong results whenever the query reached
    // grouped execution here instead of the general join engine.
    JsonValues(Vec<Value>),
    JsonPairs(Vec<(Value, Value)>),
    /// `COUNT(DISTINCT expr)`: the canonical SQL texts of the non-`NULL`
    /// values seen, mirroring the join engine's `seen` set and the
    /// single-target `COUNT(DISTINCT ...)` fast path. Textual (not `Value`)
    /// keys keep `Int8(1)`, `Float8(1.0)`, and `Numeric(1.00)` unified, as
    /// every other deduplication path in this engine does.
    CountDistinct(HashSet<String>),
}

impl AggregateAccumulator {
    pub(crate) fn new(name: &str, arg: Option<&Expression>, distinct: bool) -> Self {
        // JSON aggregates keep every input value (SQL NULL included for the
        // array forms) so finalization can reuse the shared JSON helpers.
        // Object aggregates store key/value pairs separately because the
        // two-argument form does not fit the single-value accumulator.
        if crate::json::is_json_aggregate(name) {
            if crate::json::is_json_object_aggregate(name) {
                return Self::JsonPairs(Vec::new());
            }
            return Self::JsonValues(Vec::new());
        }
        // `COUNT(DISTINCT expr)` (but never `COUNT(DISTINCT *)`, which keeps
        // the historical `COUNT(*)` behavior) deduplicates through the set
        // above; every other `DISTINCT` combination keeps its historical
        // accumulator exactly as before.
        if distinct
            && name.eq_ignore_ascii_case("count")
            && !matches!(arg, Some(Expression::Star) | None)
        {
            return Self::CountDistinct(HashSet::new());
        }
        match name.to_uppercase().as_str() {
            "COUNT" if matches!(arg, Some(Expression::Star)) => Self::CountStar(0),
            "COUNT" => Self::Count(0),
            "SUM" => Self::SumN(0, false),
            "AVG" => Self::AvgN { sum: 0, count: 0 },
            "MIN" => Self::Min(None),
            "MAX" => Self::Max(None),
            _ => Self::CountStar(0),
        }
    }

    /// Accumulates one row by evaluating `arg` in the single-table fast path
    /// context, or COUNT(*) semantics when `arg` is None/Star.
    pub(crate) fn step(
        &mut self,
        arg: Option<&Expression>,
        row: &[Value],
        schema: &TableSchema,
    ) -> SqlResult<()> {
        let value = if let Some(expr) = arg {
            if matches!(expr, Expression::Star) {
                None
            } else {
                Some(evaluate_expression(row, schema, expr)?)
            }
        } else {
            None
        };
        if let Self::CountDistinct(seen) = self {
            // Distinct counting skips NULLs like COUNT and unifies values by
            // canonical text, exactly like the single-target fast path.
            if let Some(value) = value.as_ref() {
                if !value.is_null() {
                    seen.insert(value.to_sql_text());
                }
            }
            return Ok(());
        }
        self.step_value(value.as_ref());
        Ok(())
    }

    /// Accumulates a pre-evaluated value into aggregate state. Used by the
    /// join-aware engine where expression evaluation uses a different row
    /// context than the single-table fast path.
    pub(crate) fn step_value(&mut self, value: Option<&Value>) {
        match self {
            Self::CountStar(n) => *n += 1,
            Self::Count(n) => {
                if value.is_some_and(|v| !v.is_null()) {
                    *n += 1;
                }
            }
            Self::CountDistinct(seen) => {
                if let Some(value) = value {
                    if !value.is_null() {
                        seen.insert(value.to_sql_text());
                    }
                }
            }
            Self::SumN(acc, has_any) => {
                if let Some(v) = value {
                    if let Some(n) = number(v) {
                        *acc += n;
                        *has_any = true;
                    } else if let Value::Numeric(n) = v {
                        *self = Self::SumNumeric(
                            Some(
                                n.clone()
                                    .add(plomid_types::Numeric::from_i64(*acc))
                                    .unwrap_or_else(|_| n.clone()),
                            ),
                            true,
                        );
                    } else if let Some(n) = number_f64(v) {
                        *self = Self::SumF(n + *acc as f64, true);
                    }
                }
            }
            Self::SumF(acc, has_any) => {
                if let Some(n) = value.and_then(number_f64) {
                    *acc += n;
                    *has_any = true;
                }
            }
            Self::SumNumeric(acc, has_any) => {
                if let Some(Value::Numeric(n)) = value {
                    *acc = acc
                        .clone()
                        .map(|existing| existing.add(n.clone()).unwrap_or_else(|_| n.clone()))
                        .or_else(|| Some(n.clone()));
                    *has_any = true;
                }
            }
            Self::AvgN { sum, count } => {
                if let Some(v) = value {
                    if let Some(n) = number(v) {
                        *sum += n;
                        *count += 1;
                    } else if let Value::Numeric(n) = v {
                        *self = Self::AvgNumeric {
                            sum: n
                                .clone()
                                .add(plomid_types::Numeric::from_i64(*sum))
                                .unwrap_or_else(|_| n.clone()),
                            count: *count + 1,
                        };
                    } else if let Some(n) = number_f64(v) {
                        *self = Self::AvgF {
                            sum: n + *sum as f64,
                            count: *count + 1,
                        };
                    }
                }
            }
            Self::AvgF { sum, count } => {
                if let Some(n) = value.and_then(number_f64) {
                    *sum += n;
                    *count += 1
                }
            }
            Self::AvgNumeric { sum, count } => {
                if let Some(Value::Numeric(n)) = value {
                    *sum = sum.clone().add(n.clone()).unwrap_or_else(|_| sum.clone());
                    *count += 1
                }
            }
            Self::Min(acc) => {
                if let Some(v) = value {
                    if !v.is_null() {
                        match acc {
                            None => *acc = Some(v.clone()),
                            Some(cur) => {
                                if let Some(ord) = value_cmp(v, cur) {
                                    if ord == std::cmp::Ordering::Less {
                                        *acc = Some(v.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Self::Max(acc) => {
                if let Some(v) = value {
                    if !v.is_null() {
                        match acc {
                            None => *acc = Some(v.clone()),
                            Some(cur) => {
                                if let Some(ord) = value_cmp(v, cur) {
                                    if ord == std::cmp::Ordering::Greater {
                                        *acc = Some(v.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // json_agg/jsonb_agg keep SQL NULL rows (PostgreSQL aggregates a
            // JSON null for each NULL input); object aggregates skip NULL
            // keys at finalization time via the shared helper.
            Self::JsonValues(values) => {
                values.push(value.cloned().unwrap_or(Value::Null));
            }
            Self::JsonPairs(_) => {}
        }
    }

    /// Accumulates the (key, value) pair of an object aggregate. The
    /// single-table fast path evaluates both argument expressions per row,
    /// mirroring the join engine's step_pair handling.
    pub(crate) fn step_pair(&mut self, key: Value, value: Value) {
        if let Self::JsonPairs(pairs) = self {
            if !key.is_null() {
                pairs.push((key, value));
            }
        }
    }

    pub(crate) fn finalize(&self) -> Value {
        match self {
            Self::CountStar(n) => Value::Int8(*n),
            Self::Count(n) => Value::Int8(*n),
            Self::CountDistinct(seen) => Value::Int8(seen.len() as i64),
            // PostgreSQL: SUM(empty) = NULL, not 0. Track whether any non-NULL
            // value was processed so we can distinguish "no input" from "sum is 0".
            Self::SumN(acc, has_any) => {
                if *has_any {
                    Value::Int8(*acc)
                } else {
                    Value::Null
                }
            }
            Self::SumF(acc, has_any) => {
                if *has_any {
                    Value::Numeric(plomid_types::Numeric::from_f64(*acc))
                } else {
                    Value::Null
                }
            }
            Self::SumNumeric(acc, has_any) => {
                if *has_any {
                    Value::Numeric(
                        acc.clone()
                            .unwrap_or_else(|| plomid_types::Numeric::from_i64(0)),
                    )
                } else {
                    Value::Null
                }
            }
            Self::AvgN { sum, count } => {
                if *count == 0 {
                    Value::Null
                } else {
                    // PostgreSQL AVG(integer) returns NUMERIC, not integer.
                    // Perform division in Numeric space to avoid integer truncation,
                    // then normalize so integral means render as "21" not "21.0000".
                    let sum_numeric = plomid_types::Numeric::from_i64(*sum);
                    let count_numeric = plomid_types::Numeric::from_i64(*count);
                    sum_numeric
                        .div(count_numeric)
                        .map(|v| Value::Numeric(v.normalize()))
                        .unwrap_or_else(|_| Value::Numeric(plomid_types::Numeric::from_i64(*sum)))
                }
            }
            Self::AvgF { sum, count } => {
                if *count == 0 {
                    Value::Null
                } else {
                    Value::Numeric(
                        plomid_types::Numeric::from_f64(*sum / *count as f64).normalize(),
                    )
                }
            }
            Self::AvgNumeric { sum, count } => {
                if *count == 0 {
                    Value::Null
                } else {
                    Value::Numeric(
                        sum.clone()
                            .div(plomid_types::Numeric::from_i64(*count))
                            .map(|v| v.normalize())
                            .unwrap_or_else(|_| sum.clone()),
                    )
                }
            }
            Self::Min(v) => v.clone().unwrap_or(Value::Null),
            Self::Max(v) => v.clone().unwrap_or(Value::Null),
            Self::JsonValues(_) | Self::JsonPairs(_) => Value::Null,
        }
    }

    /// Finalizes a JSON aggregate with its function name so the shared
    /// json_aggregate_result / json_object_aggregate_result helpers pick the
    /// json vs jsonb output encoding. Separated from finalize() because the
    /// accumulator alone does not retain the original function name.
    pub(crate) fn finalize_json(&self, name: &str) -> Value {
        match self {
            Self::JsonValues(values) => {
                crate::json::json_aggregate_result(name, values).unwrap_or(Value::Null)
            }
            Self::JsonPairs(pairs) => {
                crate::json::json_object_aggregate_result(name, pairs).unwrap_or(Value::Null)
            }
            _ => self.finalize(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AggregateSlot {
    pub(super) name: String,
    pub(super) arg: Option<Expression>,
    /// Second argument of two-argument aggregates
    /// (`json_object_agg(key, value)` / `jsonb_object_agg(key, value)`).
    /// Stored separately because the join engine's accumulator consumes the
    /// pair together; without it the value half would be dropped.
    pub(super) second_arg: Option<Expression>,
    /// FILTER predicate: only rows where this evaluates to TRUE are passed
    /// to the aggregate. NULL or FALSE results exclude the row.
    pub(super) filter: Option<Expression>,
    /// `DISTINCT` modifier of the aggregate call (`COUNT(DISTINCT expr)`).
    /// Carried so grouped lookup and accumulator construction can tell
    /// `COUNT(x)` apart from `COUNT(DISTINCT x)`, which previously degraded
    /// silently into the same slot.
    pub(super) distinct: bool,
}

impl AggregateSlot {
    pub(crate) fn arg(&self) -> Option<&Expression> {
        self.arg.as_ref()
    }

    pub(crate) fn second_arg(&self) -> Option<&Expression> {
        self.second_arg.as_ref()
    }

    pub(crate) fn filter(&self) -> Option<&Expression> {
        self.filter.as_ref()
    }

    pub(crate) fn distinct(&self) -> bool {
        self.distinct
    }
}

pub(crate) fn collect_aggregate_slots(
    targets: &[SelectTarget],
    having: Option<&Expression>,
) -> Vec<AggregateSlot> {
    let mut slots = Vec::new();
    for target in targets {
        visit_target(target, &mut slots);
    }
    if let Some(expr) = having {
        visit_expr(expr, &mut slots);
    }
    slots
}

pub(crate) fn visit_target(target: &SelectTarget, slots: &mut Vec<AggregateSlot>) {
    match target {
        SelectTarget::All | SelectTarget::Function(_) => {}
        SelectTarget::FunctionCall { name, args } if is_aggregate_function(name) => {
            slots.push(AggregateSlot {
                name: name.clone(),
                arg: args.first().cloned(),
                second_arg: args.get(1).cloned(),
                filter: None,
                // The bare-call shape carries no DISTINCT modifier.
                distinct: false,
            });
        }
        SelectTarget::Expr { expr, .. } => visit_expr(expr, slots),
        SelectTarget::Aliased { target, .. } => visit_target(target, slots),
        _ => {}
    }
}

pub(crate) fn visit_expr(expr: &Expression, slots: &mut Vec<AggregateSlot>) {
    match expr {
        Expression::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } if is_aggregate_function(name) => {
            slots.push(AggregateSlot {
                name: name.clone(),
                arg: args.first().cloned(),
                second_arg: args.get(1).cloned(),
                filter: filter.as_deref().cloned(),
                distinct: *distinct,
            });
            // An aggregate nested inside another aggregate's argument (for
            // example `jsonb_path_query_array(jsonb_agg(payload), ...)`) is
            // still evaluated per input row and accumulated through the same
            // aggregate state. Descend into the arguments so the inner call
            // is registered; the projection lookup matches on the full
            // expression and therefore keeps working.
            for arg in args {
                visit_expr(arg, slots);
            }
        }
        // Aggregates nested inside ordinary scalar calls (for example
        // `jsonb_path_query_array(jsonb_agg(payload), '...')`) are evaluated
        // per input row and accumulated through the query's aggregate state.
        // Without descending here the outer scalar call reaches projection
        // with no registered slot and falls back to "cannot be evaluated
        // here". Recursing preserves the aggregate framework instead of
        // special-casing particular function names.
        Expression::FunctionCall { args, .. } => {
            for arg in args {
                visit_expr(arg, slots);
            }
        }
        Expression::ColumnRef(_) | Expression::Literal(_) | Expression::Star => {}
        Expression::Equal(l, r)
        | Expression::NotEqual(l, r)
        | Expression::Less(l, r)
        | Expression::LessOrEqual(l, r)
        | Expression::Greater(l, r)
        | Expression::GreaterOrEqual(l, r)
        | Expression::And(l, r)
        | Expression::Or(l, r)
        | Expression::Add(l, r)
        | Expression::Subtract(l, r)
        | Expression::Multiply(l, r)
        | Expression::Divide(l, r)
        | Expression::Modulo(l, r)
        | Expression::Concat(l, r) => {
            visit_expr(l, slots);
            visit_expr(r, slots);
        }
        Expression::IsNull(inner)
        | Expression::IsNotNull(inner)
        | Expression::Not(inner)
        | Expression::Negate(inner) => visit_expr(inner, slots),
        _ => {}
    }
}

pub(crate) fn lookup_aggregate(
    slots: &[AggregateSlot],
    accs: &[AggregateAccumulator],
    name: &str,
    arg: Option<&Expression>,
    filter: Option<&Expression>,
    distinct: bool,
) -> Option<Value> {
    let target_name = name.to_uppercase();
    for (i, slot) in slots.iter().enumerate() {
        if slot.name.to_uppercase() != target_name {
            continue;
        }
        if slot.filter().as_ref().map(|e| e as &Expression) != filter {
            continue;
        }
        // DISTINCT is part of a slot's identity: without it
        // `COUNT(x)` and `COUNT(DISTINCT x)` in one query resolve to the
        // same accumulator and one of them answers wrongly.
        if slot.distinct() != distinct {
            continue;
        }
        let same_arg = match (slot.arg.as_ref(), arg) {
            (None, None) => true,
            (Some(Expression::Star), Some(Expression::Star)) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        if same_arg {
            // JSON aggregates need their function name to pick the json vs
            // jsonb encoding; other aggregates finalize from state alone.
            // Routing through finalize_json keeps one shared JSON result path
            // for both the single-table and join engines.
            return Some(accs[i].finalize_json(&slot.name));
        }
    }
    None
}

pub(super) fn evaluate_grouped_expression(
    row: &[Value],
    schema: &TableSchema,
    expr: &Expression,
    slots: &[AggregateSlot],
    accs: &[AggregateAccumulator],
) -> SqlResult<Value> {
    match expr {
        Expression::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } if is_aggregate_function(name) => Ok(lookup_aggregate(
            slots,
            accs,
            name,
            args.first(),
            filter.as_deref(),
            *distinct,
        )
        .unwrap_or(Value::Null)),
        // Scalar calls wrapping aggregates (for example
        // `jsonb_path_query_array(jsonb_agg(payload), '...')`) are evaluated
        // after aggregation: each nested aggregate resolves through the
        // accumulated slots, then the outer scalar function runs over those
        // grouped values. This keeps jsonb_agg on the aggregate path instead
        // of reaching the scalar evaluator's "cannot be evaluated here"
        // fallback, without special-casing individual function names.
        Expression::FunctionCall {
            name,
            args,
            returning,
            null_handling,
            unique_keys,
            ..
        } if !crate::catalog_fn::is_session_function(name)
            && args.iter().any(expression_has_aggregate) =>
        {
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                if expression_has_aggregate(arg) {
                    values.push(evaluate_grouped_expression(row, schema, arg, slots, accs)?);
                } else {
                    values.push(evaluate_expression(row, schema, arg)?);
                }
            }
            if crate::scalar::is_scalar_function(name) {
                return crate::scalar::scalar_function_value_extended(
                    name,
                    &values,
                    returning.clone(),
                    *null_handling,
                    *unique_keys,
                );
            }
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Unsupported,
                format!("function \"{name}\" is not supported in grouped projection"),
            )));
        }
        Expression::Equal(l, r) => {
            let left = evaluate_grouped_expression(row, schema, l, slots, accs)?;
            let right = evaluate_grouped_expression(row, schema, r, slots, accs)?;
            if left.is_null() || right.is_null() {
                Ok(Value::Null)
            } else {
                Ok(Value::Bool(values_equal(&left, &right)))
            }
        }
        Expression::NotEqual(l, r) => {
            let left = evaluate_grouped_expression(row, schema, l, slots, accs)?;
            let right = evaluate_grouped_expression(row, schema, r, slots, accs)?;
            if left.is_null() || right.is_null() {
                Ok(Value::Null)
            } else {
                Ok(Value::Bool(!values_equal(&left, &right)))
            }
        }
        Expression::Less(l, r) => {
            let left = evaluate_grouped_expression(row, schema, l, slots, accs)?;
            let right = evaluate_grouped_expression(row, schema, r, slots, accs)?;
            if left.is_null() || right.is_null() {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(
                value_cmp(&left, &right) == Some(std::cmp::Ordering::Less),
            ))
        }
        Expression::LessOrEqual(l, r) => {
            let left = evaluate_grouped_expression(row, schema, l, slots, accs)?;
            let right = evaluate_grouped_expression(row, schema, r, slots, accs)?;
            if left.is_null() || right.is_null() {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(
                value_cmp(&left, &right) != Some(std::cmp::Ordering::Greater),
            ))
        }
        Expression::Greater(l, r) => {
            let left = evaluate_grouped_expression(row, schema, l, slots, accs)?;
            let right = evaluate_grouped_expression(row, schema, r, slots, accs)?;
            if left.is_null() || right.is_null() {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(
                value_cmp(&left, &right) == Some(std::cmp::Ordering::Greater),
            ))
        }
        Expression::GreaterOrEqual(l, r) => {
            let left = evaluate_grouped_expression(row, schema, l, slots, accs)?;
            let right = evaluate_grouped_expression(row, schema, r, slots, accs)?;
            if left.is_null() || right.is_null() {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(
                value_cmp(&left, &right) != Some(std::cmp::Ordering::Less),
            ))
        }
        _ => evaluate_expression(row, schema, expr),
    }
}
