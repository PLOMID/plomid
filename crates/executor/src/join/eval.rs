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
//! The join engine's expression evaluator.

use crate::catalog_fn::{
    function_value, is_aggregate_function, is_session_function, sequence_value,
};
use crate::error::{SqlError, SqlResult};
use crate::query::value_cmp;
use crate::row::values_equal;
use crate::scalar::{is_scalar_function, scalar_function_value_extended};
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{
    Expression, InMemoryCatalog, IsBooleanKind, Lexer, Parser, Quantifier, QueryResult, Statement,
    Value,
};
use plomid_txn::StorageEngine;

use super::{
    expr::{
        arithmetic, in_membership, like_match_with_escape, subquery_first_column,
        substitute_function_parameters, system_relation_oid,
    },
    scope::{lookup_column_value, lookup_outer_value, resolve_column, JoinScope, OuterContext},
    select::{cast_to_user_type, eval_all_quantified, eval_any_quantified},
    statement::execute_statement,
    support::unsupported,
};
use plomid_sql::Catalog;

/// Evaluation context for join-aware expression evaluation. Bundles the
/// engine, catalog, scopes, and outer-row binding so subqueries can recurse.
pub(crate) struct JoinEval<'a, E> {
    engine: &'a mut E,
    catalog: &'a InMemoryCatalog,
    scopes: &'a [JoinScope],
    outer: Option<&'a OuterContext>,
    depth: usize,
    current_database: &'a str,
    current_user: &'a str,
    /// Set-returning expansion binding: the current `generate_series` element
    /// while a select-level SRF-in-targetlist expansion evaluates a target
    /// expression. `None` outside such an expansion.
    pub(super) srf_element: Option<Value>,
}

