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
//! Relation materialization and the join algorithms (nested-loop, hash, indexed, lateral).

use crate::encoding::decode_row;
use crate::error::{SqlError, SqlResult};
use crate::row::values_equal;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{
    ColumnType, Expression, FromClause, InMemoryCatalog, JoinKind, QueryResult, TableSchema, Value,
};
use plomid_txn::StorageEngine;
use std::collections::HashMap;
use std::collections::HashSet;

use super::{
    aggregate::concat_slices,
    eval::JoinEval,
    scope::{effective_alias, resolve_column, synthetic_schema, JoinScope, OuterContext},
    statement::{execute_statement, is_json_each_function, srf_default_columns, srf_result_rows},
};
use plomid_sql::Catalog;
use std::hash::{Hash, Hasher};

pub(crate) fn unsupported(message: impl Into<String>) -> SqlError {
    SqlError::Storage(PlomidError::new(ErrorKind::Unsupported, message))
}

/// Adapts the join engine's expression evaluator to the document modality.
///
/// `JSON_TABLE`'s context-item and path arguments are SQL expressions, so the
/// document crate asks the host to evaluate them via
/// [`plomid_json::table::ExpressionEval`]. This adapter is what lets the
/// `JSON_TABLE` algorithm live in `plomid-json` without that crate depending on
/// the SQL AST or on this engine.
pub(super) struct SqlExpressionEval<'a, E: StorageEngine> {
    engine: &'a mut E,
    catalog: &'a InMemoryCatalog,
    outer: Option<&'a OuterContext>,
    depth: usize,
    current_database: &'a str,
    current_user: &'a str,
}

impl<E: StorageEngine> plomid_json::table::ExpressionEval for SqlExpressionEval<'_, E> {
    type Expr = Expression;

    fn eval(&mut self, expr: &Expression) -> SqlResult<Value> {
        JoinEval::eval_join_expr(
            &mut *self.engine,
            self.catalog,
            &[],
            &[],
            expr,
            self.outer,
            self.depth,
            self.current_database,
            self.current_user,
        )
    }
}

pub(super) fn postgres_keyword_rows() -> Vec<Vec<Value>> {
    // pg_get_keywords() is a stable PostgreSQL catalog function. These are
    // the keywords understood by Plomid's SQL parser; exposing them through
    // the normal table-function path lets JDBC/DBeaver complete startup
    // metadata queries without a client-specific response.
    [
        ("select", "unreserved", "U"),
        ("insert", "unreserved", "U"),
        ("update", "unreserved", "U"),
        ("delete", "unreserved", "U"),
        ("create", "unreserved", "U"),
        ("table", "unreserved", "U"),
        ("schema", "unreserved", "U"),
        ("from", "reserved", "R"),
        ("where", "reserved", "R"),
        ("join", "reserved", "R"),
    ]
    .into_iter()
    .map(|(word, description, code)| {
        vec![
            Value::Name(word.to_string()),
            Value::Text(description.to_string()),
            Value::BpChar(code.to_string()),
        ]
    })
    .collect()
}

