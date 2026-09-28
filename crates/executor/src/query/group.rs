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
//! Grouped execution: `GROUP BY`, `ORDER BY`, `DISTINCT` and projection.
//!
//! The grouped path materialises one synthetic row per group and folds the
//! grouped targets over it, which keeps grouped projection separate from the
//! plain row projection.

use super::aggregate::{
    collect_aggregate_slots, evaluate_grouped_expression, lookup_aggregate, AggregateAccumulator,
    AggregateSlot,
};
use super::expr::evaluate_expression;
use super::project::{select_column_types, select_columns};
use super::scan::value_cmp;
use crate::catalog_fn::function_value;
use crate::catalog_fn::is_aggregate_function;
use crate::catalog_fn::is_session_function;
use crate::error::{SqlError, SqlResult};
use crate::row::{equality_signature, values_equal};
use crate::util::unqualify;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::Expression;
use plomid_sql::QueryResult;
use plomid_sql::SelectTarget;
use plomid_sql::TableSchema;
use plomid_sql::Value;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_grouped_select(
    schema: &TableSchema,
    targets: &[SelectTarget],
    group_by: Option<plomid_sql::GroupByClause>,
    having: Option<Expression>,
    rows: Vec<Vec<Value>>,
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    let slots = collect_aggregate_slots(targets, having.as_ref());
    let grouping_sets: Vec<Vec<Expression>> = group_by
        .as_ref()
        .map(|g| g.to_sets())
        .unwrap_or_else(|| vec![Vec::new()]);
    // Fast path: single grouping set keeps the existing behavior exactly.
    if grouping_sets.len() == 1 {
        let group_exprs: &[Expression] = &grouping_sets[0];
        return execute_single_grouped_select(
            schema,
            targets,
            group_exprs,
            having.as_ref(),
            rows,
            &slots,
            current_database,
            current_user,
        );
    }
    let mut group_rows: Vec<Vec<Value>> = Vec::new();
    for group_exprs in &grouping_sets {
        let mut rows_for_set = execute_single_grouped_select(
            schema,
            targets,
            group_exprs,
            having.as_ref(),
            rows.clone(),
            &slots,
            current_database,
            current_user,
        )?;
        if let QueryResult::Rows { rows, .. } = &mut rows_for_set {
            group_rows.append(rows);
        }
    }
    let cols = select_columns(schema, targets)?;
    let column_types = select_column_types(schema, targets)?;
    Ok(QueryResult::Rows {
        columns: cols,
        column_types,
        rows: group_rows,
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_single_grouped_select(
    schema: &TableSchema,
    targets: &[SelectTarget],
    group_exprs: &[Expression],
    having: Option<&Expression>,
    rows: Vec<Vec<Value>>,
    slots: &[AggregateSlot],
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    // Incremental path shared with the streaming scan: feed rows one at a
    // time so GROUP BY memory scales with group count, not input size.
    let mut streaming = StreamingGroupedAgg::new(slots, group_exprs);
    for row in &rows {
        streaming.feed(row, schema)?;
    }
    streaming.finish(schema, targets, having, current_database, current_user)
}

/// Incremental GROUP BY accumulator shared by the materialized and streaming
/// scan paths.
///
/// Semantics are exactly those of the former two-pass loop: one hash entry
/// per group holding the group's key values and one accumulator per aggregate
/// slot, fed row-at-a-time. The streaming SELECT path feeds decoded rows
/// directly from `scan_for_each` chunks, so peak memory is
/// `O(groups + chunk)` instead of `O(rows)`.
pub(super) struct StreamingGroupedAgg {
    slots: Vec<AggregateSlot>,
    group_exprs: Vec<Expression>,
    grouped: HashMap<Vec<String>, (Vec<Value>, Vec<AggregateAccumulator>)>,
}

impl StreamingGroupedAgg {
    pub(super) fn new(slots: &[AggregateSlot], group_exprs: &[Expression]) -> Self {
        let mut grouped: HashMap<Vec<String>, (Vec<Value>, Vec<AggregateAccumulator>)> =
            HashMap::new();
        if group_exprs.is_empty() {
            grouped.insert(
                Vec::new(),
                (
                    Vec::new(),
                    slots
                        .iter()
                        .map(|slot| {
                            AggregateAccumulator::new(&slot.name, slot.arg.as_ref(), slot.distinct)
                        })
                        .collect(),
                ),
            );
        }
        Self {
            slots: slots.to_vec(),
            group_exprs: group_exprs.to_vec(),
            grouped,
        }
    }

    pub(super) fn feed(&mut self, row: &[Value], schema: &TableSchema) -> SqlResult<()> {
        let mut key_values = Vec::with_capacity(self.group_exprs.len());
        let mut key_parts = Vec::with_capacity(self.group_exprs.len());
        for expr in &self.group_exprs {
            let value = evaluate_expression(row, schema, expr)?;
            key_parts.push(format!("{:?}", value));
            key_values.push(value);
        }
        let slots = &self.slots;
        let (_, accs) = self.grouped.entry(key_parts).or_insert_with(|| {
            (
                key_values.clone(),
                slots
                    .iter()
                    .map(|slot| {
                        AggregateAccumulator::new(&slot.name, slot.arg.as_ref(), slot.distinct)
                    })
                    .collect(),
            )
        });
        for (index, slot) in slots.iter().enumerate() {
            let passes_filter = match slot.filter().as_ref() {
                None => true,
                Some(filter_expr) => matches!(
                    evaluate_expression(row, schema, filter_expr)?,
                    Value::Bool(true)
                ),
            };
            if !passes_filter {
                continue;
            }
            if crate::json::is_json_object_aggregate(&slot.name) {
                let key = match slot.arg().as_ref() {
                    Some(Expression::Star) | None => Value::Null,
                    Some(arg) => evaluate_expression(row, schema, arg)?,
                };
                let value = match slot.second_arg().as_ref() {
                    Some(arg) => evaluate_expression(row, schema, arg)?,
                    None => Value::Null,
                };
                accs[index].step_pair(key, value);
            } else {
                accs[index].step(slot.arg(), row, schema)?;
            }
        }
        Ok(())
    }

    pub(super) fn finish(
        self,
        schema: &TableSchema,
        targets: &[SelectTarget],
        having: Option<&Expression>,
        current_database: &str,
        current_user: &str,
    ) -> SqlResult<QueryResult> {
        let mut group_rows: Vec<Vec<Value>> = Vec::new();
        for (key_vals, accs) in self.grouped.into_values() {
            if let Some(having_expr) = having {
                let synthetic = build_group_synthetic(schema, &key_vals, &self.group_exprs);
                let result = evaluate_grouped_expression(
                    &synthetic,
                    schema,
                    having_expr,
                    &self.slots,
                    &accs,
                )?;
                if !matches!(result, Value::Bool(true)) {
                    continue;
                }
            }
            let repr = key_vals.clone();
            let evaluated = project_grouped_row(
                &repr,
                &self.group_exprs,
                schema,
                targets,
                &self.slots,
                &accs,
                current_database,
                current_user,
            )?;
            group_rows.push(evaluated);
        }
        let cols = select_columns(schema, targets)?;
        let column_types = select_column_types(schema, targets)?;
        Ok(QueryResult::Rows {
            columns: cols,
            column_types,
            rows: group_rows,
        })
    }
}

pub(super) fn sort_rows(
    rows: Vec<Vec<Value>>,
    schema: &TableSchema,
    order_by: &[plomid_sql::OrderByItem],
    top_k: Option<usize>,
) -> SqlResult<Vec<Vec<Value>>> {
    let mut keyed = rows
        .into_iter()
        .map(|row| {
            let keys = order_by
                .iter()
                .map(|item| evaluate_expression(&row, schema, &item.expr))
                .collect::<SqlResult<Vec<_>>>()?;
            Ok((row, keys))
        })
        .collect::<SqlResult<Vec<_>>>()?;

    if let Some(limit) = top_k {
        if limit == 0 {
            return Ok(Vec::new());
        }
        if limit < keyed.len() {
            let split = limit - 1;
            keyed
                .select_nth_unstable_by(split, |(_, a), (_, b)| compare_order_keys(a, b, order_by));
            keyed.truncate(limit);
        }
    }
    keyed.sort_by(|(_, a), (_, b)| compare_order_keys(a, b, order_by));
    Ok(keyed.into_iter().map(|(row, _)| row).collect())
}

fn compare_order_keys(
    a: &[Value],
    b: &[Value],
    order_by: &[plomid_sql::OrderByItem],
) -> std::cmp::Ordering {
    for (index, item) in order_by.iter().enumerate() {
        let va = &a[index];
        let vb = &b[index];
        let nulls_first = item.nulls_first.unwrap_or(item.descending);
        let cmp = match (va.is_null(), vb.is_null()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) if nulls_first => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) if nulls_first => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => value_cmp(va, vb).unwrap_or(std::cmp::Ordering::Equal),
        };
        let cmp = if item.descending && !va.is_null() && !vb.is_null() {
            cmp.reverse()
        } else {
            cmp
        };
        if cmp != std::cmp::Ordering::Equal {
            return cmp;
        }
    }
    std::cmp::Ordering::Equal
}

/// Applies PostgreSQL `DISTINCT ON` semantics to already `ORDER BY`-sorted
/// source (storage) rows: evaluate the ON expressions per row and keep the
/// first row of each group. Because rows are sorted, the survivor is the
/// first row per group under the ORDER BY ordering.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_distinct_on(
    rows: Vec<Vec<Value>>,
    schema: &TableSchema,
    distinct_on: &[Expression],
    _order_by: &[plomid_sql::OrderByItem],
    _targets: &[SelectTarget],
    _current_database: &str,
    _current_user: &str,
) -> SqlResult<Vec<Vec<Value>>> {
    // Hashed path with the authoritative `values_equal` as the final word:
    // bucket candidate keys by `equality_signature` (implied by equality, so
    // no duplicate is ever missed), then confirm inside the bucket. This is
    // `O(rows)` instead of the previous `O(rows x distinct)` linear scan.
    let mut buckets: HashMap<u64, Vec<Vec<Value>>> = HashMap::new();
    let mut out: Vec<Vec<Value>> = Vec::new();
    for row in rows {
        let mut key = Vec::with_capacity(distinct_on.len());
        for expr in distinct_on {
            key.push(evaluate_expression(&row, schema, expr)?);
        }
        let sig = equality_signature(&key);
        let bucket = buckets.entry(sig).or_default();
        let duplicate = bucket.iter().any(|prev| {
            prev.len() == key.len() && prev.iter().zip(key.iter()).all(|(l, r)| values_equal(l, r))
        });
        if !duplicate {
            bucket.push(key);
            out.push(row);
        }
    }
    Ok(out)
}

