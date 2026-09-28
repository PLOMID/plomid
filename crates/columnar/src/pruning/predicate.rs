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
//! Safe metadata evaluation of predicates against zone maps.
//!
//! # The correctness rule
//!
//! Every function in this module answers one question: *can the metadata
//! prove that no row in a range satisfies the predicate?* A `PRUNE` answer
//! must be backed by a proof; anything less — missing bounds, an unsupported
//! literal, incomparable types, a predicate over a column the metadata does
//! not describe — answers `KEEP`/`UNKNOWN`, and the caller scans.
//!
//! # SQL three-valued logic
//!
//! Range comparisons (`= < <= > >= !=`) are never TRUE for NULL rows, so a
//! NULL row can never be the reason a `PRUNE` was wrong: pruning reasons
//! only ever talk about non-NULL rows. `IS NULL` / `IS NOT NULL` reason
//! directly about the NULL state.
//!
//! # Operator proofs (bounds `min`/`max` over non-NULL rows, `v` literal)
//!
//! With rows confined to `[min, max]`:
//!
//! * `col = v`  prunes iff `v < min` or `v > max`
//! * `col < v`  prunes iff `min >= v`
//! * `col <= v` prunes iff `min >  v`
//! * `col > v`  prunes iff `max <= v`
//! * `col >= v` prunes iff `max <  v`
//! * `col != v` **never** prunes on bounds (min=1, max=10 says nothing about
//!   whether 5 occurs); only an all-NULL range prunes
//! * `IS NULL`     prunes iff the range holds no NULLs
//! * `IS NOT NULL` prunes iff every row is NULL
//!
//! Comparisons never do arithmetic on values, so there is no overflow: the
//! `i64::MIN`/`i64::MAX` boundaries fall out of plain `Ord` comparisons.
//!
//! # Compound predicates
//!
//! `A AND B` prunes when *either* branch proves impossibility; `A OR B`
//! prunes only when *both* branches do; `NOT` is handled by pushing it down
//! through safe operator duals (`NOT (col < v)` becomes `col >= v`, and so
//! on) — no Boolean simplification beyond De Morgan's law on the proof
//! structure.

use super::verdict::PruneVerdict;
use super::zonemap::{NullState, ZoneMap};
use crate::statistics::compare_fields;
use plomid_core::ColumnId;
use plomid_storage::Field;
use std::cmp::Ordering;
use std::fmt;

/// Range comparison operators the pruning layer can evaluate safely.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum PruneOperator {
    /// `column = value`.
    Equal,
    /// `column != value` (never prunes on min/max alone).
    NotEqual,
    /// `column < value`.
    Less,
    /// `column <= value`.
    LessOrEqual,
    /// `column > value`.
    Greater,
    /// `column >= value`.
    GreaterOrEqual,
    /// `column IS NULL`.
    IsNull,
    /// `column IS NOT NULL`.
    IsNotNull,
}

impl PruneOperator {
    /// Returns the operator dual under `NOT`, when a safe dual exists.
    ///
    /// Every supported operator has a dual that evaluates `NOT (col OP v)`
    /// soundly. The duals inherit three-valued logic: negating a comparison
    /// is still NULL (not TRUE) for NULL rows, exactly like the dual
    /// operator.
    #[must_use]
    pub fn negate(self) -> Self {
        match self {
            Self::Equal => Self::NotEqual,
            Self::NotEqual => Self::Equal,
            Self::Less => Self::GreaterOrEqual,
            Self::LessOrEqual => Self::Greater,
            Self::Greater => Self::LessOrEqual,
            Self::GreaterOrEqual => Self::Less,
            Self::IsNull => Self::IsNotNull,
            Self::IsNotNull => Self::IsNull,
        }
    }
}

impl fmt::Display for PruneOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Equal => "=",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::Greater => ">",
            Self::GreaterOrEqual => ">=",
            Self::IsNull => "IS NULL",
            Self::IsNotNull => "IS NOT NULL",
        };
        write!(f, "{text}")
    }
}

/// A pruning predicate over `(column, operator, literal)` leaves plus
/// `AND`/`OR`/`NOT`.
///
/// This is deliberately *not* a second SQL AST: it is the small model the
/// pruning layer can reason about. Anything the SQL layer expresses beyond
/// this shape simply does not bridge, and the caller keeps the range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrunePredicate {
    /// `column OP literal`; `literal` is `None` only for
    /// `IS NULL`/`IS NOT NULL`.
    Compare {
        /// Column the predicate filters.
        column_id: ColumnId,
        /// Comparison to evaluate.
        operator: PruneOperator,
        /// Literal in the column's own [`Field`] representation.
        literal: Option<Field>,
    },
    /// `left AND right`.
    And(Box<PrunePredicate>, Box<PrunePredicate>),
    /// `left OR right`.
    Or(Box<PrunePredicate>, Box<PrunePredicate>),
    /// `NOT inner` (evaluated through safe operator duals).
    Not(Box<PrunePredicate>),
}