impl<'a, E: StorageEngine> JoinEval<'a, E> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        engine: &'a mut E,
        catalog: &'a InMemoryCatalog,
        scopes: &'a [JoinScope],
        outer: Option<&'a OuterContext>,
        depth: usize,
        current_database: &'a str,
        current_user: &'a str,
    ) -> Self {
        Self {
            engine,
            catalog,
            scopes,
            outer,
            depth,
            current_database,
            current_user,
            srf_element: None,
        }
    }

    /// Entry point used by statement-level code.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn eval_join_expr(
        engine: &mut E,
        catalog: &InMemoryCatalog,
        scopes: &[JoinScope],
        row: &[Value],
        expr: &Expression,
        outer: Option<&OuterContext>,
        depth: usize,
        current_database: &str,
        current_user: &str,
    ) -> SqlResult<Value> {
        let mut eval = JoinEval::new(
            engine,
            catalog,
            scopes,
            outer,
            depth,
            current_database,
            current_user,
        );
        eval.eval(row, expr)
    }

    pub(crate) fn eval(&mut self, row: &[Value], expr: &Expression) -> SqlResult<Value> {
        match expr {
            Expression::ColumnRef(name) => {
                if is_session_function(name) {
                    return function_value(name, self.current_database, self.current_user);
                }
                if let Some(value) = lookup_column_value(self.scopes, row, name)? {
                    return Ok(value);
                }
                if !name.contains('.') {
                    if let Some(scope) = self
                        .scopes
                        .iter()
                        .find(|scope| scope.alias.eq_ignore_ascii_case(name))
                    {
                        // A bare alias of a single-column relation resolves as
                        // that column's scalar value: PostgreSQL's single-column
                        // set-returning-function results (the one generated
                        // column of `FROM generate_series(...) x`) are the row
                        // itself, so scalar evaluation must see the value rather
                        // than a whole-row composite.
                        if scope.schema.columns.len() == 1 && scope.is_srf {
                            return Ok(row.get(scope.offset).cloned().unwrap_or(Value::Null));
                        }
                        let fields = scope
                            .schema
                            .columns
                            .iter()
                            .enumerate()
                            .map(|(index, column)| {
                                (
                                    column.name.clone(),
                                    row.get(scope.offset + index)
                                        .cloned()
                                        .unwrap_or(Value::Null),
                                )
                            })
                            .collect();
                        return Ok(Value::Composite {
                            type_oid: plomid_types::TypeOid::RECORD,
                            fields,
                        });
                    }
                }
                if let Some(outer) = self.outer {
                    if let Some(value) = lookup_outer_value(outer, name)? {
                        return Ok(value);
                    }
                }
                // Reuse the resolver for its PostgreSQL-shaped diagnostic.
                let _ = resolve_column(self.scopes, self.outer, name)?;
                unreachable!("resolve_column returned an unresolved column")
            }
            Expression::Literal(value) => Ok(value.clone()),
            Expression::Star => Ok(Value::Null),
            Expression::Equal(left, right) => {
                let (l, r) = self.eval_pair(row, left, right)?;
                if l.is_null() || r.is_null() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Bool(values_equal(&l, &r)))
                }
            }
            Expression::NotEqual(left, right) => {
                let (l, r) = self.eval_pair(row, left, right)?;
                if l.is_null() || r.is_null() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Bool(!values_equal(&l, &r)))
                }
            }
            Expression::Less(left, right)
            | Expression::LessOrEqual(left, right)
            | Expression::Greater(left, right)
            | Expression::GreaterOrEqual(left, right) => {
                let (l, r) = self.eval_pair(row, left, right)?;
                if l.is_null() || r.is_null() {
                    return Ok(Value::Null);
                }
                let Some(order) = value_cmp(&l, &r) else {
                    return Ok(Value::Null);
                };
                let result = match expr {
                    Expression::Greater(_, _) => order == std::cmp::Ordering::Greater,
                    Expression::GreaterOrEqual(_, _) => order != std::cmp::Ordering::Less,
                    Expression::Less(_, _) => order == std::cmp::Ordering::Less,
                    _ => order != std::cmp::Ordering::Greater,
                };
                Ok(Value::Bool(result))
            }
            Expression::IsNull(inner) => Ok(Value::Bool(self.eval(row, inner)?.is_null())),
            Expression::IsNotNull(inner) => Ok(Value::Bool(!self.eval(row, inner)?.is_null())),
            Expression::And(left, right) => {
                let l = self.eval(row, left)?;
                if matches!(l, Value::Bool(false)) {
                    return Ok(Value::Bool(false));
                }
                let r = self.eval(row, right)?;
                if matches!(r, Value::Bool(false)) {
                    return Ok(Value::Bool(false));
                }
                if l.is_null() || r.is_null() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Bool(true))
                }
            }
            Expression::Or(left, right) => {
                let l = self.eval(row, left)?;
                if matches!(l, Value::Bool(true)) {
                    return Ok(Value::Bool(true));
                }
                let r = self.eval(row, right)?;
                if matches!(r, Value::Bool(true)) {
                    return Ok(Value::Bool(true));
                }
                if l.is_null() || r.is_null() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Bool(false))
                }
            }
            Expression::Not(inner) => match self.eval(row, inner)? {
                Value::Bool(true) => Ok(Value::Bool(false)),
                Value::Bool(false) => Ok(Value::Bool(true)),
                _ => Ok(Value::Null),
            },
            _ => self.eval_rest(row, expr),
        }
    }

    /// Second half of the expression match: predicates, conditionals,
    /// subqueries, and function calls.
    fn eval_rest(&mut self, row: &[Value], expr: &Expression) -> SqlResult<Value> {
        match expr {
            Expression::Add(left, right)
            | Expression::Subtract(left, right)
            | Expression::Multiply(left, right)
            | Expression::Divide(left, right)
            | Expression::Modulo(left, right) => {
                let (l, r) = self.eval_pair(row, left, right)?;
                arithmetic(&l, &r, expr)
            }
            Expression::Concat(left, right) => {
                let (left, right) = self.eval_pair(row, left, right)?;
                crate::scalar::concat_operator(&left, &right)
            }
            Expression::Power(left, right) => {
                let l = self.eval(row, left)?;
                let r = self.eval(row, right)?;
                crate::scalar::scalar_function_value("power", &[l, r])
            }
            Expression::BitAnd(left, right)
            | Expression::BitOr(left, right)
            | Expression::BitXor(left, right)
            | Expression::ShiftLeft(left, right)
            | Expression::ShiftRight(left, right) => {
                let (l, r) = self.eval_pair(row, left, right)?;
                if l.is_null() || r.is_null() {
                    Ok(Value::Null)
                } else {
                    crate::scalar::integer_bitwise(expr, &l, &r)
                }
            }
            Expression::Negate(inner) => match self.eval(row, inner)? {
                Value::Null => Ok(Value::Null),
                Value::Int2(x) => Ok(Value::Int2(-x)),
                Value::Int4(x) => Ok(Value::Int4(-x)),
                Value::Int8(x) => Ok(Value::Int8(-x)),
                Value::Float4(x) => Ok(Value::Float4(-x)),
                Value::Float8(x) => Ok(Value::Float8(-x)),
                Value::Numeric(n) => Ok(Value::Numeric(n.clone().neg())),
                other => Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    format!("cannot negate {other:?}"),
                ))),
            },
            Expression::In {
                expr: inner,
                list,
                subquery,
                negated,
            } => {
                let v = self.eval(row, inner)?;
                let candidates: Vec<Value> = if let Some(sub) = subquery {
                    let result = self.run_subquery(row, sub)?;
                    subquery_first_column(result)?
                } else {
                    list.iter()
                        .map(|item| self.eval(row, item))
                        .collect::<SqlResult<_>>()?
                };
                in_membership(&v, &candidates, *negated)
            }
            Expression::Between {
                expr: inner,
                low,
                high,
                negated,
            } => {
                let v = self.eval(row, inner)?;
                let lo = self.eval(row, low)?;
                let hi = self.eval(row, high)?;
                if v.is_null() || lo.is_null() || hi.is_null() {
                    return Ok(Value::Null);
                }
                let inside = matches!(
                    value_cmp(&v, &lo),
                    Some(std::cmp::Ordering::Greater) | Some(std::cmp::Ordering::Equal)
                ) && matches!(
                    value_cmp(&v, &hi),
                    Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal)
                );
                Ok(Value::Bool(if *negated { !inside } else { inside }))
            }
            Expression::Like {
                expr: inner,
                pattern,
                escape,
                negated,
            } => {
                let v = self.eval(row, inner)?;
                let p = self.eval(row, pattern)?;
                if v.is_null() || p.is_null() {
                    return Ok(Value::Null);
                }
                let matched = like_match_with_escape(&v.to_sql_text(), &p.to_sql_text(), *escape);
                Ok(Value::Bool(if *negated { !matched } else { matched }))
            }
            _ => self.eval_rest_2(row, expr),
        }
    }

    /// Final segment: CASE, COALESCE-family, subqueries, and function calls.
    fn eval_rest_2(&mut self, row: &[Value], expr: &Expression) -> SqlResult<Value> {
        match expr {
            Expression::Case {
                operand,
                whens,
                default,
            } => self.eval_case(row, operand, whens, default),
            Expression::Coalesce(args) => {
                for arg in args {
                    let v = self.eval(row, arg)?;
                    if !v.is_null() {
                        return Ok(v);
                    }
                }
                Ok(Value::Null)
            }
            Expression::NullIf(a, b) => {
                let av = self.eval(row, a)?;
                let bv = self.eval(row, b)?;
                if !av.is_null() && !bv.is_null() && values_equal(&av, &bv) {
                    Ok(Value::Null)
                } else {
                    Ok(av)
                }
            }
            Expression::IsDistinctFrom(left, right) => {
                let (l, r) = self.eval_pair(row, left, right)?;
                if l.is_null() && r.is_null() {
                    Ok(Value::Bool(false))
                } else if l.is_null() || r.is_null() {
                    Ok(Value::Bool(true))
                } else {
                    Ok(Value::Bool(!values_equal(&l, &r)))
                }
            }
            Expression::Exists(sub) => {
                let result = self.run_subquery(row, sub)?;
                let QueryResult::Rows { rows, .. } = result else {
                    return Err(unsupported("EXISTS requires a SELECT subquery"));
                };
                Ok(Value::Bool(!rows.is_empty()))
            }
            Expression::ScalarSubquery(sub) => {
                let result = self.run_subquery(row, sub)?;
                let QueryResult::Rows { rows, .. } = result else {
                    return Err(unsupported("scalar subquery requires a SELECT statement"));
                };
                match rows.len() {
                    0 => Ok(Value::Null),
                    1 if rows[0].len() == 1 => Ok(rows[0][0].clone()),
                    1 => Err(unsupported(
                        "scalar subquery must return exactly one column",
                    )),
                    _ => Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::InvalidArgument,
                        "more than one row returned by a subquery used as an expression",
                    ))),
                }
            }
            Expression::QuantifiedComparison {
                left,
                operator,
                quantifier,
                subquery,
            } => {
                let left_val = self.eval(row, left)?;
                let result = self.run_subquery(row, subquery)?;
                let QueryResult::Rows { rows, .. } = result else {
                    return Err(unsupported(
                        "quantified comparison requires a SELECT subquery",
                    ));
                };

                // Collect values from subquery (each row must have exactly one column)
                let mut values: Vec<Value> = Vec::with_capacity(rows.len());
                for row_data in rows {
                    match row_data.len() {
                        1 => values.push(row_data.into_iter().next().unwrap()),
                        _ => {
                            return Err(SqlError::Storage(PlomidError::new(
                                ErrorKind::InvalidArgument,
                                format!(
                                "quantified comparison subquery returned {} columns, expected 1",
                                row_data.len()
                            ),
                            )))
                        }
                    }
                }

                // Evaluate based on quantifier
                match quantifier {
                    Quantifier::Any => eval_any_quantified(left_val, *operator, values),
                    Quantifier::All => eval_all_quantified(left_val, *operator, values),
                }
            }
            Expression::WindowFunction { .. } => Err(unsupported(
                "window functions are only allowed in the SELECT list or ORDER BY",
            )),
            Expression::TypeCast { expr, type_name } => {
                let value = self.eval(row, expr)?;
                self.cast_value(&value, type_name)
            }
            Expression::Cast { expr, type_name } => {
                let value = self.eval(row, expr)?;
                self.cast_value(&value, type_name)
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
                // Same single timestamptz representation as the simple query
                // path: the join/general engine must produce the identical
                // value for `TIMESTAMPTZ '...'` literals.
                plomid_types::text::parse_value(text, plomid_types::PgType::Timestamptz)
                    .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
            }
            Expression::TimeLiteral(text) => {
                plomid_types::text::parse_value(text, plomid_types::PgType::Time)
                    .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
            }
            Expression::TypedTimeLiteral { text, precision } => {
                let base = plomid_types::text::parse_value(text, plomid_types::PgType::Time)
                    .map_err(|e| {
                        SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e))
                    })?;
                plomid_types::apply_typmod(base, i32::from(*precision), plomid_types::PgType::Time)
                    .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
            }
            Expression::TypedTimestampLiteral { text, precision } => {
                let base = plomid_types::text::parse_value(text, plomid_types::PgType::Timestamp)
                    .map_err(|e| {
                    SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e))
                })?;
                plomid_types::apply_typmod(
                    base,
                    i32::from(*precision),
                    plomid_types::PgType::Timestamp,
                )
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
            }
            Expression::TypedTimestamptzLiteral { text, precision } => {
                let base = plomid_types::text::parse_value(text, plomid_types::PgType::Timestamptz)
                    .map_err(|e| {
                        SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e))
                    })?;
                plomid_types::apply_typmod(
                    base,
                    i32::from(*precision),
                    plomid_types::PgType::Timestamptz,
                )
                .map_err(|e| SqlError::Storage(PlomidError::new(ErrorKind::InvalidArgument, e)))
            }
            Expression::Extract { field, expr } => {
                let value = self.eval(row, expr)?;
                crate::query::extract_datetime_field(&value, field)
            }
            Expression::JsonArrow {
                left,
                right,
                as_text,
            } => {
                let lhs = self.eval(row, left)?;
                let rhs = self.eval(row, right)?;
                crate::json::json_arrow_value(&lhs, &rhs, *as_text)
            }
            Expression::ArrayIndex { array, index } => {
                let arr = self.eval(row, array)?;
                let idx = self.eval(row, index)?;
                crate::util::array_index_value(&arr, &idx)
            }
            Expression::JsonSubscript { array, index } => {
                let arr = self.eval(row, array)?;
                let idx = self.eval(row, index)?;
                crate::json::json_subscript_value(&arr, &idx)
            }
            Expression::RowField { expr, field } => {
                let value = self.eval(row, expr)?;
                crate::scalar::row_field_value(&value, field)
            }
            Expression::FunctionCall {
                name,
                args,
                returning,
                null_handling,
                unique_keys,
                ..
            } => self.eval_extended_function_call(
                row,
                name,
                args,
                returning.clone(),
                *null_handling,
                *unique_keys,
            ),
            Expression::IsJson {
                expr,
                kind,
                negated,
            } => {
                let value = self.eval(row, expr)?;
                // SQL three-valued logic: NULL input yields NULL result.
                if value.is_null() {
                    return Ok(Value::Null);
                }
                let is_json = crate::query::json_kind_matches(&value, *kind)?;
                Ok(Value::Bool(if *negated { !is_json } else { is_json }))
            }
            Expression::IsBoolean {
                expr,
                kind,
                negated,
            } => {
                let value = self.eval(row, expr)?;
                let result = match (kind, &value) {
                    (IsBooleanKind::True, Value::Bool(true)) => true,
                    (IsBooleanKind::False, Value::Bool(false)) => true,
                    (IsBooleanKind::Unknown, Value::Null) => true,
                    _ => false,
                };
                Ok(Value::Bool(if *negated { !result } else { result }))
            }
            other => Err(unsupported(format!(
                "expression is not supported here: {other:?}"
            ))),
        }
    }

    fn cast_value(&self, value: &Value, type_name: &str) -> SqlResult<Value> {
        let normalized = type_name.trim().to_ascii_lowercase();
        if normalized == "regclass" {
            if value.is_null() {
                return Ok(Value::Null);
            }
            let relation = value.to_sql_text().replace('"', "");
            let bare = relation
                .rsplit_once('.')
                .map(|(_, tail)| tail)
                .unwrap_or(relation.as_str());
            let candidates: Vec<String> = {
                let mut list = Vec::new();
                for candidate in [relation.clone(), bare.to_string()] {
                    for form in [candidate.clone(), format!("public.{candidate}")] {
                        if !list.contains(&form) {
                            list.push(form);
                        }
                    }
                }
                list
            };
            for candidate in &candidates {
                if let Ok(table) = self.catalog.get_table(candidate) {
                    return Ok(Value::Reg {
                        oid: table.table_id.get() as u32,
                        name: Some(candidate.clone()),
                    });
                }
            }
            for candidate in &candidates {
                if let Some(oid) = system_relation_oid(candidate) {
                    return Ok(Value::Reg {
                        oid,
                        name: Some(candidate.clone()),
                    });
                }
            }
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::NotFound,
                format!("relation \"{relation}\" does not exist"),
            )));
        }
        // Check for user-defined types (composites, enums, domains) in the catalog.
        // These are schema-qualified names like `schema.typename` that PgType::by_name
        // won't recognize. For composite types, we tag the value with the correct OID.
        if self.catalog.has_type(type_name) {
            return cast_to_user_type(value, type_name, self.catalog);
        }
        crate::coerce::cast_value(value, type_name)
    }

    /// `CASE x WHEN v1 THEN r1 ... [ELSE d] END` and the searched form.
    fn eval_case(
        &mut self,
        row: &[Value],
        operand: &Option<Box<Expression>>,
        whens: &[(Expression, Expression)],
        default: &Option<Box<Expression>>,
    ) -> SqlResult<Value> {
        let operand_value = match operand {
            Some(op) => Some(self.eval(row, op)?),
            None => None,
        };
        for (cond, val) in whens {
            let test = match &operand_value {
                Some(op_value) => {
                    let candidate = self.eval(row, cond)?;
                    if op_value.is_null() || candidate.is_null() {
                        Value::Null
                    } else {
                        Value::Bool(values_equal(op_value, &candidate))
                    }
                }
                None => self.eval(row, cond)?,
            };
            if matches!(test, Value::Bool(true)) {
                return self.eval(row, val);
            }
        }
        match default {
            Some(def) => self.eval(row, def),
            None => Ok(Value::Null),
        }
    }

    /// Executes a nested SELECT and returns its full row result.
    fn run_subquery(&mut self, row: &[Value], sub: &Statement) -> SqlResult<QueryResult> {
        let current = OuterContext {
            scopes: self.scopes.to_vec(),
            row: row.to_vec(),
            parent: self.outer.cloned().map(Box::new),
        };
        execute_statement(
            self.engine,
            self.catalog,
            sub,
            self.current_database,
            self.current_user,
            Some(&current),
            self.depth + 1,
        )
    }

    /// Scalar function calls, session functions, and sequence functions.
    /// Extended variant carrying SQL/JSON constructor clauses (RETURNING /
    /// NULL ON NULL / ABSENT ON NULL / WITH UNIQUE KEYS) so JSON constructors
    /// can honour them at evaluation time.
    #[allow(clippy::too_many_arguments)]
    fn eval_extended_function_call(
        &mut self,
        row: &[Value],
        name: &str,
        args: &[Expression],
        returning: Option<String>,
        null_handling: Option<plomid_sql::NullHandling>,
        unique_keys: Option<bool>,
    ) -> SqlResult<Value> {
        if is_aggregate_function(name) {
            return Err(unsupported(format!(
                "aggregate function {name}(...) cannot be evaluated here"
            )));
        }
        if is_session_function(name) {
            return function_value(name, self.current_database, self.current_user);
        }
        if name.eq_ignore_ascii_case("generate_series") {
            return match &self.srf_element {
                Some(element) => Ok(element.clone()),
                None => Err(unsupported(format!(
                    "function \"{name}\" is not supported without set-returning expansion"
                ))),
            };
        }
        let values = args
            .iter()
            .map(|arg| self.eval(row, arg))
            .collect::<SqlResult<Vec<_>>>()?;
        let lname = name.to_ascii_lowercase();
        if lname == "pg_get_constraintdef" || lname == "pg_get_indexdef" {
            return Ok(crate::catalog_fn::catalog_object_definition(
                self.catalog,
                &lname,
                values.first(),
            ));
        }
        if matches!(lname.as_str(), "to_regclass" | "to_regnamespace") {
            return crate::catalog_fn::reg_lookup(self.catalog, &lname, values.first());
        }
        if matches!(name.to_ascii_lowercase().as_str(), "nextval" | "currval") {
            let argument = values.first().map(|v| v.to_sql_text()).unwrap_or_default();
            return sequence_value(self.engine, name, &argument);
        }
        if is_scalar_function(name) {
            return scalar_function_value_extended(
                name,
                &values,
                returning,
                null_handling,
                unique_keys,
            );
        }
        if let Some(func) = self.catalog.find_function_by_call(name, values.len()) {
            return self.call_user_function(&func, &values);
        }
        Err(unsupported(format!("function \"{name}\" is not supported")))
    }

    /// Executes a user-defined SQL function: substitutes `$1..$n` parameter
    /// references with the evaluated arguments, parses the body, and runs it
    /// as a subquery, returning its single scalar result.
    fn call_user_function(
        &mut self,
        func: &plomid_sql::StoredFunction,
        values: &[Value],
    ) -> SqlResult<Value> {
        if !func.language.eq_ignore_ascii_case("sql") {
            return Err(unsupported(format!(
                "language \"{}\" for function \"{}\" is not supported",
                func.language, func.name
            )));
        }
        let body = substitute_function_parameters(&func.body, values);
        let tokens = Lexer::new(&body)
            .lex()
            .map_err(|e| unsupported(format!("in body of function \"{}\": {e}", func.name)))?;
        // Parsing only reads catalog metadata; parse against a scratch clone
        // because the parser requires mutable catalog access.
        let mut scratch = self.catalog.clone();
        let stmt = {
            let parser = Parser::new(tokens, &mut scratch);
            let stmts = parser
                .parse_statements()
                .map_err(|e| unsupported(format!("in body of function \"{}\": {e}", func.name)))?;
            stmts
                .into_iter()
                .next()
                .ok_or_else(|| unsupported("function body is empty"))?
        };
        let result = self.run_subquery(&[], &stmt)?;
        let QueryResult::Rows { rows, .. } = result else {
            return Err(unsupported("SQL function body must return a query result"));
        };
        match rows.len() {
            0 => Ok(Value::Null),
            1 if rows[0].len() == 1 => Ok(rows[0][0].clone()),
            _ => Err(unsupported("function returned more than one row or column")),
        }
    }

    fn eval_pair(
        &mut self,
        row: &[Value],
        left: &Expression,
        right: &Expression,
    ) -> SqlResult<(Value, Value)> {
        let l = self.eval(row, left)?;
        let r = self.eval(row, right)?;
        Ok(crate::coerce::coerce_comparison_operands(l, r))
    }
}
