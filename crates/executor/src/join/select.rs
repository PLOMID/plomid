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
//! The join SELECT driver: planning, execution, grouping, ordering.

use crate::catalog_fn::{function_value, sequence_value};
use crate::error::{SqlError, SqlResult};
use crate::query::value_cmp;
use crate::row::values_equal;
use crate::window::compute_window;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{
    ColumnType, ComparisonOperator, Expression, FromClause, InMemoryCatalog, OrderByItem,
    QueryResult, SelectTarget, Value,
};
use plomid_txn::StorageEngine;
use std::collections::HashMap;

use super::plan::{
    expr_has_aggregate, plan_targets, planned_has_aggregate, validate_group_usage, Planned,
    PlannedKind,
};
use super::{
    aggregate::{
        collect_aggregate_slots, collect_target_aggregate_slots, push_slot, slot_matches,
        AggregateSlot, JoinAggregate,
    },
    eval::JoinEval,
    scope::{resolve_column, JoinScope, OuterContext},
    statement::{
        find_generate_series_call, generate_series_element_values, generate_series_expr_target,
        generate_series_rows, generate_series_target, rewrite_grouped, srf_result_rows,
        unnest_target,
    },
    support::{materialize_relation, try_indexed_join, unsupported},
};
use plomid_sql::Catalog;