/// Bounded top-K heap for `ORDER BY ... LIMIT k`.
///
/// Holds at most `cap` entries as a max-heap over the ORDER BY comparator
/// (root = current worst), so streaming `N` rows costs `O(N log k)` time and
/// `O(k)` memory instead of materializing all `N` rows. NULL ordering and
/// direction handling delegate to [`compare_order_keys`], the same comparator
/// `sort_rows` uses, so heap output sorted at the end matches the full sort
/// for the retained prefix.
pub(super) struct TopKHeap {
    cap: usize,
    entries: Vec<(Vec<Value>, Vec<Value>)>,
}

impl TopKHeap {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            cap,
            entries: Vec::new(),
        }
    }

    /// Offers one decoded `(row, keys)` pair to the heap.
    pub(super) fn offer(
        &mut self,
        row: Vec<Value>,
        keys: Vec<Value>,
        order_by: &[plomid_sql::OrderByItem],
    ) {
        if self.cap == 0 {
            return;
        }
        if self.entries.len() < self.cap {
            self.entries.push((row, keys));
            if self.entries.len() == self.cap {
                self.build_max_heap(order_by);
            }
            return;
        }
        // Full: keep the candidate only when it sorts before the current
        // worst (the heap root). Ties keep the incumbent, matching the
        // full-sort prefix up to tie order, which SQL leaves unspecified
        // without a unique tiebreaker.
        let dominated = match self.entries.first() {
            Some((_, worst_keys)) => {
                compare_order_keys(&keys, worst_keys, order_by) != std::cmp::Ordering::Less
            }
            None => false,
        };
        if dominated {
            return;
        }
        if let Some(root) = self.entries.first_mut() {
            *root = (row, keys);
            let len = self.entries.len();
            self.sift_down_range(0, len, order_by);
        }
    }

    /// Sorted top-K rows, ascending per ORDER BY.
    pub(super) fn into_sorted_rows(
        mut self,
        order_by: &[plomid_sql::OrderByItem],
    ) -> Vec<Vec<Value>> {
        if self.entries.len() < self.cap {
            // Never reached capacity: plain sort of the retained prefix.
            self.entries
                .sort_by(|(_, a), (_, b)| compare_order_keys(a, b, order_by));
        } else {
            // Heap-sort the bounded heap so output order matches `sort_rows`.
            let len = self.entries.len();
            for i in (0..len / 2).rev() {
                self.sift_down_range(i, len, order_by);
            }
            let mut out = Vec::with_capacity(len);
            let mut active = len;
            while active > 0 {
                self.entries.swap(0, active - 1);
                let (row, _) = self.entries.pop().expect("heap entry");
                out.push(row);
                active -= 1;
                if active > 0 {
                    self.sift_down_range(0, active, order_by);
                }
            }
            out.reverse();
            return out;
        }
        self.entries.into_iter().map(|(row, _)| row).collect()
    }

    fn build_max_heap(&mut self, order_by: &[plomid_sql::OrderByItem]) {
        let len = self.entries.len();
        for i in (0..len / 2).rev() {
            self.sift_down_range(i, len, order_by);
        }
    }

    fn sift_down_range(&mut self, mut i: usize, len: usize, order_by: &[plomid_sql::OrderByItem]) {
        loop {
            let left = 2 * i + 1;
            let right = left + 1;
            let mut worst = i;
            if left < len
                && compare_order_keys(&self.entries[left].1, &self.entries[worst].1, order_by)
                    == std::cmp::Ordering::Greater
            {
                worst = left;
            }
            if right < len
                && compare_order_keys(&self.entries[right].1, &self.entries[worst].1, order_by)
                    == std::cmp::Ordering::Greater
            {
                worst = right;
            }
            if worst == i {
                break;
            }
            self.entries.swap(i, worst);
            i = worst;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn project_grouped_row(
    group_key: &[Value],
    group_exprs: &[Expression],
    schema: &TableSchema,
    targets: &[SelectTarget],
    slots: &[AggregateSlot],
    accs: &[AggregateAccumulator],
    current_database: &str,
    current_user: &str,
) -> SqlResult<Vec<Value>> {
    let synthetic: Vec<Value> = build_group_synthetic(schema, group_key, group_exprs);
    if targets.len() == 1 && matches!(targets[0], SelectTarget::All) {
        return Ok(synthetic);
    }
    let mut projected = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            SelectTarget::All | SelectTarget::QualifiedStar { .. } => {
                projected.extend(synthetic.iter().cloned());
            }
            SelectTarget::Expr { expr, .. } => projected.push(evaluate_grouped_expression(
                &synthetic, schema, expr, slots, accs,
            )?),
            SelectTarget::Function(name) => {
                projected.push(function_value(name, current_database, current_user)?)
            }
            SelectTarget::FunctionCall { name, args } => {
                if is_aggregate_function(name) {
                    // Unqualified `SELECT agg(...)` targets carry no FILTER, so
                    // pass `None` explicitly. Filtered forms arrive as
                    // `SelectTarget::Expr` and flow through
                    // `evaluate_grouped_expression`, which matches on the
                    // full FILTER expression.
                    projected.push(
                        lookup_aggregate(slots, accs, name, args.first(), None, false)
                            .unwrap_or(Value::Null),
                    );
                } else if is_session_function(name) {
                    projected.push(function_value(name, current_database, current_user)?)
                } else {
                    let argument = args
                        .first()
                        .and_then(|e| match e {
                            Expression::Literal(Value::Text(v)) => Some(v.as_str()),
                            _ => None,
                        })
                        .unwrap_or("");
                    let _ = (argument, slots, accs);
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::Unsupported,
                        format!("function \"{name}\" is not supported in projection"),
                    )));
                }
            }
            SelectTarget::WindowFunction { name, .. } => {
                let _ = name;
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Unsupported,
                    "window functions must be evaluated by the general query engine",
                )));
            }
            SelectTarget::Aliased { target, .. } => {
                projected.extend(project_grouped_row(
                    group_key,
                    group_exprs,
                    schema,
                    std::slice::from_ref(target),
                    slots,
                    accs,
                    current_database,
                    current_user,
                )?);
            }
        }
    }
    Ok(projected)
}

