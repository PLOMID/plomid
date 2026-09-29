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
//! SQL execution boundary: lower `plomid_sql::Expression` to [`PrunePredicate`].
//!
//! The pruning layer keeps its own small predicate model on purpose: the SQL
//! AST is a general expression language (arithmetic, functions, subqueries,
//! casts, …) while [`PrunePredicate`] admits only the shapes whose metadata
//! proof the pruner has verified. This module is the *only* bridge between the
//! two — no parallel SQL AST is created here, and no planner or executor is
//! redesigned.
//!
//! Lowering is conservative in a single, auditable direction:
//!
//! * every supported shape maps to the equivalent [`PrunePredicate`];
//! * everything else maps to [`SqlLowering::Unprunable`], which the caller
//!   must treat as "scan everything".
//!
//! A lowered predicate is evaluated by exactly the same proof rules as a
//! hand-built one (`predicate.rs`), so the false-negative guarantee is
//! inherited unchanged: PRUNE still requires a proof, and uncertainty never
//! prunes.
//!
//! Supported shapes (column references resolve through the caller's
//! name→[`ColumnId`] map, unqualified names match `table.column` too):
//!
//! * `col = lit`, `col != lit`, `col < lit`, `col <= lit`, `col > lit`,
//!   `col >= lit` (literal on either side; the operator is mirrored);
//! * `col IS NULL`, `col IS NOT NULL`;
//! * `A AND B`, `A OR B`, `NOT A`;
//! * `col BETWEEN low AND high` (inclusive conjunction of two comparisons);
//! * `col IN (lit, …)` (disjunction of equalities; empty or non-literal lists
//!   are unprunable).
//!
//! Literals map to storage [`Field`] values: integers (all widths, checked
//! for `i64` range), text-like values, and byte strings. Every other literal
//! kind has no storage ordering the pruner understands, so it is unprunable.

use crate::pruning::predicate::{PruneOperator, PrunePredicate};
use plomid_core::ColumnId;
use plomid_storage::Field;
use std::collections::BTreeMap;
use std::fmt;

/// Conservative result of lowering one SQL expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SqlLowering {
    /// The expression lowered to an equivalent pruning predicate.
    Prunable(PrunePredicate),
    /// The expression has no safe pruning form — the caller must scan.
    Unprunable(String),
}

impl SqlLowering {
    /// Returns the lowered predicate, if the expression was prunable.
    #[must_use]
    pub fn predicate(&self) -> Option<&PrunePredicate> {
        match self {
            Self::Prunable(predicate) => Some(predicate),
            Self::Unprunable(_) => None,
        }
    }

    /// Returns true when the expression can participate in pruning.
    #[must_use]
    pub fn prunable(&self) -> bool {
        matches!(self, Self::Prunable(_))
    }
}

/// Lowers a SQL `WHERE`-style expression to a pruning predicate.
///
/// `columns` maps column names to stable [`ColumnId`]s; both `column` and
/// `table.column` spellings resolve. Unknown columns, unsupported operators,
/// non-literal operands, and unrepresentable literals yield
/// [`SqlLowering::Unprunable`] — never an error, never a prune.
#[must_use]
pub fn lower_sql_expression(
    expression: &plomid_sql::Expression,
    columns: &BTreeMap<String, ColumnId>,
) -> SqlLowering {
    lower(expression, columns)
}

/// Explains why an expression did not lower (for EXPLAIN-style callers).
#[must_use]
pub fn explain_unprunable(lowering: &SqlLowering) -> Option<&str> {
    match lowering {
        SqlLowering::Prunable(_) => None,
        SqlLowering::Unprunable(reason) => Some(reason),
    }
}

