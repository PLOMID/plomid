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
//! Statement-level dispatch and set operations.

use crate::catalog_fn::is_aggregate_function;
use crate::error::{SqlError, SqlResult};
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{
    ColumnType, Expression, InMemoryCatalog, QueryResult, SelectTarget, SetOpKind, Statement, Value,
};
use plomid_txn::StorageEngine;
use std::collections::HashMap;

use plomid_core::MAX_SUBQUERY_DEPTH;

use super::{
    aggregate::{slot_matches_expression, AggregateSlot},
    scope::{synthetic_schema, OuterContext},
    select::execute_join_select,
    support::unsupported,
};

/// `outer` makes the enclosing row's columns visible to the subquery so that
/// correlated references such as `EXISTS (SELECT 1 FROM o WHERE o.uid = u.id)`
/// resolve correctly.
pub(crate) fn execute_statement<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    stmt: &Statement,
    current_database: &str,
    current_user: &str,
    outer: Option<&OuterContext>,
    depth: usize,
) -> SqlResult<QueryResult> {
    if depth > MAX_SUBQUERY_DEPTH {
        return Err(unsupported("subquery nesting is too deep"));
    }
    match stmt {
        Statement::Values(rows) => {
            // A bare VALUES list: evaluate each expression and expose the
            // result as `column1`, `column2`, ... like PostgreSQL.
            let width = rows.first().map(Vec::len).unwrap_or(0);
            if rows.iter().any(|row| row.len() != width) {
                return Err(unsupported("VALUES rows must all have the same arity"));
            }
            let empty_schema = synthetic_schema("__values", &[], &[]);
            let mut out_rows = Vec::with_capacity(rows.len());
            for row in rows {
                let mut out = Vec::with_capacity(width);
                for expr in row {
                    out.push(crate::query::evaluate_expression(&[], &empty_schema, expr)?);
                }
                out_rows.push(out);
            }
            let columns = (1..=width)
                .map(|i| format!("column{i}"))
                .collect::<Vec<_>>();
            let column_types = (0..width)
                .map(|i| {
                    out_rows
                        .iter()
                        .find_map(|row| crate::coerce::value_type(&row[i]))
                })
                .collect::<Vec<_>>();
            Ok(QueryResult::Rows {
                columns,
                column_types,
                rows: out_rows,
            })
        }
        Statement::Select {
            targets,
            distinct,
            distinct_on,
            from,
            where_expr,
            group_by,
            having,
            order_by,
            limit,
            offset,
        } => {
            let has_distinct_on = distinct_on.as_ref().is_some_and(|v| !v.is_empty());
            let inner_limit = if *distinct || has_distinct_on {
                None
            } else {
                *limit
            };
            let inner_offset = if *distinct || has_distinct_on {
                None
            } else {
                *offset
            };
            execute_join_select(
                engine,
                catalog,
                from.as_ref(),
                targets.clone(),
                where_expr.clone(),
                group_by.clone(),
                having.clone(),
                order_by.clone(),
                inner_limit,
                inner_offset,
                current_database,
                current_user,
                outer,
                depth,
                distinct_on.clone().unwrap_or_default(),
            )
            .map(|result| {
                crate::distinct::apply_distinct(
                    result,
                    *distinct,
                    distinct_on.as_deref().unwrap_or_default(),
                    *limit,
                    *offset,
                )
            })
        }
        Statement::SetOperation {
            op,
            all,
            left,
            right,
        } => execute_set_operation(
            engine,
            catalog,
            *op,
            *all,
            left,
            right,
            current_database,
            current_user,
            outer,
            depth,
        ),
        other => Err(unsupported(format!(
            "statement is not valid inside a subquery: {other:?}"
        ))),
    }
}