impl PrunePredicate {
    /// Builds `column = value`.
    #[must_use]
    pub fn eq(column_id: ColumnId, literal: Field) -> Self {
        Self::Compare {
            column_id,
            operator: PruneOperator::Equal,
            literal: Some(literal),
        }
    }

    /// Builds `column != value`.
    #[must_use]
    pub fn ne(column_id: ColumnId, literal: Field) -> Self {
        Self::Compare {
            column_id,
            operator: PruneOperator::NotEqual,
            literal: Some(literal),
        }
    }

    /// Builds `column IS NULL`.
    #[must_use]
    pub fn is_null(column_id: ColumnId) -> Self {
        Self::Compare {
            column_id,
            operator: PruneOperator::IsNull,
            literal: None,
        }
    }

    /// Builds `column IS NOT NULL`.
    #[must_use]
    pub fn is_not_null(column_id: ColumnId) -> Self {
        Self::Compare {
            column_id,
            operator: PruneOperator::IsNotNull,
            literal: None,
        }
    }

    /// Builds `column OP value` for an arbitrary operator.
    #[must_use]
    pub fn compare(column_id: ColumnId, operator: PruneOperator, literal: Field) -> Self {
        Self::Compare {
            column_id,
            operator,
            literal: Some(literal),
        }
    }

    /// Conjoins two predicates.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        Self::And(Box::new(self), Box::new(other))
    }

    /// Disjoins two predicates.
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self::Or(Box::new(self), Box::new(other))
    }

    /// Negates a predicate (safe duals only).
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Self {
        Self::Not(Box::new(self))
    }
}

/// Collects the column identities a predicate constrains, in order of first
/// appearance. Used to drive per-column pruning decisions.
#[must_use]
pub fn predicate_columns(predicate: &PrunePredicate) -> Vec<ColumnId> {
    fn collect(predicate: &PrunePredicate, out: &mut Vec<ColumnId>) {
        match predicate {
            PrunePredicate::Compare { column_id, .. } => {
                if !out.contains(column_id) {
                    out.push(*column_id);
                }
            }
            PrunePredicate::And(left, right) | PrunePredicate::Or(left, right) => {
                collect(left, out);
                collect(right, out);
            }
            PrunePredicate::Not(inner) => collect(inner, out),
        }
    }
    let mut out = Vec::new();
    collect(predicate, &mut out);
    out
}

/// Pushes `NOT` inside a predicate using safe operator duals; returns `None`
/// only when the shape has no dual (the caller must then keep the range).
#[must_use]
pub fn negate_predicate(predicate: &PrunePredicate) -> Option<PrunePredicate> {
    match predicate {
        PrunePredicate::Compare {
            column_id,
            operator,
            literal,
        } => Some(PrunePredicate::Compare {
            column_id: *column_id,
            operator: operator.negate(),
            literal: literal.clone(),
        }),
        // De Morgan, applied to the proof structure only.
        PrunePredicate::And(left, right) => Some(PrunePredicate::Or(
            Box::new(negate_predicate(left)?),
            Box::new(negate_predicate(right)?),
        )),
        PrunePredicate::Or(left, right) => Some(PrunePredicate::And(
            Box::new(negate_predicate(left)?),
            Box::new(negate_predicate(right)?),
        )),
        PrunePredicate::Not(inner) => Some((**inner).clone()),
    }
}

/// Returns true when the zone maps of one column *cover* `[start, end)`.
///
/// Coverage is the guard that makes per-column pruning sound: a column may
/// only prove anything about an extent when every row of the extent lies in
/// some zone map of that column. Uncovered rows have no evidence, so an
/// uncovered extent must never be pruned (this defends against metadata
/// whose zones do not align with the extent being pruned, including
/// hand-crafted trailer bytes).
pub fn zones_cover_extent(zones: &[&ZoneMap], start: u64, end: u64) -> bool {
    if start >= end {
        return true;
    }
    let mut sorted: Vec<&ZoneMap> = zones
        .iter()
        .copied()
        .filter(|zone| zone.row_count > 0)
        .collect();
    sorted.sort_by_key(|zone| zone.start_row);
    let mut expected = start;
    for zone in sorted {
        if zone.start_row > expected {
            return false; // gap: rows [expected, zone.start_row) uncovered
        }
        expected = expected.max(zone.end_row());
        if expected >= end {
            return true;
        }
    }
    expected >= end
}