/// FROM-subqueries, window functions, or subqueries in the query.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_join_select<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    from: Option<&FromClause>,
    targets: Vec<SelectTarget>,
    where_expr: Option<Expression>,
    group_by: Option<plomid_sql::GroupByClause>,
    having: Option<Expression>,
    order_by: Vec<OrderByItem>,
    limit: Option<usize>,
    offset: Option<usize>,
    current_database: &str,
    current_user: &str,
    outer: Option<&OuterContext>,
    depth: usize,
    distinct_on: Vec<Expression>,
) -> SqlResult<QueryResult> {
    let Some(from) = from else {
        if targets.len() == 1 {
            if let Some((expr, alias)) = generate_series_target(&targets[0]) {
                let empty_scopes = Vec::new();
                let empty_row = Vec::new();
                let Expression::FunctionCall { args, .. } = expr else {
                    unreachable!()
                };
                let values = args
                    .iter()
                    .map(|arg| {
                        JoinEval::eval_join_expr(
                            engine,
                            catalog,
                            &empty_scopes,
                            &empty_row,
                            arg,
                            outer,
                            depth,
                            current_database,
                            current_user,
                        )
                    })
                    .collect::<SqlResult<Vec<_>>>()?;
                let column = alias.unwrap_or_else(|| "generate_series".into());
                return Ok(QueryResult::Rows {
                    columns: vec![column],
                    column_types: vec![Some(ColumnType::new(
                        plomid_types::TypeOid::INT8,
                        plomid_types::NO_TYPEMOD,
                    ))],
                    rows: generate_series_rows(Value::Array {
                        element_oid: plomid_types::TypeOid::INT8,
                        elements: generate_series_element_values(&values)?,
                    })?,
                });
            }
            // `SELECT 1000 - generate_series(1, 3);`: a target expression
            // *containing* a generate_series call expands the SRF across the
            // output rows (PostgreSQL SRF-in-targetlist semantics) — the
            // surrounding expression is evaluated once per generated element.
            if let Some((expr, alias)) = generate_series_expr_target(&targets[0]) {
                let empty_scopes = Vec::new();
                let empty_row = Vec::new();
                let (_, srf_args) = find_generate_series_call(&expr)
                    .ok_or_else(|| unsupported("generate_series call not found"))?;
                let values = srf_args
                    .iter()
                    .map(|arg| {
                        JoinEval::eval_join_expr(
                            engine,
                            catalog,
                            &empty_scopes,
                            &empty_row,
                            arg,
                            outer,
                            depth,
                            current_database,
                            current_user,
                        )
                    })
                    .collect::<SqlResult<Vec<_>>>()?;
                let column = alias.unwrap_or_else(|| "generate_series".into());
                let mut rows = Vec::new();
                for element in generate_series_element_values(&values)? {
                    let mut eval = JoinEval::new(
                        engine,
                        catalog,
                        &empty_scopes,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    );
                    eval.srf_element = Some(element);
                    rows.push(vec![eval.eval(&empty_row, &expr)?]);
                }
                let column_types: Vec<Option<ColumnType>> = rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(crate::coerce::value_column_type)
                    .map(Some)
                    .into_iter()
                    .collect();
                return Ok(QueryResult::Rows {
                    columns: vec![column],
                    column_types,
                    rows,
                });
            }
        }
        if targets.len() == 1 {
            if let Some((expr, columns)) = unnest_target(&targets[0]) {
                let empty_scopes = Vec::new();
                let empty_row = Vec::new();
                let value = JoinEval::eval_join_expr(
                    engine,
                    catalog,
                    &empty_scopes,
                    &empty_row,
                    &expr,
                    outer,
                    depth,
                    current_database,
                    current_user,
                )?;
                let (column_types, rows) = srf_result_rows(value)?;
                return Ok(QueryResult::Rows {
                    columns,
                    column_types: column_types.into_iter().map(Some).collect(),
                    rows,
                });
            }
        }
        if let Some(predicate) = where_expr.as_ref() {
            let empty: Vec<Value> = Vec::new();
            let empty_scopes: Vec<JoinScope> = Vec::new();
            let value = JoinEval::eval_join_expr(
                engine,
                catalog,
                &empty_scopes,
                &empty,
                predicate,
                outer,
                depth,
                current_database,
                current_user,
            )?;
            if !matches!(value, Value::Bool(true)) {
                return Ok(QueryResult::Rows {
                    columns: Vec::new(),
                    column_types: Vec::new(),
                    rows: Vec::new(),
                });
            }
        }
        return bare_select(engine, catalog, &targets, current_database, current_user);
    };
    // Selective joins are served from indexes; every other FROM tree keeps the
    // general materialization path below.
    let (scopes, rows) = match try_indexed_join(
        engine,
        catalog,
        from,
        where_expr.as_ref(),
        outer,
        depth,
        current_database,
        current_user,
    )? {
        Some(result) => result,
        None => materialize_relation(
            engine,
            catalog,
            from,
            outer,
            depth,
            current_database,
            current_user,
        )?,
    };

    // WHERE filtering (three-valued: only TRUE survives).
    let mut filtered: Vec<Vec<Value>> = Vec::new();
    if let Some(predicate) = &where_expr {
        for row in rows {
            let v = JoinEval::eval_join_expr(
                engine,
                catalog,
                &scopes,
                &row,
                predicate,
                outer,
                depth,
                current_database,
                current_user,
            )?;
            if matches!(v, Value::Bool(true)) {
                filtered.push(row);
            }
        }
    } else {
        filtered = rows;
    }

    if targets.len() == 1 {
        if let Some((expr, columns)) = unnest_target(&targets[0]) {
            let mut eval = JoinEval::new(
                engine,
                catalog,
                &scopes,
                outer,
                depth,
                current_database,
                current_user,
            );
            let mut output = Vec::new();
            let mut column_types: Option<Vec<ColumnType>> = None;
            for row in &filtered {
                let value = eval.eval(row, &expr)?;
                let (types, mut rows) = srf_result_rows(value)?;
                if column_types.is_none() {
                    column_types = Some(types);
                }
                output.append(&mut rows);
            }
            let types = column_types.unwrap_or_else(|| vec![ColumnType::text()]);
            return Ok(QueryResult::Rows {
                columns,
                column_types: types.into_iter().map(Some).collect(),
                rows: output,
            });
        }
    }

    let planned = plan_targets(&scopes, &targets)?;
    if group_by.is_none() && having.is_none() && targets.len() == 1 {
        let aggregate_expr = match &targets[0] {
            SelectTarget::Expr { expr, .. } => Some(expr.clone()),
            SelectTarget::FunctionCall { name, args } => Some(Expression::FunctionCall {
                name: name.clone(),
                args: args.clone(),
                distinct: false,
                filter: None,
                order_by: Vec::new(),
                returning: None,
                null_handling: None,
                unique_keys: None,
            }),
            _ => None,
        };
        if let Some(Expression::FunctionCall {
            name, args, filter, ..
        }) = aggregate_expr.as_ref()
        {
            if name.eq_ignore_ascii_case("string_agg") || name.eq_ignore_ascii_case("array_agg") {
                if args.is_empty() {
                    return Err(unsupported(format!("function {name} requires an argument")));
                }
                let mut values = Vec::new();
                for row in &filtered {
                    if let Some(predicate) = filter {
                        if !matches!(
                            JoinEval::eval_join_expr(
                                engine,
                                catalog,
                                &scopes,
                                row,
                                predicate,
                                outer,
                                depth,
                                current_database,
                                current_user
                            )?,
                            Value::Bool(true)
                        ) {
                            continue;
                        }
                    }
                    let value = JoinEval::eval_join_expr(
                        engine,
                        catalog,
                        &scopes,
                        row,
                        &args[0],
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )?;
                    if !value.is_null() {
                        values.push(value);
                    }
                }
                let result = if name.eq_ignore_ascii_case("string_agg") {
                    let separator = args
                        .get(1)
                        .map(|e| {
                            JoinEval::eval_join_expr(
                                engine,
                                catalog,
                                &scopes,
                                &[],
                                e,
                                outer,
                                depth,
                                current_database,
                                current_user,
                            )
                        })
                        .transpose()?
                        .unwrap_or(Value::Text(",".into()))
                        .to_sql_text();
                    if values.is_empty() {
                        Value::Null
                    } else {
                        Value::Text(
                            values
                                .iter()
                                .map(Value::to_sql_text)
                                .collect::<Vec<_>>()
                                .join(&separator),
                        )
                    }
                } else if values.is_empty() {
                    Value::Null
                } else {
                    Value::Array {
                        element_oid: plomid_types::TypeOid::TEXT,
                        elements: values,
                    }
                };
                return Ok(QueryResult::Rows {
                    columns: vec![name.clone()],
                    column_types: vec![None],
                    rows: vec![vec![result]],
                });
            }
        }
    }
    let is_grouped = group_by.is_some()
        || having.is_some()
        || planned_has_aggregate(&planned)
        || order_by.iter().any(|item| expr_has_aggregate(&item.expr));

    if is_grouped {
        return execute_grouped_join(
            engine,
            catalog,
            &scopes,
            filtered,
            planned,
            &targets,
            group_by,
            having,
            order_by,
            limit,
            offset,
            outer,
            depth,
            current_database,
            current_user,
        );
    }

    // Window functions need the whole result set before projection.
    let mut window_columns: HashMap<usize, Vec<Value>> = HashMap::new();
    for (index, plan) in planned.iter().enumerate() {
        if let PlannedKind::Window { fname, args, over } = &plan.kind {
            let values = compute_window(
                engine,
                catalog,
                &scopes,
                &filtered,
                fname,
                args,
                over,
                outer,
                depth,
                current_database,
                current_user,
            )?;
            window_columns.insert(index, values);
        }
    }

    let mut projected: Vec<Vec<Value>> = Vec::with_capacity(filtered.len());
    for (row_index, row) in filtered.iter().enumerate() {
        projected.push(project_plain(
            engine,
            catalog,
            &scopes,
            row,
            row_index,
            &planned,
            &window_columns,
            outer,
            depth,
            current_database,
            current_user,
        )?);
    }

    // ORDER BY over the pre-projection rows, with output-name/ordinal
    // fallbacks for aliases and positional ordering.
    // `keyed` keeps (projected_row, order_keys, source_row) so DISTINCT ON
    // can evaluate its ON expressions on the pre-projection row after the
    // ORDER BY sort, then keep the first row per ON-group.
    let mut keyed: Vec<(Vec<Value>, Vec<Value>, Vec<Value>)> = Vec::with_capacity(projected.len());
    for (row_index, row) in filtered.iter().enumerate() {
        keyed.push((
            projected[row_index].clone(),
            order_keys(
                engine,
                catalog,
                &scopes,
                row,
                &planned,
                &projected[row_index],
                &order_by,
                outer,
                depth,
                current_database,
                current_user,
            )?,
            row.clone(),
        ));
    }
    if let Some(take) = limit.map(|take| offset.unwrap_or(0).saturating_add(take)) {
        if take == 0 {
            keyed.clear();
        } else if take < keyed.len() {
            keyed.select_nth_unstable_by(take - 1, |(_, a, _), (_, b, _)| {
                compare_keys(a, b, &order_by)
            });
            keyed.truncate(take);
        }
    }
    keyed.sort_by(|(_, a, _), (_, b, _)| compare_keys(a, b, &order_by));

    if !distinct_on.is_empty() {
        if group_by.is_some()
            || having.is_some()
            || planned_has_aggregate(&planned)
            || order_by.iter().any(|item| expr_has_aggregate(&item.expr))
            || planned
                .iter()
                .any(|p| matches!(p.kind, PlannedKind::Window { .. }))
        {
            return Err(unsupported(
                "DISTINCT ON is not supported with GROUP BY or aggregates",
            ));
        }
        let mut seen: Vec<Vec<Value>> = Vec::new();
        let mut deduped: Vec<(Vec<Value>, Vec<Value>, Vec<Value>)> = Vec::new();
        for entry in keyed {
            let source_row = &entry.2;
            let projected_row = &entry.0;
            let mut key = Vec::with_capacity(distinct_on.len());
            for expr in &distinct_on {
                // Prefer output-alias resolution (mirrors ORDER BY), then
                // fall back to evaluating against the source row.
                let value = match expr {
                    Expression::ColumnRef(name) if !name.contains('.') => {
                        match planned
                            .iter()
                            .position(|p| p.name.eq_ignore_ascii_case(name))
                        {
                            Some(index) => projected_row[index].clone(),
                            None => JoinEval::eval_join_expr(
                                engine,
                                catalog,
                                &scopes,
                                source_row,
                                expr,
                                outer,
                                depth,
                                current_database,
                                current_user,
                            )?,
                        }
                    }
                    other => JoinEval::eval_join_expr(
                        engine,
                        catalog,
                        &scopes,
                        source_row,
                        other,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )?,
                };
                key.push(value);
            }
            let duplicate = seen.iter().any(|prev: &Vec<Value>| {
                prev.len() == key.len()
                    && prev.iter().zip(key.iter()).all(|(l, r)| values_equal(l, r))
            });
            if !duplicate {
                seen.push(key);
                deduped.push(entry);
            }
        }
        keyed = deduped;
    }

    let final_rows: Vec<Vec<Value>> = keyed
        .into_iter()
        .map(|(row, _, _)| row)
        .skip(offset.unwrap_or(0))
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    Ok(QueryResult::Rows {
        columns: planned.iter().map(|p| p.name.clone()).collect(),
        column_types: planned.iter().map(|p| p.ty).collect(),
        rows: final_rows,
    })
}

