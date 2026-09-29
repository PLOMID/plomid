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
//! Narrow column projection for the table scan path.
//!
//! A table row is stored as one self-describing blob (see [`crate::encoding`]):
//! every column carries its own length prefix, so a column a statement never
//! reads can be stepped over without allocating a `String` for its bytes or
//! parsing its stored SQL text into a typed [`plomid_sql::Value`]. Before this
//! module the table scan decoded *every* column of *every* row and then threw
//! most of the decoded values away, which is what made `COUNT(*)`, `SUM(amount)`,
//! `GROUP BY` and `DISTINCT` pay for NUMERIC, DATE, TIMESTAMP and JSON parsing
//! of columns they never looked at.
//!
//! # Shape of this module
//!
//! Reference collection happens in exactly one place — [`requested_indexes`],
//! built on the AST's exhaustive [`Expression::column_refs`] and the private
//! [`collect_target_columns`] walk. The two consumers are thin adapters over it:
//!
//! ```text
//! statement  ──▶ requested_indexes ──▶ ColumnMask          (row scan decode mask)
//!                     │
//!                     └────────────▶ column indexes        (columnar reader)
//! ```
//!
//! # Safety contract
//!
//! A mask may only name columns the statement cannot read outside of, and
//! [`crate::encoding::decode_row_selected`] fills unselected positions with
//! [`plomid_sql::Value::Null`], so a mask that missed a column would silently
//! produce NULL instead of the stored value — a wrong answer, not an error. The
//! analysis therefore fails closed: [`requested_indexes`] returns `None` (and
//! the caller falls back to decoding the whole row) whenever
//!
//! * an expression references a name the table does not have,
//! * a target has a shape this module does not know, or
//! * a construct that carries references of its own scope is present.
//!
//! Subquery references are deliberately not descended into: they belong to the
//! subquery's scope and are executed separately.

use plomid_sql::{Expression, SelectTarget, TableSchema};
use std::collections::BTreeSet;

/// The stored columns one statement can read.
///
/// Positions are stored-column positions, i.e. the same order rows are encoded
/// in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ColumnMask {
    selected: Vec<bool>,
    selected_count: usize,
}

impl ColumnMask {
    /// A mask that selects every column (equivalent to decoding the whole row).
    pub(crate) fn all(width: usize) -> Self {
        Self {
            selected: vec![true; width],
            selected_count: width,
        }
    }

    fn from_indexes(width: usize, indexes: BTreeSet<usize>) -> Self {
        let mut selected = vec![false; width];
        let mut selected_count = 0;
        for index in indexes {
            if index < width && !selected[index] {
                selected[index] = true;
                selected_count += 1;
            }
        }
        Self {
            selected,
            selected_count,
        }
    }

    /// Selection flags in stored-column order, as consumed by
    /// [`crate::encoding::decode_row_selected`].
    pub(crate) fn selected(&self) -> &[bool] {
        &self.selected
    }

    /// True when the statement reads no stored column value at all — the
    /// `COUNT(*)` shape with no predicate. Such a statement never needs to
    /// decode a row; it only needs to know how many visible rows there are.
    pub(crate) fn reads_no_column(&self) -> bool {
        self.selected_count == 0
    }
}

/// The stored columns a whole `SELECT` may read, as a decode mask for the row
/// scan, or `None` when that set cannot be proven (see the module contract).
///
/// Every clause of the statement is passed in, because all of them are
/// evaluated over the source row: the projection, the `WHERE` predicate, the
/// grouping sets, `HAVING`, the `ORDER BY` keys and the `DISTINCT ON` keys.
#[allow(clippy::too_many_arguments)]
pub(crate) fn required_columns(
    schema: &TableSchema,
    targets: &[SelectTarget],
    where_expr: Option<&Expression>,
    group_by: Option<&plomid_sql::GroupByClause>,
    having: Option<&Expression>,
    order_by: &[plomid_sql::OrderByItem],
    distinct_on: &[Expression],
) -> Option<ColumnMask> {
    // Every grouping set is evaluated over the source row, not only their union.
    let grouping_sets = group_by.map(|clause| clause.to_sets()).unwrap_or_default();
    let expressions = where_expr
        .into_iter()
        .chain(grouping_sets.iter().flatten())
        .chain(having)
        .chain(order_by.iter().map(|item| &item.expr))
        .chain(distinct_on.iter());
    requested_indexes(schema, expressions, targets)
        .map(|indexes| ColumnMask::from_indexes(schema.columns.len(), indexes))
}