/// Recursive core of the lowering.
fn lower(expression: &plomid_sql::Expression, columns: &BTreeMap<String, ColumnId>) -> SqlLowering {
    use plomid_sql::Expression as E;
    match expression {
        E::Equal(left, right) => lower_comparison(left, right, PruneOperator::Equal, columns),
        E::NotEqual(left, right) => lower_comparison(left, right, PruneOperator::NotEqual, columns),
        E::Less(left, right) => lower_comparison(left, right, PruneOperator::Less, columns),
        E::LessOrEqual(left, right) => {
            lower_comparison(left, right, PruneOperator::LessOrEqual, columns)
        }
        E::Greater(left, right) => lower_comparison(left, right, PruneOperator::Greater, columns),
        E::GreaterOrEqual(left, right) => {
            lower_comparison(left, right, PruneOperator::GreaterOrEqual, columns)
        }
        E::IsNull(inner) => match column_of(inner, columns) {
            Some(column_id) => SqlLowering::Prunable(PrunePredicate::is_null(column_id)),
            None => SqlLowering::Unprunable("IS NULL over a non-column".to_owned()),
        },
        E::IsNotNull(inner) => match column_of(inner, columns) {
            Some(column_id) => SqlLowering::Prunable(PrunePredicate::is_not_null(column_id)),
            None => SqlLowering::Unprunable("IS NOT NULL over a non-column".to_owned()),
        },
        E::And(left, right) => match (lower(left, columns), lower(right, columns)) {
            (SqlLowering::Prunable(left), SqlLowering::Prunable(right)) => {
                SqlLowering::Prunable(left.and(right))
            }
            // `A AND B` prunes when *either* branch proves impossibility, so a
            // single prunable branch is still useful: the unprunable branch
            // can never add rows back.
            (SqlLowering::Prunable(kept), SqlLowering::Unprunable(_))
            | (SqlLowering::Unprunable(_), SqlLowering::Prunable(kept)) => {
                SqlLowering::Prunable(kept)
            }
            (SqlLowering::Unprunable(left), SqlLowering::Unprunable(right)) => {
                SqlLowering::Unprunable(format!("AND is unprunable ({left}; {right})"))
            }
        },
        E::Or(left, right) => match (lower(left, columns), lower(right, columns)) {
            (SqlLowering::Prunable(left), SqlLowering::Prunable(right)) => {
                SqlLowering::Prunable(left.or(right))
            }
            // `A OR B` prunes only when *both* branches prove impossibility;
            // a half-prunable disjunction cannot prune, so report it whole.
            (left, right) => SqlLowering::Unprunable(format!(
                "OR needs both branches (left={}; right={})",
                short(&left),
                short(&right),
            )),
        },
        E::Not(inner) => match lower(inner, columns) {
            SqlLowering::Prunable(predicate) => SqlLowering::Prunable(predicate.not()),
            SqlLowering::Unprunable(reason) => {
                SqlLowering::Unprunable(format!("NOT of unprunable predicate ({reason})"))
            }
        },
        // Range predicates and `IN` decompose into the conjunction/disjunction
        // forms above, so they reuse the same conservative combinators.
        E::Between {
            expr,
            low,
            high,
            negated,
        } => lower_between(expr, low, high, *negated, columns),
        E::In {
            expr,
            list,
            subquery,
            negated,
        } => lower_in(expr, list, subquery.is_some(), *negated, columns),
        other => SqlLowering::Unprunable(format!("unsupported expression {}", kind_of(other))),
    }
}

/// Lowers `left OP right` where exactly one side is a column and the other a
/// supported literal. A literal on the left mirrors the operator.
fn lower_comparison(
    left: &plomid_sql::Expression,
    right: &plomid_sql::Expression,
    operator: PruneOperator,
    columns: &BTreeMap<String, ColumnId>,
) -> SqlLowering {
    if let Some(column_id) = column_of(left, columns) {
        return match literal_field(right) {
            Some(literal) => {
                SqlLowering::Prunable(PrunePredicate::compare(column_id, operator, literal))
            }
            None => SqlLowering::Unprunable(non_literal_reason(right)),
        };
    }
    if let Some(column_id) = column_of(right, columns) {
        return match literal_field(left) {
            Some(literal) => SqlLowering::Prunable(PrunePredicate::compare(
                column_id,
                mirror(operator),
                literal,
            )),
            None => SqlLowering::Unprunable(non_literal_reason(left)),
        };
    }
    SqlLowering::Unprunable("comparison without a column".to_owned())
}