/// Materializes a `FROM` tree into join scopes plus flattened rows.
pub(super) fn materialize_relation<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    from: &FromClause,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<(Vec<JoinScope>, Vec<Vec<Value>>)> {
    match from {
        FromClause::TableFunction {
            name,
            args,
            alias,
            column_aliases,
            column_defs,
            json_columns,
            lateral: _,
        } => {
            if name.eq_ignore_ascii_case("pg_catalog.pg_get_keywords")
                || name.eq_ignore_ascii_case("pg_get_keywords")
            {
                let schema = synthetic_schema(
                    alias.as_deref().unwrap_or(name),
                    &[
                        "word".to_string(),
                        "catdesc".to_string(),
                        "catcode".to_string(),
                    ],
                    &[
                        Some(ColumnType::new(
                            plomid_types::TypeOid::NAME,
                            plomid_types::NO_TYPEMOD,
                        )),
                        Some(ColumnType::new(
                            plomid_types::TypeOid::TEXT,
                            plomid_types::NO_TYPEMOD,
                        )),
                        Some(ColumnType::new(
                            plomid_types::TypeOid::CHAR,
                            plomid_types::NO_TYPEMOD,
                        )),
                    ],
                );
                return Ok((
                    vec![JoinScope {
                        alias: alias.clone().unwrap_or_else(|| name.clone()),
                        schema,
                        offset: 0,
                        is_srf: false,
                    }],
                    postgres_keyword_rows(),
                ));
            }
            if name.eq_ignore_ascii_case("generate_series") {
                if args.len() != 2 && args.len() != 3 {
                    return Err(unsupported(
                        "generate_series requires two or three integer arguments",
                    ));
                }
                let values = args
                    .iter()
                    .map(|arg| {
                        JoinEval::eval_join_expr(
                            engine,
                            catalog,
                            &[],
                            &[],
                            arg,
                            outer,
                            depth,
                            current_database,
                            current_user,
                        )
                    })
                    .collect::<SqlResult<Vec<_>>>()?;
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
                let start = as_i64(&values[0])?;
                let stop = as_i64(&values[1])?;
                let step = values.get(2).map(as_i64).transpose()?.unwrap_or(1);
                if step == 0 {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::InvalidArgument,
                        "step size cannot equal zero",
                    )));
                }
                let mut rows = Vec::new();
                let mut current = start;
                while (step > 0 && current <= stop) || (step < 0 && current >= stop) {
                    rows.push(vec![Value::Int8(current)]);
                    current = current.checked_add(step).ok_or_else(|| {
                        SqlError::Storage(PlomidError::new(
                            ErrorKind::InvalidArgument,
                            "generate_series result overflow",
                        ))
                    })?;
                }
                let relation_name = alias.clone().unwrap_or_else(|| name.clone());
                let column = column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "generate_series".into());
                let schema = synthetic_schema(
                    &relation_name,
                    &[column],
                    &[Some(ColumnType::new(
                        plomid_types::TypeOid::INT8,
                        plomid_types::NO_TYPEMOD,
                    ))],
                );
                return Ok((
                    vec![JoinScope {
                        alias: relation_name,
                        schema,
                        offset: 0,
                        is_srf: true,
                    }],
                    rows,
                ));
            }
            // JSON set-returning functions in FROM (`FROM json_each(...) AS
            // t(key, value)`): evaluate the argument once and expand the
            // result into the relation's rows.
            if is_json_each_function(name)
                || matches!(
                    name.to_ascii_lowercase().as_str(),
                    "json_array_elements_text"
                        | "jsonb_array_elements_text"
                        | "jsonb_array_elements"
                        | "json_array_elements"
                )
            {
                let alias_name = alias.clone().unwrap_or_else(|| name.clone());
                // These functions convert a JSON document (object for
                // `json_each`, array for `jsonb_array_elements`/`_text`) into a
                // `Value::Array` of output rows, so evaluate the whole call --
                // including the array/record conversion and SQL NULL
                // propagation -- rather than feeding the raw argument to the
                // row expander.
                let call = Expression::FunctionCall {
                    name: name.clone(),
                    args: args.clone(),
                    distinct: false,
                    filter: None,
                    order_by: Vec::new(),
                    returning: None,
                    null_handling: None,
                    unique_keys: None,
                };
                let value = JoinEval::eval_join_expr(
                    engine,
                    catalog,
                    &[],
                    &[],
                    &call,
                    outer,
                    depth,
                    current_database,
                    current_user,
                )?;
                let (column_types, rows) = srf_result_rows(value)?;
                let columns = if column_aliases.is_empty() {
                    srf_default_columns(name)
                } else {
                    column_aliases.clone()
                };
                let schema = synthetic_schema(
                    &alias_name,
                    &columns,
                    &column_types.into_iter().map(Some).collect::<Vec<_>>(),
                );
                return Ok((
                    vec![JoinScope {
                        alias: alias_name,
                        schema,
                        offset: 0,
                        is_srf: true,
                    }],
                    rows,
                ));
            }
            // json_to_record / jsonb_to_record / json_populate_record /
            // jsonb_populate_record / json_populate_recordset /
            // jsonb_populate_recordset: expand a JSON value into a record
            // (or set of records) with the column types declared in the
            // AS x(col type, ...) clause.
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "json_to_record"
                    | "jsonb_to_record"
                    | "json_to_recordset"
                    | "jsonb_to_recordset"
                    | "json_populate_record"
                    | "jsonb_populate_record"
                    | "json_populate_recordset"
                    | "jsonb_populate_recordset"
            ) {
                let alias_name = alias.clone().unwrap_or_else(|| name.clone());
                let json_arg = args
                    .first()
                    .ok_or_else(|| unsupported(format!("{name} requires a JSON argument")))?;
                let json_value = JoinEval::eval_join_expr(
                    engine,
                    catalog,
                    &[],
                    &[],
                    json_arg,
                    outer,
                    depth,
                    current_database,
                    current_user,
                )?;
                // Determine output columns from column_defs, falling back to
                // column_aliases, then to default names.
                let (columns, col_types) = if !column_defs.is_empty() {
                    let cols: Vec<String> = column_defs.iter().map(|(n, _)| n.clone()).collect();
                    let types: Vec<Option<ColumnType>> = column_defs
                        .iter()
                        .map(|(_, t)| ColumnType::from_type_name(t.as_str()))
                        .collect();
                    (cols, types)
                } else if !column_aliases.is_empty() {
                    (column_aliases.clone(), vec![None; column_aliases.len()])
                } else {
                    (vec![name.to_ascii_lowercase()], vec![None])
                };
                let is_recordset = name.to_ascii_lowercase().contains("recordset");
                let rows = if is_recordset {
                    plomid_json::table::json_recordset_rows(&json_value, &col_types)?
                } else {
                    plomid_json::table::json_record_rows(&json_value, &col_types)?
                };
                let schema = synthetic_schema(
                    &alias_name,
                    &columns,
                    &col_types
                        .into_iter()
                        .map(|t| t.or_else(|| Some(ColumnType::text())))
                        .collect::<Vec<_>>(),
                );
                return Ok((
                    vec![JoinScope {
                        alias: alias_name,
                        schema,
                        offset: 0,
                        is_srf: true,
                    }],
                    rows,
                ));
            }
            // JSON_TABLE: `JSON_TABLE(json_expr, 'path' COLUMNS (...))`.  The
            // COLUMNS specs were parsed by `parse_json_table_call` and attached
            // to this FromClause::TableFunction node in `json_columns`.  We
            // evaluate the JSON document and path, expand the path to a set of
            // JSON elements, and then extract each declared column from every
            // element.
            if name.eq_ignore_ascii_case("json_table") {
                let jt_rows = {
                    let mut evaluator = SqlExpressionEval {
                        engine: &mut *engine,
                        catalog,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    };
                    plomid_json::table::json_table_rows(&mut evaluator, &args, json_columns)?
                };
                let alias_name = alias.clone().unwrap_or_else(|| name.clone());
                let (col_names, col_types) = plomid_json::table::json_table_schema(json_columns);
                let schema = synthetic_schema(
                    &alias_name,
                    &col_names,
                    &col_types.into_iter().map(Some).collect::<Vec<_>>(),
                );
                return Ok((
                    vec![JoinScope {
                        alias: alias_name,
                        schema,
                        offset: 0,
                        is_srf: true,
                    }],
                    jt_rows,
                ));
            }
            let relation = crate::system_catalog::SystemCatalog::new(
                catalog,
                current_database,
                current_user,
                catalog.database_names(),
            )
            .relation(name)
            .ok_or_else(|| unsupported(format!("table function \"{name}\" does not exist")))?;
            Ok((
                vec![JoinScope {
                    alias: alias.clone().unwrap_or_else(|| name.clone()),
                    schema: relation.schema,
                    offset: 0,
                    is_srf: false,
                }],
                relation.rows,
            ))
        }
        FromClause::Table { name, alias } => {
            if let Some(relation) = crate::system_catalog::SystemCatalog::new(
                catalog,
                current_database,
                current_user,
                catalog.database_names(),
            )
            .relation(name)
            {
                let alias = effective_alias(name, alias);
                return Ok((
                    vec![JoinScope {
                        alias,
                        schema: relation.schema,
                        offset: 0,
                        is_srf: false,
                    }],
                    relation.rows,
                ));
            }
            // Views resolve to their defining query materialized as a subquery.
            if let Some(view) = catalog.get_view(name) {
                let subquery = FromClause::Subquery {
                    statement: view.query.clone(),
                    alias: alias
                        .clone()
                        .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name).to_string()),
                    column_aliases: Vec::new(),
                    lateral: false,
                };
                return materialize_relation(
                    engine,
                    catalog,
                    &subquery,
                    outer,
                    depth,
                    current_database,
                    current_user,
                );
            }
            let alias = effective_alias(name, alias);
            let schema = catalog.get_table(name)?.clone();
            let entries = engine.scan(
                Some(format!("{name}:").as_bytes()),
                Some(format!("{name}:\u{10FFFF}").as_bytes()),
            )?;
            let rows = entries
                .iter()
                .map(|(_, bytes)| decode_row(bytes))
                .collect::<plomid_core::Result<Vec<_>>>()?;
            Ok((
                vec![JoinScope {
                    alias,
                    schema,
                    offset: 0,
                    is_srf: false,
                }],
                rows,
            ))
        }
        FromClause::Subquery {
            statement,
            alias,
            column_aliases,
            lateral: _,
        } => {
            let result = execute_statement(
                engine,
                catalog,
                statement,
                current_database,
                current_user,
                outer,
                depth + 1,
            )?;
            let QueryResult::Rows {
                mut columns,
                column_types,
                rows,
            } = result
            else {
                return Err(unsupported("a subquery in FROM must produce rows"));
            };
            // Apply an explicit column alias list: `FROM (...) AS x(a, b)`.
            if !column_aliases.is_empty() {
                if column_aliases.len() != columns.len() {
                    return Err(unsupported(
                        "derived table column alias count does not match subquery columns",
                    ));
                }
                columns = column_aliases.clone();
            }
            let schema = synthetic_schema(alias, &columns, &column_types);
            Ok((
                vec![JoinScope {
                    alias: alias.clone(),
                    schema,
                    offset: 0,
                    is_srf: false,
                }],
                rows,
            ))
        }
        FromClause::Join {
            left,
            kind,
            right,
            on,
        } => {
            let (lscopes, lrows) = materialize_relation(
                engine,
                catalog,
                left,
                outer,
                depth,
                current_database,
                current_user,
            )?;
            let left_width: usize = lscopes.iter().map(JoinScope::width).sum();
            // LATERAL / correlated right side: PostgreSQL evaluates the right
            // relation once per left row with the left row visible. Table
            // functions (and subqueries) may reference left columns, so when
            // the right side is lateral-capable, materialize it per left row
            // instead of once with an empty scope.
            if from_clause_is_lateral(right) {
                let mut scopes = lscopes.clone();
                let mut right_width = 0usize;
                let mut right_scopes_template: Option<Vec<JoinScope>> = None;
                let mut per_left: Vec<(Vec<Value>, Vec<Vec<Value>>)> = Vec::new();
                for lrow in &lrows {
                    let current = OuterContext {
                        scopes: lscopes.clone(),
                        row: lrow.clone(),
                        parent: outer.cloned().map(Box::new),
                    };
                    let (mut rscopes, rrows) = materialize_relation(
                        engine,
                        catalog,
                        right,
                        Some(&current),
                        depth,
                        current_database,
                        current_user,
                    )?;
                    for scope in &mut rscopes {
                        scope.offset += left_width;
                    }
                    right_width = right_width.max(rscopes.iter().map(JoinScope::width).sum());
                    if right_scopes_template.is_none() {
                        right_scopes_template = Some(rscopes.clone());
                        scopes.extend(rscopes);
                    }
                    per_left.push((lrow.clone(), rrows));
                }
                let _rscopes = right_scopes_template.unwrap_or_default();
                let rows = lateral_join_rows(
                    engine,
                    catalog,
                    &scopes,
                    per_left,
                    *kind,
                    on.as_ref(),
                    left_width,
                    right_width,
                    outer,
                    depth,
                    current_database,
                    current_user,
                )?;
                return Ok((scopes, rows));
            }
            let (mut rscopes, rrows) = materialize_relation(
                engine,
                catalog,
                right,
                outer,
                depth,
                current_database,
                current_user,
            )?;
            for scope in &mut rscopes {
                scope.offset += left_width;
            }
            let right_width: usize = rscopes.iter().map(JoinScope::width).sum();
            let mut scopes = lscopes.clone();
            scopes.extend(rscopes.clone());
            let rows = perform_join(
                engine,
                catalog,
                &scopes,
                &lrows,
                &rrows,
                *kind,
                on.as_ref(),
                left_width,
                right_width,
                outer,
                depth,
                current_database,
                current_user,
            )?;
            Ok((scopes, rows))
        }
    }
}