/// The stored columns the given expressions and projection targets read.
///
/// Returns `None` when any reference cannot be resolved against `schema`, which
/// is what makes the whole analysis fail closed. This is the single place
/// column references are collected from; every other function in this module
/// either feeds it or adapts its result.
pub(crate) fn requested_indexes<'e>(
    schema: &TableSchema,
    expressions: impl IntoIterator<Item = &'e Expression>,
    targets: &[SelectTarget],
) -> Option<BTreeSet<usize>> {
    let mut indexes = BTreeSet::new();
    for expression in expressions {
        if !collect_columns(expression, schema, &mut indexes) {
            return None;
        }
    }
    for target in targets {
        if !collect_target_columns(target, schema, &mut indexes) {
            return None;
        }
    }
    Some(indexes)
}

/// True when a projection target's value is produced from stored columns alone.
///
/// Session functions (`SELECT current_timestamp`), window functions and the
/// aggregate/session target shapes are not plain projections: a consumer that
/// can only serve stored columns must refuse them rather than read a column set
/// that does not describe their output.
pub(crate) fn target_is_plain_projection(target: &SelectTarget) -> bool {
    match target {
        SelectTarget::All | SelectTarget::QualifiedStar { .. } | SelectTarget::Expr { .. } => true,
        SelectTarget::Aliased { target, .. } => target_is_plain_projection(target.as_ref()),
        SelectTarget::Function(_)
        | SelectTarget::FunctionCall { .. }
        | SelectTarget::WindowFunction { .. } => false,
    }
}

/// Collects the stored columns one projection target reads.
///
/// The match is exhaustive with no wildcard arm, so a new [`SelectTarget`]
/// variant is a compile error here rather than a silently missed column set.
///
/// `SelectTarget::Function` is a zero-argument session function
/// (`SELECT current_timestamp`), which reads no stored column.
fn collect_target_columns(
    target: &SelectTarget,
    schema: &TableSchema,
    indexes: &mut BTreeSet<usize>,
) -> bool {
    match target {
        SelectTarget::All | SelectTarget::QualifiedStar { .. } => {
            indexes.extend(0..schema.columns.len());
            true
        }
        SelectTarget::Expr { expr, .. } => collect_columns(expr, schema, indexes),
        SelectTarget::Aliased { target, .. } => {
            collect_target_columns(target.as_ref(), schema, indexes)
        }
        SelectTarget::Function(_) => true,
        SelectTarget::FunctionCall { args, .. } => {
            args.iter().all(|arg| collect_columns(arg, schema, indexes))
        }
        SelectTarget::WindowFunction { .. } => {
            // Window functions are executed by the general engine, but if one
            // reaches this path its frame/partition references are not
            // accounted for here. Refuse rather than guess.
            false
        }
    }
}

/// Adds every stored column `expr` reads to `indexes`.
///
/// Returns `false` when the expression reads a name the table does not have, so
/// the caller refuses projection. The walk is the AST's own exhaustive
/// [`Expression::column_refs`], which does not descend into subqueries.
fn collect_columns(expr: &Expression, schema: &TableSchema, indexes: &mut BTreeSet<usize>) -> bool {
    let mut names = Vec::new();
    expr.column_refs(&mut names);
    for name in names {
        match schema.column_index(crate::util::unqualify(&name)) {
            Ok(index) => {
                indexes.insert(index);
            }
            Err(_) => return false,
        }
    }
    true
}

