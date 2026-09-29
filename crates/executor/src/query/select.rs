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
//! The single-table SELECT fast path.
//!
//! [`execute_select`] answers scans, predicates and simple projections
//! directly from key/value rows. Anything needing joins, CTEs, window
//! functions or subqueries is handed to the general engine in `crate::join`
//! before the fast path commits to its plan.

use super::aggregate::collect_aggregate_slots;
use super::expr::{evaluate_expression, evaluate_predicate};
use super::group::{
    apply_distinct_on, execute_grouped_select, project_row, sort_rows, StreamingGroupedAgg,
    TopKHeap,
};
use super::project::{
    project_bare_select, select_bare_column_types, select_bare_columns, select_column_types,
    select_columns,
};
use super::scan::{
    columnar_requested_columns, columnar_row_to_values, normalize_time_pruning_expression,
};
use crate::catalog_fn::is_aggregate_function;
use crate::catalog_fn::is_session_function;
use crate::encoding::decode_row_selected;
use crate::error::{SqlError, SqlResult};
use crate::index::{index_value_prefix, prefix_end};
use crate::util::unqualify;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::Catalog;
use plomid_sql::ColumnType;
use plomid_sql::Expression;
use plomid_sql::FromClause;
use plomid_sql::InMemoryCatalog;
use plomid_sql::QueryResult;
use plomid_sql::SelectTarget;
use plomid_sql::Value;
use plomid_txn::StorageEngine;
use std::collections::BTreeMap;

/// Rows decoded per storage callback in the streaming SELECT path.
///
/// The scan callback holds at most this many `(key, payload)` entries plus
/// whatever aggregate/heap state the query accumulates, so peak transient
/// memory is `O(chunk + groups/top-K)` instead of `O(table)`.
///
/// Measured on a 30K-row full-table walk, feeding the same statement through
/// `scan_for_each` with varying chunks (see the
/// `scan_chunk_size_sweep_informs_default` test): 256–8192 all land within
/// noise of each other, so 1024 is chosen to match the MVCC paged-scan quantum
/// (`SCAN_CHUNK_ROWS`), keeping one lock hold per chunk sub-millisecond.
pub(crate) const ANALYTICAL_SCAN_CHUNK_ROWS: usize = 1024;

/// True when the expression tree contains a subquery node.
pub(crate) fn expr_has_subquery(expr: &Expression) -> bool {
    match expr {
        Expression::ScalarSubquery(_) | Expression::Exists(_) => true,
        Expression::QuantifiedComparison { .. } => true,
        Expression::In { subquery, .. } => subquery.is_some(),
        Expression::ColumnRef(_) | Expression::Literal(_) | Expression::Star => false,
        Expression::FunctionCall { name, args, .. } => {
            !is_aggregate_function(name) || args.iter().any(expr_has_subquery)
        }
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
        | Expression::IsDistinctFrom(l, r)
        | Expression::NullIf(l, r) => expr_has_subquery(l) || expr_has_subquery(r),
        Expression::IsNull(i)
        | Expression::IsNotNull(i)
        | Expression::Not(i)
        | Expression::Negate(i) => expr_has_subquery(i),
        Expression::Between {
            expr, low, high, ..
        } => expr_has_subquery(expr) || expr_has_subquery(low) || expr_has_subquery(high),
        Expression::Like { expr, pattern, .. } => {
            expr_has_subquery(expr) || expr_has_subquery(pattern)
        }
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            operand.as_ref().is_some_and(|o| expr_has_subquery(o))
                || whens
                    .iter()
                    .any(|(c, v)| expr_has_subquery(c) || expr_has_subquery(v))
                || default.as_ref().is_some_and(|d| expr_has_subquery(d))
        }
        Expression::Coalesce(args) => args.iter().any(expr_has_subquery),
        _ => false,
    }
}

/// True when the select list needs the general engine (window functions or
/// embedded subqueries).
fn targets_need_general_engine(targets: &[SelectTarget]) -> bool {
    targets.iter().any(|target| match target {
        SelectTarget::WindowFunction { .. } => true,
        SelectTarget::Expr { expr, .. } => expr_has_subquery(expr),
        SelectTarget::FunctionCall { name, args } => {
            !is_aggregate_function(name) || args.iter().any(expr_has_subquery)
        }
        SelectTarget::Aliased { target, .. } => {
            targets_need_general_engine(std::slice::from_ref(target))
        }
        _ => false,
    })
}

