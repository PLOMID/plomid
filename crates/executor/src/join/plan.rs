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
//! Target planning, type inference, grouping validation, and `WITH` / recursive CTE execution.

use crate::catalog_fn::{is_aggregate_function, is_session_function, session_function_type};
use crate::coerce::value_type;
use crate::error::{SqlError, SqlResult};
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{
    ColumnType, Cte, Expression, FromClause, InMemoryCatalog, QueryResult, SelectTarget, SetOpKind,
    Statement, Value, WindowSpec,
};
use plomid_txn::StorageEngine;
use std::collections::HashMap;
use std::collections::HashSet;

use super::{
    scope::{qualifier_of, JoinScope},
    statement::execute_statement,
    support::unsupported,
};

/// One fully-planned output column of a SELECT.
#[derive(Clone)]
pub(super) struct Planned {
    pub(super) name: String,
    pub(super) ty: Option<ColumnType>,
    pub(super) kind: PlannedKind,
}

#[derive(Debug, Clone)]
pub(super) enum PlannedKind {
    /// A column of the joined row at the given absolute index.
    Source(usize),
    /// An arbitrary expression evaluated against the joined row.
    Expr(Expression),
    /// A window function computed across the result set.
    Window {
        fname: String,
        args: Vec<Expression>,
        over: WindowSpec,
    },
    /// A zero-argument session function.
    Session(String),
    /// A sequence function such as `nextval('s')`.
    Sequence { name: String, argument: String },
    /// An aggregate computed per group.
    Aggregate {
        name: String,
        arg: Option<Expression>,
    },
}

/// Walks the select list and produces one planned output per result column.
pub(super) fn plan_targets(
    scopes: &[JoinScope],
    targets: &[SelectTarget],
) -> SqlResult<Vec<Planned>> {
    let mut planned = Vec::new();
    for target in targets {
        plan_target(scopes, target, None, &mut planned)?;
    }
    Ok(planned)
}

pub(super) fn plan_target(
    scopes: &[JoinScope],
    target: &SelectTarget,
    forced_alias: Option<&str>,
    planned: &mut Vec<Planned>,
) -> SqlResult<()> {
    match target {
        SelectTarget::All => {
            for scope in scopes {
                for (i, col) in scope.schema.columns.iter().enumerate() {
                    planned.push(Planned {
                        name: forced_alias
                            .map(str::to_string)
                            .unwrap_or_else(|| col.name.clone()),
                        ty: Some(col.col_type),
                        kind: PlannedKind::Source(scope.offset + i),
                    });
                }
            }
            Ok(())
        }
        SelectTarget::QualifiedStar { qualifier } => {
            let qualifier_lower = qualifier.to_ascii_lowercase();
            let scope = scopes
                .iter()
                .find(|s| s.alias.eq_ignore_ascii_case(&qualifier_lower))
                .ok_or_else(|| {
                    SqlError::Storage(PlomidError::new(
                        ErrorKind::NotFound,
                        format!("table or alias \"{qualifier}\" not found in FROM clause"),
                    ))
                })?;
            for (i, col) in scope.schema.columns.iter().enumerate() {
                planned.push(Planned {
                    name: forced_alias
                        .map(str::to_string)
                        .unwrap_or_else(|| col.name.clone()),
                    ty: Some(col.col_type),
                    kind: PlannedKind::Source(scope.offset + i),
                });
            }
            Ok(())
        }
        SelectTarget::Function(name) => {
            let kind = if is_aggregate_function(name) {
                PlannedKind::Aggregate {
                    name: name.clone(),
                    arg: None,
                }
            } else if is_session_function(name) {
                PlannedKind::Session(name.clone())
            } else {
                return Err(unsupported(format!("function \"{name}\" is not supported")));
            };
            planned.push(Planned {
                name: forced_alias.unwrap_or(name).to_string(),
                ty: session_function_type(name),
                kind,
            });
            Ok(())
        }
        SelectTarget::FunctionCall { name, args } => {
            plan_function_call(scopes, name, args, forced_alias, planned)
        }
        SelectTarget::WindowFunction { name, args, over } => {
            planned.push(Planned {
                name: forced_alias
                    .unwrap_or(&name.to_ascii_lowercase())
                    .to_string(),
                ty: window_type(name, args, scopes),
                kind: PlannedKind::Window {
                    fname: name.clone(),
                    args: args.clone(),
                    over: over.clone(),
                },
            });
            Ok(())
        }
        SelectTarget::Expr { expr, alias } => {
            if let Expression::ColumnRef(name) = expr {
                if let Some(qualifier) = name.strip_suffix(".*") {
                    let scope = scopes
                        .iter()
                        .find(|scope| scope.alias.eq_ignore_ascii_case(qualifier))
                        .ok_or_else(|| {
                            unsupported(format!("missing relation alias \"{qualifier}\""))
                        })?;
                    for (i, col) in scope.schema.columns.iter().enumerate() {
                        planned.push(Planned {
                            name: col.name.clone(),
                            ty: Some(col.col_type),
                            kind: PlannedKind::Source(scope.offset + i),
                        });
                    }
                    return Ok(());
                }
            }
            let name = forced_alias
                .map(str::to_string)
                .or_else(|| alias.clone())
                .unwrap_or_else(|| match expr {
                    Expression::ColumnRef(name) => {
                        name.rsplit('.').next().unwrap_or(name).to_string()
                    }
                    _ => "?column?".to_string(),
                });
            planned.push(Planned {
                name,
                ty: infer_expr_type(scopes, expr),
                kind: PlannedKind::Expr(expr.clone()),
            });
            Ok(())
        }
        SelectTarget::Aliased { target, alias } => {
            plan_target(scopes, target, Some(alias), planned)
        }
    }
}

