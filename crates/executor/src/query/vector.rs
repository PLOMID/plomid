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
//! Vector columnar planning for the single-table SELECT fast path.
//!
//! [`try_vector_columnar`] translates supported aggregate shapes into a
//! [`VectorRequest`](plomid_columnar::vector::VectorRequest) and maps the
//! typed result back to SQL `Value`s. Anything outside the supported subset
//! returns `Ok(None)` and the caller keeps the scalar columnar path, which
//! is behaviorally identical by construction (see the vector module's
//! exactness contract).
//!
//! Supported shapes (phase 1):
//!
//! * top-level `COUNT(*)` / `COUNT(col)` / `COUNT(DISTINCT col)` /
//!   `SUM` / `MIN` / `MAX` / `AVG` over plain column references, each with
//!   an optional `FILTER (WHERE ...)`;
//! * optional `GROUP BY` over plain column references;
//! * optional `WHERE` that lowers to a prunable predicate;
//! * bare `Literal` targets and group-key column references (mirroring the
//!   scalar grouped projection: unresolvable column references yield `NULL`).
//!
//! Everything else — `HAVING`, `ORDER BY`, `DISTINCT ON`, nested/scalar
//! expressions over aggregates, JSON aggregates, distinct sums, star
//! projections — declines. Result mapping reuses the scalar path's own
//! helpers (`select_columns`, `select_column_types`,
//! `columnar_row_to_values`) and mirrors the scalar accumulator
//! finalization formulas, so the only behavioral surface is which physical
//! path computed the numbers.

use super::project::{select_column_types, select_columns};
use super::scan::{columnar_row_to_values, normalize_time_pruning_expression};
use crate::catalog_fn::is_aggregate_function;
use crate::error::{SqlError, SqlResult};
use crate::util::unqualify;
use plomid_columnar::vector::{VectorAcc, VectorAggKind, VectorAggregate, VectorRequest};
use plomid_columnar::{lower_sql_expression, ColumnarStore, PrunePredicate};
use plomid_sql::{Expression, OrderByItem, QueryResult, SelectTarget, TableSchema, Value};
use plomid_storage::{Field, Row, StorageEngine};
use std::collections::{BTreeMap, BTreeSet};