/// True when a bare SELECT (no FROM) can be served by the simple literal /
/// session-function fast path. Anything beyond that — scalar function calls,
/// aggregates, arithmetic, predicates, measured over the single implicit row
/// — must go through the general engine's `bare_select`.
fn bare_select_fast_path_ok(targets: &[SelectTarget]) -> bool {
    targets.iter().all(|target| match target {
        SelectTarget::Expr {
            expr: Expression::Literal(_),
            ..
        } => true,
        SelectTarget::Expr { expr, .. } => match expr {
            Expression::ColumnRef(name) => is_session_function(name),
            Expression::FunctionCall { name, args, .. }
                if args.len() == 1 && matches!(args[0], Expression::Literal(Value::Text(_))) =>
            {
                matches!(name.to_ascii_lowercase().as_str(), "nextval" | "currval")
            }
            _ => false,
        },
        SelectTarget::Function(name) => is_session_function(name),
        SelectTarget::FunctionCall { name, args } => {
            if is_session_function(name) {
                true
            } else {
                args.len() == 1
                    && matches!(args[0], Expression::Literal(Value::Text(_)))
                    && matches!(name.to_ascii_lowercase().as_str(), "nextval" | "currval")
            }
        }
        SelectTarget::Aliased { target, .. } => {
            bare_select_fast_path_ok(std::slice::from_ref(target))
        }
        _ => false,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn execute_select<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    targets: Vec<SelectTarget>,
    distinct: bool,
    distinct_on: Vec<Expression>,
    from: Option<FromClause>,
    where_expr: Option<Expression>,
    group_by: Option<plomid_sql::GroupByClause>,
    having: Option<Expression>,
    order_by: Vec<plomid_sql::OrderByItem>,
    limit: Option<usize>,
    offset: Option<usize>,
    current_database: &str,
    current_user: &str,
    columnar_store: Option<&plomid_columnar::ColumnarStore>,
) -> SqlResult<QueryResult> {
    // DISTINCT / DISTINCT ON must remove duplicates before LIMIT/OFFSET are
    // applied, so the inner execution runs unbounded and post-processing
    // applies both.
    let has_distinct_on = !distinct_on.is_empty();
    let (inner_limit, inner_offset) = if distinct || has_distinct_on {
        (None, None)
    } else {
        (limit, offset)
    };
    let result = execute_select_impl(
        engine,
        catalog,
        targets,
        from,
        where_expr,
        group_by,
        having,
        order_by,
        inner_limit,
        inner_offset,
        current_database,
        current_user,
        distinct_on.clone(),
        columnar_store,
    )?;
    Ok(crate::distinct::apply_distinct(
        result,
        distinct,
        &distinct_on,
        limit,
        offset,
    ))
}

#[allow(clippy::too_many_arguments)]
fn execute_select_impl<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    targets: Vec<SelectTarget>,
    from: Option<FromClause>,
    where_expr: Option<Expression>,
    group_by: Option<plomid_sql::GroupByClause>,
    having: Option<Expression>,
    order_by: Vec<plomid_sql::OrderByItem>,
    limit: Option<usize>,
    offset: Option<usize>,
    current_database: &str,
    current_user: &str,
    distinct_on: Vec<Expression>,
    columnar_store: Option<&plomid_columnar::ColumnarStore>,
) -> SqlResult<QueryResult> {
    // Queries needing the general engine: joins, FROM subqueries, window
    // functions, or subqueries anywhere in the statement.
    let group_sets: Option<Vec<Vec<Expression>>> = group_by.as_ref().map(|g| g.to_sets());
    let group_union: Vec<Expression> = group_by
        .as_ref()
        .map(|g| g.union_exprs())
        .unwrap_or_default();
    let needs_general_engine = from.as_ref().is_some_and(|f| {
        !matches!(f, FromClause::Table { .. })
            || matches!(f, FromClause::Table { name, .. } if catalog.get_view(name).is_some() || crate::system_catalog::SystemCatalog::is_relation(name))
    }) || targets.iter().any(|t| matches!(t, SelectTarget::FunctionCall { name, .. } if name.eq_ignore_ascii_case("string_agg") || name.eq_ignore_ascii_case("array_agg") || crate::json::is_json_aggregate(name)))
        || targets.iter().any(target_has_json_aggregate)
        || group_union.iter().any(|e| !matches!(e, Expression::ColumnRef(_)))
        || group_by.is_some() && !order_by.is_empty()
        || group_sets.as_ref().is_some_and(|sets| sets.len() > 1)
        || targets_need_general_engine(&targets)
        || where_expr.as_ref().is_some_and(expr_has_subquery)
        || group_union.iter().any(expr_has_subquery)
        || having.as_ref().is_some_and(expr_has_subquery)
        || order_by.iter().any(|item| expr_has_subquery(&item.expr))
        || (from.is_none() && !bare_select_fast_path_ok(&targets));
    if needs_general_engine {
        return crate::join::execute_join_select(
            engine,
            catalog,
            from.as_ref(),
            targets,
            where_expr,
            group_by,
            having,
            order_by,
            limit,
            offset,
            current_database,
            current_user,
            None,
            0,
            distinct_on,
        );
    }
    tracing::trace!(
        target: "sql::query",
        "select from={:?}",
        from.as_ref().map(|f| match f {
            FromClause::Table { name, .. } => name.clone(),
            FromClause::TableFunction { name, .. } => name.clone(),
            FromClause::Join { .. } => "<join>".to_string(),
            FromClause::Subquery { .. } => "<subquery>".to_string(),
        })
    );
    if let Some(from) = from {
        // Multi-source FROM: joins or subqueries need the full join executor.
        if !matches!(from, FromClause::Table { .. }) {
            return crate::join::execute_join_select(
                engine,
                catalog,
                Some(&from),
                targets,
                where_expr,
                group_by,
                having,
                order_by,
                limit,
                offset,
                current_database,
                current_user,
                None,
                0,
                distinct_on,
            );
        }
        let FromClause::Table { name: from, .. } = from else {
            unreachable!("guarded above");
        };
        let schema = catalog.get_table(&from)?;
        // Index bounds are resolved BEFORE the columnar decision: a usable
        // index range answers selective predicates from a handful of entries,
        // while the columnar path would materialize the whole generation and
        // filter it row-at-a-time (measured 100x slower on point lookups).
        // Columnar stays eligible only when no index range applies.
        let index_range = index_bounds_for_predicate(catalog, &from, where_expr.as_ref());
        // Columnar eligibility is decided from authoritative shared state,
        // never from session-local memory: the table's published generation
        // must be proven to cover every committed write (shared freshness
        // map, maintained by all sessions' writes and vacuums), and that
        // generation must still be the currently published one. Fresh
        // connections discover eligibility without ever having run VACUUM;
        // anything unproven falls back to Hot Row Store state.
        let columnar_rows = if index_range.is_none()
            && columnar_store.is_some_and(|store| {
                catalog
                    .resolve_table_name(&from)
                    .ok()
                    .is_some_and(|qualified| {
                        crate::columnar_freshness::is_generation_fresh(
                            store,
                            engine.root(),
                            current_database,
                            &qualified,
                            schema.table_id.get(),
                        )
                    })
            }) {
            let mut columns = BTreeMap::new();
            for (index, column) in schema.columns.iter().enumerate() {
                // Columnar materialization uses zero-based positional column
                // identities; the SQL catalog's ColumnIds are global metadata
                // identities and must not be passed to the pruner.
                columns.insert(
                    column.name.clone(),
                    plomid_core::ColumnId::new(index as u64),
                );
            }
            let pruning = where_expr.as_ref().and_then(|expr| {
                let normalized = normalize_time_pruning_expression(expr, schema);
                plomid_columnar::lower_sql_expression(&normalized, &columns)
                    .predicate()
                    .cloned()
            });
            let requested_columns =
                columnar_requested_columns(&targets, where_expr.as_ref(), &order_by, schema);
            let store = columnar_store.expect("columnar store checked above");
            // Vector attempt first: supported aggregate shapes never
            // materialize Rows at all. Anything unsupported returns None and
            // the scalar scan below answers exactly as before.
            if let Some(vector_result) = super::vector::try_vector_columnar(
                engine,
                schema,
                &targets,
                where_expr.as_ref(),
                group_by.as_ref(),
                having.as_ref(),
                &order_by,
                limit,
                offset,
                &distinct_on,
                &columns,
                store,
                plomid_core::ObjectId::new(schema.table_id.get()),
            )? {
                tracing::debug!(
                    target: "sql::query",
                    event = "analytical_path",
                    path = "vector",
                    table = %from,
                );
                return Ok(vector_result);
            }
            let scan = plomid_columnar::read_current_generation_rows(
                &store,
                engine,
                plomid_core::ObjectId::new(schema.table_id.get()),
                pruning.as_ref(),
                requested_columns.as_deref(),
            )?;
            if let Some(scan) = scan {
                tracing::debug!(
                    target: "sql::columnar",
                    table = %from,
                    segments_considered = scan.segments_considered,
                    segments_skipped = scan.segments_skipped,
                    rows_examined = scan.rows_examined,
                    rows_skipped = scan.rows_skipped,
                    columns_read = scan.columns_read,
                    "columnar SQL scan"
                );
                Some((scan.rows, scan.column_ids))
            } else {
                None
            }
        } else {
            None
        };
        // Index-driven candidate discovery. A plain single-column index can
        // answer `=`, `<`, `<=`, `>`, `>=` and `BETWEEN` as one contiguous key
        // range, which turns a selective predicate from a full table scan into
        // an index range probe. When no index or no order-preserving bound
        // applies, this returns `None` and the table scan below answers the
        // statement exactly as before.
        // Which stored columns can this statement actually read? The mask is
        // built once per scan, then every row decodes only those columns;
        // when the analysis cannot prove a set it yields the all-columns mask,
        // which is exactly the historical full decode.
        let projection = crate::projection::required_columns(
            schema,
            &targets,
            where_expr.as_ref(),
            group_by.as_ref(),
            having.as_ref(),
            &order_by,
            &distinct_on,
        )
        .unwrap_or_else(|| crate::projection::ColumnMask::all(schema.columns.len()));

        // `SELECT COUNT(*)` with no predicate reads no column at all: count
        // visible rows without materializing them. This runs before any scan
        // so a million-row count walks keys only (~µs, zero transient rows)
        // instead of cloning every key and payload first (~450MB transient
        // measured at 1M rows). Row-framing validation is skipped on this
        // path; page checksums still guard storage integrity, corrupting rows
        // surface on reads that decode them, and the columnar branch never
        // validated either.
        //
        // `is_bare_count_star` is required as well as the empty column set:
        // `count(NULL)` also references no column but must count zero, so only
        // the literal `count(*)` shape may be answered by a row count.
        if projection.reads_no_column()
            && crate::projection::is_bare_count_star(&targets)
            && where_expr.is_none()
            && group_by.is_none()
            && having.is_none()
            && order_by.is_empty()
            && distinct_on.is_empty()
        {
            let visible = engine.count_range(
                Some(format!("{from}:").as_bytes()),
                Some(format!("{from}:\u{10FFFF}").as_bytes()),
            )? as i64;
            let mut rows = vec![vec![Value::Int8(visible)]];
            if offset.unwrap_or(0) > 0 || limit.is_some() {
                rows = rows
                    .into_iter()
                    .skip(offset.unwrap_or(0))
                    .take(limit.unwrap_or(usize::MAX))
                    .collect();
            }
            tracing::trace!(
                target: "sql::query",
                "select count-only complete from={} row_count={visible}",
                from
            );
            tracing::debug!(
                target: "sql::query",
                event = "analytical_path",
                path = "row_count",
                table = %from,
            );
            return Ok(QueryResult::Rows {
                columns: select_columns(schema, &targets)?,
                column_types: select_column_types(schema, &targets)?,
                rows,
            });
        }

        // Early-termination cap for plain SELECT ... LIMIT/OFFSET without
        // ordering, grouping, aggregation, or dedup (DISTINCT clears the
        // limit in the outer wrapper, so `Some` here always permits capping).
        // The cap bounds *matches*: the streaming walk below stops after the
        // cap-th match (predicateless or filtered alike), so a capped plain
        // SELECT never holds more than `cap` decoded rows.
        let needs_full_scan = !order_by.is_empty()
            || group_by.is_some()
            || having.is_some()
            || !distinct_on.is_empty()
            || count_distinct_target(&targets).is_some()
            || count_filter_target(&targets).is_some()
            || targets.iter().any(|t| match t {
                SelectTarget::Function(name) => is_aggregate_function(name),
                SelectTarget::FunctionCall { name, .. } => is_aggregate_function(name),
                SelectTarget::Expr { expr, .. } => expression_has_aggregate(expr),
                SelectTarget::Aliased { target, .. } => matches!(target.as_ref(), SelectTarget::Expr { expr, .. } if expression_has_aggregate(expr)),
                _ => false,
            });
        let row_cap = match limit {
            Some(take) if !needs_full_scan => Some(offset.unwrap_or(0).saturating_add(take)),
            _ => None,
        };

        let has_aggregation = targets.iter().any(|t| match t {
            SelectTarget::Function(name) => is_aggregate_function(name),
            SelectTarget::FunctionCall { name, .. } => is_aggregate_function(name),
            SelectTarget::Expr { expr, .. } => expression_has_aggregate(expr),
            SelectTarget::Aliased { target, .. } => matches!(target.as_ref(), SelectTarget::Expr { expr, .. } if expression_has_aggregate(expr)),
            _ => false,
        }) || group_by.is_some()
            || having.is_some();

        // Indexed probe path: selective by construction (one contiguous index
        // range resolved to K row keys + one batched `get_many`). K is the
        // match count, not the table size, so the historical materialized flow
        // is kept verbatim here: point-read latency is the regression guard
        // and must not change for OLAP work.
        if let Some((_index_name, idx_start, idx_end)) = index_range {
            let index_entries = engine.scan(Some(&idx_start), Some(&idx_end))?;
            let keys: Vec<Vec<u8>> = index_entries
                .into_iter()
                .map(|(_, row_key)| row_key)
                .collect();
            let values = engine.get_many(&keys)?;
            let mut entries = Vec::with_capacity(keys.len());
            for (row_key, value) in keys.into_iter().zip(values) {
                if let Some(value) = value {
                    entries.push((row_key, value));
                }
            }
            let mut filtered_rows: Vec<Vec<Value>> = Vec::new();
            if let Some(cap) = row_cap {
                filtered_rows.reserve(cap.min(1024));
            }
            'scan: for (_, value_bytes) in entries {
                let row = decode_row_selected(&value_bytes, projection.selected())?;
                if let Some(ref expr) = where_expr {
                    if !evaluate_predicate(&row, schema, expr)? {
                        continue;
                    }
                }
                filtered_rows.push(row);
                if row_cap.is_some_and(|cap| filtered_rows.len() >= cap) {
                    break 'scan;
                }
            }
            if let Some((arg, filter)) = count_distinct_target(&targets) {
                let mut values = std::collections::HashSet::new();
                for row in &filtered_rows {
                    if let Some(filter) = filter.as_ref() {
                        if !matches!(evaluate_expression(row, schema, filter)?, Value::Bool(true)) {
                            continue;
                        }
                    }
                    let value = evaluate_expression(row, schema, &arg)?;
                    if !value.is_null() {
                        values.insert(value.to_sql_text());
                    }
                }
                return Ok(QueryResult::Rows {
                    columns: vec!["count".into()],
                    column_types: vec![Some(ColumnType::bigint())],
                    rows: vec![vec![Value::Int8(values.len() as i64)]],
                });
            }
            if let Some(filter) = count_filter_target(&targets) {
                let mut count = 0i64;
                for row in &filtered_rows {
                    if matches!(
                        evaluate_expression(row, schema, &filter)?,
                        Value::Bool(true)
                    ) {
                        count += 1;
                    }
                }
                return Ok(QueryResult::Rows {
                    columns: vec!["count".into()],
                    column_types: vec![Some(ColumnType::bigint())],
                    rows: vec![vec![Value::Int8(count)]],
                });
            }
            if has_aggregation {
                return execute_grouped_select(
                    schema,
                    &targets,
                    group_by,
                    having,
                    filtered_rows,
                    current_database,
                    current_user,
                );
            }
            let mut source_rows = filtered_rows;
            if !order_by.is_empty() {
                let top_k = limit.map(|take| offset.unwrap_or(0).saturating_add(take));
                source_rows = sort_rows(source_rows, schema, &order_by, top_k)?;
            }
            if !distinct_on.is_empty() {
                if group_by.is_some()
                    || having.is_some()
                    || targets.iter().any(|t| match t {
                        SelectTarget::Function(name) => is_aggregate_function(name),
                        SelectTarget::FunctionCall { name, .. } => is_aggregate_function(name),
                        SelectTarget::Expr { expr, .. } => expression_has_aggregate(expr),
                        SelectTarget::Aliased { target, .. } => matches!(target.as_ref(), SelectTarget::Expr { expr, .. } if expression_has_aggregate(expr)),
                        _ => false,
                    })
                {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::Unsupported,
                        "DISTINCT ON is not supported with GROUP BY or aggregates",
                    )));
                }
                source_rows = apply_distinct_on(
                    source_rows,
                    schema,
                    &distinct_on,
                    &order_by,
                    &targets,
                    current_database,
                    current_user,
                )?;
            }
            let mut rows: Vec<Vec<Value>> = source_rows
                .into_iter()
                .map(|row| project_row(&row, schema, &targets, current_database, current_user))
                .collect::<SqlResult<_>>()?;
            if offset.unwrap_or(0) > 0 || limit.is_some() {
                rows = rows
                    .into_iter()
                    .skip(offset.unwrap_or(0))
                    .take(limit.unwrap_or(usize::MAX))
                    .collect();
            }
            let cols = select_columns(schema, &targets)?;
            let column_types = select_column_types(schema, &targets)?;
            tracing::trace!(
                target: "sql::query",
                "select complete from={} row_count={}",
                from,
                rows.len()
            );
            return Ok(QueryResult::Rows {
                columns: cols,
                column_types,
                rows,
            });
        }

        // Non-indexed path: full-range scans (row store via `scan_for_each`,
        // or an already-materialized columnar generation). Peak memory here
        // used to be `O(table)` twice over — the storage `scan` Vec plus the
        // decoded `filtered_rows` Vec (~450MB transient at 1M rows). The
        // streaming branches below accumulate only aggregate/group/heap/count
        // state over bounded scan chunks instead.
        let count_distinct = count_distinct_target(&targets);
        let count_filter = count_filter_target(&targets);
        let grouping_sets_len = group_by.as_ref().map(|g| g.to_sets().len()).unwrap_or(0);
        let stream_agg = has_aggregation
            && count_distinct.is_none()
            && count_filter.is_none()
            && grouping_sets_len <= 1;
        // Bounded top-K applies only when LIMIT exists, no aggregation, and
        // DISTINCT ON is absent (dedup happens after the sort, so truncating
        // the pre-dedup stream to K would change results; that shape keeps the
        // historical full-sort path).
        let heap_cap: Option<usize> = if !order_by.is_empty()
            && !has_aggregation
            && count_distinct.is_none()
            && count_filter.is_none()
            && distinct_on.is_empty()
        {
            limit.map(|take| offset.unwrap_or(0).saturating_add(take))
        } else {
            None
        };

        // Columnar generation path. The generation Vec itself is still
        // materialized by `read_current_generation_rows` (a Priority-2
        // columnar-chunking follow-up); the second copy — decoded
        // `filtered_rows` — is eliminated by feeding each row straight into
        // the aggregate/heap/count state.
        if let Some((rows, columnar_columns)) = columnar_rows {
            tracing::debug!(
                target: "sql::query",
                event = "analytical_path",
                path = "columnar_scan",
                table = %from,
            );
            if let Some((arg, filter)) = count_distinct {
                let mut values = std::collections::HashSet::new();
                for row in rows {
                    let row = columnar_row_to_values(row, schema, &columnar_columns)?;
                    if let Some(ref expr) = where_expr {
                        if !evaluate_predicate(&row, schema, expr)? {
                            continue;
                        }
                    }
                    if let Some(filter) = filter.as_ref() {
                        if !matches!(
                            evaluate_expression(&row, schema, filter)?,
                            Value::Bool(true)
                        ) {
                            continue;
                        }
                    }
                    let value = evaluate_expression(&row, schema, &arg)?;
                    if !value.is_null() {
                        values.insert(value.to_sql_text());
                    }
                }
                return Ok(QueryResult::Rows {
                    columns: vec!["count".into()],
                    column_types: vec![Some(ColumnType::bigint())],
                    rows: vec![vec![Value::Int8(values.len() as i64)]],
                });
            }
            if let Some(filter) = count_filter {
                let mut count = 0i64;
                for row in rows {
                    let row = columnar_row_to_values(row, schema, &columnar_columns)?;
                    if let Some(ref expr) = where_expr {
                        if !evaluate_predicate(&row, schema, expr)? {
                            continue;
                        }
                    }
                    if matches!(
                        evaluate_expression(&row, schema, &filter)?,
                        Value::Bool(true)
                    ) {
                        count += 1;
                    }
                }
                return Ok(QueryResult::Rows {
                    columns: vec!["count".into()],
                    column_types: vec![Some(ColumnType::bigint())],
                    rows: vec![vec![Value::Int8(count)]],
                });
            }
            if stream_agg {
                let slots = collect_aggregate_slots(&targets, having.as_ref());
                let group_exprs: Vec<Expression> = group_by
                    .as_ref()
                    .map(|g| g.to_sets().into_iter().next().unwrap_or_default())
                    .unwrap_or_default();
                let mut agg = StreamingGroupedAgg::new(&slots, &group_exprs);
                for row in rows {
                    let row = columnar_row_to_values(row, schema, &columnar_columns)?;
                    if let Some(ref expr) = where_expr {
                        if !evaluate_predicate(&row, schema, expr)? {
                            continue;
                        }
                    }
                    agg.feed(&row, schema)?;
                }
                return agg.finish(
                    schema,
                    &targets,
                    having.as_ref(),
                    current_database,
                    current_user,
                );
            }
            if let Some(cap) = heap_cap {
                let mut heap = TopKHeap::new(cap);
                for row in rows {
                    let row = columnar_row_to_values(row, schema, &columnar_columns)?;
                    if let Some(ref expr) = where_expr {
                        if !evaluate_predicate(&row, schema, expr)? {
                            continue;
                        }
                    }
                    // No early break: ORDER BY must observe every match; the
                    // bound is the heap itself, not the walk length.
                    let keys = order_by
                        .iter()
                        .map(|item| evaluate_expression(&row, schema, &item.expr))
                        .collect::<SqlResult<Vec<_>>>()?;
                    heap.offer(row, keys, &order_by);
                }
                let mut rows: Vec<Vec<Value>> = heap
                    .into_sorted_rows(&order_by)
                    .into_iter()
                    .map(|row| project_row(&row, schema, &targets, current_database, current_user))
                    .collect::<SqlResult<_>>()?;
                if offset.unwrap_or(0) > 0 || limit.is_some() {
                    // Heap already holds only `offset + limit` rows in order;
                    // skip the offset prefix that `sort_rows` callers used to
                    // drop after the full sort.
                    rows = rows.into_iter().skip(offset.unwrap_or(0)).collect();
                }
                let cols = select_columns(schema, &targets)?;
                let column_types = select_column_types(schema, &targets)?;
                return Ok(QueryResult::Rows {
                    columns: cols,
                    column_types,
                    rows,
                });
            }
            let mut filtered_rows: Vec<Vec<Value>> = Vec::new();
            if let Some(cap) = row_cap {
                filtered_rows.reserve(cap.min(1024));
            }
            'scan: for row in rows {
                let row = columnar_row_to_values(row, schema, &columnar_columns)?;
                if let Some(ref expr) = where_expr {
                    if !evaluate_predicate(&row, schema, expr)? {
                        continue;
                    }
                }
                filtered_rows.push(row);
                if row_cap.is_some_and(|cap| filtered_rows.len() >= cap) {
                    break 'scan;
                }
            }
            if has_aggregation {
                return execute_grouped_select(
                    schema,
                    &targets,
                    group_by,
                    having,
                    filtered_rows,
                    current_database,
                    current_user,
                );
            }
            let mut source_rows = filtered_rows;
            if !order_by.is_empty() {
                let top_k = limit.map(|take| offset.unwrap_or(0).saturating_add(take));
                source_rows = sort_rows(source_rows, schema, &order_by, top_k)?;
            }
            if !distinct_on.is_empty() {
                if group_by.is_some()
                    || having.is_some()
                    || targets.iter().any(|t| match t {
                        SelectTarget::Function(name) => is_aggregate_function(name),
                        SelectTarget::FunctionCall { name, .. } => is_aggregate_function(name),
                        SelectTarget::Expr { expr, .. } => expression_has_aggregate(expr),
                        SelectTarget::Aliased { target, .. } => matches!(target.as_ref(), SelectTarget::Expr { expr, .. } if expression_has_aggregate(expr)),
                        _ => false,
                    })
                {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::Unsupported,
                        "DISTINCT ON is not supported with GROUP BY or aggregates",
                    )));
                }
                source_rows = apply_distinct_on(
                    source_rows,
                    schema,
                    &distinct_on,
                    &order_by,
                    &targets,
                    current_database,
                    current_user,
                )?;
            }
            let mut rows: Vec<Vec<Value>> = source_rows
                .into_iter()
                .map(|row| project_row(&row, schema, &targets, current_database, current_user))
                .collect::<SqlResult<_>>()?;
            if offset.unwrap_or(0) > 0 || limit.is_some() {
                rows = rows
                    .into_iter()
                    .skip(offset.unwrap_or(0))
                    .take(limit.unwrap_or(usize::MAX))
                    .collect();
            }
            let cols = select_columns(schema, &targets)?;
            let column_types = select_column_types(schema, &targets)?;
            return Ok(QueryResult::Rows {
                columns: cols,
                column_types,
                rows,
            });
        }

        let scan_start: Vec<u8> = format!("{from}:").into_bytes();
        let scan_end: Vec<u8> = format!("{from}:\u{10FFFF}").into_bytes();

        // Per-chunk decode + WHERE helper. Errors are funneled through the
        // enclosing `stream_err` slot because the storage callback returns
        // the storage error type, not `SqlError`.
        macro_rules! decode_filter {
            ($bytes:expr, $stream_err:ident) => {{
                let decoded = decode_row_selected($bytes, projection.selected());
                let row = match decoded {
                    Ok(row) => row,
                    Err(e) => {
                        $stream_err = Some(SqlError::Storage(e));
                        return Ok(false);
                    }
                };
                if let Some(ref expr) = where_expr {
                    match evaluate_predicate(&row, schema, expr) {
                        Ok(true) => Some(row),
                        Ok(false) => None,
                        Err(e) => {
                            $stream_err = Some(e);
                            return Ok(false);
                        }
                    }
                } else {
                    Some(row)
                }
            }};
        }

        // COUNT(DISTINCT expr): streaming distinct-value set, no row storage.
        if let Some((arg, filter)) = count_distinct {
            let mut values = std::collections::HashSet::new();
            let mut stream_err: Option<SqlError> = None;
            let scan_res = engine.scan_for_each(
                Some(&scan_start),
                Some(&scan_end),
                ANALYTICAL_SCAN_CHUNK_ROWS,
                &mut |chunk| {
                    for (_, bytes) in chunk {
                        let row = match decode_filter!(bytes, stream_err) {
                            Some(row) => row,
                            None => continue,
                        };
                        if let Some(filter) = filter.as_ref() {
                            match evaluate_expression(&row, schema, filter) {
                                Ok(Value::Bool(true)) => {}
                                Ok(_) => continue,
                                Err(e) => {
                                    stream_err = Some(e);
                                    return Ok(false);
                                }
                            }
                        }
                        match evaluate_expression(&row, schema, &arg) {
                            Ok(value) => {
                                if !value.is_null() {
                                    values.insert(value.to_sql_text());
                                }
                            }
                            Err(e) => {
                                stream_err = Some(e);
                                return Ok(false);
                            }
                        }
                    }
                    Ok(true)
                },
            );
            scan_res.map_err(SqlError::Storage)?;
            if let Some(e) = stream_err {
                return Err(e);
            }
            return Ok(QueryResult::Rows {
                columns: vec!["count".into()],
                column_types: vec![Some(ColumnType::bigint())],
                rows: vec![vec![Value::Int8(values.len() as i64)]],
            });
        }
        // COUNT(*) FILTER (WHERE ...): streaming counter, no row storage.
        if let Some(filter) = count_filter {
            tracing::debug!(
                target: "sql::query",
                event = "analytical_path",
                path = "row_count_filter",
                table = %from,
            );
            let mut count = 0i64;
            let mut stream_err: Option<SqlError> = None;
            let scan_res = engine.scan_for_each(
                Some(&scan_start),
                Some(&scan_end),
                ANALYTICAL_SCAN_CHUNK_ROWS,
                &mut |chunk| {
                    for (_, bytes) in chunk {
                        let row = match decode_filter!(bytes, stream_err) {
                            Some(row) => row,
                            None => continue,
                        };
                        match evaluate_expression(&row, schema, &filter) {
                            Ok(Value::Bool(true)) => count += 1,
                            Ok(_) => {}
                            Err(e) => {
                                stream_err = Some(e);
                                return Ok(false);
                            }
                        }
                    }
                    Ok(true)
                },
            );
            scan_res.map_err(SqlError::Storage)?;
            if let Some(e) = stream_err {
                return Err(e);
            }
            return Ok(QueryResult::Rows {
                columns: vec!["count".into()],
                column_types: vec![Some(ColumnType::bigint())],
                rows: vec![vec![Value::Int8(count)]],
            });
        }
        // Streaming GROUP BY / aggregation: bounded to groups + one chunk.
        if stream_agg {
            tracing::debug!(
                target: "sql::query",
                event = "analytical_path",
                path = "row_group",
                table = %from,
            );
            let slots = collect_aggregate_slots(&targets, having.as_ref());
            let group_exprs: Vec<Expression> = group_by
                .as_ref()
                .map(|g| g.to_sets().into_iter().next().unwrap_or_default())
                .unwrap_or_default();
            let mut agg = StreamingGroupedAgg::new(&slots, &group_exprs);
            let mut stream_err: Option<SqlError> = None;
            let scan_res = engine.scan_for_each(
                Some(&scan_start),
                Some(&scan_end),
                ANALYTICAL_SCAN_CHUNK_ROWS,
                &mut |chunk| {
                    for (_, bytes) in chunk {
                        let row = match decode_filter!(bytes, stream_err) {
                            Some(row) => row,
                            None => continue,
                        };
                        if let Err(e) = agg.feed(&row, schema) {
                            stream_err = Some(e);
                            return Ok(false);
                        }
                    }
                    Ok(true)
                },
            );
            scan_res.map_err(SqlError::Storage)?;
            if let Some(e) = stream_err {
                return Err(e);
            }
            return agg.finish(
                schema,
                &targets,
                having.as_ref(),
                current_database,
                current_user,
            );
        }
        // Streaming ORDER BY ... LIMIT via bounded heap: O(K) memory.
        if let Some(cap) = heap_cap {
            let mut heap = TopKHeap::new(cap);
            let mut stream_err: Option<SqlError> = None;
            let scan_res = engine.scan_for_each(
                Some(&scan_start),
                Some(&scan_end),
                ANALYTICAL_SCAN_CHUNK_ROWS,
                &mut |chunk| {
                    for (_, bytes) in chunk {
                        let row = match decode_filter!(bytes, stream_err) {
                            Some(row) => row,
                            None => continue,
                        };
                        // No early break: ORDER BY must observe every match;
                        // the bound is the heap itself, not the walk length.
                        let keys = match order_by
                            .iter()
                            .map(|item| evaluate_expression(&row, schema, &item.expr))
                            .collect::<SqlResult<Vec<_>>>()
                        {
                            Ok(keys) => keys,
                            Err(e) => {
                                stream_err = Some(e);
                                return Ok(false);
                            }
                        };
                        heap.offer(row, keys, &order_by);
                    }
                    Ok(true)
                },
            );
            scan_res.map_err(SqlError::Storage)?;
            if let Some(e) = stream_err {
                return Err(e);
            }
            let mut rows: Vec<Vec<Value>> = heap
                .into_sorted_rows(&order_by)
                .into_iter()
                .map(|row| project_row(&row, schema, &targets, current_database, current_user))
                .collect::<SqlResult<_>>()?;
            if offset.unwrap_or(0) > 0 {
                rows = rows.into_iter().skip(offset.unwrap_or(0)).collect();
            }
            let cols = select_columns(schema, &targets)?;
            let column_types = select_column_types(schema, &targets)?;
            return Ok(QueryResult::Rows {
                columns: cols,
                column_types,
                rows,
            });
        }

        // Remaining shapes (plain SELECT, ORDER BY without LIMIT, DISTINCT ON):
        // the result itself is O(matches), so one result Vec is unavoidable,
        // but the storage scan streams in bounded chunks instead of arriving
        // as a second full-table Vec.
        let mut source_rows: Vec<Vec<Value>> = Vec::new();
        if let Some(cap) = row_cap {
            source_rows.reserve(cap.min(1024));
        }
        {
            let mut stream_err: Option<SqlError> = None;
            let mut matched = 0usize;
            let stop_at = row_cap;
            let scan_res = engine.scan_for_each(
                Some(&scan_start),
                Some(&scan_end),
                ANALYTICAL_SCAN_CHUNK_ROWS,
                &mut |chunk| {
                    for (_, bytes) in chunk {
                        let row = match decode_filter!(bytes, stream_err) {
                            Some(row) => row,
                            None => continue,
                        };
                        source_rows.push(row);
                        matched += 1;
                        if stop_at.is_some_and(|c| matched >= c) {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                },
            );
            scan_res.map_err(SqlError::Storage)?;
            if let Some(e) = stream_err {
                return Err(e);
            }
        }
        if has_aggregation {
            return execute_grouped_select(
                schema,
                &targets,
                group_by,
                having,
                source_rows,
                current_database,
                current_user,
            );
        }
        if !order_by.is_empty() {
            let top_k = limit.map(|take| offset.unwrap_or(0).saturating_add(take));
            source_rows = sort_rows(source_rows, schema, &order_by, top_k)?;
        }
        if !distinct_on.is_empty() {
            if group_by.is_some()
                || having.is_some()
                || targets.iter().any(|t| match t {
                    SelectTarget::Function(name) => is_aggregate_function(name),
                    SelectTarget::FunctionCall { name, .. } => is_aggregate_function(name),
                    SelectTarget::Expr { expr, .. } => expression_has_aggregate(expr),
                    SelectTarget::Aliased { target, .. } => matches!(target.as_ref(), SelectTarget::Expr { expr, .. } if expression_has_aggregate(expr)),
                    _ => false,
                })
            {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Unsupported,
                    "DISTINCT ON is not supported with GROUP BY or aggregates",
                )));
            }
            source_rows = apply_distinct_on(
                source_rows,
                schema,
                &distinct_on,
                &order_by,
                &targets,
                current_database,
                current_user,
            )?;
        }
        let mut rows: Vec<Vec<Value>> = source_rows
            .into_iter()
            .map(|row| project_row(&row, schema, &targets, current_database, current_user))
            .collect::<SqlResult<_>>()?;
        if offset.unwrap_or(0) > 0 || limit.is_some() {
            rows = rows
                .into_iter()
                .skip(offset.unwrap_or(0))
                .take(limit.unwrap_or(usize::MAX))
                .collect();
        }

        let cols = select_columns(schema, &targets)?;
        let column_types = select_column_types(schema, &targets)?;
        tracing::trace!(
            target: "sql::query",
            "select complete from={} row_count={}",
            from,
            rows.len()
        );
        Ok(QueryResult::Rows {
            columns: cols,
            column_types,
            rows,
        })
    } else {
        let row = project_bare_select(engine, catalog, &targets, current_database, current_user)?;
        let cols = select_bare_columns(&targets)?;
        let column_types = select_bare_column_types(&targets)?;
        tracing::trace!(target: "sql::query", "bare select complete row_count=1");
        Ok(QueryResult::Rows {
            columns: cols,
            column_types,
            rows: vec![row],
        })
    }
}