/// True when a FROM item may reference preceding FROM items (explicit
/// LATERAL, or a table function which PostgreSQL treats as implicitly
/// lateral when it references earlier columns).
pub(super) fn from_clause_is_lateral(from: &FromClause) -> bool {
    match from {
        FromClause::TableFunction { lateral, .. } => *lateral,
        FromClause::Subquery { lateral, .. } => *lateral,
        FromClause::Join { left, right, .. } => {
            from_clause_is_lateral(left) || from_clause_is_lateral(right)
        }
        FromClause::Table { .. } => false,
    }
}

/// Nested-loop join where the right rows were produced per left row.
/// For INNER/CROSS the combined rows are emitted directly; for OUTER joins
/// an unmatched left row is padded with NULLs, matching `perform_join`.
#[allow(clippy::too_many_arguments)]
pub(super) fn lateral_join_rows<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    per_left: Vec<(Vec<Value>, Vec<Vec<Value>>)>,
    kind: JoinKind,
    on: Option<&Expression>,
    left_width: usize,
    right_width: usize,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<Vec<Vec<Value>>> {
    if on.is_some() && matches!(kind, JoinKind::Cross) {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("{kind:?} JOIN requires an ON or USING clause"),
        )));
    }
    let right_nulls = vec![Value::Null; right_width];
    let mut out = Vec::new();
    match kind {
        JoinKind::Inner | JoinKind::Cross => {
            for (lrow, rrows) in &per_left {
                for rrow in rrows {
                    let combined = concat_slices(lrow, rrow);
                    if join_predicate_true(
                        engine,
                        catalog,
                        scopes,
                        &combined,
                        on,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )? {
                        out.push(combined);
                    }
                }
            }
        }
        JoinKind::Left => {
            for (lrow, rrows) in &per_left {
                let mut any = false;
                for rrow in rrows {
                    let combined = concat_slices(lrow, rrow);
                    if join_predicate_true(
                        engine,
                        catalog,
                        scopes,
                        &combined,
                        on,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )? {
                        out.push(combined);
                        any = true;
                    }
                }
                if !any {
                    out.push(concat_slices(lrow, &right_nulls));
                }
            }
        }
        JoinKind::Right | JoinKind::Full => {
            return Err(unsupported(
                "RIGHT/FULL JOIN with LATERAL right side is not supported",
            ));
        }
    }
    let _ = left_width;
    Ok(out)
}