fn build_group_synthetic(
    schema: &TableSchema,
    group_key: &[Value],
    group_exprs: &[Expression],
) -> Vec<Value> {
    let n = schema.columns.len();
    let mut synthetic = vec![Value::Null; n];
    for (i, expr) in group_exprs.iter().enumerate() {
        if let Some(val) = group_key.get(i) {
            if let Expression::ColumnRef(name) = expr {
                if let Ok(idx) = schema.column_index(unqualify(name)) {
                    synthetic[idx] = val.clone();
                }
            }
        }
    }
    synthetic
}

pub(super) fn project_row(
    row: &[Value],
    schema: &TableSchema,
    targets: &[SelectTarget],
    current_database: &str,
    current_user: &str,
) -> SqlResult<Vec<Value>> {
    if targets.len() == 1 && matches!(targets[0], SelectTarget::All) {
        return Ok(row.to_vec());
    }
    let mut projected = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            SelectTarget::All | SelectTarget::QualifiedStar { .. } => {
                projected.extend_from_slice(row);
            }
            SelectTarget::Expr { expr, .. } => {
                projected.push(evaluate_expression(row, schema, expr)?)
            }
            SelectTarget::Function(name) => {
                projected.push(function_value(name, current_database, current_user)?)
            }
            SelectTarget::FunctionCall { name, args: _ } => {
                if name.eq_ignore_ascii_case("count") {
                    projected.push(Value::Int8(1));
                } else {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::Unsupported,
                        format!("function \"{}\" is not supported", name),
                    )));
                }
            }
            SelectTarget::WindowFunction { name, .. } => {
                let _ = name;
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Unsupported,
                    "window functions must be evaluated by the general query engine",
                )));
            }
            SelectTarget::Aliased { target, .. } => {
                projected.extend(project_row(
                    row,
                    schema,
                    std::slice::from_ref(target),
                    current_database,
                    current_user,
                )?);
            }
        }
    }
    Ok(projected)
}