/// The column and literal of a comparison with one bare column on one side.
fn column_vs_literal(left: &Expression, right: &Expression) -> Option<(String, Value)> {
    match (left, right) {
        (Expression::ColumnRef(column), Expression::Literal(value)) => {
            Some((unqualify(column).to_string(), value.clone()))
        }
        (Expression::Literal(value), Expression::ColumnRef(column)) => {
            Some((unqualify(column).to_string(), value.clone()))
        }
        _ => None,
    }
}

/// The one-column key range a predicate implies: the compared column plus, for
/// each direction, the bound operator and its literal.
///
/// Only shapes a single index can serve are accepted. `OR` is deliberately
/// rejected — the disjunction of two ranges is not one range — and so is any
/// comparison whose column is not compared against a literal. `AND` keeps the
/// bounds of whichever conjunct is usable, and merges two conjuncts on the same
/// column, which is what makes `k >= a AND k <= b` a single bounded probe.
#[allow(clippy::type_complexity)]
fn column_range_predicate(
    expr: &Expression,
) -> Option<(
    String,
    Option<(crate::index::IndexRangeOp, Value)>,
    Option<(crate::index::IndexRangeOp, Value)>,
)> {
    use crate::index::IndexRangeOp;
    use Expression as E;
    match expr {
        E::Equal(left, right) => column_vs_literal(left, right).map(|(column, value)| {
            (
                column,
                Some((IndexRangeOp::GreaterOrEqual, value.clone())),
                Some((IndexRangeOp::LessOrEqual, value)),
            )
        }),
        E::Less(left, right) => match (
            column_vs_literal(left, right),
            column_vs_literal(right, left),
        ) {
            (Some((column, value)), _) => Some((column, None, Some((IndexRangeOp::Less, value)))),
            (None, Some((column, value))) => {
                Some((column, Some((IndexRangeOp::Greater, value)), None))
            }
            _ => None,
        },
        E::LessOrEqual(left, right) => {
            match (
                column_vs_literal(left, right),
                column_vs_literal(right, left),
            ) {
                (Some((column, value)), _) => {
                    Some((column, None, Some((IndexRangeOp::LessOrEqual, value))))
                }
                (None, Some((column, value))) => {
                    Some((column, Some((IndexRangeOp::GreaterOrEqual, value)), None))
                }
                _ => None,
            }
        }
        E::Greater(left, right) => match (
            column_vs_literal(left, right),
            column_vs_literal(right, left),
        ) {
            (Some((column, value)), _) => {
                Some((column, Some((IndexRangeOp::Greater, value)), None))
            }
            (None, Some((column, value))) => {
                Some((column, None, Some((IndexRangeOp::Less, value))))
            }
            _ => None,
        },
        E::GreaterOrEqual(left, right) => {
            match (
                column_vs_literal(left, right),
                column_vs_literal(right, left),
            ) {
                (Some((column, value)), _) => {
                    Some((column, Some((IndexRangeOp::GreaterOrEqual, value)), None))
                }
                (None, Some((column, value))) => {
                    Some((column, None, Some((IndexRangeOp::LessOrEqual, value))))
                }
                _ => None,
            }
        }
        E::Between {
            expr,
            low,
            high,
            negated: false,
        } => {
            let Expression::ColumnRef(column) = &**expr else {
                return None;
            };
            let (Expression::Literal(low), Expression::Literal(high)) = (&**low, &**high) else {
                return None;
            };
            Some((
                unqualify(column).to_string(),
                Some((IndexRangeOp::GreaterOrEqual, low.clone())),
                Some((IndexRangeOp::LessOrEqual, high.clone())),
            ))
        }
        E::And(left, right) => {
            match (column_range_predicate(left), column_range_predicate(right)) {
                (
                    Some((left_column, left_low, left_high)),
                    Some((right_column, right_low, right_high)),
                ) if left_column == right_column => Some((
                    left_column,
                    left_low.or(right_low),
                    left_high.or(right_high),
                )),
                (Some(found), _) => Some(found),
                (None, other) => other,
            }
        }
        _ => None,
    }
}