/// Evaluates one ORDER BY key set for a row, with output-alias and positional
/// ordinals as fallbacks (PostgreSQL semantics).
#[allow(clippy::too_many_arguments)]
pub(super) fn order_keys<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    row: &[Value],
    planned: &[Planned],
    projected: &[Value],
    order_by: &[OrderByItem],
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<Vec<Value>> {
    let mut keys = Vec::with_capacity(order_by.len());
    for item in order_by {
        let value = match &item.expr {
            Expression::Literal(Value::Int4(n)) if *n > 0 => {
                projected.get((*n - 1) as usize).cloned().ok_or_else(|| {
                    SqlError::Storage(PlomidError::new(
                        ErrorKind::InvalidArgument,
                        format!("ORDER BY position {n} is not in select list"),
                    ))
                })?
            }
            Expression::ColumnRef(name) if !name.contains('.') => {
                match planned
                    .iter()
                    .position(|p| p.name.eq_ignore_ascii_case(name))
                {
                    Some(index) => projected[index].clone(),
                    None => JoinEval::eval_join_expr(
                        engine,
                        catalog,
                        scopes,
                        row,
                        &item.expr,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )?,
                }
            }
            other => JoinEval::eval_join_expr(
                engine,
                catalog,
                scopes,
                row,
                other,
                outer,
                depth,
                current_database,
                current_user,
            )?,
        };
        keys.push(value);
    }
    Ok(keys)
}