/// Resolves a column reference to its [`ColumnId`], accepting qualified names.
fn column_of(
    expression: &plomid_sql::Expression,
    columns: &BTreeMap<String, ColumnId>,
) -> Option<ColumnId> {
    if let plomid_sql::Expression::ColumnRef(name) = expression {
        if let Some(found) = columns.get(name) {
            return Some(*found);
        }
        if let Some(unqualified) = name.rsplit('.').next() {
            if let Some(found) = columns.get(unqualified) {
                return Some(*found);
            }
        }
    }
    None
}

/// Maps a SQL literal to its storage [`Field`], when representable.
fn literal_field(expression: &plomid_sql::Expression) -> Option<Field> {
    use plomid_sql::Value as V;
    let literal = match expression {
        plomid_sql::Expression::Literal(value) => value,
        _ => return None,
    };
    match literal {
        V::Null => None,
        V::Int2(value) => Some(Field::Integer(i64::from(*value))),
        V::Int4(value) => Some(Field::Integer(i64::from(*value))),
        V::Int8(value) => Some(Field::Integer(*value)),
        V::Text(text) | V::VarChar(text) | V::BpChar(text) | V::Name(text) | V::Unknown(text) => {
            Some(Field::String(text.clone()))
        }
        V::Bytea(bytes) => Some(Field::Bytes(bytes.clone())),
        V::Date(_)
        | V::Time(_)
        | V::TimeTz { .. }
        | V::Timestamp(_)
        | V::Timestamptz(_)
        | V::Interval(_)
        | V::Bool(_) => Some(Field::String(literal.to_sql_text())),
        // Every other literal kind has no storage ordering the pruner
        // understands — report it as unrepresentable (scan).
        _ => None,
    }
}

fn non_literal_reason(expression: &plomid_sql::Expression) -> String {
    if matches!(
        expression,
        plomid_sql::Expression::Literal(plomid_sql::Value::Null)
    ) {
        return "NULL literal is not a pruning bound".to_owned();
    }
    "non-literal comparison operand".to_owned()
}

/// Mirrors an operator when the literal moves to the left of the column.
fn mirror(operator: PruneOperator) -> PruneOperator {
    match operator {
        PruneOperator::Equal => PruneOperator::Equal,
        PruneOperator::NotEqual => PruneOperator::NotEqual,
        PruneOperator::Less => PruneOperator::Greater,
        PruneOperator::LessOrEqual => PruneOperator::GreaterOrEqual,
        PruneOperator::Greater => PruneOperator::Less,
        PruneOperator::GreaterOrEqual => PruneOperator::LessOrEqual,
        PruneOperator::IsNull => PruneOperator::IsNull,
        PruneOperator::IsNotNull => PruneOperator::IsNotNull,
    }
}

fn short(lowering: &SqlLowering) -> String {
    match lowering {
        SqlLowering::Prunable(_) => "prunable".to_owned(),
        SqlLowering::Unprunable(reason) => format!("unprunable ({reason})"),
    }
}

fn kind_of(expression: &plomid_sql::Expression) -> &'static str {
    use plomid_sql::Expression as E;
    match expression {
        E::ColumnRef(_) => "column",
        E::Literal(_) => "literal",
        E::Star => "star",
        E::Equal(..) => "equal",
        E::NotEqual(..) => "not-equal",
        E::Less(..) => "less",
        E::LessOrEqual(..) => "less-or-equal",
        E::Greater(..) => "greater",
        E::GreaterOrEqual(..) => "greater-or-equal",
        E::IsNull(_) => "is-null",
        E::IsNotNull(_) => "is-not-null",
        E::And(..) => "and",
        E::Or(..) => "or",
        E::Not(_) => "not",
        _ => "other",
    }
}