/// Evaluates the join predicate for one candidate combined row.
#[allow(clippy::too_many_arguments)]
pub(super) fn join_predicate_true<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    combined: &[Value],
    on: Option<&Expression>,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<bool> {
    let Some(expr) = on else {
        return Ok(true);
    };
    let v = JoinEval::eval_join_expr(
        engine,
        catalog,
        scopes,
        combined,
        expr,
        outer,
        depth,
        current_database,
        current_user,
    )?;
    Ok(matches!(v, Value::Bool(true)))
}

/// Returns the left/right row positions for a plain equi-join predicate.
/// The hash table is only a candidate generator; the original predicate is
/// still evaluated for every candidate so coercion and additional semantics
/// remain authoritative.
pub(super) fn equi_join_columns(
    scopes: &[JoinScope],
    on: Option<&Expression>,
    left_width: usize,
) -> Option<(usize, usize)> {
    let Some(Expression::Equal(left, right)) = on else {
        return None;
    };
    let (Expression::ColumnRef(left_name), Expression::ColumnRef(right_name)) = (&**left, &**right)
    else {
        return None;
    };
    let left_index = resolve_column(scopes, None, left_name).ok()?;
    let right_index = resolve_column(scopes, None, right_name).ok()?;
    match (left_index < left_width, right_index < left_width) {
        (true, false) => Some((left_index, right_index - left_width)),
        (false, true) => Some((right_index, left_index - left_width)),
        _ => None,
    }
}