/// The index and byte bounds `[start, end)` that answer `where_expr` for
/// `table`, or `None` when no single-column index can serve it.
///
/// Only a plain single-column index is eligible: a composite index's leading
/// column is not itself the indexed value, and an expression index has no
/// column to compare. The literal must also be one whose canonical encoding is
/// order-preserving, which is why the integer family is served here and text
/// falls back to the scan (see `crate::index::index_range_bounds`).
pub(crate) fn index_bounds_for_predicate(
    catalog: &InMemoryCatalog,
    table: &str,
    where_expr: Option<&Expression>,
) -> Option<(String, Vec<u8>, Vec<u8>)> {
    let (column, lower, upper) = column_range_predicate(where_expr?)?;
    let index = catalog
        .all_indexes_for_table(table)
        .into_iter()
        .find(|index| {
            index.expression.is_none() && crate::index::index_columns(index) == [column.clone()]
        })?;
    let namespace = index_value_prefix(&index.name, None);
    let mut start = namespace.clone();
    let mut end = prefix_end(&namespace);
    // Text equality answers through the exact-match prefix before the
    // integer path runs: `col = 'lit'` on a text-family column names exactly
    // the entries carrying `lit` spelled in the column's own representation
    // (see `text_equality_bounds` for the proof obligations). Anything
    // outside that shape falls through unchanged.
    if let (
        Some((crate::index::IndexRangeOp::GreaterOrEqual, low)),
        Some((crate::index::IndexRangeOp::LessOrEqual, high)),
    ) = (&lower, &upper)
    {
        if let Some((exact_start, exact_end)) =
            crate::index::text_equality_bounds(catalog, table, &column, &index.name, low, high)
        {
            return Some((index.name.clone(), exact_start, exact_end));
        }
    }
    if let Some((op, literal)) = lower {
        start = crate::index::index_range_bounds_coerced(&index.name, op, &literal)?.0;
    }
    if let Some((op, literal)) = upper {
        end = crate::index::index_range_bounds_coerced(&index.name, op, &literal)?.1;
    }
    // An unsatisfiable conjunction (`k > 10 AND k < 5`) yields an empty range;
    // returning it is correct and lets the scan short-circuit to no rows.
    (start <= end).then_some((index.name, start, end))
}