/// Plans a `FunctionCall` select target: aggregate, session, sequence, or
/// plain scalar function call.
pub(super) fn plan_function_call(
    scopes: &[JoinScope],
    name: &str,
    args: &[Expression],
    forced_alias: Option<&str>,
    planned: &mut Vec<Planned>,
) -> SqlResult<()> {
    let name_out = forced_alias.unwrap_or(name).to_string();
    if is_aggregate_function(name) {
        planned.push(Planned {
            name: name_out,
            ty: aggregate_type(name, args.first(), scopes),
            kind: PlannedKind::Aggregate {
                name: name.to_string(),
                arg: args.first().cloned(),
            },
        });
        return Ok(());
    }
    if is_session_function(name) {
        planned.push(Planned {
            name: name_out,
            ty: session_function_type(name),
            kind: PlannedKind::Session(name.to_string()),
        });
        return Ok(());
    }
    let lname = name.to_ascii_lowercase();
    if matches!(lname.as_str(), "nextval" | "currval") {
        let argument = match args.first() {
            Some(Expression::Literal(Value::Text(v))) => v.clone(),
            _ => String::new(),
        };
        planned.push(Planned {
            name: name_out,
            ty: Some(ColumnType::bigint()),
            kind: PlannedKind::Sequence {
                name: name.to_string(),
                argument,
            },
        });
        return Ok(());
    }
    planned.push(Planned {
        name: name_out,
        ty: None,
        kind: PlannedKind::Expr(Expression::FunctionCall {
            name: name.to_string(),
            args: args.to_vec(),
            distinct: false,
            filter: None,
            order_by: Vec::new(),
            returning: None,
            null_handling: None,
            unique_keys: None,
        }),
    });
    Ok(())
}

/// Best-effort static type for an aggregate result column.
pub(super) fn aggregate_type(
    name: &str,
    arg: Option<&Expression>,
    scopes: &[JoinScope],
) -> Option<ColumnType> {
    match name.to_ascii_lowercase().as_str() {
        "count" => Some(ColumnType::bigint()),
        "avg" => Some(ColumnType::new(plomid_types::TypeOid::NUMERIC, -1)),
        "sum" | "min" | "max" => match arg? {
            Expression::Literal(value) => value_type(value),
            Expression::ColumnRef(column_name) => {
                let (_, column) = qualifier_of(column_name);
                scopes.iter().find_map(|scope| {
                    scope
                        .schema
                        .columns
                        .iter()
                        .find(|col| col.name == column)
                        .map(|col| {
                            if name.eq_ignore_ascii_case("sum") {
                                match col.col_type.type_oid {
                                    plomid_types::TypeOid::INT2 | plomid_types::TypeOid::INT4 => {
                                        ColumnType::bigint()
                                    }
                                    plomid_types::TypeOid::INT8 => ColumnType::new(
                                        plomid_types::TypeOid::NUMERIC,
                                        plomid_types::NO_TYPEMOD,
                                    ),
                                    _ => col.col_type,
                                }
                            } else {
                                col.col_type
                            }
                        })
                })
            }
            _ => None,
        },
        _ => None,
    }
}