impl fmt::Display for SqlLowering {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prunable(predicate) => write!(formatter, "prunable({predicate:?})"),
            Self::Unprunable(reason) => write!(formatter, "unprunable({reason})"),
        }
    }
}

fn lower_between(
    expr: &plomid_sql::Expression,
    low: &plomid_sql::Expression,
    high: &plomid_sql::Expression,
    negated: bool,
    columns: &BTreeMap<String, ColumnId>,
) -> SqlLowering {
    let Some(column_id) = column_of(expr, columns) else {
        return SqlLowering::Unprunable("BETWEEN over a non-column".to_owned());
    };
    let (Some(low), Some(high)) = (literal_field(low), literal_field(high)) else {
        return SqlLowering::Unprunable("BETWEEN with non-literal bound".to_owned());
    };
    let between = PrunePredicate::compare(column_id, PruneOperator::GreaterOrEqual, low).and(
        PrunePredicate::compare(column_id, PruneOperator::LessOrEqual, high),
    );
    if negated {
        SqlLowering::Prunable(between.not())
    } else {
        SqlLowering::Prunable(between)
    }
}

fn lower_in(
    expr: &plomid_sql::Expression,
    list: &[plomid_sql::Expression],
    has_subquery: bool,
    negated: bool,
    columns: &BTreeMap<String, ColumnId>,
) -> SqlLowering {
    if has_subquery {
        return SqlLowering::Unprunable("IN with a subquery".to_owned());
    }
    let Some(column_id) = column_of(expr, columns) else {
        return SqlLowering::Unprunable("IN over a non-column".to_owned());
    };
    if list.is_empty() {
        // `IN ()` matches nothing; pruning it would require proving
        // the planner treats it as FALSE, which this layer does not
        // know. Stay conservative and scan.
        return SqlLowering::Unprunable("IN with an empty list".to_owned());
    }
    let mut values: Vec<Field> = Vec::with_capacity(list.len());
    for item in list {
        match literal_field(item) {
            Some(field) => values.push(field),
            None => {
                return SqlLowering::Unprunable("IN with a non-literal element".to_owned());
            }
        }
    }
    let mut predicate: Option<PrunePredicate> = None;
    for value in values {
        let equality = PrunePredicate::compare(column_id, PruneOperator::Equal, value);
        predicate = Some(match predicate {
            Some(accumulated) => accumulated.or(equality),
            None => equality,
        });
    }
    let predicate = predicate.expect("non-empty list yields a predicate");
    if negated {
        SqlLowering::Prunable(predicate.not())
    } else {
        SqlLowering::Prunable(predicate)
    }
}

#[cfg(test)]
mod tests {
    use super::{lower_sql_expression, SqlLowering};
    use crate::pruning::predicate::{PruneOperator, PrunePredicate};
    use plomid_core::ColumnId;
    use plomid_storage::Field;
    use std::collections::BTreeMap;

    fn columns() -> BTreeMap<String, ColumnId> {
        BTreeMap::from([
            ("id".to_owned(), ColumnId::new(0)),
            ("name".to_owned(), ColumnId::new(1)),
        ])
    }

    fn col(name: &str) -> Box<plomid_sql::Expression> {
        Box::new(plomid_sql::Expression::ColumnRef(name.to_owned()))
    }

    fn lit(value: plomid_sql::Value) -> Box<plomid_sql::Expression> {
        Box::new(plomid_sql::Expression::Literal(value))
    }

    fn opaque() -> plomid_sql::Expression {
        plomid_sql::Expression::FunctionCall {
            name: "f".to_owned(),
            args: Vec::new(),
            distinct: false,
            filter: None,
            order_by: Vec::new(),
            returning: None,
            null_handling: None,
            unique_keys: None,
        }
    }