/// Prunes `[start_row, end_row)` using the zone maps of a set of columns.
///
/// Per-column rule (the soundness core at extent granularity):
///
/// 1. For each column the predicate constrains, gather that column's zone
///    maps overlapping the extent.
/// 2. A column can prove impossibility only when its overlapping zones
///    *cover* the whole extent (see [`zones_cover_extent`]) **and** every
///    one of them evaluates to `Prune`.
/// 3. The extent prunes when at least one column proves impossibility.
///    Otherwise it keeps, and the verdict distinguishes Keep (there was
///    applicable metadata) from Unknown (no evidence at all) — both mean
///    "scan".
pub fn prune_extent_with_zones(
    zones: &[ZoneMap],
    predicate: &PrunePredicate,
    start_row: u64,
    end_row: u64,
) -> PruneVerdict {
    if start_row >= end_row {
        // An empty extent holds no rows; nothing can match it.
        return PruneVerdict::Prune;
    }
    let mut saw_evidence = false;
    for column_id in predicate_columns(predicate) {
        let overlapping: Vec<&ZoneMap> = zones
            .iter()
            .filter(|zone| zone.column_id == column_id && zone.overlaps(start_row, end_row))
            .collect();
        if overlapping.is_empty() {
            continue; // no evidence for this column
        }
        saw_evidence = true;
        if zones_cover_extent(&overlapping, start_row, end_row)
            && overlapping
                .iter()
                .all(|zone| zone.evaluate(predicate) == PruneVerdict::Prune)
        {
            return PruneVerdict::Prune;
        }
    }
    if saw_evidence {
        PruneVerdict::Keep
    } else {
        PruneVerdict::Unknown
    }
}

/// SQL three-valued result of evaluating a predicate against one row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tri {
    /// The predicate is TRUE for this row.
    True,
    /// The predicate is FALSE for this row.
    False,
    /// The predicate is NULL (a required value is NULL, or the operands are
    /// incomparable): SQL treats this as "not in the result set".
    Null,
}

/// Evaluates a predicate against one row's field lookup with SQL
/// three-valued logic.
///
/// `field_of(column_id)` returns the row's value for a column (`None` when
/// the row does not carry that column, treated as NULL). This is the
/// ground-truth semantics the property tests compare pruning against: a
/// range may be pruned only when no row in it evaluates [`Tri::True`].
pub fn row_matches<F>(predicate: &PrunePredicate, field_of: &F) -> Tri
where
    F: Fn(ColumnId) -> Option<Field>,
{
    match predicate {
        PrunePredicate::Compare {
            column_id,
            operator,
            literal,
        } => {
            let value = field_of(*column_id);
            match operator {
                PruneOperator::IsNull => match value {
                    None => Tri::Null,
                    Some(Field::Null) => Tri::True,
                    Some(_) => Tri::False,
                },
                PruneOperator::IsNotNull => match value {
                    None => Tri::Null,
                    Some(Field::Null) => Tri::False,
                    Some(_) => Tri::True,
                },
                _ => {
                    let Some(literal) = literal else {
                        return Tri::Null;
                    };
                    if matches!(literal, Field::Null) {
                        // `col = NULL` is NULL, never TRUE.
                        return Tri::Null;
                    }
                    match value {
                        None | Some(Field::Null) => Tri::Null,
                        Some(value) => match compare_fields(&value, literal) {
                            None => Tri::Null,
                            Some(ordering) => {
                                let is_true = match operator {
                                    PruneOperator::Equal => ordering == Ordering::Equal,
                                    PruneOperator::NotEqual => ordering != Ordering::Equal,
                                    PruneOperator::Less => ordering == Ordering::Less,
                                    PruneOperator::LessOrEqual => ordering != Ordering::Greater,
                                    PruneOperator::Greater => ordering == Ordering::Greater,
                                    PruneOperator::GreaterOrEqual => ordering != Ordering::Less,
                                    PruneOperator::IsNull | PruneOperator::IsNotNull => false,
                                };
                                if is_true {
                                    Tri::True
                                } else {
                                    Tri::False
                                }
                            }
                        },
                    }
                }
            }
        }
        // AND: any FALSE wins (even over NULLs); TRUE requires both TRUE.
        PrunePredicate::And(left, right) => {
            let l = row_matches(left, field_of);
            let r = row_matches(right, field_of);
            if matches!(l, Tri::False) || matches!(r, Tri::False) {
                Tri::False
            } else if matches!(l, Tri::True) && matches!(r, Tri::True) {
                Tri::True
            } else {
                Tri::Null
            }
        }
        // OR: any TRUE wins; FALSE requires both FALSE; else NULL.
        PrunePredicate::Or(left, right) => {
            let l = row_matches(left, field_of);
            let r = row_matches(right, field_of);
            if matches!(l, Tri::True) || matches!(r, Tri::True) {
                Tri::True
            } else if matches!(l, Tri::False) && matches!(r, Tri::False) {
                Tri::False
            } else {
                Tri::Null
            }
        }
        // NOT TRUE → FALSE, NOT FALSE → TRUE, NOT NULL → NULL.
        PrunePredicate::Not(inner) => match row_matches(inner, field_of) {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Null => Tri::Null,
        },
    }
}