/// Best-effort static type for a window function result column.
pub(super) fn window_type(
    name: &str,
    args: &[Expression],
    scopes: &[JoinScope],
) -> Option<ColumnType> {
    match name.to_ascii_lowercase().as_str() {
        "row_number" | "rank" | "dense_rank" => Some(ColumnType::bigint()),
        "lag" | "lead" => match args.first() {
            Some(Expression::ColumnRef(name)) => {
                let (_, column) = qualifier_of(name);
                scopes.iter().find_map(|scope| {
                    scope
                        .schema
                        .columns
                        .iter()
                        .find(|col| col.name == column)
                        .map(|col| col.col_type)
                })
            }
            _ => None,
        },
        _ => None,
    }
}

/// Static type inference for expression outputs (best effort, like psql).
pub(super) fn infer_expr_type(scopes: &[JoinScope], expr: &Expression) -> Option<ColumnType> {
    match expr {
        Expression::ColumnRef(name) => {
            if is_session_function(name) {
                return session_function_type(name);
            }
            let (_, column) = qualifier_of(name);
            scopes.iter().find_map(|scope| {
                scope
                    .schema
                    .columns
                    .iter()
                    .find(|col| col.name == column)
                    .map(|col| col.col_type)
            })
        }
        Expression::Literal(value) => value_type(value),
        Expression::Add(_, _)
        | Expression::Subtract(_, _)
        | Expression::Multiply(_, _)
        | Expression::Divide(_, _)
        | Expression::Modulo(_, _) => Some(ColumnType::bigint()),
        Expression::Concat(_, _) => Some(ColumnType::text()),
        Expression::Equal(_, _)
        | Expression::NotEqual(_, _)
        | Expression::Less(_, _)
        | Expression::LessOrEqual(_, _)
        | Expression::Greater(_, _)
        | Expression::GreaterOrEqual(_, _)
        | Expression::And(_, _)
        | Expression::Or(_, _)
        | Expression::Not(_)
        | Expression::IsNull(_)
        | Expression::IsNotNull(_)
        | Expression::In { .. }
        | Expression::Between { .. }
        | Expression::Like { .. }
        | Expression::Exists(_)
        | Expression::IsDistinctFrom(_, _) => Some(ColumnType::boolean()),
        _ => None,
    }
}

/// True when the expression tree contains an aggregate call.
pub(super) fn expr_has_aggregate(expr: &Expression) -> bool {
    match expr {
        Expression::FunctionCall { name, args, .. } => {
            let outer_is_agg = is_aggregate_function(name);
            let args_have_agg = args.iter().any(expr_has_aggregate);
            outer_is_agg || args_have_agg
        }
        Expression::ColumnRef(_) | Expression::Literal(_) | Expression::Star => false,
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
        | Expression::NullIf(l, r) => expr_has_aggregate(l) || expr_has_aggregate(r),
        Expression::IsNull(i)
        | Expression::IsNotNull(i)
        | Expression::Not(i)
        | Expression::Negate(i) => expr_has_aggregate(i),
        Expression::In { expr, list, .. } => {
            expr_has_aggregate(expr) || list.iter().any(expr_has_aggregate)
        }
        Expression::Between {
            expr, low, high, ..
        } => expr_has_aggregate(expr) || expr_has_aggregate(low) || expr_has_aggregate(high),
        Expression::Like { expr, pattern, .. } => {
            expr_has_aggregate(expr) || expr_has_aggregate(pattern)
        }
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            operand.as_ref().is_some_and(|o| expr_has_aggregate(o))
                || whens
                    .iter()
                    .any(|(c, v)| expr_has_aggregate(c) || expr_has_aggregate(v))
                || default.as_ref().is_some_and(|d| expr_has_aggregate(d))
        }
        Expression::Coalesce(args) => args.iter().any(expr_has_aggregate),
        _ => false,
    }
}

pub(super) fn planned_has_aggregate(planned: &[Planned]) -> bool {
    planned.iter().any(|p| match &p.kind {
        PlannedKind::Aggregate { .. } => true,
        PlannedKind::Expr(expr) => expr_has_aggregate(expr),
        _ => false,
    })
}