/// Compares two ORDER BY key tuples honouring per-item direction and NULL
/// placement (NULLS LAST ascending, NULLS FIRST descending).
pub(super) fn compare_keys(
    a: &[Value],
    b: &[Value],
    order_by: &[OrderByItem],
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

/// Projects one row in the non-grouped path.
#[allow(clippy::too_many_arguments)]
pub(super) fn project_plain<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    row: &[Value],
    row_index: usize,
    planned: &[Planned],
    window_columns: &HashMap<usize, Vec<Value>>,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<Vec<Value>> {
    let mut out = Vec::with_capacity(planned.len());
    for (index, plan) in planned.iter().enumerate() {
        let value = match &plan.kind {
            PlannedKind::Source(i) => row.get(*i).cloned().unwrap_or(Value::Null),
            PlannedKind::Expr(expr) => JoinEval::eval_join_expr(
                engine,
                catalog,
                scopes,
                row,
                expr,
                outer,
                depth,
                current_database,
                current_user,
            )?,
            PlannedKind::Window { .. } => window_columns
                .get(&index)
                .and_then(|values| values.get(row_index).cloned())
                .unwrap_or(Value::Null),
            PlannedKind::Session(name) => function_value(name, current_database, current_user)?,
            PlannedKind::Sequence { name, argument } => sequence_value(engine, name, argument)?,
            PlannedKind::Aggregate { .. } => {
                return Err(unsupported(
                    "aggregate function used outside of aggregation",
                ));
            }
        };
        out.push(value);
    }
    Ok(out)
}

/// Grouped (aggregate) execution over joined rows.
#[allow(clippy::too_many_arguments)]
pub(super) fn execute_grouped_join<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    rows: Vec<Vec<Value>>,
    planned: Vec<Planned>,
    targets: &[SelectTarget],
    group_by: Option<plomid_sql::GroupByClause>,
    having: Option<Expression>,
    order_by: Vec<OrderByItem>,
    limit: Option<usize>,
    offset: Option<usize>,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    if planned
        .iter()
        .any(|p| matches!(p.kind, PlannedKind::Window { .. }))
    {
        return Err(unsupported(
            "window functions combined with GROUP BY or aggregates are not supported",
        ));
    }

    // Validate against union of grouping exprs; preserves legacy behavior.
    let union_exprs: Vec<Expression> = group_by
        .as_ref()
        .map(|g| g.union_exprs())
        .unwrap_or_default();
    for plan in &planned {
        if let PlannedKind::Expr(expr) = &plan.kind {
            validate_group_usage(expr, &format!("column \"{}\"", plan.name), &union_exprs)?;
        }
    }
    if let Some(having_expr) = &having {
        validate_group_usage(having_expr, "HAVING", &union_exprs)?;
    }

    // Collect aggregate slots across the select list, HAVING, and ORDER BY.
    let mut slots: Vec<AggregateSlot> = Vec::new();
    for target in targets {
        collect_target_aggregate_slots(target, &mut slots);
    }
    for plan in &planned {
        match &plan.kind {
            PlannedKind::Aggregate { name, arg }
                if !matches!(
                    name.to_ascii_lowercase().as_str(),
                    "string_agg" | "array_agg"
                ) =>
            {
                push_slot(&mut slots, name, arg.as_ref())
            }
            PlannedKind::Expr(expr) => collect_aggregate_slots(expr, &mut slots),
            _ => {}
        }
    }
    if let Some(having_expr) = &having {
        collect_aggregate_slots(having_expr, &mut slots);
    }
    for item in &order_by {
        collect_aggregate_slots(&item.expr, &mut slots);
    }

    // Group rows per grouping set via a shared helper.
    let grouping_sets: Vec<Vec<Expression>> = group_by
        .as_ref()
        .map(|g| g.to_sets())
        .unwrap_or_else(|| vec![Vec::new()]);
    if grouping_sets.len() == 1 {
        let group_exprs = grouping_sets.into_iter().next().unwrap_or_default();
        let groups = build_join_groups(
            engine,
            catalog,
            scopes,
            &rows,
            &group_exprs,
            outer,
            depth,
            current_database,
            current_user,
        )?;
        return execute_grouped_join_groups(
            engine,
            catalog,
            scopes,
            rows,
            planned,
            group_exprs,
            having,
            order_by,
            limit,
            offset,
            outer,
            depth,
            current_database,
            current_user,
            groups,
            slots,
        );
    }
    // Advanced grouping: concatenate per-set results, then ORDER/LIMIT.
    let mut combined: Vec<Vec<Value>> = Vec::new();
    let mut out_cols: Vec<String> = Vec::new();
    let mut out_types: Vec<Option<plomid_sql::ColumnType>> = Vec::new();
    let mut seen_schema = false;
    for group_exprs in grouping_sets {
        let groups = build_join_groups(
            engine,
            catalog,
            scopes,
            &rows,
            &group_exprs,
            outer,
            depth,
            current_database,
            current_user,
        )?;
        let res = execute_grouped_join_groups(
            engine,
            catalog,
            scopes,
            rows.clone(),
            planned.clone(),
            group_exprs,
            having.clone(),
            Vec::new(),
            None,
            None,
            outer,
            depth,
            current_database,
            current_user,
            groups,
            slots.clone(),
        )?;
        if let QueryResult::Rows {
            columns,
            column_types,
            rows,
        } = res
        {
            if !seen_schema {
                out_cols = columns;
                out_types = column_types;
                seen_schema = true;
            }
            combined.extend(rows);
        }
    }
    // ORDER BY on combined output: resolve output aliases/positions.
    let mut keyed: Vec<(Vec<Value>, Vec<Value>)> = Vec::with_capacity(combined.len());
    for row in combined {
        let mut keys = Vec::with_capacity(order_by.len());
        for item in &order_by {
            if let Expression::ColumnRef(name) = &item.expr {
                if let Some(idx) = out_cols
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(name))
                    .or_else(|| {
                        planned
                            .iter()
                            .position(|p| p.name.eq_ignore_ascii_case(name))
                    })
                {
                    keys.push(row[idx].clone());
                    continue;
                }
            }
            keys.push(Value::Null);
        }
        keyed.push((row, keys));
    }
    if !order_by.is_empty() {
        keyed.sort_by(|(_, a), (_, b)| compare_keys(a, b, &order_by));
    }
    let final_rows: Vec<Vec<Value>> = keyed
        .into_iter()
        .map(|(r, _)| r)
        .skip(offset.unwrap_or(0))
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    return Ok(QueryResult::Rows {
        columns: out_cols,
        column_types: out_types,
        rows: final_rows,
    });
}

