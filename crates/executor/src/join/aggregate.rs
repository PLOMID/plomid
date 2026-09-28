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
//! Aggregate accumulators for the join engine.

use crate::catalog_fn::is_aggregate_function;
use crate::query::AggregateAccumulator;
use plomid_sql::{Expression, OrderByItem, SelectTarget, Value};
use std::collections::HashSet;

#[derive(Clone)]
pub(super) struct JoinAggregate {
    pub(super) name: String,
    pub(super) acc: AggregateAccumulator,
    pub(super) distinct: bool,
    pub(super) seen: HashSet<String>,
    pub(super) filter: Option<Expression>,
    pub(super) values: Vec<Value>,
    pub(super) pairs: Vec<(Value, Value)>,
    pub(super) separator: String,
    pub(super) separator_expr: Option<Expression>,
    pub(super) order_by: Vec<OrderByItem>,
    /// `NULL ON NULL` / `ABSENT ON NULL` for SQL/JSON constructors.
    pub(super) null_handling: Option<plomid_sql::NullHandling>,
    /// `WITH UNIQUE KEYS` / `WITHOUT UNIQUE KEYS` for JSON_OBJECT.
    pub(super) unique_keys: Option<bool>,
}

impl JoinAggregate {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        name: &str,
        arg: Option<&Expression>,
        distinct: bool,
        filter: Option<&Expression>,
        separator: Option<&Expression>,
        order_by: &[OrderByItem],
        null_handling: Option<plomid_sql::NullHandling>,
        unique_keys: Option<bool>,
    ) -> Self {
        Self {
            name: name.to_ascii_lowercase(),
            acc: AggregateAccumulator::new(name, arg, false),
            distinct,
            seen: HashSet::new(),
            filter: filter.cloned(),
            values: Vec::new(),
            pairs: Vec::new(),
            separator: ",".into(),
            separator_expr: separator.cloned(),
            order_by: order_by.to_vec(),
            null_handling,
            unique_keys,
        }
    }

    pub(super) fn step(&mut self, value: Option<Value>) {
        if matches!(self.name.as_str(), "string_agg" | "array_agg") {
            if let Some(value) = value.as_ref().filter(|value| !value.is_null()) {
                if self.distinct && !self.seen.insert(value.to_sql_text()) {
                    return;
                }
                self.values.push(value.clone());
            }
            return;
        }
        if crate::json::is_json_aggregate(&self.name) {
            // json_agg/jsonb_agg keep SQL NULL; the object aggregates skip it.
            if crate::json::is_json_object_aggregate(&self.name) {
                return;
            }
            self.values.push(value.unwrap_or(Value::Null));
            return;
        }
        if self.distinct {
            if let Some(value) = value.as_ref().filter(|value| !value.is_null()) {
                if !self.seen.insert(value.to_sql_text()) {
                    return;
                }
            }
        }
        self.acc.step_value(value.as_ref());
    }

    /// Accumulates the (key, value) argument pair of an object aggregate.
    pub(super) fn step_pair(&mut self, key: Value, value: Value) {
        if key.is_null() {
            return;
        }
        self.pairs.push((key, value));
    }

    pub(super) fn finalize(&self) -> Value {
        if self.name == "string_agg" {
            if self.values.is_empty() {
                return Value::Null;
            }
            return Value::Text(
                self.values
                    .iter()
                    .map(Value::to_sql_text)
                    .collect::<Vec<_>>()
                    .join(&self.separator),
            );
        }
        if self.name == "array_agg" {
            return if self.values.is_empty() {
                Value::Null
            } else {
                Value::Array {
                    element_oid: self
                        .values
                        .iter()
                        .find_map(plomid_sql::value_pg_type)
                        .map(|ty| ty.oid())
                        .unwrap_or(plomid_types::TypeOid::TEXT),
                    elements: self.values.clone(),
                }
            };
        }
        if crate::json::is_json_aggregate(&self.name) {
            if crate::json::is_json_object_aggregate(&self.name) {
                return crate::json::json_object_aggregate_result(&self.name, &self.pairs)
                    .unwrap_or(Value::Null);
            }
            return crate::json::json_aggregate_result(&self.name, &self.values)
                .unwrap_or(Value::Null);
        }
        self.acc.finalize()
    }
}

/// One collected aggregate slot: (function name, argument, accumulator).
pub(super) type AggregateSlot = (String, Option<Expression>, JoinAggregate);