/// Collects column references that appear outside aggregate calls.
pub(super) fn collect_non_aggregate_columns(expr: &Expression, out: &mut Vec<String>) {
    match expr {
        Expression::ColumnRef(name) => out.push(name.clone()),
        Expression::FunctionCall { name, args, .. } => {
            if is_aggregate_function(name) {
                // Aggregate functions hide their arguments from non-aggregate
                // column collection: arguments are evaluated per-row during
                // aggregation, not in the surrounding non-grouped context.
            } else {
                for arg in args {
                    collect_non_aggregate_columns(arg, out);
                }
            }
        }
        Expression::Literal(_) | Expression::Star | Expression::ScalarSubquery(_) => {}
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
        | Expression::NullIf(l, r) => {
            collect_non_aggregate_columns(l, out);
            collect_non_aggregate_columns(r, out);
        }
        Expression::IsNull(i)
        | Expression::IsNotNull(i)
        | Expression::Not(i)
        | Expression::Negate(i) => collect_non_aggregate_columns(i, out),
        Expression::In { expr, list, .. } => {
            collect_non_aggregate_columns(expr, out);
            list.iter()
                .for_each(|e| collect_non_aggregate_columns(e, out));
        }
        Expression::Between {
            expr, low, high, ..
        } => {
            collect_non_aggregate_columns(expr, out);
            collect_non_aggregate_columns(low, out);
            collect_non_aggregate_columns(high, out);
        }
        Expression::Like { expr, pattern, .. } => {
            collect_non_aggregate_columns(expr, out);
            collect_non_aggregate_columns(pattern, out);
        }
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            if let Some(o) = operand {
                collect_non_aggregate_columns(o, out);
            }
            for (c, v) in whens {
                collect_non_aggregate_columns(c, out);
                collect_non_aggregate_columns(v, out);
            }
            if let Some(d) = default {
                collect_non_aggregate_columns(d, out);
            }
        }
        Expression::Coalesce(args) => {
            args.iter()
                .for_each(|e| collect_non_aggregate_columns(e, out));
        }
        _ => {}
    }
}

/// True when `expr` structurally equals one of the GROUP BY expressions
/// (case-insensitive on the column part of qualified references).
pub(super) fn matches_group_expr(expr: &Expression, group_exprs: &[Expression]) -> bool {
    group_exprs.iter().any(|g| g == expr)
        || match expr {
            Expression::ColumnRef(name) => group_exprs.iter().any(|g| {
                matches!(g, Expression::ColumnRef(other) if {
                    let (_, a) = qualifier_of(name);
                    let (_, b) = qualifier_of(other);
                    a.eq_ignore_ascii_case(b)
                })
            }),
            _ => false,
        }
}

/// Enforces the aggregate + non-aggregate GROUP BY rule: every column that is
/// referenced outside an aggregate call must appear in the GROUP BY clause.
pub(super) fn validate_group_usage(
    expr: &Expression,
    label: &str,
    group_exprs: &[Expression],
) -> SqlResult<()> {
    if matches_group_expr(expr, group_exprs) {
        return Ok(());
    }
    let mut columns = Vec::new();
    collect_non_aggregate_columns(expr, &mut columns);
    for column in columns {
        let probe = Expression::ColumnRef(column.clone());
        if !matches_group_expr(&probe, group_exprs) {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!(
                    "column \"{column}\" must appear in the GROUP BY clause or be used in an aggregate function ({label})"
                ),
            )));
        }
    }
    Ok(())
}