/// Attempts the vector columnar path for one single-table SELECT.
///
/// Returns `Ok(None)` when any part of the statement falls outside the
/// supported subset; the caller then runs the scalar columnar path.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_vector_columnar<E: StorageEngine>(
    engine: &mut E,
    schema: &TableSchema,
    targets: &[SelectTarget],
    where_expr: Option<&Expression>,
    group_by: Option<&plomid_sql::GroupByClause>,
    having: Option<&Expression>,
    order_by: &[OrderByItem],
    limit: Option<usize>,
    offset: Option<usize>,
    distinct_on: &[Expression],
    pos_columns: &BTreeMap<String, plomid_core::ColumnId>,
    store: &ColumnarStore,
    object: plomid_core::ObjectId,
) -> SqlResult<Option<QueryResult>> {
    // Vectorization covers pure unordered aggregation. Grouped ordering,
    // HAVING, and DISTINCT ON keep the scalar path (GROUP BY + ORDER BY is
    // already routed to the general engine above this hook).
    if having.is_some() || !order_by.is_empty() || !distinct_on.is_empty() {
        return Ok(None);
    }
    let group_sets = group_by.map(|clause| clause.to_sets()).unwrap_or_default();
    if group_sets.len() > 1 {
        return Ok(None);
    }
    let group_exprs: &[Expression] = group_sets.first().map(Vec::as_slice).unwrap_or(&[]);

    // Resolve GROUP BY keys to positional columnar identities.
    let mut group_ids: Vec<plomid_core::ColumnId> = Vec::with_capacity(group_exprs.len());
    let mut group_indexes: Vec<usize> = Vec::with_capacity(group_exprs.len());
    for expr in group_exprs {
        let Expression::ColumnRef(name) = expr else {
            return Ok(None);
        };
        let Ok(index) = schema.column_index(unqualify(name)) else {
            return Ok(None);
        };
        group_ids.push(plomid_core::ColumnId::new(index as u64));
        group_indexes.push(index);
    }

    // Translate the WHERE clause with the exact same normalization and
    // lowering the scalar scan uses; an unprunable predicate declines.
    let filter = match where_expr {
        Some(expr) => {
            let normalized = normalize_time_pruning_expression(expr, schema);
            match lower_sql_expression(&normalized, pos_columns).predicate() {
                Some(predicate) => Some(predicate.clone()),
                None => return Ok(None),
            }
        }
        None => None,
    };

    // Translate targets, collecting aggregates in target order (duplicates
    // computed twice, exactly like the scalar slot list).
    let mut aggregates: Vec<VectorAggregate> = Vec::new();
    let mut planned: Vec<PlannedTarget> = Vec::with_capacity(targets.len());
    for target in targets {
        let inner = match target {
            SelectTarget::Aliased { target, .. } => target.as_ref(),
            other => other,
        };
        match plan_target(inner, schema, pos_columns, &group_indexes)? {
            Some(plan) => {
                if let PlannedTarget::AggCall {
                    kind,
                    column,
                    filter,
                    raw,
                } = plan
                {
                    let agg_index = aggregates.len();
                    aggregates.push(VectorAggregate {
                        kind,
                        column,
                        filter,
                    });
                    // Raw-capable aggregates over text/bytes columns read the
                    // byte-ordered state; integer columns read typed state.
                    planned.push(PlannedTarget::Agg { agg_index, raw });
                } else {
                    planned.push(plan);
                }
            }
            None => return Ok(None),
        }
    }

    // Text-ordering verification: every string-compared column must be a
    // plain text-family type (or bytea for bytes), otherwise byte order is
    // not value order and the request declines. IS NULL leaves and integer
    // leaves need no check.
    if !verify_text_leaves(schema, filter.as_ref(), &aggregates)? {
        return Ok(None);
    }

    let request = VectorRequest {
        aggregates,
        filter,
        group_by: group_ids.clone(),
        batch_rows: 0,
        // Verified above (vacuously true when no string leaves exist).
        text_ordering_exact: true,
    };
    let Some(vector) = plomid_columnar::vector::vector_aggregate(store, engine, object, &request)?
    else {
        return Ok(None);
    };
    tracing::debug!(
        target: "sql::columnar",
        segments_considered = vector.segments_considered,
        segments_skipped = vector.segments_skipped,
        rows_examined = vector.rows_examined,
        rows_skipped = vector.rows_skipped,
        columns_read = vector.columns_read,
        groups = vector.groups.len(),
        "columnar vector aggregation"
    );
    let cols = select_columns(schema, targets)?;
    let column_types = select_column_types(schema, targets)?;
    // Result mapping needs each aggregate's kind/column/raw flag by
    // position (planned Agg nodes reference positions).
    let mut kinds_by_agg: Vec<(VectorAggKind, Option<plomid_core::ColumnId>, bool)> = request
        .aggregates
        .iter()
        .map(|aggregate| (aggregate.kind, aggregate.column, false))
        .collect();
    for plan in &planned {
        if let PlannedTarget::Agg { agg_index, raw } = plan {
            if let Some(slot) = kinds_by_agg.get_mut(*agg_index) {
                slot.2 = *raw;
            }
        }
    }
    let mut rows: Vec<Vec<Value>> = Vec::with_capacity(vector.groups.len().max(1));
    if group_exprs.is_empty() {
        let Some(group) = vector.groups.first() else {
            return Ok(None);
        };
        rows.push(project_vector_row(
            schema,
            &planned,
            &kinds_by_agg,
            &[],
            &group_ids,
            group,
        )?);
    } else {
        for group in &vector.groups {
            rows.push(project_vector_row(
                schema,
                &planned,
                &kinds_by_agg,
                &group.keys,
                &group_ids,
                group,
            )?);
        }
    }
    if offset.unwrap_or(0) > 0 || limit.is_some() {
        rows = rows
            .into_iter()
            .skip(offset.unwrap_or(0))
            .take(limit.unwrap_or(usize::MAX))
            .collect();
    }
    Ok(Some(QueryResult::Rows {
        columns: cols,
        column_types,
        rows,
    }))
}