/// Evaluates one leaf predicate against one zone map.
///
/// Soundness per operator follows the module-level proof table. The verdict
/// for a foreign column is `Unknown`: this zone map simply says nothing
/// about a predicate over another column.
fn evaluate_leaf(
    zone: &ZoneMap,
    column_id: ColumnId,
    operator: PruneOperator,
    literal: Option<&Field>,
) -> PruneVerdict {
    if zone.column_id != column_id {
        return PruneVerdict::Unknown;
    }
    match operator {
        PruneOperator::IsNull => {
            if zone.row_count == 0 {
                return PruneVerdict::Prune;
            }
            match zone.null_state {
                NullState::NoNulls => PruneVerdict::Prune,
                NullState::HasNulls | NullState::AllNulls => PruneVerdict::Keep,
            }
        }
        PruneOperator::IsNotNull => {
            if zone.row_count == 0 {
                return PruneVerdict::Prune;
            }
            match zone.null_state {
                NullState::AllNulls => PruneVerdict::Prune,
                NullState::NoNulls | NullState::HasNulls => PruneVerdict::Keep,
            }
        }
        PruneOperator::NotEqual => {
            // min/max can never prove absence for `!=` (min=1, max=10 says
            // nothing about whether 5 occurs). Only ranges with no non-NULL
            // row at all are provably empty.
            if zone.row_count == 0 {
                return PruneVerdict::Prune;
            }
            match zone.null_state {
                NullState::AllNulls => PruneVerdict::Prune,
                NullState::NoNulls | NullState::HasNulls => PruneVerdict::Keep,
            }
        }
        PruneOperator::Equal
        | PruneOperator::Less
        | PruneOperator::LessOrEqual
        | PruneOperator::Greater
        | PruneOperator::GreaterOrEqual => {
            let Some(literal) = literal else {
                return PruneVerdict::Unknown;
            };
            // `col = NULL` (and friends) is never TRUE under SQL semantics.
            if matches!(literal, Field::Null) {
                return PruneVerdict::Prune;
            }
            let (Some(min), Some(max)) = (&zone.min, &zone.max) else {
                // Without bounds only a value-free range can be proven.
                if zone.row_count == 0 {
                    return PruneVerdict::Prune;
                }
                return match zone.null_state {
                    NullState::AllNulls => PruneVerdict::Prune,
                    _ => PruneVerdict::Unknown,
                };
            };
            // `cmp_*` compares the literal against the stored bound. An
            // incomparable literal type cannot reason: keep.
            let (Some(cmp_min), Some(cmp_max)) =
                (compare_fields(literal, min), compare_fields(literal, max))
            else {
                return PruneVerdict::Unknown;
            };
            // The proof table from the module docs. Comparisons only — no
            // arithmetic, so extreme values are safe.
            let proven_empty = match operator {
                PruneOperator::Equal => cmp_min == Ordering::Less || cmp_max == Ordering::Greater,
                PruneOperator::Less => cmp_min != Ordering::Greater,
                PruneOperator::LessOrEqual => cmp_min == Ordering::Less,
                PruneOperator::Greater => cmp_max != Ordering::Less,
                PruneOperator::GreaterOrEqual => cmp_max == Ordering::Greater,
                _ => return PruneVerdict::Unknown,
            };
            if proven_empty {
                PruneVerdict::Prune
            } else {
                PruneVerdict::Keep
            }
        }
    }
}

/// Evaluates a full predicate against one zone map.
///
/// `AND` prunes when either branch proves impossibility; `OR` prunes only
/// when both do; `NOT` is pushed down through safe duals. Unknown branches
/// degrade the verdict to Unknown/Keep — they never upgrade it to Prune.
pub fn evaluate_on_zone(zone: &ZoneMap, predicate: &PrunePredicate) -> PruneVerdict {
    match predicate {
        PrunePredicate::Compare {
            column_id,
            operator,
            literal,
        } => evaluate_leaf(zone, *column_id, *operator, literal.as_ref()),
        PrunePredicate::And(left, right) => {
            match (evaluate_on_zone(zone, left), evaluate_on_zone(zone, right)) {
                (PruneVerdict::Prune, _) | (_, PruneVerdict::Prune) => PruneVerdict::Prune,
                (PruneVerdict::Keep, PruneVerdict::Keep) => PruneVerdict::Keep,
                _ => PruneVerdict::Unknown,
            }
        }
        PrunePredicate::Or(left, right) => {
            match (evaluate_on_zone(zone, left), evaluate_on_zone(zone, right)) {
                (PruneVerdict::Prune, PruneVerdict::Prune) => PruneVerdict::Prune,
                (PruneVerdict::Keep, _) | (_, PruneVerdict::Keep) => PruneVerdict::Keep,
                _ => PruneVerdict::Unknown,
            }
        }
        PrunePredicate::Not(inner) => evaluate_not_on_zone(zone, inner),
    }
}