/// Runs a `WITH (...)` statement by rewriting body/Cte queries so that every
/// CTE reference resolves to an inlined `FromClause::Subquery`. This reuses
/// the general engine's existing subquery materialization path instead of
/// adding a separate CTE binding mechanism to the catalog.
pub(crate) fn execute_with<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    stmt: Statement,
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    let Statement::With {
        recursive,
        ctes,
        body,
    } = stmt
    else {
        return Err(unsupported("expected WITH statement"));
    };
    if ctes.is_empty() {
        return execute_statement(
            engine,
            catalog,
            &body,
            current_database,
            current_user,
            None,
            0,
        );
    }
    if recursive {
        return execute_recursive_with(
            engine,
            catalog,
            &ctes,
            &body,
            current_database,
            current_user,
        );
    }
    let names: Vec<String> = ctes.iter().map(|cte| cte.name.clone()).collect();
    // Build the rewritten body where every CTE reference becomes an inlined
    // subquery. Earlier CTEs are visible inside later CTE queries.
    let mut rewritten_ctes = Vec::with_capacity(ctes.len());
    for (index, cte) in ctes.iter().enumerate() {
        let visible: Vec<&str> = names[..index].iter().map(String::as_str).collect();
        let rewritten = rewrite_cte_references(&cte.query, &visible, &ctes);
        rewritten_ctes.push((cte.name.clone(), rewritten));
    }
    let materialized_ctes: Vec<Cte> = rewritten_ctes
        .iter()
        .map(|(name, query)| Cte {
            name: name.clone(),
            columns: Vec::new(),
            query: Box::new(query.clone()),
        })
        .collect();
    let visible: Vec<&str> = names.iter().map(String::as_str).collect();
    let rewritten_body = rewrite_cte_references(&body, &visible, &materialized_ctes);
    execute_statement(
        engine,
        catalog,
        &rewritten_body,
        current_database,
        current_user,
        None,
        0,
    )
}

/// A materialized CTE product: the output column names plus every result row.
#[derive(Clone)]
pub(super) struct RecursiveMaterial {
    pub(super) columns: Vec<String>,
    pub(super) rows: Vec<Vec<Value>>,
}

impl RecursiveMaterial {
    pub(super) fn from_result(
        columns: Vec<String>,
        column_types: Vec<Option<ColumnType>>,
        rows: Vec<Vec<Value>>,
    ) -> Self {
        let _ = column_types;
        Self { columns, rows }
    }
}

/// Cap on recursive-CTE iterations. A well-formed recursive term terminates on
/// its own when it produces no new rows; this guards against a runaway term
/// (for example `SELECT n + 1 FROM nums` with no `WHERE`) exhausting memory. It
/// is far above any legitimate recursion the engine is expected to execute, so
/// it does not break valid queries (the section-10 regression tests use at most
/// 100 iterations).
use plomid_core::MAX_RECURSION_ITERATIONS;

/// Executes a `WITH RECURSIVE` statement.
///
/// CTEs are processed left to right. A CTE that references its own name is
/// treated as recursive and evaluated with the standard iterative semantics:
/// the anchor branch seeds an initial working set, then the recursive branch is
/// re-executed against the current working set until it produces no more rows.
/// The accumulated relation is exposed to later CTEs and to the outer body as a
/// plain materialized subquery, so every outer-query feature (JOIN, LEFT JOIN,
/// GROUP BY, HAVING, aggregates, ORDER BY, LIMIT, DISTINCT, set operations,
/// subqueries, CASE, ...) flows through the existing engine untouched.
pub(super) fn execute_recursive_with<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    ctes: &[Cte],
    body: &Statement,
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    // name -> materialized result for every CTE processed so far.
    let mut resolved: HashMap<String, RecursiveMaterial> = HashMap::new();

    for cte in ctes {
        let needs_update = self_references(&cte.query, &cte.name);
        if needs_update {
            let material = execute_recursive_cte(
                engine,
                catalog,
                cte,
                &resolved,
                current_database,
                current_user,
            )?;
            resolved.insert(cte.name.clone(), material);
        } else {
            // Plain CTE: rewrite references to already-materialized CTEs and
            // run it through the normal query engine.
            let rewritten =
                rewrite_materialized_refs(&cte.query, &resolved, None, &[], cte.name.clone());
            let result = execute_statement(
                engine,
                catalog,
                &rewritten,
                current_database,
                current_user,
                None,
                0,
            )?;
            let QueryResult::Rows {
                columns,
                column_types,
                rows,
            } = result
            else {
                return Err(unsupported("CTE query must produce rows"));
            };
            resolved.insert(
                cte.name.clone(),
                RecursiveMaterial::from_result(columns, column_types, rows),
            );
        }
    }

    // Rewrite the outer body so every CTE reference (recursive or not) resolves
    // to its materialized result, then execute through the normal engine.
    let rewritten_body = rewrite_materialized_refs(body, &resolved, None, &[], String::new());
    execute_statement(
        engine,
        catalog,
        &rewritten_body,
        current_database,
        current_user,
        None,
        0,
    )
}