/// Produces a typed, allocation-free hash fingerprint for the scalar values
/// normally used as join keys. The original predicate is still rechecked, so
/// a fingerprint collision can only add a candidate, never change results.
/// Complex values deliberately decline the hash path and use the existing
/// nested-loop semantics instead of inventing a second equality model.
pub(super) fn join_hash_key(value: &Value) -> Option<u64> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match value {
        Value::Bool(v) => (0_u8, *v).hash(&mut hasher),
        Value::Int2(v) => (1_u8, *v as i128).hash(&mut hasher),
        Value::Int4(v) => (1_u8, *v as i128).hash(&mut hasher),
        Value::Int8(v) => (1_u8, *v as i128).hash(&mut hasher),
        Value::Numeric(v) => (4_u8, v.mantissa, v.scale).hash(&mut hasher),
        Value::Float4(v) => (5_u8, v.to_bits()).hash(&mut hasher),
        Value::Float8(v) => (6_u8, v.to_bits()).hash(&mut hasher),
        Value::Money(v) => (7_u8, *v).hash(&mut hasher),
        Value::BpChar(v) => (8_u8, v.as_bytes()).hash(&mut hasher),
        Value::VarChar(v) => (8_u8, v.as_bytes()).hash(&mut hasher),
        Value::Text(v) => (8_u8, v.as_bytes()).hash(&mut hasher),
        Value::Name(v) => (8_u8, v.as_bytes()).hash(&mut hasher),
        Value::Date(v) => (12_u8, *v).hash(&mut hasher),
        Value::Time(v) => (13_u8, *v).hash(&mut hasher),
        Value::TimeTz {
            micros,
            offset_secs,
        } => (14_u8, *micros, *offset_secs).hash(&mut hasher),
        Value::Timestamp(v) => (15_u8, *v).hash(&mut hasher),
        Value::Timestamptz(v) => (16_u8, *v).hash(&mut hasher),
        Value::Uuid(v) => (17_u8, v).hash(&mut hasher),
        Value::Bytea(v) => (18_u8, v.as_slice()).hash(&mut hasher),
        Value::Json(v) => (19_u8, v.as_bytes()).hash(&mut hasher),
        Value::Jsonb(v) => (20_u8, v.as_slice()).hash(&mut hasher),
        Value::Macaddr(v) => (21_u8, v).hash(&mut hasher),
        Value::Macaddr8(v) => (22_u8, v).hash(&mut hasher),
        Value::Oid(v) => (23_u8, *v).hash(&mut hasher),
        Value::Tid { block, offset } => (24_u8, *block, *offset).hash(&mut hasher),
        Value::Xid(v) => (25_u8, *v).hash(&mut hasher),
        Value::Cid(v) => (26_u8, *v).hash(&mut hasher),
        Value::PgLsn(v) => (27_u8, *v).hash(&mut hasher),
        _ => return None,
    }
    Some(hasher.finish())
}