fn count_filter_target(targets: &[SelectTarget]) -> Option<Expression> {
    if targets.len() != 1 {
        return None;
    }
    let expr = match targets.first()? {
        SelectTarget::Expr { expr, .. } => expr,
        _ => return None,
    };
    match expr {
        Expression::FunctionCall {
            name,
            args,
            filter: Some(filter),
            ..
        } if name.eq_ignore_ascii_case("count")
            && matches!(args.first(), Some(Expression::Star)) =>
        {
            Some((**filter).clone())
        }
        _ => None,
    }
}

fn count_distinct_target(targets: &[SelectTarget]) -> Option<(Expression, Option<Expression>)> {
    let target = targets.first()?;
    // COUNT(DISTINCT expr) is parsed as:
    //   SelectTarget::Expr { expr: Expression::FunctionCall { distinct: true, ... }, ... }
    // We extract the inner Expression::FunctionCall and check for the COUNT + DISTINCT pattern.
    let expr = match target {
        SelectTarget::Expr { expr, .. } => expr,
        SelectTarget::Aliased { target, .. } => match target.as_ref() {
            SelectTarget::Expr { expr, .. } => expr,
            _ => return None,
        },
        _ => return None,
    };
    match expr {
        Expression::FunctionCall {
            name,
            args,
            distinct: true,
            filter,
            order_by: _,
            returning: _,
            null_handling: _,
            unique_keys: _,
        } if name.eq_ignore_ascii_case("count")
            && args.len() == 1
            && !matches!(args[0], Expression::Star) =>
        {
            Some((args[0].clone(), filter.as_deref().cloned()))
        }
        _ => None,
    }
}