/// True when `stmt` (transitively, through its FROM clauses and subqueries)
/// references the given table/CTE name. Used to detect self-recursive CTEs.
pub(super) fn self_references(stmt: &Statement, name: &str) -> bool {
    match stmt {
        Statement::Select { from, .. } => from
            .as_ref()
            .is_some_and(|clause| from_clause_references_table(clause, name)),
        Statement::SetOperation { left, right, .. } => {
            self_references(left, name) || self_references(right, name)
        }
        Statement::With { body, .. } => self_references(body, name),
        Statement::Values(_) => false,
        _ => false,
    }
}

pub(super) fn from_clause_references_table(clause: &FromClause, name: &str) -> bool {
    match clause {
        FromClause::Table { name: table, .. } => table.eq_ignore_ascii_case(name),
        FromClause::TableFunction { .. } => false,
        FromClause::Join { left, right, .. } => {
            from_clause_references_table(left, name) || from_clause_references_table(right, name)
        }
        FromClause::Subquery { statement, .. } => self_references(statement, name),
    }
}

/// Rewrites a statement tree so that:
///   * a FROM reference to `rec_name` becomes a `VALUES` subquery carrying the
///     current recursive working set (when `working` is provided), and
///   * a FROM reference to any earlier materialized CTE (present in `resolved`)
///     becomes a `VALUES` subquery carrying that CTE's materialized rows.
/// This keeps the recursive reference a first-class relation so the existing
/// engine (JOIN, WHERE, GROUP BY, aggregates, ...) applies unchanged.
pub(super) fn rewrite_materialized_refs(
    stmt: &Statement,
    resolved: &HashMap<String, RecursiveMaterial>,
    working: Option<&[Vec<Value>]>,
    rec_columns: &[String],
    rec_name: String,
) -> Statement {
    match stmt {
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
        } => Statement::Select {
            targets: targets.clone(),
            distinct: *distinct,
            distinct_on: distinct_on.clone(),
            from: from.as_ref().map(|clause| {
                rewrite_materialized_from(clause, resolved, working, rec_columns, &rec_name)
            }),
            where_expr: where_expr.clone(),
            group_by: group_by.clone(),
            having: having.clone(),
            order_by: order_by.clone(),
            limit: *limit,
            offset: *offset,
        },
        Statement::SetOperation {
            op,
            all,
            left,
            right,
        } => Statement::SetOperation {
            op: *op,
            all: *all,
            left: Box::new(rewrite_materialized_refs(
                left,
                resolved,
                working,
                rec_columns,
                rec_name.clone(),
            )),
            right: Box::new(rewrite_materialized_refs(
                right,
                resolved,
                working,
                rec_columns,
                rec_name,
            )),
        },
        Statement::With {
            recursive,
            ctes: inner,
            body: inner_body,
        } => Statement::With {
            recursive: *recursive,
            ctes: inner.clone(),
            body: Box::new(rewrite_materialized_refs(
                inner_body,
                resolved,
                working,
                rec_columns,
                rec_name,
            )),
        },
        other => other.clone(),
    }
}

