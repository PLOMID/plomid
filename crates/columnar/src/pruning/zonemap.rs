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
//! Zone maps: min/max/NULL summaries of immutable row ranges.
//!
//! A zone map summarizes one row range of one column with four facts:
//! row count, NULL state, and — when the column type has a storage-defined
//! ordering and the range holds at least one non-NULL value — the minimum
//! and maximum non-NULL values. These are the only facts min/max pruning is
//! allowed to reason from; everything else about the data is invisible to
//! the pruner and therefore always results in a scan.
//!
//! # NULL semantics
//!
//! NULL is never a minimum or maximum. [`NullState`] tracks NULL occupancy
//! separately (`NO_NULLS` / `HAS_NULLS` / `ALL_NULLS`), and range
//! comparisons never match NULL rows (SQL three-valued logic), so a NULL
//! row can never make a `PRUNE` wrong.
//!
//! # Supported types
//!
//! Ordering — and therefore min/max pruning — is supported exactly for the
//! columnar types whose ordering the storage layer already defines in
//! [`crate::statistics::compare_fields`]:
//!
//! * `Integer` — signed 64-bit numeric order;
//! * `Bytes` — unsigned lexicographic byte order;
//! * `String` — UTF-8 byte order (code-point preserving), cross-comparable
//!   with `Bytes` because both are byte sequences.
//!
//! Every other type (including the `Null` and reserved `Bool`/`I16`/
//! `I32`/`F32`/`F64` tags) reports no bounds: its zone maps still carry row
//! count and NULL state, but range predicates always evaluate to KEEP.

use crate::column::ColumnType;
use crate::layout::corruption;
use crate::statistics::compare_fields;
use plomid_core::{ColumnId, Result};
use plomid_storage::Field;
use std::cmp::Ordering;

use super::predicate::PrunePredicate;
use super::verdict::PruneVerdict;

/// Persisted tag: the summarized range holds no NULL rows.
pub const NULL_STATE_NO_NULLS: u8 = 0;
/// Persisted tag: the range holds at least one NULL and one non-NULL row.
pub const NULL_STATE_HAS_NULLS: u8 = 1;
/// Persisted tag: every row in the range is NULL.
pub const NULL_STATE_ALL_NULLS: u8 = 2;
/// NULL occupancy of one summarized row range.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum NullState {
    NoNulls,
    HasNulls,
    AllNulls,
}
impl NullState {
    #[must_use]
    pub fn observe(current: Option<Self>, is_null: bool) -> Self {
        match (current, is_null) {
            (None, true) => Self::AllNulls,
            (None, false) => Self::NoNulls,
            (Some(Self::AllNulls), true) => Self::AllNulls,
            (Some(Self::NoNulls), false) => Self::NoNulls,
            _ => Self::HasNulls,
        }
    }
    #[must_use]
    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::NoNulls, Self::NoNulls) => Self::NoNulls,
            (Self::AllNulls, Self::AllNulls) => Self::AllNulls,
            _ => Self::HasNulls,
        }
    }
    pub fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            NULL_STATE_NO_NULLS => Ok(Self::NoNulls),
            NULL_STATE_HAS_NULLS => Ok(Self::HasNulls),
            NULL_STATE_ALL_NULLS => Ok(Self::AllNulls),
            other => Err(corruption(format!(
                "pruning zone has invalid null state {other}"
            ))),
        }
    }
    #[must_use]
    pub fn tag(self) -> u8 {
        match self {
            Self::NoNulls => NULL_STATE_NO_NULLS,
            Self::HasNulls => NULL_STATE_HAS_NULLS,
            Self::AllNulls => NULL_STATE_ALL_NULLS,
        }
    }
}
impl std::fmt::Display for NullState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoNulls => write!(f, "NO_NULLS"),
            Self::HasNulls => write!(f, "HAS_NULLS"),
            Self::AllNulls => write!(f, "ALL_NULLS"),
        }
    }
}