/// A translated SELECT target.
#[derive(Clone, Debug)]
enum PlannedTarget {
    /// Aggregate call resolved to `request.aggregates[agg_index]`; `raw`
    /// selects the byte-ordered accumulator state for text/bytes columns.
    Agg { agg_index: usize, raw: bool },
    /// Aggregate call awaiting index assignment.
    AggCall {
        kind: VectorAggKind,
        column: Option<plomid_core::ColumnId>,
        filter: Option<PrunePredicate>,
        raw: bool,
    },
    /// `GROUP BY` key reference, by position in the request key list.
    GroupKey { key_position: usize },
    /// Literal target (evaluates to itself, as in grouped projection).
    Literal(Value),
    /// Unresolvable column reference (scalar grouped projection yields
    /// `NULL` for these over the synthetic row).
    Null,
}

/// Translates one (unaliased) target, or `None` to decline.
fn plan_target(
    target: &SelectTarget,
    schema: &TableSchema,
    pos_columns: &BTreeMap<String, plomid_core::ColumnId>,
    group_indexes: &[usize],
) -> SqlResult<Option<PlannedTarget>> {
    match target {
        SelectTarget::Expr { expr, .. } => plan_expr(expr, schema, pos_columns, group_indexes),
        SelectTarget::FunctionCall { name, args } if is_aggregate_function(name) => {
            plan_agg_call(name, args, false, None, schema)
        }
        _ => Ok(None),
    }
}

/// Translates one expression target.
fn plan_expr(
    expr: &Expression,
    schema: &TableSchema,
    pos_columns: &BTreeMap<String, plomid_core::ColumnId>,
    group_indexes: &[usize],
) -> SqlResult<Option<PlannedTarget>> {
    match expr {
        Expression::FunctionCall {
            name,
            args,
            distinct,
            filter,
            order_by,
            ..
        } if is_aggregate_function(name) => {
            if !order_by.is_empty() {
                return Ok(None);
            }
            let lowered = match filter.as_deref() {
                Some(predicate) => {
                    let normalized = normalize_time_pruning_expression(predicate, schema);
                    match lower_sql_expression(&normalized, pos_columns).predicate() {
                        Some(lowered) => Some(lowered.clone()),
                        None => return Ok(None),
                    }
                }
                None => None,
            };
            plan_agg_call(name, args, *distinct, lowered, schema)
        }
        Expression::ColumnRef(name) => {
            let Ok(index) = schema.column_index(unqualify(name)) else {
                return Ok(Some(PlannedTarget::Null));
            };
            match group_indexes.iter().position(|key| *key == index) {
                Some(key_position) => Ok(Some(PlannedTarget::GroupKey { key_position })),
                // Mirrors the scalar synthetic row: a non-key column
                // reference over grouped input evaluates to NULL.
                None => Ok(Some(PlannedTarget::Null)),
            }
        }
        Expression::Literal(value) => Ok(Some(PlannedTarget::Literal(value.clone()))),
        _ => Ok(None),
    }
}