/// True when the slot matches the given aggregate name/argument pair.
pub(super) fn slot_matches(
    slot: &(String, Option<Expression>, JoinAggregate),
    name: &str,
    arg: Option<&Expression>,
) -> bool {
    let (slot_name, slot_arg, _) = slot;
    if !slot_name.eq_ignore_ascii_case(name) {
        return false;
    }
    match (slot_arg.as_ref(), arg) {
        (None, None) => true,
        (Some(Expression::Star), Some(Expression::Star)) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

pub(super) fn slot_matches_expression(slot: &AggregateSlot, expr: &Expression) -> bool {
    let Expression::FunctionCall {
        name,
        args,
        distinct,
        filter,
        order_by: expr_order_by,
        returning: _,
        null_handling: _,
        unique_keys: _,
    } = expr
    else {
        return false;
    };
    slot_matches(slot, name, args.first())
        && slot.2.distinct == *distinct
        && slot.2.filter.as_ref() == filter.as_deref()
        && slot_matches_order_by(slot, expr_order_by)
}

/// True when the aggregate slot's ORDER BY (if any) matches the given
/// expression-level ORDER BY. Slots are matched as identical only when
/// their ORDER BY clauses are structurally equivalent.
pub(super) fn slot_matches_order_by(slot: &AggregateSlot, expr_order_by: &[OrderByItem]) -> bool {
    let (_, _, slot_agg) = slot;
    let slot_order_by = &slot_agg.order_by;
    if slot_order_by.is_empty() != expr_order_by.is_empty() {
        return false;
    }
    slot_order_by.len() == expr_order_by.len()
        && slot_order_by.iter().zip(expr_order_by).all(|(a, b)| {
            // Match on the expression structure and the ordering direction
            // / NULLs-first flag. Exact structural equality of the ORDER
            // BY expression is sufficient for slot deduplication here.
            a.expr == b.expr && a.descending == b.descending && a.nulls_first == b.nulls_first
        })
}

/// Adds an aggregate slot unless an identical one is already present.
pub(super) fn push_slot(slots: &mut Vec<AggregateSlot>, name: &str, arg: Option<&Expression>) {
    push_slot_with(slots, name, arg, false, None, None, &[][..], None, None);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_slot_with(
    slots: &mut Vec<AggregateSlot>,
    name: &str,
    arg: Option<&Expression>,
    distinct: bool,
    filter: Option<&Expression>,
    separator: Option<&Expression>,
    order_by: &[OrderByItem],
    null_handling: Option<plomid_sql::NullHandling>,
    unique_keys: Option<bool>,
) {
    if slots.iter().any(|slot| {
        slot_matches(slot, name, arg)
            && slot.2.distinct == distinct
            && slot.2.filter.as_ref() == filter
            && slot.2.separator_expr.as_ref() == separator
            && slot_matches_order_by(slot, order_by)
    }) {
        return;
    }
    slots.push((
        name.to_string(),
        arg.cloned(),
        JoinAggregate::new(
            name,
            arg,
            distinct,
            filter,
            separator,
            order_by,
            null_handling,
            unique_keys,
        ),
    ));
}

/// Collects aggregate function calls inside an expression into `slots`.
pub(super) fn collect_aggregate_slots(expr: &Expression, slots: &mut Vec<AggregateSlot>) {
    match expr {
        Expression::FunctionCall {
            name,
            args,
            distinct,
            filter,
            order_by,
            returning: _,
            null_handling,
            unique_keys,
        } if is_aggregate_function(name) => {
            push_slot_with(
                slots,
                name,
                args.first(),
                *distinct,
                filter.as_deref(),
                args.get(1),
                order_by,
                *null_handling,
                *unique_keys,
            );
            for arg in args {
                collect_aggregate_slots(arg, slots);
            }
        }
        Expression::FunctionCall { args, .. } => {
            for arg in args {
                collect_aggregate_slots(arg, slots);
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
        | Expression::Concat(l, r)
        | Expression::IsDistinctFrom(l, r)
        | Expression::NullIf(l, r) => {
            collect_aggregate_slots(l, slots);
            collect_aggregate_slots(r, slots);
        }
        Expression::IsNull(i)
        | Expression::IsNotNull(i)
        | Expression::Not(i)
        | Expression::Negate(i) => collect_aggregate_slots(i, slots),
        Expression::In {
            expr,
            list,
            subquery,
            ..
        } => {
            collect_aggregate_slots(expr, slots);
            for item in list {
                collect_aggregate_slots(item, slots);
            }
            let _ = subquery;
        }
        Expression::Between {
            expr, low, high, ..
        } => {
            collect_aggregate_slots(expr, slots);
            collect_aggregate_slots(low, slots);
            collect_aggregate_slots(high, slots);
        }
        Expression::Like { expr, pattern, .. } => {
            collect_aggregate_slots(expr, slots);
            collect_aggregate_slots(pattern, slots);
        }
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            if let Some(op) = operand {
                collect_aggregate_slots(op, slots);
            }
            for (cond, value) in whens {
                collect_aggregate_slots(cond, slots);
                collect_aggregate_slots(value, slots);
            }
            if let Some(def) = default {
                collect_aggregate_slots(def, slots);
            }
        }
        Expression::Coalesce(args) => {
            for arg in args {
                collect_aggregate_slots(arg, slots);
            }
        }
        Expression::WindowFunction { args, .. } => {
            for arg in args {
                collect_aggregate_slots(arg, slots);
            }
        }
        _ => {}
    }
}

pub(super) fn collect_target_aggregate_slots(
    target: &SelectTarget,
    slots: &mut Vec<AggregateSlot>,
) {
    match target {
        SelectTarget::FunctionCall { name, args, .. } if is_aggregate_function(name) => {
            push_slot_with(
                slots,
                name,
                args.first(),
                false,
                None,
                args.get(1),
                &[][..],
                None,
                None,
            );
        }
        SelectTarget::Expr { expr, .. } => collect_aggregate_slots(expr, slots),
        SelectTarget::Aliased { target, .. } => collect_target_aggregate_slots(target, slots),
        _ => {}
    }
}

/// Concatenates two row slices into one owned row.
pub(super) fn concat_slices(left: &[Value], right: &[Value]) -> Vec<Value> {
    let mut out = Vec::with_capacity(left.len() + right.len());
    out.extend_from_slice(left);
    out.extend_from_slice(right);
    out
}