/// Evaluates `NOT inner` against one zone map.
///
/// Pushes the negation inside through safe duals (see
/// [`PruneOperator::negate`] and `negate_predicate`) and reuses the plain
/// evaluator on the pushed-down shape. `NOT NOT x` collapses to `x`.
pub fn evaluate_not_on_zone(zone: &ZoneMap, inner: &PrunePredicate) -> PruneVerdict {
    match inner {
        PrunePredicate::Compare {
            column_id,
            operator,
            literal,
        } => evaluate_leaf(zone, *column_id, operator.negate(), literal.as_ref()),
        // NOT (A AND B) == NOT A OR NOT B: prune only when both duals prune.
        PrunePredicate::And(left, right) => match (
            evaluate_not_on_zone(zone, left),
            evaluate_not_on_zone(zone, right),
        ) {
            (PruneVerdict::Prune, PruneVerdict::Prune) => PruneVerdict::Prune,
            (PruneVerdict::Keep, _) | (_, PruneVerdict::Keep) => PruneVerdict::Keep,
            _ => PruneVerdict::Unknown,
        },
        // NOT (A OR B) == NOT A AND NOT B: prune when either dual prunes.
        PrunePredicate::Or(left, right) => match (
            evaluate_not_on_zone(zone, left),
            evaluate_not_on_zone(zone, right),
        ) {
            (PruneVerdict::Prune, _) | (_, PruneVerdict::Prune) => PruneVerdict::Prune,
            (PruneVerdict::Keep, PruneVerdict::Keep) => PruneVerdict::Keep,
            _ => PruneVerdict::Unknown,
        },
        PrunePredicate::Not(inner) => evaluate_on_zone(zone, inner),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pruning::zonemap::{ZoneMap, ZoneMapBuilder};

    fn col() -> ColumnId {
        ColumnId::new(3)
    }

    /// Builds one zone map over `values` (row `i` is physical row `i`).
    fn zone_of(values: &[Option<i64>]) -> ZoneMap {
        let mut builder = ZoneMapBuilder::new(col(), crate::column::ColumnType::Integer);
        for (row, value) in values.iter().enumerate() {
            builder.observe(row as u64, value.map(Field::Integer));
        }
        builder.finish()
    }

    fn verdict(values: &[Option<i64>], predicate: &PrunePredicate) -> PruneVerdict {
        zone_of(values).evaluate(predicate)
    }

    fn cmp(operator: PruneOperator, literal: i64) -> PrunePredicate {
        PrunePredicate::compare(col(), operator, Field::Integer(literal))
    }

    /// Ground truth: does any row of `values` satisfy `predicate`?
    fn any_row_matches(values: &[Option<i64>], predicate: &PrunePredicate) -> bool {
        values
            .iter()
            .any(|value| row_matches(predicate, &|_| value.map(Field::Integer)) == Tri::True)
    }

    /// The invariant every prune must satisfy, checked against a full scan.
    fn assert_no_false_negative(values: &[Option<i64>], predicate: &PrunePredicate, label: &str) {
        let verdict = verdict(values, predicate);
        if verdict == PruneVerdict::Prune {
            assert!(
                !any_row_matches(values, predicate),
                "false negative ({label}): pruned {values:?} for {predicate:?} but a row matches"
            );
        }
    }

    #[test]
    fn equal_prunes_only_outside_the_closed_bounds() {
        let values = [Some(10), Some(20), Some(30)];
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Equal, 9)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Equal, 10)),
            PruneVerdict::Keep
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Equal, 20)),
            PruneVerdict::Keep
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Equal, 30)),
            PruneVerdict::Keep
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Equal, 31)),
            PruneVerdict::Prune
        );
    }

    #[test]
    fn less_and_less_or_equal_respect_the_min_boundary() {
        let values = [Some(10), Some(20), Some(30)];
        // min == literal: `< 10` cannot match, `<= 10` can.
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Less, 10)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::LessOrEqual, 10)),
            PruneVerdict::Keep
        );
        // just below the minimum prunes both.
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Less, 9)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::LessOrEqual, 9)),
            PruneVerdict::Prune
        );
        // just above the minimum keeps.
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Less, 11)),
            PruneVerdict::Keep
        );
    }

    #[test]
    fn greater_and_greater_or_equal_respect_the_max_boundary() {
        let values = [Some(10), Some(20), Some(30)];
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Greater, 30)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::GreaterOrEqual, 30)),
            PruneVerdict::Keep
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Greater, 31)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::GreaterOrEqual, 31)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Greater, 29)),
            PruneVerdict::Keep
        );
    }

    #[test]
    fn not_equal_never_prunes_on_bounds() {
        // min = 1, max = 10 says nothing about whether 5 occurs.
        let values = [Some(1), Some(10)];
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::NotEqual, 5)),
            PruneVerdict::Keep
        );
        // Even a single-valued range keeps: `col != v` may hold for no row,
        // but min/max alone cannot prove it.
        let single = [Some(7)];
        assert_eq!(
            verdict(&single, &cmp(PruneOperator::NotEqual, 7)),
            PruneVerdict::Keep
        );
        assert_eq!(
            verdict(&single, &cmp(PruneOperator::NotEqual, 8)),
            PruneVerdict::Keep
        );
    }

    #[test]
    fn value_comparisons_prune_all_null_and_empty_ranges() {
        assert_eq!(
            verdict(&[None, None], &cmp(PruneOperator::Equal, 1)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&[None, None], &cmp(PruneOperator::Less, 1)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&[None, None], &cmp(PruneOperator::GreaterOrEqual, 1)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&[], &cmp(PruneOperator::Equal, 1)),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&[], &PrunePredicate::is_null(col())),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&[], &PrunePredicate::is_not_null(col())),
            PruneVerdict::Prune
        );
    }

    #[test]
    fn null_predicates_follow_three_valued_logic() {
        let no_nulls = [Some(1), Some(2)];
        let all_nulls = [None, None];
        let mixed = [None, Some(1)];
        assert_eq!(
            verdict(&no_nulls, &PrunePredicate::is_null(col())),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&no_nulls, &PrunePredicate::is_not_null(col())),
            PruneVerdict::Keep
        );
        assert_eq!(
            verdict(&all_nulls, &PrunePredicate::is_null(col())),
            PruneVerdict::Keep
        );
        assert_eq!(
            verdict(&all_nulls, &PrunePredicate::is_not_null(col())),
            PruneVerdict::Prune
        );
        assert_eq!(
            verdict(&mixed, &PrunePredicate::is_null(col())),
            PruneVerdict::Keep
        );
        assert_eq!(
            verdict(&mixed, &PrunePredicate::is_not_null(col())),
            PruneVerdict::Keep
        );
    }

    #[test]
    fn null_literal_comparisons_are_never_true() {
        let values = [Some(1), Some(2), None];
        for operator in [
            PruneOperator::Equal,
            PruneOperator::Less,
            PruneOperator::LessOrEqual,
            PruneOperator::Greater,
            PruneOperator::GreaterOrEqual,
        ] {
            let predicate = PrunePredicate::compare(col(), operator, Field::Null);
            assert_eq!(
                verdict(&values, &predicate),
                PruneVerdict::Prune,
                "{operator}"
            );
            assert_eq!(
                row_matches(&predicate, &|_| Some(Field::Integer(1))),
                Tri::Null,
                "{operator}"
            );
        }
        // `!=` never prunes on bounds; the NULL literal case keeps the range
        // too (conservative, and still never a false negative).
        let not_equal = PrunePredicate::compare(col(), PruneOperator::NotEqual, Field::Null);
        assert_ne!(verdict(&values, &not_equal), PruneVerdict::Prune);
        assert_eq!(
            row_matches(&not_equal, &|_| Some(Field::Integer(1))),
            Tri::Null
        );
        // An all-NULL range has nothing `!=` could be TRUE for.
        assert_eq!(verdict(&[None], &not_equal), PruneVerdict::Prune);
    }

    #[test]
    fn foreign_columns_and_untyped_literals_are_unknown() {
        let zone = zone_of(&[Some(1)]);
        let foreign = PrunePredicate::eq(ColumnId::new(99), Field::Integer(5));
        assert_eq!(zone.evaluate(&foreign), PruneVerdict::Unknown);
        let incomparable = PrunePredicate::eq(col(), Field::String("x".to_owned()));
        assert_eq!(zone.evaluate(&incomparable), PruneVerdict::Unknown);
        // A missing literal (only legal for IS NULL / IS NOT NULL) is unknown.
        let missing = PrunePredicate::Compare {
            column_id: col(),
            operator: PruneOperator::Equal,
            literal: None,
        };
        assert_eq!(zone.evaluate(&missing), PruneVerdict::Unknown);
    }

    #[test]
    fn extreme_boundaries_never_overflow() {
        let zone = zone_of(&[Some(i64::MIN), Some(i64::MAX)]);
        assert_eq!(
            zone.evaluate(&cmp(PruneOperator::Less, i64::MIN)),
            PruneVerdict::Prune
        );
        assert_eq!(
            zone.evaluate(&cmp(PruneOperator::LessOrEqual, i64::MIN)),
            PruneVerdict::Keep
        );
        assert_eq!(
            zone.evaluate(&cmp(PruneOperator::Greater, i64::MAX)),
            PruneVerdict::Prune
        );
        assert_eq!(
            zone.evaluate(&cmp(PruneOperator::GreaterOrEqual, i64::MAX)),
            PruneVerdict::Keep
        );
        assert_eq!(
            zone.evaluate(&cmp(PruneOperator::Equal, i64::MIN)),
            PruneVerdict::Keep
        );
        assert_eq!(
            zone.evaluate(&cmp(PruneOperator::Equal, i64::MAX)),
            PruneVerdict::Keep
        );
        let zero = zone_of(&[Some(0)]);
        assert_eq!(
            zero.evaluate(&cmp(PruneOperator::Equal, 0)),
            PruneVerdict::Keep
        );
        assert_eq!(
            zero.evaluate(&cmp(PruneOperator::Less, -1)),
            PruneVerdict::Prune
        );
        assert_eq!(
            zero.evaluate(&cmp(PruneOperator::Greater, 0)),
            PruneVerdict::Prune
        );
    }

    #[test]
    fn and_prunes_when_either_branch_proves_impossibility() {
        let values = [Some(10), Some(20)];
        // Impossible left, possible right: AND can never be TRUE.
        let predicate = cmp(PruneOperator::Equal, 99).and(cmp(PruneOperator::Greater, 5));
        assert_eq!(verdict(&values, &predicate), PruneVerdict::Prune);
        // Possible left, impossible right.
        let predicate = cmp(PruneOperator::Greater, 5).and(cmp(PruneOperator::Equal, 99));
        assert_eq!(verdict(&values, &predicate), PruneVerdict::Prune);
        // Both possible: keep.
        let predicate = cmp(PruneOperator::Greater, 5).and(cmp(PruneOperator::Less, 99));
        assert_eq!(verdict(&values, &predicate), PruneVerdict::Keep);
        // Unknown branch (foreign column) cannot upgrade to Prune.
        let predicate = cmp(PruneOperator::Greater, 5)
            .and(PrunePredicate::eq(ColumnId::new(99), Field::Integer(1)));
        assert_ne!(verdict(&values, &predicate), PruneVerdict::Prune);
    }

    #[test]
    fn or_prunes_only_when_every_branch_proves_impossibility() {
        let values = [Some(10), Some(20)];
        let both_impossible = cmp(PruneOperator::Equal, 99).or(cmp(PruneOperator::Less, 1));
        assert_eq!(verdict(&values, &both_impossible), PruneVerdict::Prune);
        let one_possible = cmp(PruneOperator::Equal, 99).or(cmp(PruneOperator::Greater, 5));
        assert_eq!(verdict(&values, &one_possible), PruneVerdict::Keep);
        // An unknown branch keeps the range: it might be TRUE somewhere.
        let with_unknown = cmp(PruneOperator::Equal, 99)
            .or(PrunePredicate::eq(ColumnId::new(99), Field::Integer(1)));
        assert_ne!(verdict(&values, &with_unknown), PruneVerdict::Prune);
    }

    #[test]
    fn not_pushes_down_through_safe_duals() {
        let values = [Some(10), Some(20)];
        // NOT (col < 10) == col >= 10: possible.
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Less, 10).not()),
            PruneVerdict::Keep
        );
        // NOT (col < 5) == col >= 5: possible.
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Less, 5).not()),
            PruneVerdict::Keep
        );
        // NOT (col > 20) == col <= 20: possible.
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Greater, 20).not()),
            PruneVerdict::Keep
        );
        // NOT (col IS NOT NULL) == col IS NULL: no NULLs, impossible.
        assert_eq!(
            verdict(&values, &PrunePredicate::is_not_null(col()).not()),
            PruneVerdict::Prune
        );
        // NOT (col = v) == col != v: never prunes on bounds.
        assert_ne!(
            verdict(&values, &cmp(PruneOperator::Equal, 10).not()),
            PruneVerdict::Prune
        );
        // Double negation collapses.
        assert_eq!(
            verdict(&values, &cmp(PruneOperator::Equal, 99).not().not()),
            PruneVerdict::Prune
        );
        // De Morgan: NOT (A OR B) == NOT A AND NOT B.
        let inner = cmp(PruneOperator::Equal, 10).or(cmp(PruneOperator::Equal, 20));
        assert_ne!(verdict(&values, &inner.not()), PruneVerdict::Prune);
        // NOT (A AND B) == NOT A OR NOT B: `col != 10 OR col != 20` is TRUE for
        // row 10, so it must never be pruned.
        let inner = cmp(PruneOperator::Equal, 10).and(cmp(PruneOperator::Equal, 20));
        assert_eq!(verdict(&values, &inner.not()), PruneVerdict::Keep);
    }

    #[test]
    fn negate_predicate_is_an_involution_where_defined() {
        for predicate in [
            cmp(PruneOperator::Equal, 1),
            cmp(PruneOperator::NotEqual, 1),
            cmp(PruneOperator::Less, 1),
            cmp(PruneOperator::LessOrEqual, 1),
            cmp(PruneOperator::Greater, 1),
            cmp(PruneOperator::GreaterOrEqual, 1),
            PrunePredicate::is_null(col()),
            PrunePredicate::is_not_null(col()),
        ] {
            let dual = negate_predicate(&predicate).expect("dual");
            let round_trip = negate_predicate(&dual).expect("dual");
            assert_eq!(round_trip, predicate, "{predicate:?}");
        }
    }

    #[test]
    fn predicate_columns_are_collected_once_in_order() {
        let other = ColumnId::new(9);
        let predicate = cmp(PruneOperator::Equal, 1)
            .and(PrunePredicate::eq(other, Field::Integer(2)))
            .or(cmp(PruneOperator::Less, 3));
        assert_eq!(predicate_columns(&predicate), vec![col(), other]);
    }

    /// Deterministic randomized property test over generated datasets and
    /// predicates: whenever the metadata says PRUNE, a full scan of the very
    /// same values must find no matching row.
    ///
    /// Seeds are fixed so any failure is replayable byte for byte.
    #[test]
    fn pruned_ranges_never_hide_a_matching_row() {
        let operators = [
            PruneOperator::Equal,
            PruneOperator::NotEqual,
            PruneOperator::Less,
            PruneOperator::LessOrEqual,
            PruneOperator::Greater,
            PruneOperator::GreaterOrEqual,
        ];
        for seed in [
            1_u64, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233, 377, 610, 987,
        ] {
            let mut state = seed | 1;
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            for shape in 0..24 {
                let len = (next() % 40) as usize;
                let mut values: Vec<Option<i64>> = Vec::with_capacity(len);
                for _ in 0..len {
                    match next() % 6 {
                        0 | 1 => values.push(None),
                        2 => values.push(Some((next() % 3) as i64)), // clustered
                        3 => values.push(Some(next() as i64 / 1_000_000)), // wide
                        _ => values.push(Some((next() % 201) as i64 - 100)),
                    }
                }

                // Literals drawn from the data (so hits are common), from just
                // outside the data, and from the representable extremes.
                let non_null: Vec<i64> = values.iter().flatten().copied().collect();
                let mut literals: Vec<i64> = Vec::new();
                if let (Some(min), Some(max)) = (
                    non_null.iter().copied().min(),
                    non_null.iter().copied().max(),
                ) {
                    literals.extend([
                        min,
                        max,
                        min.saturating_sub(1),
                        max.saturating_add(1),
                        min.saturating_add(1),
                        max.saturating_sub(1),
                    ]);
                }
                literals.extend([0, -1, 1, i64::MIN, i64::MAX, 42, -42]);
                for _ in 0..4 {
                    literals.push((next() % 201) as i64 - 100);
                }

                for literal in literals {
                    for operator in operators {
                        let leaf =
                            PrunePredicate::compare(col(), operator, Field::Integer(literal));
                        let label = format!("seed={seed} shape={shape} {operator} {literal}");
                        assert_no_false_negative(&values, &leaf, &label);
                        assert_no_false_negative(
                            &values,
                            &leaf.clone().and(PrunePredicate::is_not_null(col())),
                            &label,
                        );
                        assert_no_false_negative(
                            &values,
                            &leaf.clone().or(PrunePredicate::is_null(col())),
                            &label,
                        );
                        assert_no_false_negative(&values, &leaf.not(), &label);
                    }
                }
                for predicate in [
                    PrunePredicate::is_null(col()),
                    PrunePredicate::is_not_null(col()),
                    PrunePredicate::is_null(col()).not(),
                ] {
                    assert_no_false_negative(
                        &values,
                        &predicate,
                        &format!("seed={seed} shape={shape}"),
                    );
                }
            }
        }
    }
}