/// Builds one [`ZoneMap`] incrementally from observed rows.
///
/// Each call to `observe` adds one row with its NULL flag and (optionally) its
/// value. The builder computes row_count, null state, min, and max in one pass.
/// Supported types (`Integer`, `Bytes`, `String`) produce bounds when at least
/// one non-NULL value exists and all non-NULL values are mutually comparable;
/// unsupported types and incomparable values leave bounds empty (the range
/// keeps on everything).
#[derive(Clone, Debug)]
pub struct ZoneMapBuilder {
    column_id: ColumnId,
    column_type: ColumnType,
    start_row: Option<u64>,
    end_row: u64,
    row_count: u64,
    null_state: Option<NullState>,
    min: Option<Field>,
    max: Option<Field>,
    /// Set when non-NULL values are mutually incomparable; bounds cleared.
    incomparable: bool,
}

impl ZoneMapBuilder {
    #[must_use]
    pub fn new(column_id: ColumnId, column_type: ColumnType) -> Self {
        Self {
            column_id,
            column_type,
            start_row: None,
            end_row: 0,
            row_count: 0,
            null_state: None,
            min: None,
            max: None,
            incomparable: false,
        }
    }

    /// Observes one row: `value` is the stored [`Field`] or `None` for NULL.
    pub fn observe(&mut self, row: u64, value: Option<Field>) {
        self.start_row = Some(self.start_row.map_or(row, |s| s.min(row)));
        self.end_row = self.end_row.max(row.saturating_add(1));
        self.row_count = self.row_count.saturating_add(1);

        match value {
            None => {
                self.null_state = Some(NullState::observe(self.null_state, true));
            }
            Some(Field::Null) => {
                self.null_state = Some(NullState::observe(self.null_state, true));
            }
            Some(field) => {
                self.null_state = Some(NullState::observe(self.null_state, false));

                // Only supported types carry bounds.
                if !supports_bounds(self.column_type) || self.incomparable {
                    return;
                }

                if self.min.is_none() && self.max.is_none() {
                    self.min = Some(field.clone());
                    self.max = Some(field);
                } else {
                    // Compare against current bounds; mutually incomparable
                    // values clear bounds and keep the range conservative.
                    match (
                        compare_fields(&field, self.min.as_ref().unwrap()),
                        compare_fields(&field, self.max.as_ref().unwrap()),
                    ) {
                        (Some(Ordering::Less), _) => self.min = Some(field.clone()),
                        (_, Some(Ordering::Greater)) => self.max = Some(field.clone()),
                        (Some(_), Some(_)) => {}
                        // Incomparable: clear bounds permanently.
                        _ => {
                            self.incomparable = true;
                            self.min = None;
                            self.max = None;
                        }
                    }
                }
            }
        }
    }

    /// Finishes this builder, returning the completed [`ZoneMap`].
    #[must_use]
    pub fn finish(self) -> ZoneMap {
        let (min, max) = if self.incomparable || !supports_bounds(self.column_type) {
            (None, None)
        } else if let (Some(min), Some(max)) = (self.min, self.max) {
            // Defensive: never emit min > max (builder invariant).
            match compare_fields(&min, &max) {
                Some(Ordering::Greater) => (None, None),
                _ => (Some(min), Some(max)),
            }
        } else {
            (None, None)
        };

        ZoneMap {
            column_id: self.column_id,
            column_type: self.column_type,
            start_row: self.start_row.unwrap_or(0),
            row_count: self.row_count,
            null_state: self.null_state.unwrap_or(NullState::AllNulls),
            min,
            max,
        }
    }
}

/// Returns true when a column type has a storage-defined ordering and can
/// produce min/max bounds for pruning.
#[must_use]
pub fn supports_bounds(column_type: ColumnType) -> bool {
    matches!(
        column_type,
        ColumnType::Integer | ColumnType::Bytes | ColumnType::String
    )
}