/// Resolves a FROM relation that is a plain base table (not a system
/// catalog relation and not a view), which is the only shape
/// [`try_indexed_join`] serves directly.
pub(super) fn plain_table_schema(
    catalog: &InMemoryCatalog,
    current_database: &str,
    current_user: &str,
    name: &str,
) -> Option<TableSchema> {
    if crate::system_catalog::SystemCatalog::new(
        catalog,
        current_database,
        current_user,
        catalog.database_names(),
    )
    .relation(name)
    .is_some()
    {
        return None;
    }
    if catalog.get_view(name).is_some() {
        return None;
    }
    catalog.get_table(name).ok().cloned()
}

/// Serves a plain two-table INNER JOIN from indexes instead of materializing
/// both relations with full table scans.
///
/// The general path ([`materialize_relation`] + [`perform_join`]) reads every
/// row of *both* relations and only afterwards applies the WHERE, so a
/// selective join such as
///
/// ```text
/// SELECT ... FROM events e JOIN customers c ON e.customer_id = c.customer_id
/// WHERE e.id = 42
/// ```
///
/// pays for a full scan of both tables (and a hash build over the whole right
/// relation) to return a single row. This path instead:
///
/// 1. serves the LEFT relation from its own single-column index when the WHERE
///    holds a conjunct `<left>.<col> = <constant>` — the same provably-safe
///    equality the `UPDATE ... FROM` source-prune path uses — and
/// 2. when the ON is exactly `left.col = right.col` and the right column has a
///    single-column index, materializes the RIGHT relation by probing that
///    index for each left key value instead of scanning the whole table.
///
/// Both prunings yield a superset of the rows the join can match: a left
/// restriction to a WHERE conjunct can only remove rows an INNER JOIN's WHERE
/// would have rejected, and a right row can only join when its key equals some
/// left key. The pruned rows are handed to the unchanged [`perform_join`], so
/// the ON and WHERE predicates are still evaluated by the normal evaluator and
/// the result set is identical. Row order is preserved because both relations
/// are repopulated in storage-key order — which is exactly the order the full
/// scans produced.
///
/// Returns `Ok(None)` for every shape this does not prove safe (outer or cross
/// joins, non-table relations, no usable prune, a non-index-exact key), leaving
/// those queries on the existing path.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub(super) fn try_indexed_join<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    from: &FromClause,
    where_expr: Option<&Expression>,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<Option<(Vec<JoinScope>, Vec<Vec<Value>>)>> {
    let FromClause::Join {
        left,
        kind: JoinKind::Inner,
        right,
        on,
    } = from
    else {
        return Ok(None);
    };
    if from_clause_is_lateral(right) {
        return Ok(None);
    }
    let (
        FromClause::Table {
            name: lname,
            alias: lalias,
        },
        FromClause::Table {
            name: rname,
            alias: ralias,
        },
    ) = (&**left, &**right)
    else {
        return Ok(None);
    };
    let Some(lschema) = plain_table_schema(catalog, current_database, current_user, lname) else {
        return Ok(None);
    };
    let Some(rschema) = plain_table_schema(catalog, current_database, current_user, rname) else {
        return Ok(None);
    };

    // (1) LEFT side: only an index-served left relation makes this path
    // worthwhile. Without a usable prune the general path runs unchanged, so
    // no behavior is altered for unselective joins.
    let lresolved = catalog
        .resolve_table_name(lname)
        .unwrap_or_else(|_| lname.to_string());
    let mut left_rows: Option<Vec<Vec<Value>>> = None;
    for prune in crate::update_from::source_prune_predicates(where_expr, left, catalog) {
        let Some(index) = catalog
            .all_indexes_for_table(&lresolved)
            .into_iter()
            .find(|ix| {
                ix.expression.is_none()
                    && crate::index::index_columns(ix) == [prune.column.as_str()]
            })
        else {
            continue;
        };
        let Some(column) = lschema.columns.get(prune.col_pos) else {
            continue;
        };
        let empty: Vec<Value> = Vec::new();
        let Ok(key) = crate::query::evaluate_expression(&empty, &lschema, &prune.const_expr) else {
            // Let the general path surface the binder/evaluator error.
            return Ok(None);
        };
        if matches!(key, Value::Null) || !crate::update_from::probe_key_is_index_exact(column, &key)
        {
            continue;
        }
        let prefix = crate::update_from::index_probe_prefix(&index.name, &key);
        let end = crate::index::prefix_end(&prefix);
        let mut rows = Vec::new();
        for (_, row_key) in engine.scan(Some(&prefix), Some(&end))? {
            let Some(bytes) = engine.get(&row_key)? else {
                continue; // index entry without a visible row: MVCC-safe
            };
            let row = decode_row(&bytes)?;
            // Re-check through `values_equal`, the predicate equality the
            // general path's WHERE would apply.
            if row
                .get(prune.col_pos)
                .is_some_and(|actual| values_equal(actual, &key))
            {
                rows.push(row);
            }
        }
        left_rows = Some(rows);
        break;
    }
    let Some(left_rows) = left_rows else {
        return Ok(None);
    };

    let left_width = lschema.columns.len();
    let scopes = vec![
        JoinScope {
            alias: effective_alias(lname, lalias),
            schema: lschema,
            offset: 0,
            is_srf: false,
        },
        JoinScope {
            alias: effective_alias(rname, ralias),
            schema: rschema.clone(),
            offset: left_width,
            is_srf: false,
        },
    ];
    let right_width = rschema.columns.len();
    if left_rows.is_empty() {
        // No qualifying left row means an INNER JOIN has no rows at all, so
        // the right relation need not be read.
        return Ok(Some((scopes, Vec::new())));
    }

    // (2) RIGHT side: probe the right index for the left key values present.
    let mut right_rows: Option<Vec<Vec<Value>>> = None;
    if let Some((left_key, right_key)) = equi_join_columns(&scopes, on.as_ref(), left_width) {
        if let Some(column) = rschema.columns.get(right_key) {
            let rresolved = catalog
                .resolve_table_name(rname)
                .unwrap_or_else(|_| rname.to_string());
            if let Some(index) = catalog
                .all_indexes_for_table(&rresolved)
                .into_iter()
                .find(|ix| {
                    ix.expression.is_none()
                        && crate::index::index_columns(ix) == [column.name.as_str()]
                })
            {
                let mut distinct: Vec<Value> = Vec::new();
                let mut seen: HashSet<Vec<u8>> = HashSet::new();
                let mut usable = true;
                for row in &left_rows {
                    let value = row.get(left_key).cloned().unwrap_or(Value::Null);
                    if value.is_null() {
                        continue; // NULL never joins
                    }
                    if !crate::update_from::probe_key_is_index_exact(column, &value) {
                        usable = false;
                        break;
                    }
                    if seen.insert(crate::index::index_value_bytes(&value)) {
                        distinct.push(value);
                    }
                }
                if usable {
                    // A row key may be reachable from several probe values (a
                    // non-unique index), so collect by row key: each matching
                    // row is decoded once, in storage-key (scan) order.
                    let mut collected: std::collections::BTreeMap<Vec<u8>, Vec<u8>> =
                        std::collections::BTreeMap::new();
                    for value in &distinct {
                        let prefix = crate::update_from::index_probe_prefix(&index.name, value);
                        let end = crate::index::prefix_end(&prefix);
                        for (_, row_key) in engine.scan(Some(&prefix), Some(&end))? {
                            if let Some(bytes) = engine.get(&row_key)? {
                                collected.entry(row_key).or_insert(bytes);
                            }
                        }
                    }
                    let mut rows = Vec::with_capacity(collected.len());
                    for (_, bytes) in collected {
                        rows.push(decode_row(&bytes)?);
                    }
                    right_rows = Some(rows);
                }
            }
        }
    }
    let right_rows = match right_rows {
        Some(rows) => rows,
        None => {
            let mut rows = Vec::new();
            for (_, bytes) in engine.scan(
                Some(format!("{rname}:").as_bytes()),
                Some(format!("{rname}:\u{10FFFF}").as_bytes()),
            )? {
                rows.push(decode_row(&bytes)?);
            }
            rows
        }
    };

    let rows = perform_join(
        engine,
        catalog,
        &scopes,
        &left_rows,
        &right_rows,
        JoinKind::Inner,
        on.as_ref(),
        left_width,
        right_width,
        outer,
        depth,
        current_database,
        current_user,
    )?;
    Ok(Some((scopes, rows)))
}