/// Translates one aggregate call, or `None` to decline.
///
/// `SUM`/`AVG` serve integer columns only. `MIN`/`MAX`/`COUNT DISTINCT`
/// serve integer columns (typed state) and text/bytea columns (raw
/// byte-ordered state); anything else declines. `COUNT(col)` serves every
/// column through the null bitmap.
fn plan_agg_call(
    name: &str,
    args: &[Expression],
    distinct: bool,
    filter: Option<PrunePredicate>,
    schema: &TableSchema,
) -> SqlResult<Option<PlannedTarget>> {
    let lowered = name.to_ascii_lowercase();
    // COUNT(*) — the only Star shape answered here.
    if lowered == "count" && !distinct && matches!(args, [Expression::Star]) {
        return Ok(Some(PlannedTarget::AggCall {
            kind: VectorAggKind::CountStar,
            column: None,
            filter,
            raw: false,
        }));
    }
    let [arg] = args else {
        return Ok(None);
    };
    let Expression::ColumnRef(name) = arg else {
        return Ok(None);
    };
    let Ok(index) = schema.column_index(unqualify(name)) else {
        return Ok(None);
    };
    let column = plomid_core::ColumnId::new(index as u64);
    let oid = schema.columns[index].col_type.type_oid;
    let is_int = matches!(
        oid,
        plomid_types::TypeOid::INT2 | plomid_types::TypeOid::INT4 | plomid_types::TypeOid::INT8
    );
    // Byte-ordered raw state serves plain text and binary columns, whose
    // flushed bytes are exactly the compared contents. BPCHAR is deliberately
    // excluded: the engine has no text-to-bpchar write coercion, so bpchar
    // columns cannot be populated through SQL ingress and padding semantics
    // cannot be verified empirically; bpchar shapes decline to the scalar
    // path instead of risking padded-vs-unpadded divergence.
    let is_raw_text = matches!(
        oid,
        plomid_types::TypeOid::TEXT | plomid_types::TypeOid::VARCHAR | plomid_types::TypeOid::NAME
    );
    let is_raw_bytes = oid == plomid_types::TypeOid::BYTEA;
    let (kind, raw) = match (lowered.as_str(), distinct) {
        ("count", false) => (VectorAggKind::Count, false),
        ("count", true) if is_int => (VectorAggKind::CountDistinct, false),
        ("count", true) if is_raw_text || is_raw_bytes => (VectorAggKind::CountDistinct, true),
        ("sum", false) if is_int => (VectorAggKind::Sum, false),
        ("min", false) if is_int => (VectorAggKind::Min, false),
        ("min", false) if is_raw_text || is_raw_bytes => (VectorAggKind::Min, true),
        ("max", false) if is_int => (VectorAggKind::Max, false),
        ("max", false) if is_raw_text || is_raw_bytes => (VectorAggKind::Max, true),
        ("avg", false) if is_int => (VectorAggKind::Avg, false),
        _ => return Ok(None),
    };
    Ok(Some(PlannedTarget::AggCall {
        kind,
        column: Some(column),
        filter,
        raw,
    }))
}