/// Evaluates `UNION [ALL]`, `INTERSECT [ALL]`, and `EXCEPT [ALL]`.
#[allow(clippy::too_many_arguments)]
pub(super) fn execute_set_operation<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    op: SetOpKind,
    all: bool,
    left: &Statement,
    right: &Statement,
    current_database: &str,
    current_user: &str,
    outer: Option<&OuterContext>,
    depth: usize,
) -> SqlResult<QueryResult> {
    let left_result = execute_statement(
        engine,
        catalog,
        left,
        current_database,
        current_user,
        outer,
        depth + 1,
    )?;
    let right_result = execute_statement(
        engine,
        catalog,
        right,
        current_database,
        current_user,
        outer,
        depth + 1,
    )?;
    let QueryResult::Rows {
        columns,
        column_types,
        rows: left_rows,
    } = left_result
    else {
        return Err(unsupported("set operands must be SELECT queries"));
    };
    let QueryResult::Rows {
        rows: right_rows, ..
    } = right_result
    else {
        return Err(unsupported("set operands must be SELECT statements"));
    };
    if right_rows.iter().any(|r| r.len() != columns.len())
        || left_rows.iter().any(|r| r.len() != columns.len())
    {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            "set operations require queries with the same number of columns",
        )));
    }
    let key = |row: &[Value]| -> String {
        row.iter()
            .map(|v| v.to_sql_text())
            .collect::<Vec<_>>()
            .join("\u{1}")
    };
    let rows = match (op, all) {
        (SetOpKind::Union, true) => [left_rows, right_rows].concat(),
        (SetOpKind::Union, false) => {
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for row in [left_rows, right_rows].concat() {
                if seen.insert(key(&row)) {
                    out.push(row);
                }
            }
            out
        }
        (SetOpKind::Intersect, true) => {
            let mut right_counts: HashMap<String, usize> = HashMap::new();
            for row in &right_rows {
                *right_counts.entry(key(row)).or_insert(0) += 1;
            }
            left_rows
                .into_iter()
                .filter(|row| {
                    let k = key(row);
                    if let Some(count) = right_counts.get_mut(&k) {
                        if *count > 0 {
                            *count -= 1;
                            return true;
                        }
                    }
                    false
                })
                .collect()
        }
        (SetOpKind::Intersect, false) => {
            let right_keys: std::collections::HashSet<String> =
                right_rows.iter().map(|r| key(r)).collect();
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for row in left_rows {
                let k = key(&row);
                if right_keys.contains(&k) && seen.insert(k) {
                    out.push(row);
                }
            }
            out
        }
        (SetOpKind::Except, true) => {
            let removed: std::collections::HashSet<String> =
                right_rows.iter().map(|r| key(r)).collect();
            left_rows
                .into_iter()
                .filter(|row| !removed.contains(&key(row)))
                .collect()
        }
        (SetOpKind::Except, false) => {
            let removed: std::collections::HashSet<String> =
                right_rows.iter().map(|r| key(r)).collect();
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for row in left_rows {
                let k = key(&row);
                if !removed.contains(&k) && seen.insert(k) {
                    out.push(row);
                }
            }
            out
        }
    };
    Ok(QueryResult::Rows {
        columns,
        column_types,
        rows,
    })
}