/// Builds per-set groups for the join engine (shared helper).
#[allow(clippy::too_many_arguments)]
pub(super) fn build_join_groups<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    rows: &[Vec<Value>],
    group_exprs: &[Expression],
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<Vec<(Vec<Value>, Vec<usize>)>> {
    let mut groups: Vec<(Vec<Value>, Vec<usize>)> = Vec::new();
    let mut group_index: HashMap<String, usize> = HashMap::new();
    for (row_index, row) in rows.iter().enumerate() {
        let mut key_parts = Vec::with_capacity(group_exprs.len());
        let mut key_values = Vec::with_capacity(group_exprs.len());
        for expr in group_exprs {
            let v = JoinEval::eval_join_expr(
                engine,
                catalog,
                scopes,
                row,
                expr,
                outer,
                depth,
                current_database,
                current_user,
            )?;
            key_parts.push(v.to_sql_text());
            key_values.push(v);
        }
        let entry = *group_index
            .entry(key_parts.join("\u{1}"))
            .or_insert_with(|| {
                groups.push((Vec::new(), Vec::new()));
                groups.len() - 1
            });
        groups[entry].0 = key_values;
        groups[entry].1.push(row_index);
    }
    if group_exprs.is_empty() && groups.is_empty() {
        groups.push((Vec::new(), Vec::new()));
    }
    Ok(groups)
}

/// Per-group accumulation, HAVING filtering, projection, ordering.
/// Evaluates ANY quantified comparison: returns true if at least one comparison is true.
#[allow(clippy::too_many_arguments)]
/// PostgreSQL semantics:
/// - Returns true if at least one comparison evaluates to true
/// - Returns false if all comparisons evaluate to false
/// - Returns NULL if no comparison is true and at least one comparison is NULL
/// - For empty subquery, returns false
pub(super) fn eval_any_quantified(
    left_val: Value,
    operator: ComparisonOperator,
    values: Vec<Value>,
) -> SqlResult<Value> {
    if values.is_empty() {
        return Ok(Value::Bool(false));
    }

    let mut has_true = false;
    let mut has_null = false;

    for right_val in &values {
        let result = crate::query::compare_values_for_any_all(
            left_val.clone(),
            operator,
            right_val.clone(),
        )?;
        match result {
            Value::Bool(true) => has_true = true,
            Value::Null => has_null = true,
            Value::Bool(false) => {}
            _ => {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "comparison did not return a boolean or null value",
                )));
            }
        }
    }

    if has_true {
        Ok(Value::Bool(true))
    } else if has_null {
        Ok(Value::Null)
    } else {
        Ok(Value::Bool(false))
    }
}