/// True when the projection is exactly one bare `count(*)`.
///
/// "Bare" excludes the shapes that carry their own argument (`count(expr)`),
/// `DISTINCT` (`count(DISTINCT expr)`) and `FILTER (WHERE ...)`, because those
/// must still evaluate their input per row. `count(*)` with no argument reads no
/// column, so a statement whose entire projection is a bare `count(*)` and which
/// has no predicate can be answered by counting visible rows.
///
/// The match is exhaustive with no wildcard arm.
#[must_use]
pub(crate) fn is_bare_count_star(targets: &[SelectTarget]) -> bool {
    let [target] = targets else {
        return false;
    };
    // The parser produces `SELECT COUNT(*)` directly as a `FunctionCall`
    // target (not wrapped in `Expr`); both shapes must reach the keys-only
    // fast path, or the most common production COUNT materializes every row.
    if let SelectTarget::FunctionCall { name, args } = target {
        return name.eq_ignore_ascii_case("count") && matches!(args.as_slice(), [Expression::Star]);
    }
    let expr = match target {
        SelectTarget::Expr { expr, .. } => expr,
        SelectTarget::Aliased { target, .. } => match target.as_ref() {
            SelectTarget::Expr { expr, .. } => expr,
            _ => return false,
        },
        _ => return false,
    };
    match expr {
        Expression::FunctionCall {
            name,
            args,
            distinct,
            filter,
            order_by,
            ..
        } => {
            name.eq_ignore_ascii_case("count")
                && !*distinct
                && filter.is_none()
                && order_by.is_empty()
                && matches!(args.as_slice(), [Expression::Star])
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::encode_row;
    use plomid_sql::{Catalog, ColumnDef, ColumnType, InMemoryCatalog, Value};

    /// Builds `public.t(id, amount, flag)` through the catalog, so the schema
    /// under test is exactly the one the executor sees at runtime.
    fn schema() -> TableSchema {
        let mut catalog = InMemoryCatalog::new();
        let column = |name: &str| ColumnDef {
            name: name.to_string(),
            col_type: ColumnType::int4(),
            constraints: Vec::new(),
        };
        catalog
            .create_table(
                "public.t".to_string(),
                vec![column("id"), column("amount"), column("flag")],
                Vec::new(),
            )
            .expect("create table");
        catalog.get_table("public.t").expect("get table").clone()
    }

    fn function_call(name: &str, args: Vec<Expression>) -> SelectTarget {
        SelectTarget::Expr {
            expr: Expression::FunctionCall {
                name: name.into(),
                args,
                distinct: false,
                filter: None,
                order_by: Vec::new(),
                returning: None,
                null_handling: None,
                unique_keys: None,
            },
            alias: None,
        }
    }

    fn count_star_target() -> SelectTarget {
        function_call("count", vec![Expression::Star])
    }

    fn mask_for(targets: &[SelectTarget], where_expr: Option<&Expression>) -> Option<ColumnMask> {
        let schema = schema();
        required_columns(&schema, targets, where_expr, None, None, &[], &[])
    }

    fn indexes_for(targets: &[SelectTarget]) -> Option<BTreeSet<usize>> {
        let schema = schema();
        requested_indexes(&schema, std::iter::empty(), targets)
    }

    #[test]
    fn count_star_reads_no_column() {
        let targets = vec![count_star_target()];
        let mask = mask_for(&targets, None).expect("count(*) is projectable");
        assert!(mask.reads_no_column());
        assert_eq!(indexes_for(&targets), Some(BTreeSet::new()));
        assert!(is_bare_count_star(&targets));
    }

    #[test]
    fn bare_function_call_count_star_is_bare() {
        // The parser produces `SELECT COUNT(*)` directly as a `FunctionCall`
        // target. Without this shape the most common production COUNT misses
        // the keys-only fast path and materializes every row.
        let direct = SelectTarget::FunctionCall {
            name: "count".into(),
            args: vec![Expression::Star],
        };
        assert!(is_bare_count_star(&[direct]));
        let other = SelectTarget::FunctionCall {
            name: "count".into(),
            args: vec![Expression::Literal(Value::Null)],
        };
        assert!(!is_bare_count_star(&[other]));
        let sum = SelectTarget::FunctionCall {
            name: "sum".into(),
            args: vec![Expression::Star],
        };
        assert!(!is_bare_count_star(&[sum]));
    }

    #[test]
    fn count_star_with_filter_is_not_bare() {
        let mut target = count_star_target();
        if let SelectTarget::Expr { expr, .. } = &mut target {
            if let Expression::FunctionCall { filter, .. } = expr {
                *filter = Some(Box::new(Expression::IsNull(Box::new(
                    Expression::ColumnRef("flag".into()),
                ))));
            }
        }
        assert!(!is_bare_count_star(&[target]));
    }

    #[test]
    fn count_of_a_column_is_not_bare() {
        // `count(NULL)` reads no column but must count zero, so the bare shape
        // is required before a row count may answer the statement.
        let targets = vec![function_call(
            "count",
            vec![Expression::Literal(Value::Null)],
        )];
        assert!(!is_bare_count_star(&targets));
        let mask = mask_for(&targets, None).expect("count(NULL) is projectable");
        assert!(mask.reads_no_column());
    }

    #[test]
    fn sum_reads_only_its_argument() {
        let targets = vec![function_call(
            "sum",
            vec![Expression::ColumnRef("amount".into())],
        )];
        assert_eq!(indexes_for(&targets), Some(BTreeSet::from([1])));
        let mask = mask_for(&targets, None).expect("sum(amount) is projectable");
        assert_eq!(mask.selected(), [false, true, false]);
    }

    #[test]
    fn predicate_columns_are_included() {
        let targets = vec![SelectTarget::All];
        let where_expr = Expression::IsNull(Box::new(Expression::ColumnRef("flag".into())));
        let mask = mask_for(&targets, Some(&where_expr)).expect("select * is projectable");
        // `SELECT *` needs every column regardless of the predicate.
        assert_eq!(mask.selected(), [true, true, true]);
    }

    #[test]
    fn predicate_narrows_a_narrow_projection() {
        let targets = vec![count_star_target()];
        let where_expr = Expression::IsNull(Box::new(Expression::ColumnRef("flag".into())));
        let mask = mask_for(&targets, Some(&where_expr)).expect("count(*) where flag is null");
        assert_eq!(mask.selected(), [false, false, true]);
        assert!(!mask.reads_no_column());
    }

    #[test]
    fn order_and_group_keys_are_included() {
        let schema = schema();
        let targets = vec![count_star_target()];
        let order_by = vec![plomid_sql::OrderByItem {
            expr: Expression::ColumnRef("amount".into()),
            descending: false,
            nulls_first: None,
        }];
        let group_by =
            plomid_sql::GroupByClause::Simple(vec![Expression::ColumnRef("flag".into())]);
        let mask = required_columns(
            &schema,
            &targets,
            None,
            Some(&group_by),
            None,
            &order_by,
            &[],
        )
        .expect("group/order keys are projectable");
        assert_eq!(mask.selected(), [false, true, true]);
    }

    #[test]
    fn unknown_column_refuses_projection() {
        let targets = vec![SelectTarget::Expr {
            expr: Expression::ColumnRef("missing".into()),
            alias: None,
        }];
        assert!(mask_for(&targets, None).is_none());
        assert!(indexes_for(&targets).is_none());
    }

    #[test]
    fn qualified_column_refs_resolve() {
        let targets = vec![function_call(
            "sum",
            vec![Expression::ColumnRef("public.t.amount".into())],
        )];
        assert_eq!(indexes_for(&targets), Some(BTreeSet::from([1])));
    }

    #[test]
    fn window_function_refuses_projection() {
        let targets = vec![SelectTarget::WindowFunction {
            name: "row_number".into(),
            args: Vec::new(),
            over: plomid_sql::WindowSpec::default(),
        }];
        assert!(mask_for(&targets, None).is_none());
        assert!(indexes_for(&targets).is_none());
    }

    #[test]
    fn plain_projection_admits_stored_columns_only() {
        assert!(target_is_plain_projection(&SelectTarget::All));
        assert!(target_is_plain_projection(&count_star_target()));
        assert!(target_is_plain_projection(&SelectTarget::Aliased {
            target: Box::new(SelectTarget::All),
            alias: "every".into(),
        }));
        assert!(!target_is_plain_projection(&SelectTarget::WindowFunction {
            name: "row_number".into(),
            args: Vec::new(),
            over: plomid_sql::WindowSpec::default(),
        }));
    }

    #[test]
    fn unselected_positions_decode_to_null() {
        // The mask contract: unselected positions are NULL placeholders that
        // the caller has proven unreachable. Decoding must still step over the
        // real bytes so the walk stays aligned with the stored row.
        let row =
            encode_row(&[Value::Int8(7), Value::Int8(1234), Value::Bool(true)]).expect("encode");
        let values = crate::encoding::decode_row_selected(&row, &[false, true, false])
            .expect("partial decode");
        assert_eq!(values[0], Value::Null);
        assert_eq!(values[1], Value::Int8(1234));
        assert_eq!(values[2], Value::Null);

        let all = crate::encoding::decode_row(&row).expect("full decode");
        assert_eq!(
            all,
            vec![Value::Int8(7), Value::Int8(1234), Value::Bool(true)]
        );
    }

    #[test]
    fn validate_row_accepts_well_formed_rows() {
        let row = encode_row(&[Value::Null, Value::Bool(false)]).expect("encode");
        crate::encoding::validate_row(&row).expect("well formed");
        assert!(crate::encoding::validate_row(&[]).is_err());
        assert!(crate::encoding::validate_row(&[0xff, 1, 0]).is_err());
    }

    #[test]
    fn width_mismatch_falls_back_to_full_decode() {
        let row = encode_row(&[Value::Int8(1), Value::Int8(2)]).expect("encode");
        let values = crate::encoding::decode_row_selected(&row, &[false]).expect("fallback");
        assert_eq!(values, vec![Value::Int8(1), Value::Int8(2)]);
    }
}