pub(super) fn rewrite_materialized_from(
    clause: &FromClause,
    resolved: &HashMap<String, RecursiveMaterial>,
    working: Option<&[Vec<Value>]>,
    rec_columns: &[String],
    rec_name: &str,
) -> FromClause {
    match clause {
        FromClause::Table { name, alias } => {
            if name.eq_ignore_ascii_case(rec_name) {
                // This is a self-reference to the recursive CTE: expose the
                // current working set as a literal VALUES relation.
                if let Some(working_rows) = working {
                    let rows: Vec<Vec<Expression>> = working_rows
                        .iter()
                        .map(|row| {
                            row.iter()
                                .map(|value| Expression::Literal(value.clone()))
                                .collect()
                        })
                        .collect();
                    let sub_alias = alias.clone().unwrap_or_else(|| rec_name.to_string());
                    return FromClause::Subquery {
                        statement: Box::new(Statement::Values(rows)),
                        alias: sub_alias,
                        column_aliases: rec_columns.to_vec(),
                        lateral: false,
                    };
                }
                clause.clone()
            } else if let Some(material) = resolved.get(name) {
                let rows: Vec<Vec<Expression>> = material
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|value| Expression::Literal(value.clone()))
                            .collect()
                    })
                    .collect();
                let sub_alias = alias
                    .clone()
                    .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name).to_string());
                FromClause::Subquery {
                    statement: Box::new(Statement::Values(rows)),
                    alias: sub_alias,
                    column_aliases: material.columns.clone(),
                    lateral: false,
                }
            } else {
                clause.clone()
            }
        }
        FromClause::Join {
            left,
            kind,
            right,
            on,
        } => FromClause::Join {
            left: Box::new(rewrite_materialized_from(
                left,
                resolved,
                working,
                rec_columns,
                rec_name,
            )),
            kind: *kind,
            right: Box::new(rewrite_materialized_from(
                right,
                resolved,
                working,
                rec_columns,
                rec_name,
            )),
            on: on.clone(),
        },
        FromClause::Subquery {
            statement,
            alias,
            column_aliases,
            lateral,
        } => FromClause::Subquery {
            statement: Box::new(rewrite_materialized_refs(
                statement,
                resolved,
                working,
                rec_columns,
                rec_name.to_string(),
            )),
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            lateral: *lateral,
        },
        FromClause::TableFunction { .. } => clause.clone(),
    }
}

/// Executes a single self-recursive CTE using iterative working-set semantics:
/// the anchor seeds the initial working set, then the recursive branch is
/// re-evaluated against each working set until it produces no further rows.
pub(super) fn execute_recursive_cte<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    cte: &Cte,
    resolved: &HashMap<String, RecursiveMaterial>,
    current_database: &str,
    current_user: &str,
) -> SqlResult<RecursiveMaterial> {
    // The recursive query must be `anchor UNION [ALL] recursive_term`.
    let Statement::SetOperation {
        op,
        all,
        left,
        right,
    } = &*cte.query
    else {
        return Err(unsupported(
            "a recursive common table expression must be a UNION or UNION ALL of a non-recursive and a recursive term",
        ));
    };
    if !matches!(op, SetOpKind::Union) {
        return Err(unsupported(
            "a recursive common table expression must use UNION or UNION ALL",
        ));
    }

    // Anchor: references earlier CTEs but never the recursive name itself.
    let anchor = rewrite_materialized_refs(left, resolved, None, &[], cte.name.clone());
    let anchor_result = execute_statement(
        engine,
        catalog,
        &anchor,
        current_database,
        current_user,
        None,
        0,
    )?;
    let QueryResult::Rows {
        columns,
        column_types,
        rows,
    } = anchor_result
    else {
        return Err(unsupported("recursive CTE anchor must produce rows"));
    };

    // Effective output columns: the explicit CTE column list wins when present,
    // otherwise fall back to the anchor's projected columns.
    let output_columns: Vec<String> = if !cte.columns.is_empty() {
        cte.columns.clone()
    } else {
        columns.clone()
    };
    // Validate arity according to the existing type system: the explicit
    // column list must match the anchor's projection, and the recursive term
    // must produce the same number of columns as the anchor.
    if !cte.columns.is_empty() && cte.columns.len() != columns.len() {
        return Err(unsupported(
            "recursive common table expression column list does not match the anchor query's column count",
        ));
    }
    let width = columns.len();
    if rows.iter().any(|row| row.len() != width) {
        return Err(unsupported(
            "recursive common table expression anchor rows have inconsistent arity",
        ));
    }

    let mut working: Vec<Vec<Value>> = rows.clone();
    let mut accumulator: Vec<Vec<Value>> = rows;
    // For a duplicate-eliminating `UNION`, track keys already emitted so each
    // iteration yields only rows not seen in any prior iteration or the anchor.
    let mut seen: HashSet<String> = HashSet::new();
    if !all {
        for row in &accumulator {
            seen.insert(recursion_row_key(row));
        }
    }

    let mut iterations: usize = 0;
    loop {
        if working.is_empty() {
            break;
        }
        let rewritten = rewrite_materialized_refs(
            right,
            resolved,
            Some(&working),
            &output_columns,
            cte.name.clone(),
        );
        let next_result = execute_statement(
            engine,
            catalog,
            &rewritten,
            current_database,
            current_user,
            None,
            0,
        )?;
        let QueryResult::Rows { rows: next, .. } = next_result else {
            return Err(unsupported("recursive CTE body must produce rows"));
        };
        // The recursive term must produce the same arity as the anchor.
        if next.iter().any(|row| row.len() != width) {
            return Err(unsupported(
                "recursive common table expression recursive term has a different column count than the anchor",
            ));
        }
        let mut fresh = next;
        if !all {
            fresh.retain(|row| seen.insert(recursion_row_key(row)));
        }
        if fresh.is_empty() {
            break;
        }
        accumulator.extend(fresh.clone());
        working = fresh;
        iterations += 1;
        if iterations > MAX_RECURSION_ITERATIONS {
            return Err(unsupported(
                "recursive common table expression exceeded the maximum number of iterations",
            ));
        }
    }

    Ok(RecursiveMaterial::from_result(
        output_columns,
        column_types,
        accumulator,
    ))
}