/// Evaluates ALL quantified comparison: returns true if all comparisons are true.
/// PostgreSQL semantics:
/// - Returns true if all comparisons evaluate to true
/// - Returns false if at least one comparison evaluates to false
/// - Returns NULL if no comparison is false and at least one comparison is NULL
/// - For empty subquery, returns true
pub(super) fn eval_all_quantified(
    left_val: Value,
    operator: ComparisonOperator,
    values: Vec<Value>,
) -> SqlResult<Value> {
    if values.is_empty() {
        return Ok(Value::Bool(true));
    }

    let mut has_false = false;
    let mut has_null = false;

    for right_val in &values {
        let result = crate::query::compare_values_for_any_all(
            left_val.clone(),
            operator,
            right_val.clone(),
        )?;
        match result {
            Value::Bool(false) => has_false = true,
            Value::Null => has_null = true,
            Value::Bool(true) => {}
            _ => {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "comparison did not return a boolean or null value",
                )));
            }
        }
    }

    if has_false {
        Ok(Value::Bool(false))
    } else if has_null {
        Ok(Value::Null)
    } else {
        Ok(Value::Bool(true))
    }
}

/// Compares two rows by their aggregate ORDER BY key expressions, respecting
/// ASC/DESC direction and NULLS FIRST/NULLS LAST placement.
#[allow(clippy::too_many_arguments)]
pub(super) fn compare_aggregate_order<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    row_a: &[Value],
    row_b: &[Value],
    order_by: &[OrderByItem],
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> std::cmp::Ordering {
    for item in order_by {
        let a = JoinEval::eval_join_expr(
            engine,
            catalog,
            scopes,
            row_a,
            &item.expr,
            outer,
            depth,
            current_database,
            current_user,
        )
        .unwrap_or(Value::Null);
        let b = JoinEval::eval_join_expr(
            engine,
            catalog,
            scopes,
            row_b,
            &item.expr,
            outer,
            depth,
            current_database,
            current_user,
        )
        .unwrap_or(Value::Null);

        if a.is_null() && b.is_null() {
            // Both NULL — equal for this key; proceed to next ORDER BY item.
            continue;
        }
        let nulls_first = item.nulls_first.unwrap_or(item.descending);
        if a.is_null() {
            // a is NULL, b is not.
            // NULLS FIRST → a comes first (Less).
            // NULLS LAST  → a comes last (Greater).
            return if nulls_first {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        if b.is_null() {
            // b is NULL, a is not.
            return if nulls_first {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            };
        }

        // Both non-null: compare, then apply DESC direction if needed.
        let ord = crate::query::value_cmp(&a, &b).unwrap_or(std::cmp::Ordering::Equal);
        let ord = if item.descending { ord.reverse() } else { ord };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// Steps a single aggregate slot for a single input row, including FILTER
/// evaluation and JSON object-pair handling.
#[allow(clippy::too_many_arguments)]
pub(super) fn step_aggregate_slot<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    row: &[Value],
    slot: &mut AggregateSlot,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<()> {
    if let Some(separator) = slot.2.separator_expr.as_ref() {
        if slot.2.separator == "," {
            let value = JoinEval::eval_join_expr(
                engine,
                catalog,
                scopes,
                row,
                separator,
                outer,
                depth,
                current_database,
                current_user,
            )?;
            if !value.is_null() {
                slot.2.separator = value.to_sql_text();
            }
        }
    }
    if let Some(filter) = slot.2.filter.as_ref() {
        let matches = JoinEval::eval_join_expr(
            engine,
            catalog,
            scopes,
            row,
            filter,
            outer,
            depth,
            current_database,
            current_user,
        )?;
        if !matches!(matches, Value::Bool(true)) {
            return Ok(());
        }
    }
    let value = match &slot.1 {
        Some(Expression::Star) => None,
        Some(arg) => Some(JoinEval::eval_join_expr(
            engine,
            catalog,
            scopes,
            row,
            arg,
            outer,
            depth,
            current_database,
            current_user,
        )?),
        None => None,
    };
    let lname = slot.0.to_ascii_lowercase();
    if crate::json::is_json_aggregate(&lname) && crate::json::is_json_object_aggregate(&lname) {
        let key = value.unwrap_or(Value::Null);
        let value = slot
            .2
            .separator_expr
            .as_ref()
            .map(|arg| {
                JoinEval::eval_join_expr(
                    engine,
                    catalog,
                    scopes,
                    row,
                    arg,
                    outer,
                    depth,
                    current_database,
                    current_user,
                )
            })
            .transpose()?
            .unwrap_or(Value::Null);
        slot.2.step_pair(key, value);
    } else {
        slot.2.step(value);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_grouped_join_groups<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    rows: Vec<Vec<Value>>,
    planned: Vec<Planned>,
    group_exprs: Vec<Expression>,
    having: Option<Expression>,
    order_by: Vec<OrderByItem>,
    limit: Option<usize>,
    offset: Option<usize>,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
    groups: Vec<(Vec<Value>, Vec<usize>)>,
    slots: Vec<AggregateSlot>,
) -> SqlResult<QueryResult> {
    let total_width: usize = scopes.iter().map(JoinScope::width).sum();
    let mut out_rows: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();

    for (key_values, member_indices) in &groups {
        let mut accs: Vec<AggregateSlot> = slots
            .iter()
            .map(|slot| {
                (
                    slot.0.clone(),
                    slot.1.clone(),
                    JoinAggregate::new(
                        &slot.0,
                        slot.1.as_ref(),
                        slot.2.distinct,
                        slot.2.filter.as_ref(),
                        slot.2.separator_expr.as_ref(),
                        &slot.2.order_by,
                        slot.2.null_handling,
                        slot.2.unique_keys,
                    ),
                )
            })
            .collect();

        // Step each slot across its member rows. Slots with a non-empty
        // ORDER BY are stepped in the ORDER BY row order; the rest use the
        // natural (input) row order.
        for slot in &mut accs {
            let ordered: Vec<usize> = if slot.2.order_by.is_empty() {
                member_indices.clone()
            } else {
                let mut indices: Vec<_> = member_indices.clone();
                indices.sort_by(|&a, &b| {
                    let row_a = &rows[a];
                    let row_b = &rows[b];
                    compare_aggregate_order(
                        engine,
                        catalog,
                        scopes,
                        row_a,
                        row_b,
                        &slot.2.order_by,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )
                });
                indices
            };
            for row_index in ordered {
                let row = &rows[row_index];
                step_aggregate_slot(
                    engine,
                    catalog,
                    scopes,
                    row,
                    slot,
                    outer,
                    depth,
                    current_database,
                    current_user,
                )?;
            }
        }

        // Synthetic row: NULL everywhere, then group-key column values.
        let mut synthetic = vec![Value::Null; total_width];
        let mut group_pairs: Vec<(Expression, Value)> = Vec::new();
        for (expr, value) in group_exprs.iter().zip(key_values.iter()) {
            if let Expression::ColumnRef(name) = expr {
                if let Ok(index) = resolve_column(scopes, None, name) {
                    synthetic[index] = value.clone();
                }
            }
            group_pairs.push((expr.clone(), value.clone()));
        }

        // HAVING filter (rewritten with finalized aggregates).
        if let Some(having_expr) = &having {
            let rewritten = rewrite_grouped(having_expr, &accs, &group_pairs);
            let result = JoinEval::eval_join_expr(
                engine,
                catalog,
                scopes,
                &synthetic,
                &rewritten,
                outer,
                depth,
                current_database,
                current_user,
            )?;
            if !matches!(result, Value::Bool(true)) {
                continue;
            }
        }

        // Projection.
        let mut projected = Vec::with_capacity(planned.len());
        for plan in &planned {
            let value = match &plan.kind {
                PlannedKind::Source(i) => synthetic.get(*i).cloned().unwrap_or(Value::Null),
                PlannedKind::Expr(expr) => {
                    let rewritten = rewrite_grouped(expr, &accs, &group_pairs);
                    JoinEval::eval_join_expr(
                        engine,
                        catalog,
                        scopes,
                        &synthetic,
                        &rewritten,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )?
                }
                PlannedKind::Aggregate { name, arg } => accs
                    .iter()
                    .find(|slot| slot_matches(slot, name, arg.as_ref()))
                    .map(|slot| slot.2.finalize())
                    .unwrap_or(Value::Null),
                PlannedKind::Session(name) => function_value(name, current_database, current_user)?,
                _ => {
                    return Err(unsupported(
                        "SELECT target is not valid in an aggregated query",
                    ));
                }
            };
            projected.push(value);
        }

        // ORDER BY keys for this group.
        let mut keys = Vec::with_capacity(order_by.len());
        for item in &order_by {
            if let Expression::ColumnRef(name) = &item.expr {
                if let Some(index) = planned
                    .iter()
                    .position(|p| p.name.eq_ignore_ascii_case(name))
                {
                    keys.push(projected[index].clone());
                    continue;
                }
            }
            let rewritten = rewrite_grouped(&item.expr, &accs, &group_pairs);
            keys.push(JoinEval::eval_join_expr(
                engine,
                catalog,
                scopes,
                &synthetic,
                &rewritten,
                outer,
                depth,
                current_database,
                current_user,
            )?);
        }
        out_rows.push((projected, keys));
    }

    if !order_by.is_empty() {
        out_rows.sort_by(|(_, a), (_, b)| compare_keys(a, b, &order_by));
    }
    let final_rows: Vec<Vec<Value>> = out_rows
        .into_iter()
        .map(|(row, _)| row)
        .skip(offset.unwrap_or(0))
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    Ok(QueryResult::Rows {
        columns: planned.iter().map(|p| p.name.clone()).collect(),
        column_types: planned.iter().map(|p| p.ty).collect(),
        rows: final_rows,
    })
}

/// `SELECT` without a FROM clause: literals, session functions, and constant
/// expressions over them.
pub(super) fn bare_select<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    targets: &[SelectTarget],
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    let empty_scopes: Vec<JoinScope> = Vec::new();
    let planned = plan_targets(&empty_scopes, targets)?;
    static EMPTY_ROW: Vec<Value> = Vec::new();

    // Aggregates without a FROM clause form a single implicit group over the
    // one empty row (PostgreSQL semantics for `SELECT COUNT(*)`).
    let mut aggregate_slots: Vec<AggregateSlot> = Vec::new();
    if planned
        .iter()
        .any(|plan| matches!(plan.kind, PlannedKind::Aggregate { .. }))
    {
        let mut collected: Vec<AggregateSlot> = Vec::new();
        for plan in &planned {
            if let PlannedKind::Aggregate { name, arg } = &plan.kind {
                push_slot(&mut collected, name, arg.as_ref());
            }
        }
        for slot in &mut collected {
            let value = match &slot.1 {
                Some(Expression::Star) => None,
                Some(arg) => Some(JoinEval::eval_join_expr(
                    engine,
                    catalog,
                    &empty_scopes,
                    &EMPTY_ROW,
                    arg,
                    None,
                    0,
                    current_database,
                    current_user,
                )?),
                None => None,
            };
            let lname = slot.0.to_ascii_lowercase();
            if crate::json::is_json_aggregate(&lname)
                && crate::json::is_json_object_aggregate(&lname)
            {
                let key = value.unwrap_or(Value::Null);
                let value = slot
                    .2
                    .separator_expr
                    .as_ref()
                    .map(|arg| {
                        JoinEval::eval_join_expr(
                            engine,
                            catalog,
                            &empty_scopes,
                            &EMPTY_ROW,
                            arg,
                            None,
                            0,
                            current_database,
                            current_user,
                        )
                    })
                    .transpose()?
                    .unwrap_or(Value::Null);
                slot.2.step_pair(key, value);
            } else {
                slot.2.step(value);
            }
        }
        aggregate_slots = collected;
    }

    let row = planned
        .iter()
        .map(|plan| match &plan.kind {
            PlannedKind::Session(name) => function_value(name, current_database, current_user),
            PlannedKind::Sequence { name, argument } => sequence_value(engine, name, argument),
            PlannedKind::Expr(expr) => JoinEval::eval_join_expr(
                engine,
                catalog,
                &empty_scopes,
                &EMPTY_ROW,
                expr,
                None,
                0,
                current_database,
                current_user,
            ),
            PlannedKind::Window { fname, args, over } => {
                let values = compute_window(
                    engine,
                    catalog,
                    &empty_scopes,
                    std::slice::from_ref(&EMPTY_ROW),
                    fname,
                    args,
                    over,
                    None,
                    0,
                    current_database,
                    current_user,
                )?;
                Ok(values.into_iter().next().unwrap_or(Value::Null))
            }
            PlannedKind::Aggregate { name, arg } => Ok(aggregate_slots
                .iter()
                .find(|slot| slot_matches(slot, name, arg.as_ref()))
                .map(|slot| slot.2.finalize())
                .unwrap_or(Value::Null)),
            other => Err(unsupported(format!(
                "SELECT target is not valid without a FROM clause: {other:?}"
            ))),
        })
        .collect::<SqlResult<Vec<_>>>()?;
    Ok(QueryResult::Rows {
        columns: planned.iter().map(|p| p.name.clone()).collect(),
        column_types: planned.iter().map(|p| p.ty).collect(),
        rows: vec![row],
    })
}

/// Casts a value to a user-defined type (composite, enum, or domain).
///
/// For composite types, this tags an existing `Value::Composite` with the
/// correct user-defined type OID so downstream consumers (to_json, etc.)
/// recognize it as that named composite rather than an anonymous record.
/// For enum types the value is the enum label, which PLOMID represents with
/// the first-class `Value::Enum` variant (not a single-field composite) so
/// the JSON conversion path renders it as a JSON string like PostgreSQL.
/// Domains intentionally retain the base-type scalar value.
pub(super) fn cast_to_user_type(
    value: &Value,
    type_name: &str,
    catalog: &InMemoryCatalog,
) -> SqlResult<Value> {
    let bare = type_name.rsplit('.').next().unwrap_or(type_name);
    let oid = catalog
        .user_type_oid(type_name)
        .or_else(|| catalog.user_type_oid(bare));
    match value {
        Value::Composite { fields, .. } => {
            let oid = oid.ok_or_else(|| {
                SqlError::Storage(PlomidError::new(
                    ErrorKind::NotFound,
                    format!("type \"{type_name}\" does not exist"),
                ))
            })?;
            Ok(Value::Composite {
                type_oid: plomid_types::TypeOid(oid),
                fields: fields.clone(),
            })
        }
        Value::Null => Ok(Value::Null),
        // An enum cast takes a scalar label. Represent it as a real enum
        // value (carrying the custom type OID) rather than wrapping it in a
        // single-field composite, so JSON conversion and equality follow
        // PostgreSQL enum semantics.
        _ => {
            let stored = catalog
                .get_type(type_name)
                .or_else(|| catalog.get_type(bare));
            let is_enum = stored.is_some_and(|ty| !ty.labels.is_empty());
            if is_enum {
                let oid = oid.ok_or_else(|| {
                    SqlError::Storage(PlomidError::new(
                        ErrorKind::NotFound,
                        format!("type \"{type_name}\" does not exist"),
                    ))
                })?;
                return Ok(Value::Enum {
                    type_oid: plomid_types::TypeOid(oid),
                    label: value.to_sql_text(),
                });
            }
            if let Some(oid) = oid {
                // Wrap scalar values as a single-field composite for tagging.
                // This handles domain values.
                Ok(Value::Composite {
                    type_oid: plomid_types::TypeOid(oid),
                    fields: vec![(bare.to_string(), value.clone())],
                })
            } else {
                Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::NotFound,
                    format!("type \"{type_name}\" does not exist"),
                )))
            }
        }
    }
}