/// Rewrites aggregate calls (and GROUP BY expression references) into
/// literals carrying their per-group values, so grouped projection reuses
/// the plain join evaluator.
pub(super) fn rewrite_grouped(
    expr: &Expression,
    slots: &[AggregateSlot],
    group_pairs: &[(Expression, Value)],
) -> Expression {
    if let Some((_, value)) = group_pairs.iter().find(|(g, _)| g == expr) {
        return Expression::Literal(value.clone());
    }
    let rewrite_box = |e: &Expression, slots: &[AggregateSlot], pairs: &[(Expression, Value)]| {
        Box::new(rewrite_grouped(e, slots, pairs))
    };
    match expr {
        Expression::FunctionCall { name, .. } if is_aggregate_function(name) => {
            match slots
                .iter()
                .find(|slot| slot_matches_expression(slot, expr))
            {
                Some(slot) => Expression::Literal(slot.2.finalize()),
                None => Expression::Literal(Value::Null),
            }
        }
        Expression::ColumnRef(_) | Expression::Literal(_) | Expression::Star => expr.clone(),
        Expression::Equal(l, r) => Expression::Equal(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::NotEqual(l, r) => Expression::NotEqual(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::Less(l, r) => Expression::Less(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::LessOrEqual(l, r) => Expression::LessOrEqual(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::Greater(l, r) => Expression::Greater(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::GreaterOrEqual(l, r) => Expression::GreaterOrEqual(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::And(l, r) => Expression::And(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::Or(l, r) => Expression::Or(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::Add(l, r) => Expression::Add(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::Subtract(l, r) => Expression::Subtract(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::Multiply(l, r) => Expression::Multiply(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::Divide(l, r) => Expression::Divide(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::Modulo(l, r) => Expression::Modulo(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::Concat(l, r) => Expression::Concat(
            rewrite_box(l, slots, group_pairs),
            rewrite_box(r, slots, group_pairs),
        ),
        Expression::IsNull(i) => Expression::IsNull(rewrite_box(i, slots, group_pairs)),
        Expression::IsNotNull(i) => Expression::IsNotNull(rewrite_box(i, slots, group_pairs)),
        Expression::Not(i) => Expression::Not(rewrite_box(i, slots, group_pairs)),
        Expression::Negate(i) => Expression::Negate(rewrite_box(i, slots, group_pairs)),
        Expression::Case {
            operand,
            whens,
            default,
        } => Expression::Case {
            operand: operand.as_ref().map(|o| rewrite_box(o, slots, group_pairs)),
            whens: whens
                .iter()
                .map(|(c, v)| {
                    (
                        rewrite_grouped(c, slots, group_pairs),
                        rewrite_grouped(v, slots, group_pairs),
                    )
                })
                .collect(),
            default: default.as_ref().map(|d| rewrite_box(d, slots, group_pairs)),
        },
        Expression::Coalesce(args) => Expression::Coalesce(
            args.iter()
                .map(|a| rewrite_grouped(a, slots, group_pairs))
                .collect(),
        ),
        Expression::NullIf(a, b) => Expression::NullIf(
            rewrite_box(a, slots, group_pairs),
            rewrite_box(b, slots, group_pairs),
        ),
        Expression::IsDistinctFrom(a, b) => Expression::IsDistinctFrom(
            rewrite_box(a, slots, group_pairs),
            rewrite_box(b, slots, group_pairs),
        ),
        // A non-aggregate (scalar) call can wrap an aggregate — for example
        // `jsonb_path_query_array(jsonb_agg(payload), '...')`. Its inner
        // aggregate must be replaced with the finalized grouped literal before
        // the outer call is handed to the plain evaluator. Omitted here, such
        // a query reached eval_extended_function_call with the aggregate still
        // intact and failed with "aggregate ... cannot be evaluated here".
        Expression::FunctionCall {
            name,
            args,
            distinct,
            filter,
            order_by,
            returning,
            null_handling,
            unique_keys,
        } => Expression::FunctionCall {
            name: name.to_string(),
            args: args
                .iter()
                .map(|a| rewrite_grouped(a, slots, group_pairs))
                .collect::<Vec<_>>(),
            distinct: *distinct,
            filter: filter.as_ref().map(|f| rewrite_box(f, slots, group_pairs)),
            order_by: order_by
                .iter()
                .map(|o| plomid_sql::OrderByItem {
                    expr: rewrite_grouped(&o.expr, slots, group_pairs),
                    descending: o.descending,
                    nulls_first: o.nulls_first,
                })
                .collect(),
            returning: returning.clone(),
            null_handling: *null_handling,
            unique_keys: *unique_keys,
        },
        other => other.clone(),
    }
}

pub(super) fn unnest_target(target: &SelectTarget) -> Option<(Expression, Vec<String>)> {
    match target {
        SelectTarget::FunctionCall { name, args }
            if is_table_valued_function(name) && args.len() == 1 =>
        {
            Some((
                Expression::FunctionCall {
                    name: name.clone(),
                    args: args.clone(),
                    distinct: false,
                    filter: None,
                    order_by: Vec::new(),
                    returning: None,
                    null_handling: None,
                    unique_keys: None,
                },
                srf_default_columns(name),
            ))
        }
        SelectTarget::Expr {
            expr: Expression::FunctionCall { name, args, .. },
            alias,
        } if is_table_valued_function(name) && args.len() == 1 => Some((
            target_expr(target)?,
            alias
                .clone()
                .map(|alias| vec![alias])
                .unwrap_or_else(|| srf_default_columns(name)),
        )),
        SelectTarget::Aliased { target, alias } => {
            let (expr, _) = unnest_target(target)?;
            Some((expr, vec![alias.clone()]))
        }
        _ => None,
    }
}

/// PostgreSQL's default output column names for a set-returning function.
pub(super) fn srf_default_columns(name: &str) -> Vec<String> {
    if is_json_each_function(name) {
        vec!["key".into(), "value".into()]
    } else if name.eq_ignore_ascii_case("unnest") {
        vec!["unnest".into()]
    } else {
        vec![name.to_ascii_lowercase()]
    }
}

/// Expands a set-returning function result into its output columns and rows.
/// Record (key, value) results fan out into multiple columns; plain arrays
/// stay single-column.
pub(super) fn srf_result_rows(value: Value) -> SqlResult<(Vec<ColumnType>, Vec<Vec<Value>>)> {
    match value {
        Value::Null => Ok((vec![ColumnType::text()], Vec::new())),
        Value::Array {
            element_oid,
            elements,
        } => {
            if element_oid == plomid_types::TypeOid::RECORD {
                let mut rows = Vec::with_capacity(elements.len());
                for element in elements {
                    let Value::Composite { fields, .. } = element else {
                        return Err(unsupported(
                            "record set-returning result must contain composite values",
                        ));
                    };
                    rows.push(fields.into_iter().map(|(_, v)| v).collect());
                }
                // Derive the (key, value) column types from the first
                // non-NULL value in each position.
                let width = rows.first().map_or(0, Vec::len);
                let mut types = Vec::with_capacity(width);
                for position in 0..width {
                    let column_type = rows
                        .iter()
                        .find_map(|row| {
                            row.get(position).and_then(|v| {
                                if v.is_null() {
                                    None
                                } else {
                                    plomid_sql::value_pg_type(v).map(|ty| {
                                        ColumnType::new(ty.oid(), plomid_types::NO_TYPEMOD)
                                    })
                                }
                            })
                        })
                        .unwrap_or_else(ColumnType::text);
                    types.push(column_type);
                }
                Ok((types, rows))
            } else {
                Ok((
                    vec![ColumnType::new(element_oid, plomid_types::NO_TYPEMOD)],
                    elements.into_iter().map(|v| vec![v]).collect(),
                ))
            }
        }
        _ => Err(unsupported(
            "set-returning function argument must be an array",
        )),
    }
}

pub(super) fn is_table_valued_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "unnest"
            | "jsonb_array_elements"
            | "json_array_elements"
            | "jsonb_object_keys"
            | "json_object_keys"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
            | "jsonb_path_query"
            | "jsonb_path_query_tz"
            | "jsonb_path_query_array"
            | "jsonb_path_query_array_tz"
            | "jsonb_path_query_first"
            | "jsonb_path_query_first_tz"
    )
}

/// True for the `json_each`-family set-returning functions whose result is a
/// (key, value) record per row.
pub(super) fn is_json_each_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text"
    )
}

pub(super) fn generate_series_target(
    target: &SelectTarget,
) -> Option<(Expression, Option<String>)> {
    let target = match target {
        SelectTarget::Expr { expr, alias } => (expr.clone(), alias.clone()),
        SelectTarget::FunctionCall { name, args }
            if name.eq_ignore_ascii_case("generate_series") =>
        {
            (
                Expression::FunctionCall {
                    name: name.clone(),
                    args: args.clone(),
                    distinct: false,
                    filter: None,
                    order_by: Vec::new(),
                    returning: None,
                    null_handling: None,
                    unique_keys: None,
                },
                None,
            )
        }
        SelectTarget::Aliased { target, alias } => {
            let (expr, _) = generate_series_target(target)?;
            return Some((expr, Some(alias.clone())));
        }
        _ => return None,
    };
    match &target.0 {
        Expression::FunctionCall { name, args, .. }
            if name.eq_ignore_ascii_case("generate_series")
                && (args.len() == 2 || args.len() == 3) =>
        {
            Some(target)
        }
        _ => None,
    }
}

/// Computes the generated elements of a `generate_series` argument list: the
/// values of two or three integer expressions from start to stop by step.
pub(super) fn generate_series_element_values(values: &[Value]) -> SqlResult<Vec<Value>> {
    let as_i64 = |value: &Value| -> SqlResult<i64> {
        match value {
            Value::Int2(v) => Ok(i64::from(*v)),
            Value::Int4(v) => Ok(i64::from(*v)),
            Value::Int8(v) => Ok(*v),
            Value::Numeric(v) => v
                .to_string()
                .parse::<i64>()
                .map_err(|_| unsupported("generate_series arguments must be integers")),
            _ => Err(unsupported("generate_series arguments must be integers")),
        }
    };
    let start = values
        .first()
        .map(as_i64)
        .transpose()?
        .ok_or_else(|| unsupported("generate_series requires a start argument"))?;
    let stop = values
        .get(1)
        .map(as_i64)
        .transpose()?
        .ok_or_else(|| unsupported("generate_series requires a stop argument"))?;
    let step = values.get(2).map(as_i64).transpose()?.unwrap_or(1);
    if step == 0 {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            "step size cannot equal zero",
        )));
    }
    let mut elements = Vec::new();
    let mut current = start;
    while (step > 0 && current <= stop) || (step < 0 && current >= stop) {
        elements.push(Value::Int8(current));
        current = current.checked_add(step).ok_or_else(|| {
            SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "generate_series result overflow",
            ))
        })?;
    }
    Ok(elements)
}

/// Finds the first `generate_series` set-returning call in an expression,
/// returning its name and arguments. Follows the `Expression::column_refs`
/// visitor shape.
pub(super) fn find_generate_series_call(expr: &Expression) -> Option<(String, Vec<Expression>)> {
    match expr {
        Expression::FunctionCall { name, args, .. }
            if name.eq_ignore_ascii_case("generate_series") =>
        {
            Some((name.clone(), args.clone()))
        }
        Expression::FunctionCall { args, .. } => args.iter().find_map(find_generate_series_call),
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
        | Expression::IsDistinctFrom(a, b)
        | Expression::Power(a, b)
        | Expression::BitAnd(a, b)
        | Expression::BitOr(a, b)
        | Expression::BitXor(a, b)
        | Expression::ShiftLeft(a, b)
        | Expression::ShiftRight(a, b)
        | Expression::JsonArrow {
            left: a, right: b, ..
        }
        | Expression::ArrayIndex { array: a, index: b } => {
            find_generate_series_call(a).or_else(|| find_generate_series_call(b))
        }
        Expression::IsNull(a)
        | Expression::IsNotNull(a)
        | Expression::IsJson { expr: a, .. }
        | Expression::IsBoolean { expr: a, .. }
        | Expression::Not(a)
        | Expression::Negate(a)
        | Expression::NullIf(a, _) => find_generate_series_call(a),
        Expression::In { expr, list, .. } => find_generate_series_call(expr)
            .or_else(|| list.iter().find_map(find_generate_series_call)),
        Expression::Between {
            expr, low, high, ..
        } => find_generate_series_call(expr)
            .or_else(|| find_generate_series_call(low))
            .or_else(|| find_generate_series_call(high)),
        Expression::Like { expr, pattern, .. } => {
            find_generate_series_call(expr).or_else(|| find_generate_series_call(pattern))
        }
        Expression::Case {
            operand,
            whens,
            default,
        } => operand
            .as_deref()
            .and_then(find_generate_series_call)
            .or_else(|| {
                whens
                    .iter()
                    .flat_map(|(condition, value)| [condition, value])
                    .find_map(find_generate_series_call)
            })
            .or_else(|| default.as_deref().and_then(find_generate_series_call)),
        Expression::Coalesce(args) => args.iter().find_map(find_generate_series_call),
        _ => None,
    }
}

/// Matches a target expression that *contains* a `generate_series` call but is
/// not the bare SRF itself (e.g. `SELECT 1000 - generate_series(1, 3);`).
pub(super) fn generate_series_expr_target(
    target: &SelectTarget,
) -> Option<(Expression, Option<String>)> {
    match target {
        SelectTarget::Expr { expr, alias } => match find_generate_series_call(expr) {
            Some(_) => Some((expr.clone(), alias.clone())),
            None => None,
        },
        SelectTarget::Aliased { target, alias } => {
            let (expr, _) = generate_series_expr_target(target)?;
            Some((expr, Some(alias.clone())))
        }
        // A bare SRF target is handled by `generate_series_target`.
        _ => None,
    }
}

pub(super) fn generate_series_rows(value: Value) -> SqlResult<Vec<Vec<Value>>> {
    let Value::Array { elements, .. } = value else {
        return Err(unsupported(
            "generate_series internal result must be an array",
        ));
    };
    Ok(elements.into_iter().map(|value| vec![value]).collect())
}

pub(super) fn target_expr(target: &SelectTarget) -> Option<Expression> {
    match target {
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
        SelectTarget::Aliased { target, .. } => target_expr(target),
        _ => None,
    }
}