/// Serializes a row into a dedup key for recursive `UNION`.
pub(super) fn recursion_row_key(row: &[Value]) -> String {
    row.iter()
        .map(|v| v.to_sql_text())
        .collect::<Vec<_>>()
        .join("\u{1}")
}

/// Rewrites a statement tree so every FROM reference to one of `visible`
/// Rewrites a statement tree so every FROM reference to one of `visible`
/// CTE names becomes an inline subquery carrying that CTE's (rewritten) query.
pub(super) fn rewrite_cte_references(
    stmt: &Statement,
    visible: &[&str],
    ctes: &[Cte],
) -> Statement {
    match stmt {
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
        } => Statement::Select {
            targets: targets.clone(),
            distinct: *distinct,
            distinct_on: distinct_on.clone(),
            from: from
                .as_ref()
                .map(|clause| rewrite_from_clause(clause, visible, ctes)),
            where_expr: where_expr.clone(),
            group_by: group_by.clone(),
            having: having.clone(),
            order_by: order_by.clone(),
            limit: *limit,
            offset: *offset,
        },
        Statement::SetOperation {
            op,
            all,
            left,
            right,
        } => Statement::SetOperation {
            op: *op,
            all: *all,
            left: Box::new(rewrite_cte_references(left, visible, ctes)),
            right: Box::new(rewrite_cte_references(right, visible, ctes)),
        },
        Statement::With {
            recursive,
            ctes: inner,
            body,
        } => Statement::With {
            recursive: *recursive,
            ctes: inner.clone(),
            body: Box::new(rewrite_cte_references(body, visible, ctes)),
        },
        other => other.clone(),
    }
}

/// Rewrites FROM clauses (including nested joins and subqueries) replacing CTE
/// table references with inline subqueries.
pub(super) fn rewrite_from_clause(
    clause: &FromClause,
    visible: &[&str],
    ctes: &[Cte],
) -> FromClause {
    match clause {
        FromClause::Table { name, alias } => {
            if visible.iter().any(|cte| cte.eq_ignore_ascii_case(name)) {
                let cte = ctes.iter().find(|cte| cte.name.eq_ignore_ascii_case(name));
                let query = cte
                    .map(|cte| rewrite_cte_references(&cte.query, visible, ctes))
                    .unwrap_or_else(|| Statement::Select {
                        targets: vec![SelectTarget::All],
                        distinct: false,
                        distinct_on: None,
                        from: None,
                        where_expr: None,
                        group_by: None,
                        having: None,
                        order_by: Vec::new(),
                        limit: None,
                        offset: None,
                    });
                FromClause::Subquery {
                    statement: Box::new(query),
                    alias: alias
                        .clone()
                        .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name).to_string()),
                    column_aliases: Vec::new(),
                    lateral: false,
                }
            } else {
                clause.clone()
            }
        }
        FromClause::Join {
            left,
            kind,
            right,
            on,
        } => FromClause::Join {
            left: Box::new(rewrite_from_clause(left, visible, ctes)),
            kind: *kind,
            right: Box::new(rewrite_from_clause(right, visible, ctes)),
            on: on.clone(),
        },
        FromClause::Subquery {
            statement,
            alias,
            column_aliases,
            lateral,
        } => FromClause::Subquery {
            statement: Box::new(rewrite_cte_references(statement, visible, ctes)),
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            lateral: *lateral,
        },
        FromClause::TableFunction { .. } => clause.clone(),
    }
}