pub(super) fn expression_has_aggregate(expr: &Expression) -> bool {
    match expr {
        // Only the outermost call is checked here historically; nested
        // aggregates inside scalar arguments are detected by recursing so
        // `jsonb_path_query_array(jsonb_agg(x), ...)` is routed to grouped
        // execution instead of the scalar "cannot be evaluated here" path.
        Expression::FunctionCall { name, args, .. } => {
            is_aggregate_function(name) || args.iter().any(expression_has_aggregate)
        }
        Expression::Equal(a, b)
        | Expression::NotEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::And(a, b)
        | Expression::Or(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b)
        | Expression::Modulo(a, b)
        | Expression::Concat(a, b)
        | Expression::Power(a, b) => expression_has_aggregate(a) || expression_has_aggregate(b),
        Expression::IsNull(a)
        | Expression::IsNotNull(a)
        | Expression::Not(a)
        | Expression::Negate(a)
        | Expression::Cast { expr: a, .. }
        | Expression::TypeCast { expr: a, .. }
        | Expression::Extract { expr: a, .. } => expression_has_aggregate(a),
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            operand
                .as_ref()
                .is_some_and(|a| expression_has_aggregate(a))
                || whens
                    .iter()
                    .any(|(a, b)| expression_has_aggregate(a) || expression_has_aggregate(b))
                || default
                    .as_ref()
                    .is_some_and(|a| expression_has_aggregate(a))
        }
        Expression::Coalesce(v) => v.iter().any(expression_has_aggregate),
        Expression::NullIf(a, b)
        | Expression::IsDistinctFrom(a, b)
        | Expression::JsonArrow {
            left: a, right: b, ..
        }
        | Expression::ArrayIndex { array: a, index: b } => {
            expression_has_aggregate(a) || expression_has_aggregate(b)
        }
        Expression::In { expr, list, .. } => {
            expression_has_aggregate(expr) || list.iter().any(expression_has_aggregate)
        }
        Expression::Between {
            expr, low, high, ..
        } => {
            expression_has_aggregate(expr)
                || expression_has_aggregate(low)
                || expression_has_aggregate(high)
        }
        Expression::Like { expr, pattern, .. } => {
            expression_has_aggregate(expr) || expression_has_aggregate(pattern)
        }
        _ => false,
    }
}