/// Verifies every string/bytes comparison leaf against its column's SQL type.
///
/// String leaves require a plain text-family column; bytes leaves require
/// `BYTEA`. Anything else (temporal/boolean/numeric canonical encodings,
/// unknown columns) declines the whole request, keeping the scalar path
/// authoritative there. Returns `true` when the request may proceed.
fn verify_text_leaves(
    schema: &TableSchema,
    filter: Option<&PrunePredicate>,
    aggregates: &[VectorAggregate],
) -> SqlResult<bool> {
    use plomid_storage::Field;
    fn leaf_ok(
        schema: &TableSchema,
        column: plomid_core::ColumnId,
        literal: Option<&Field>,
    ) -> bool {
        match literal {
            None | Some(Field::Integer(_)) => true,
            Some(Field::Bytes(_)) => {
                let Some(def) = schema.columns.get(column.get() as usize) else {
                    return false;
                };
                def.col_type.type_oid == plomid_types::TypeOid::BYTEA
            }
            Some(Field::String(_)) => {
                let Some(def) = schema.columns.get(column.get() as usize) else {
                    return false;
                };
                matches!(
                    def.col_type.type_oid,
                    plomid_types::TypeOid::TEXT
                        | plomid_types::TypeOid::VARCHAR
                        | plomid_types::TypeOid::NAME
                )
            }
            _ => false,
        }
    }
    fn walk(
        schema: &TableSchema,
        predicate: &PrunePredicate,
        seen: &mut BTreeSet<plomid_core::ColumnId>,
    ) -> bool {
        match predicate {
            PrunePredicate::Compare {
                column_id,
                operator,
                literal,
            } => {
                if matches!(
                    operator,
                    plomid_columnar::PruneOperator::IsNull
                        | plomid_columnar::PruneOperator::IsNotNull
                ) {
                    return true;
                }
                if !seen.insert(*column_id) {
                    return true;
                }
                leaf_ok(schema, *column_id, literal.as_ref())
            }
            PrunePredicate::And(left, right) | PrunePredicate::Or(left, right) => {
                walk(schema, left, seen) && walk(schema, right, seen)
            }
            // NOT pushes through duals before evaluation; the leaves stay
            // the same, so verification recurses unchanged.
            PrunePredicate::Not(inner) => walk(schema, inner, seen),
        }
    }
    let mut seen = BTreeSet::new();
    if let Some(predicate) = filter {
        if !walk(schema, predicate, &mut seen) {
            return Ok(false);
        }
    }
    for aggregate in aggregates {
        if let Some(predicate) = aggregate.filter.as_ref() {
            if !walk(schema, predicate, &mut seen) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Projects one vector result group through the planned targets.
///
/// `keys` are the group's integer key values (`None` = SQL `NULL`);
/// `key_ids` aligns them with the request key list.
fn project_vector_row(
    schema: &TableSchema,
    planned: &[PlannedTarget],
    kinds: &[(VectorAggKind, Option<plomid_core::ColumnId>, bool)],
    keys: &[Option<i64>],
    key_ids: &[plomid_core::ColumnId],
    group: &plomid_columnar::vector::VectorGroup,
) -> SqlResult<Vec<Value>> {
    let mut row = Vec::with_capacity(planned.len());
    for plan in planned {
        match plan {
            PlannedTarget::Agg { agg_index, raw } => {
                let (kind, column, _) = kinds.get(*agg_index).copied().unwrap_or((
                    VectorAggKind::CountStar,
                    None,
                    false,
                ));
                let Some(acc) = group.accs.get(*agg_index) else {
                    row.push(Value::Null);
                    continue;
                };
                // The plan-time raw flag and the accumulator state agree by
                // construction (integer SQL types flush as integers,
                // text/bytes as raw); `raw` selects the state to read.
                row.push(finalize_vector_acc(schema, kind, column, *raw, acc)?);
            }
            PlannedTarget::AggCall { .. } => {
                return Err(SqlError::Storage(plomid_core::PlomidError::new(
                    plomid_core::ErrorKind::Internal,
                    "unindexed vector aggregate call",
                )));
            }
            PlannedTarget::GroupKey { key_position } => {
                match keys.get(*key_position).and_then(|key| *key) {
                    Some(number) => {
                        let id = key_ids
                            .get(*key_position)
                            .copied()
                            .unwrap_or_else(|| plomid_core::ColumnId::new(*key_position as u64));
                        row.push(cast_integer(schema, Some(id), number)?);
                    }
                    None => row.push(Value::Null),
                }
            }
            PlannedTarget::Literal(value) => row.push(value.clone()),
            PlannedTarget::Null => row.push(Value::Null),
        }
    }
    Ok(row)
}

/// Finalizes one accumulator with the scalar path's formulas:
///
/// * `COUNT` shapes yield `Int8` (distinct counts read the integer or raw
///   set according to `raw`);
/// * `SUM` yields `Int8` when any non-`NULL` value was seen, else `NULL`
///   (mirrors `SumN::finalize`);
/// * `AVG` divides in `Numeric` space and normalizes (mirrors
///   `AvgN::finalize`, which renders integral means without scale);
/// * `MIN`/`MAX` yield the value through the scalar path's own conversion
///   (integer cast, or raw-bytes-to-field for text/bytes), or `NULL` when no
///   non-`NULL` value was seen.
fn finalize_vector_acc(
    schema: &TableSchema,
    kind: VectorAggKind,
    column: Option<plomid_core::ColumnId>,
    raw: bool,
    acc: &VectorAcc,
) -> SqlResult<Value> {
    match kind {
        VectorAggKind::CountStar => Ok(Value::Int8(acc.count_star)),
        VectorAggKind::Count => Ok(Value::Int8(acc.count)),
        VectorAggKind::CountDistinct => {
            if raw {
                Ok(Value::Int8(acc.distinct_raw.len() as i64))
            } else {
                Ok(Value::Int8(acc.distinct.len() as i64))
            }
        }
        VectorAggKind::Sum => {
            if acc.sum_has_any {
                Ok(Value::Int8(acc.sum))
            } else {
                Ok(Value::Null)
            }
        }
        VectorAggKind::Avg => {
            if acc.avg_count == 0 {
                Ok(Value::Null)
            } else {
                let sum = plomid_types::Numeric::from_i64(acc.avg_sum);
                let count = plomid_types::Numeric::from_i64(acc.avg_count);
                Ok(Value::Numeric(
                    sum.div(count)
                        .map(|value| value.normalize())
                        .unwrap_or_else(|_| plomid_types::Numeric::from_i64(acc.avg_sum)),
                ))
            }
        }
        VectorAggKind::Min => match (raw, acc.min, acc.min_raw.as_ref()) {
            (false, Some(value), _) => cast_integer(schema, column, value),
            (true, _, Some(bytes)) => cast_raw(schema, column, bytes),
            _ => Ok(Value::Null),
        },
        VectorAggKind::Max => match (raw, acc.max, acc.max_raw.as_ref()) {
            (false, Some(value), _) => cast_integer(schema, column, value),
            (true, _, Some(bytes)) => cast_raw(schema, column, bytes),
            _ => Ok(Value::Null),
        },
    }
}

/// Converts raw string/bytes extremum bytes through the scalar path's own
/// conversion. Invalid UTF-8 in a `String` payload yields `NULL`, mirroring
/// `get_field`, which refuses uninterpretable values instead of erroring.
fn cast_raw(
    schema: &TableSchema,
    column: Option<plomid_core::ColumnId>,
    bytes: &[u8],
) -> SqlResult<Value> {
    let Some(id) = column else {
        return Ok(Value::Null);
    };
    let index = id.get() as usize;
    let Some(def) = schema.columns.get(index) else {
        return Ok(Value::Null);
    };
    let field = if def.col_type.type_oid == plomid_types::TypeOid::BYTEA {
        Field::Bytes(bytes.to_vec())
    } else {
        match std::str::from_utf8(bytes) {
            Ok(text) => Field::String(text.to_owned()),
            Err(_) => return Ok(Value::Null),
        }
    };
    let row = Row::new(vec![field]);
    let converted = columnar_row_to_values(row, schema, &[id])?;
    converted.get(index).cloned().ok_or_else(|| {
        SqlError::Storage(plomid_core::PlomidError::new(
            plomid_core::ErrorKind::Corruption,
            "vector value identity exceeds the SQL schema",
        ))
    })
}

/// Casts a physical integer through the scalar path's own conversion, so
/// `INT2`/`INT4` range errors behave identically.
fn cast_integer(
    schema: &TableSchema,
    column: Option<plomid_core::ColumnId>,
    value: i64,
) -> SqlResult<Value> {
    let Some(id) = column else {
        return Ok(Value::Int8(value));
    };
    let row = Row::new(vec![Field::Integer(value)]);
    let converted = columnar_row_to_values(row, schema, &[id])?;
    let index = id.get() as usize;
    converted.get(index).cloned().ok_or_else(|| {
        SqlError::Storage(plomid_core::PlomidError::new(
            plomid_core::ErrorKind::Corruption,
            "vector key identity exceeds the SQL schema",
        ))
    })
}