    #[test]
    fn comparisons_lower_regardless_of_literal_side() {
        let lowered = lower_sql_expression(
            &plomid_sql::Expression::Equal(col("id"), lit(plomid_sql::Value::Int8(7))),
            &columns(),
        );
        assert_eq!(
            lowered,
            SqlLowering::Prunable(PrunePredicate::eq(ColumnId::new(0), Field::Integer(7)))
        );
        // `7 < id` means `id > 7`: the operator is mirrored.
        let lowered = lower_sql_expression(
            &plomid_sql::Expression::Less(lit(plomid_sql::Value::Int4(7)), col("id")),
            &columns(),
        );
        assert_eq!(
            lowered,
            SqlLowering::Prunable(PrunePredicate::compare(
                ColumnId::new(0),
                PruneOperator::Greater,
                Field::Integer(7),
            ))
        );
    }

    #[test]
    fn unknown_columns_and_unsupported_shapes_do_not_lower() {
        let lowered = lower_sql_expression(
            &plomid_sql::Expression::Equal(col("missing"), lit(plomid_sql::Value::Int8(1))),
            &columns(),
        );
        assert!(!lowered.prunable());
        // A bare column is not a predicate the pruner understands.
        let lowered = lower_sql_expression(
            &plomid_sql::Expression::ColumnRef("id".to_owned()),
            &columns(),
        );
        assert!(!lowered.prunable());
        // Float literals have no storage ordering: scan.
        let lowered = lower_sql_expression(
            &plomid_sql::Expression::Equal(col("id"), lit(plomid_sql::Value::Float8(1.5))),
            &columns(),
        );
        assert!(!lowered.prunable());
        // Function calls are opaque to the pruner.
        let lowered = lower_sql_expression(&opaque(), &columns());
        assert!(!lowered.prunable());
    }

    #[test]
    fn compound_shapes_keep_their_conservative_meaning() {
        let left = plomid_sql::Expression::Equal(col("id"), lit(plomid_sql::Value::Int8(1)));
        let opaque = opaque();
        // `prunable AND opaque` keeps the prunable branch (either branch
        // proving impossibility prunes the conjunction).
        let lowered = lower_sql_expression(
            &plomid_sql::Expression::And(Box::new(left.clone()), Box::new(opaque.clone())),
            &columns(),
        );
        assert!(lowered.prunable());
        // `prunable OR opaque` cannot prune: the opaque branch might match.
        let lowered = lower_sql_expression(
            &plomid_sql::Expression::Or(Box::new(left), Box::new(opaque)),
            &columns(),
        );
        assert!(!lowered.prunable());
    }

    #[test]
    fn between_and_in_lower_to_conjunctions_and_disjunctions() {
        let lowered = lower_sql_expression(
            &plomid_sql::Expression::Between {
                expr: col("id"),
                low: lit(plomid_sql::Value::Int8(1)),
                high: lit(plomid_sql::Value::Int8(9)),
                negated: false,
            },
            &columns(),
        );
        let expected = PrunePredicate::compare(
            ColumnId::new(0),
            PruneOperator::GreaterOrEqual,
            Field::Integer(1),
        )
        .and(PrunePredicate::compare(
            ColumnId::new(0),
            PruneOperator::LessOrEqual,
            Field::Integer(9),
        ));
        assert_eq!(lowered, SqlLowering::Prunable(expected));
        let lowered = lower_sql_expression(
            &plomid_sql::Expression::In {
                expr: col("id"),
                list: vec![
                    plomid_sql::Expression::Literal(plomid_sql::Value::Int8(1)),
                    plomid_sql::Expression::Literal(plomid_sql::Value::Int8(2)),
                ],
                subquery: None,
                negated: false,
            },
            &columns(),
        );
        assert!(lowered.prunable());
    }
}