/// Nested-loop join over materialized left/right rows.
#[allow(clippy::too_many_arguments)]
pub(super) fn perform_join<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    lrows: &[Vec<Value>],
    rrows: &[Vec<Value>],
    kind: JoinKind,
    on: Option<&Expression>,
    left_width: usize,
    right_width: usize,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<Vec<Vec<Value>>> {
    if on.is_none() && !matches!(kind, JoinKind::Inner | JoinKind::Cross) {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("{kind:?} JOIN requires an ON or USING clause"),
        )));
    }
    let right_nulls = vec![Value::Null; right_width];
    let left_nulls = vec![Value::Null; left_width];
    let mut out: Vec<Vec<Value>> = Vec::new();
    match kind {
        JoinKind::Inner | JoinKind::Cross => {
            if let Some((left_key, right_key)) = equi_join_columns(scopes, on, left_width) {
                let mut right_buckets: HashMap<u64, Vec<usize>> = HashMap::new();
                let mut hashable = true;
                for (index, row) in rrows.iter().enumerate() {
                    let value = row.get(right_key).cloned().unwrap_or(Value::Null);
                    if !value.is_null() {
                        let Some(key) = join_hash_key(&value) else {
                            hashable = false;
                            break;
                        };
                        right_buckets.entry(key).or_default().push(index);
                    }
                }
                if hashable {
                    for lrow in lrows {
                        let value = lrow.get(left_key).cloned().unwrap_or(Value::Null);
                        if value.is_null() {
                            continue;
                        }
                        let Some(key) = join_hash_key(&value) else {
                            hashable = false;
                            break;
                        };
                        if let Some(matches) = right_buckets.get(&key) {
                            for &right_index in matches {
                                let combined = concat_slices(lrow, &rrows[right_index]);
                                if join_predicate_true(
                                    engine,
                                    catalog,
                                    scopes,
                                    &combined,
                                    on,
                                    outer,
                                    depth,
                                    current_database,
                                    current_user,
                                )? {
                                    out.push(combined);
                                }
                            }
                        }
                    }
                }
                if !hashable {
                    out.clear();
                    for lrow in lrows {
                        for rrow in rrows {
                            let combined = concat_slices(lrow, rrow);
                            if join_predicate_true(
                                engine,
                                catalog,
                                scopes,
                                &combined,
                                on,
                                outer,
                                depth,
                                current_database,
                                current_user,
                            )? {
                                out.push(combined);
                            }
                        }
                    }
                }
            } else {
                for lrow in lrows {
                    for rrow in rrows {
                        let combined = concat_slices(lrow, rrow);
                        if join_predicate_true(
                            engine,
                            catalog,
                            scopes,
                            &combined,
                            on,
                            outer,
                            depth,
                            current_database,
                            current_user,
                        )? {
                            out.push(combined);
                        }
                    }
                }
            }
        }
        JoinKind::Left => {
            for lrow in lrows {
                let mut any = false;
                for rrow in rrows {
                    let combined = concat_slices(lrow, rrow);
                    if join_predicate_true(
                        engine,
                        catalog,
                        scopes,
                        &combined,
                        on,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )? {
                        out.push(combined);
                        any = true;
                    }
                }
                if !any {
                    out.push(concat_slices(lrow, &right_nulls));
                }
            }
        }
        JoinKind::Right => {
            for rrow in rrows {
                let mut any = false;
                for lrow in lrows {
                    let combined = concat_slices(lrow, rrow);
                    if join_predicate_true(
                        engine,
                        catalog,
                        scopes,
                        &combined,
                        on,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )? {
                        out.push(combined);
                        any = true;
                    }
                }
                if !any {
                    out.push(concat_slices(&left_nulls, rrow));
                }
            }
        }
        JoinKind::Full => {
            let mut matched_right = vec![false; rrows.len()];
            for lrow in lrows {
                let mut any = false;
                for (ri, rrow) in rrows.iter().enumerate() {
                    let combined = concat_slices(lrow, rrow);
                    if join_predicate_true(
                        engine,
                        catalog,
                        scopes,
                        &combined,
                        on,
                        outer,
                        depth,
                        current_database,
                        current_user,
                    )? {
                        out.push(combined);
                        any = true;
                        matched_right[ri] = true;
                    }
                }
                if !any {
                    out.push(concat_slices(lrow, &right_nulls));
                }
            }
            for (ri, rrow) in rrows.iter().enumerate() {
                if !matched_right[ri] {
                    out.push(concat_slices(&left_nulls, rrow));
                }
            }
        }
    }
    Ok(out)
}