/// Min/max + NULL summary of one immutable row range of one column.
///
/// `column_id`/`column_type` name what is summarized; `[start_row, row_count`
/// rows) names the physical rows. `min`/`max` are non-NULL bounds in the
/// column's own ordering; they are always either both present or both
/// absent, and never `Field::Null`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneMap {
    pub column_id: ColumnId,
    pub column_type: ColumnType,
    pub start_row: u64,
    pub row_count: u64,
    pub null_state: NullState,
    pub min: Option<Field>,
    pub max: Option<Field>,
}
impl ZoneMap {
    #[must_use]
    pub fn new(
        column_id: ColumnId,
        column_type: ColumnType,
        start_row: u64,
        row_count: u64,
        null_state: NullState,
        min: Option<Field>,
        max: Option<Field>,
    ) -> Self {
        let min = match min {
            Some(Field::Null) => None,
            other => other,
        };
        let max = match max {
            Some(Field::Null) => None,
            other => other,
        };
        Self {
            column_id,
            column_type,
            start_row,
            row_count,
            null_state,
            min,
            max,
        }
    }
    #[must_use]
    pub fn end_row(&self) -> u64 {
        self.start_row.saturating_add(self.row_count)
    }
    #[must_use]
    pub fn rows(&self) -> u64 {
        self.row_count
    }
    #[must_use]
    pub fn has_bounds(&self) -> bool {
        self.min.is_some() && self.max.is_some()
    }
    /// Returns true when this zone map's row range overlaps `[start_row, end_row)`.
    #[must_use]
    pub fn overlaps(&self, start_row: u64, end_row: u64) -> bool {
        // An empty zone map describes no rows and is irrelevant.
        if self.row_count == 0 {
            return false;
        }
        let self_end = self.end_row();
        self.start_row < end_row && self_end > start_row
    }
    /// Evaluates one predicate against this zone map.
    ///
    /// Returns `PruneVerdict::Prune` only when the metadata *proves* no row in this
    /// range can satisfy `predicate`; otherwise returns `Keep`/`Unknown` (both mean
    /// "scan this range"). This is the soundness core for leaf predicates.
    #[must_use]
    pub fn evaluate(&self, predicate: &PrunePredicate) -> PruneVerdict {
        super::predicate::evaluate_on_zone(self, predicate)
    }
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        super::trailer::encode_zone(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (zone, consumed) = super::trailer::decode_zone(bytes)?;
        if consumed != bytes.len() {
            return Err(corruption("pruning zone map has trailing bytes"));
        }
        Ok(zone)
    }
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.encode().len()
    }
}
impl std::fmt::Display for ZoneMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ZoneMap {{ column: {}, rows: [{}..{}), nulls: {}, min: {:?}, max: {:?} }}",
            self.column_id.get(),
            self.start_row,
            self.start_row.saturating_add(self.row_count),
            self.null_state,
            self.min,
            self.max
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pruning::{PruneOperator, PrunePredicate, PruneVerdict};

    fn col() -> ColumnId {
        ColumnId::new(0)
    }

    fn int(value: i64) -> Option<Field> {
        Some(Field::Integer(value))
    }

    /// Builds one zone map over `values`, where element `i` is physical row `i`.
    fn build(values: &[Option<Field>], column_type: ColumnType) -> ZoneMap {
        let mut builder = ZoneMapBuilder::new(col(), column_type);
        for (row, value) in values.iter().enumerate() {
            builder.observe(row as u64, value.clone());
        }
        builder.finish()
    }

    fn ints(values: &[Option<i64>]) -> ZoneMap {
        let values: Vec<Option<Field>> = values.iter().map(|v| v.map(Field::Integer)).collect();
        build(&values, ColumnType::Integer)
    }

    #[test]
    fn negative_and_extreme_values_are_bounded_exactly() {
        let zone = ints(&[Some(i64::MIN), Some(-1), Some(0), Some(i64::MAX)]);
        assert_eq!(zone.min, Some(Field::Integer(i64::MIN)));
        assert_eq!(zone.max, Some(Field::Integer(i64::MAX)));
        // Evaluation compares; it never computes, so extremes cannot overflow.
        assert_eq!(
            zone.evaluate(&PrunePredicate::compare(
                col(),
                PruneOperator::GreaterOrEqual,
                Field::Integer(i64::MAX)
            )),
            PruneVerdict::Keep
        );
        assert_eq!(
            zone.evaluate(&PrunePredicate::compare(
                col(),
                PruneOperator::Less,
                Field::Integer(i64::MIN)
            )),
            PruneVerdict::Prune
        );
    }

    #[test]
    fn null_only_range_reports_all_nulls_without_bounds() {
        let zone = ints(&[None, None, None]);
        assert_eq!(zone.row_count, 3);
        assert_eq!(zone.null_state, NullState::AllNulls);
        assert!(!zone.has_bounds());
        assert_eq!(
            zone.evaluate(&PrunePredicate::is_null(col())),
            PruneVerdict::Keep
        );
        assert_eq!(
            zone.evaluate(&PrunePredicate::is_not_null(col())),
            PruneVerdict::Prune
        );
    }

    #[test]
    fn mixed_range_bounds_cover_non_nulls_only() {
        let zone = ints(&[None, Some(3), None, Some(-2)]);
        assert_eq!(zone.null_state, NullState::HasNulls);
        assert_eq!(zone.min, Some(Field::Integer(-2)));
        assert_eq!(zone.max, Some(Field::Integer(3)));
        // NULL never becomes a bound, so both NULL predicates keep the range.
        assert_eq!(
            zone.evaluate(&PrunePredicate::is_not_null(col())),
            PruneVerdict::Keep
        );
        assert_eq!(
            zone.evaluate(&PrunePredicate::is_null(col())),
            PruneVerdict::Keep
        );
    }

    #[test]
    fn field_null_is_treated_as_null_not_as_a_value() {
        let zone = build(
            &[Some(Field::Null), Some(Field::Integer(1))],
            ColumnType::Integer,
        );
        assert_eq!(zone.null_state, NullState::HasNulls);
        assert_eq!(zone.min, Some(Field::Integer(1)));
        assert_eq!(zone.max, Some(Field::Integer(1)));
    }

    #[test]
    fn unsupported_type_yields_null_state_but_no_bounds() {
        for column_type in [
            ColumnType::Bool,
            ColumnType::I16,
            ColumnType::I32,
            ColumnType::F32,
            ColumnType::F64,
            ColumnType::Null,
        ] {
            assert!(!supports_bounds(column_type), "{column_type:?}");
            let zone = build(&[int(1), int(2)], column_type);
            assert_eq!(zone.row_count, 2);
            assert!(!zone.has_bounds(), "{column_type:?}");
            // No bounds: the range must be scanned, never pruned. `Unknown`
            // (no evidence) is exactly as conservative as `Keep`.
            let verdict = zone.evaluate(&PrunePredicate::eq(col(), Field::Integer(99)));
            assert_eq!(verdict, PruneVerdict::Unknown, "{column_type:?}");
            assert_ne!(verdict, PruneVerdict::Prune, "{column_type:?}");
        }
    }

    #[test]
    fn incomparable_values_clear_bounds_permanently() {
        // A well-formed segment never mixes these, but a corrupt in-memory
        // summary must degrade to "no bounds", never to a wrong bound.
        let zone = build(
            &[
                Some(Field::Integer(1)),
                Some(Field::String("a".to_owned())),
                Some(Field::Integer(9)),
            ],
            ColumnType::Integer,
        );
        assert!(!zone.has_bounds());
        assert_eq!(zone.null_state, NullState::NoNulls);
        assert_eq!(zone.row_count, 3);
        assert_eq!(
            zone.evaluate(&PrunePredicate::eq(col(), Field::Integer(-50))),
            PruneVerdict::Unknown
        );
        // Bounds stay cleared even after a later comparable value arrives.
        let mut builder = ZoneMapBuilder::new(col(), ColumnType::Integer);
        builder.observe(0, int(1));
        builder.observe(1, Some(Field::String("a".to_owned())));
        builder.observe(2, int(100));
        assert!(!builder.finish().has_bounds());
    }

    #[test]
    fn boundless_all_null_ranges_prune_value_comparisons_only() {
        // Every row is NULL, so no comparison can be TRUE: pruning is sound
        // even without bounds. NULL predicates still keep the range.
        let zone = ints(&[None, None]);
        assert_eq!(zone.null_state, NullState::AllNulls);
        for predicate in [
            PrunePredicate::eq(col(), Field::Integer(1)),
            PrunePredicate::compare(col(), PruneOperator::Less, Field::Integer(1)),
            PrunePredicate::compare(col(), PruneOperator::GreaterOrEqual, Field::Integer(1)),
            PrunePredicate::ne(col(), Field::Integer(1)),
        ] {
            assert_eq!(
                zone.evaluate(&predicate),
                PruneVerdict::Prune,
                "{predicate:?}"
            );
        }
        assert_eq!(
            zone.evaluate(&PrunePredicate::is_null(col())),
            PruneVerdict::Keep
        );
    }

    #[test]
    fn bounds_match_a_brute_force_scan_over_deterministic_shapes() {
        // Fixed-seed xorshift: many deterministic shapes, each compared against
        // a direct scan of the very same values.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for shape in 0..128 {
            let len = (next() % 37) as usize;
            let mut values = Vec::with_capacity(len);
            for _ in 0..len {
                if next() % 5 == 0 {
                    values.push(None);
                } else {
                    values.push(Some(next() as i64));
                }
            }
            let zone = ints(&values);
            let non_null: Vec<i64> = values.iter().flatten().copied().collect();
            assert_eq!(zone.row_count, values.len() as u64, "shape {shape}");
            assert_eq!(
                zone.min,
                non_null.iter().min().copied().map(Field::Integer),
                "shape {shape}"
            );
            assert_eq!(
                zone.max,
                non_null.iter().max().copied().map(Field::Integer),
                "shape {shape}"
            );
            let expected = if non_null.is_empty() {
                NullState::AllNulls
            } else if non_null.len() == values.len() {
                NullState::NoNulls
            } else {
                NullState::HasNulls
            };
            assert_eq!(zone.null_state, expected, "shape {shape}");
        }
    }

    #[test]
    fn string_and_bytes_use_storage_byte_order() {
        assert!(supports_bounds(ColumnType::String));
        assert!(supports_bounds(ColumnType::Bytes));
        let zone = build(
            &[
                Some(Field::String("delta".to_owned())),
                Some(Field::String("alpha".to_owned())),
                Some(Field::String("charlie".to_owned())),
            ],
            ColumnType::String,
        );
        assert_eq!(zone.min, Some(Field::String("alpha".to_owned())));
        assert_eq!(zone.max, Some(Field::String("delta".to_owned())));
        let zone = build(
            &[Some(Field::Bytes(vec![9])), Some(Field::Bytes(vec![1, 2]))],
            ColumnType::Bytes,
        );
        assert_eq!(zone.min, Some(Field::Bytes(vec![1, 2])));
        assert_eq!(zone.max, Some(Field::Bytes(vec![9])));
    }

    #[test]
    fn overlaps_tracks_half_open_intervals_and_ignores_empty_zones() {
        let zone = ints(&[Some(1), Some(2), Some(3)]); // rows [0, 3)
        assert!(zone.overlaps(0, 1));
        assert!(zone.overlaps(2, 5));
        assert!(zone.overlaps(1, 2));
        assert!(!zone.overlaps(3, 4));
        assert!(!zone.overlaps(0, 0));
        let empty = build(&[], ColumnType::Integer);
        assert!(!empty.overlaps(0, 100));
    }

    #[test]
    fn null_state_combines_as_a_lattice() {
        use NullState::{AllNulls, HasNulls, NoNulls};
        assert_eq!(NoNulls.combine(NoNulls), NoNulls);
        assert_eq!(AllNulls.combine(AllNulls), AllNulls);
        assert_eq!(NoNulls.combine(AllNulls), HasNulls);
        assert_eq!(HasNulls.combine(HasNulls), HasNulls);
        assert_eq!(NullState::observe(None, true), AllNulls);
        assert_eq!(NullState::observe(None, false), NoNulls);
        assert_eq!(NullState::observe(Some(AllNulls), false), HasNulls);
        assert_eq!(NullState::observe(Some(HasNulls), true), HasNulls);
    }

    #[test]
    fn null_state_tags_round_trip_and_reject_unknown_values() {
        for state in [NullState::NoNulls, NullState::HasNulls, NullState::AllNulls] {
            assert_eq!(NullState::from_tag(state.tag()).expect("tag"), state);
        }
        for bad in [3_u8, 7, 255] {
            let error = NullState::from_tag(bad).expect_err("unknown null state");
            assert_eq!(error.kind(), plomid_core::ErrorKind::Corruption);
        }
    }

    #[test]
    fn constructor_never_keeps_a_null_bound() {
        let zone = ZoneMap::new(
            col(),
            ColumnType::Integer,
            0,
            1,
            NullState::AllNulls,
            Some(Field::Null),
            Some(Field::Null),
        );
        assert!(!zone.has_bounds());
        assert_eq!(zone.null_state, NullState::AllNulls);
    }

    #[test]
    fn zone_map_encoding_round_trips_without_loss() {
        for zone in [
            ints(&[Some(-3), Some(8)]),
            ints(&[None, None]),
            build(&[Some(Field::String("x".to_owned()))], ColumnType::String),
            build(&[Some(Field::Bytes(vec![0, 255]))], ColumnType::Bytes),
            build(&[], ColumnType::Integer),
        ] {
            let bytes = zone.encode();
            assert_eq!(bytes.len(), zone.encoded_len());
            let decoded = ZoneMap::decode(&bytes).expect("decode");
            assert_eq!(decoded, zone);
            // Trailing bytes are corruption, not a longer zone map.
            let mut padded = bytes.clone();
            padded.push(0);
            assert!(ZoneMap::decode(&padded).is_err());
        }
    }

    #[test]
    fn empty_range_carries_no_rows_bounds_or_null_evidence() {
        let zone = build(&[], ColumnType::Integer);
        assert_eq!(zone.row_count, 0);
        assert_eq!(zone.end_row(), zone.start_row);
        assert!(!zone.has_bounds());
        assert_eq!(zone.null_state, NullState::AllNulls);
        // An empty range provably holds no matching row, whatever the shape.
        assert_eq!(
            zone.evaluate(&PrunePredicate::eq(col(), Field::Integer(1))),
            PruneVerdict::Prune
        );
        assert_eq!(
            zone.evaluate(&PrunePredicate::is_null(col())),
            PruneVerdict::Prune
        );
        assert_eq!(
            zone.evaluate(&PrunePredicate::is_not_null(col())),
            PruneVerdict::Prune
        );
    }

    #[test]
    fn single_row_range_bounds_equal_that_row() {
        let zone = ints(&[Some(-12)]);
        assert_eq!(zone.row_count, 1);
        assert_eq!(zone.min, Some(Field::Integer(-12)));
        assert_eq!(zone.max, Some(Field::Integer(-12)));
        assert_eq!(zone.null_state, NullState::NoNulls);
    }

    #[test]
    fn identical_values_collapse_to_one_bound_pair() {
        let zone = ints(&[Some(5), Some(5), Some(5), Some(5)]);
        assert_eq!(zone.min, Some(Field::Integer(5)));
        assert_eq!(zone.max, Some(Field::Integer(5)));
        assert_eq!(
            zone.evaluate(&PrunePredicate::eq(col(), Field::Integer(5))),
            PruneVerdict::Keep
        );
        assert_eq!(
            zone.evaluate(&PrunePredicate::eq(col(), Field::Integer(6))),
            PruneVerdict::Prune
        );
    }

    #[test]
    fn ascending_and_descending_ranges_agree_on_bounds() {
        let ascending = ints(&[Some(-9), Some(0), Some(4), Some(100)]);
        let descending = ints(&[Some(100), Some(4), Some(0), Some(-9)]);
        assert_eq!(ascending.min, Some(Field::Integer(-9)));
        assert_eq!(ascending.max, Some(Field::Integer(100)));
        assert_eq!(descending.min, ascending.min);
        assert_eq!(descending.max, ascending.max);
        assert_eq!(ascending.null_state, descending.null_state);
    }
}