/// True when a select target contains a JSON aggregate call. Queries with
/// JSON aggregates run through the general engine, whose accumulator keeps
/// the full value list (the single-table fast path only tracks numeric state).
fn target_has_json_aggregate(target: &SelectTarget) -> bool {
    match target {
        SelectTarget::Expr { expr, .. } => expr_has_json_aggregate(expr),
        SelectTarget::Aliased { target, .. } => target_has_json_aggregate(target),
        _ => false,
    }
}

fn expr_has_json_aggregate(expr: &Expression) -> bool {
    match expr {
        Expression::FunctionCall { name, args, .. } => {
            crate::json::is_json_aggregate(name) || args.iter().any(expr_has_json_aggregate)
        }
        Expression::Equal(a, b)
        | Expression::NotEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::And(a, b)
        | Expression::Or(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b)
        | Expression::Modulo(a, b)
        | Expression::Concat(a, b)
        | Expression::Power(a, b)
        | Expression::NullIf(a, b)
        | Expression::IsDistinctFrom(a, b)
        | Expression::JsonArrow {
            left: a, right: b, ..
        }
        | Expression::ArrayIndex { array: a, index: b } => {
            expr_has_json_aggregate(a) || expr_has_json_aggregate(b)
        }
        Expression::IsNull(a)
        | Expression::IsNotNull(a)
        | Expression::Not(a)
        | Expression::Negate(a)
        | Expression::Cast { expr: a, .. }
        | Expression::TypeCast { expr: a, .. }
        | Expression::Extract { expr: a, .. } => expr_has_json_aggregate(a),
        Expression::In { expr, list, .. } => {
            expr_has_json_aggregate(expr) || list.iter().any(expr_has_json_aggregate)
        }
        Expression::Between {
            expr, low, high, ..
        } => {
            expr_has_json_aggregate(expr)
                || expr_has_json_aggregate(low)
                || expr_has_json_aggregate(high)
        }
        Expression::Like { expr, pattern, .. } => {
            expr_has_json_aggregate(expr) || expr_has_json_aggregate(pattern)
        }
        Expression::Coalesce(v) => v.iter().any(expr_has_json_aggregate),
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            operand.as_ref().is_some_and(|a| expr_has_json_aggregate(a))
                || whens
                    .iter()
                    .any(|(a, b)| expr_has_json_aggregate(a) || expr_has_json_aggregate(b))
                || default.as_ref().is_some_and(|a| expr_has_json_aggregate(a))
        }
        _ => false,
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::executor::Executor;
    use plomid_sql::Value;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch_engine() -> (
        Executor<plomid_txn::PlomidStorageEngine>,
        std::path::PathBuf,
    ) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("plomid-routing-it-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(root.with_extension("wal"));
        let wal = root.with_extension("wal");
        let engine = plomid_txn::PlomidStorageEngine::create(&root, &wal, 32).expect("engine");
        (Executor::new(engine).expect("executor"), root)
    }

    fn eq(column: &str, literal: Value) -> Expression {
        Expression::Equal(
            Box::new(Expression::ColumnRef(column.to_string())),
            Box::new(Expression::Literal(literal)),
        )
    }

    fn fixture() -> (
        Executor<plomid_txn::PlomidStorageEngine>,
        std::path::PathBuf,
    ) {
        let (mut session, root) = scratch_engine();
        session
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, code VARCHAR, n INTEGER);")
            .unwrap();
        session
            .execute("CREATE INDEX t_name_idx ON t (name);")
            .unwrap();
        session
            .execute("CREATE UNIQUE INDEX t_code_uidx ON t (code);")
            .unwrap();
        session.execute("CREATE INDEX t_n_idx ON t (n);").unwrap();
        (session, root)
    }

    #[test]
    fn text_equality_selects_exact_index_prefix() {
        let (session, root) = fixture();
        let catalog = session.catalog();
        let bounds = index_bounds_for_predicate(
            catalog,
            "public.t",
            Some(&eq("name", Value::Text("abc".to_string()))),
        )
        .expect("text equality on an indexed TEXT column probes the index");
        // The index name is catalog-generated; resolve it for the expectation.
        let index = catalog
            .all_indexes_for_table("public.t")
            .into_iter()
            .find(|index| crate::index::index_columns(index) == ["name".to_string()])
            .expect("name index");
        let expected_start = crate::index::index_tuple_prefix(
            &index.name,
            std::slice::from_ref(&Value::Text("abc".to_string())),
        );
        assert_eq!(bounds.0, index.name);
        assert_eq!(bounds.1, expected_start);
        assert!(bounds.1 < bounds.2);
        // Upper bound is the same stem with 0xff: only exact entries match.
        let mut stem = expected_start.clone();
        stem.pop();
        let mut expected_end = stem;
        expected_end.push(0xff);
        assert_eq!(bounds.2, expected_end);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(root.with_extension("wal"));
    }

    #[test]
    fn varchar_column_probes_with_column_spelling() {
        // VARCHAR columns hold VarChar-spelled entries (tag 3), TEXT columns
        // Text-spelled entries (tag 2): the probe follows the column, which
        // is what makes it agree with the writer even though SQL literals
        // arrive in a uniform spelling.
        let (session, root) = fixture();
        let catalog = session.catalog();
        let bounds = index_bounds_for_predicate(
            catalog,
            "public.t",
            Some(&eq("code", Value::Text("k-1".to_string()))),
        )
        .expect("varchar equality probes");
        let index = catalog
            .all_indexes_for_table("public.t")
            .into_iter()
            .find(|index| crate::index::index_columns(index) == ["code".to_string()])
            .expect("code index");
        let expected = crate::index::index_tuple_prefix(
            &index.name,
            std::slice::from_ref(&Value::VarChar("k-1".to_string())),
        );
        assert_eq!(bounds.1, expected);
        // The tag byte follows the writer spelling for VARCHAR (tag 3),
        // not the literal's TEXT spelling (tag 2).
        let prefix_len = format!("__plomid_index:{}:", index.name).len();
        assert_eq!(expected[prefix_len], 3);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(root.with_extension("wal"));
    }

    #[test]
    fn text_routing_accepts_variants_and_rejects_ranges() {
        let (session, root) = fixture();
        let catalog = session.catalog();
        // Unknown/VarChar/Name literals normalize to the same probe.
        for literal in [
            Value::Unknown("abc".to_string()),
            Value::VarChar("abc".to_string()),
            Value::Name("abc".to_string()),
        ] {
            assert!(
                index_bounds_for_predicate(catalog, "public.t", Some(&eq("name", literal)))
                    .is_some(),
                "text-family literal probes"
            );
        }
        // Unique backing index qualifies identically.
        assert!(
            index_bounds_for_predicate(
                catalog,
                "public.t",
                Some(&eq("code", Value::Text("k-1".to_string())))
            )
            .is_some(),
            "unique text index probes"
        );
        // Ranges, NULL, unindexed columns, and disjunctions keep the scan.
        let less = Expression::Less(
            Box::new(Expression::ColumnRef("name".to_string())),
            Box::new(Expression::Literal(Value::Text("abc".to_string()))),
        );
        assert!(
            index_bounds_for_predicate(catalog, "public.t", Some(&less)).is_none(),
            "text ranges still scan"
        );
        assert!(
            index_bounds_for_predicate(catalog, "public.t", Some(&eq("name", Value::Null)))
                .is_none(),
            "NULL equality still scans"
        );
        assert!(
            index_bounds_for_predicate(
                catalog,
                "public.t",
                Some(&eq("missing", Value::Text("abc".to_string())))
            )
            .is_none(),
            "unindexed column still scans"
        );
        let or = Expression::Or(
            Box::new(eq("name", Value::Text("a".to_string()))),
            Box::new(eq("name", Value::Text("b".to_string()))),
        );
        assert!(
            index_bounds_for_predicate(catalog, "public.t", Some(&or)).is_none(),
            "disjunctions still scan"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(root.with_extension("wal"));
    }

    #[test]
    fn integer_routing_unchanged_and_cross_type_safe() {
        let (session, root) = fixture();
        let catalog = session.catalog();
        // Integer equality keeps its existing range probe.
        assert!(
            index_bounds_for_predicate(catalog, "public.t", Some(&eq("n", Value::Int4(7))))
                .is_some(),
            "integer equality still probes"
        );
        // Text literal against an integer column keeps prior behavior
        // (integer-coercible text probes; anything else scans) — the text
        // probe must never claim an integer column.
        assert!(
            index_bounds_for_predicate(
                catalog,
                "public.t",
                Some(&eq("n", Value::Text("x".to_string())))
            )
            .is_none(),
            "non-numeric text on int column still scans"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(root.with_extension("wal"));
    }
}
