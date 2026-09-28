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
//! Column statistics for analytical pruning.
//!
//! A segment records, for each column, the number of rows, the number of NULL
//! rows, and the minimum and maximum non-NULL values. The purpose is pruning: a
//! scan that needs `price > 500` can discard a segment whose maximum price is
//! 100 without reading a single value.
//!
//! # Conservative by construction
//!
//! A statistic that is reported is a statistic that is held. [`ColumnStatistics`]
//! tracks bound availability explicitly, so pruning never discards a segment on
//! the strength of a bound the writer could not compute. Bounds are unavailable
//! when the column type is not ordered, when every row is NULL, and when the
//! values are not mutually comparable — a mixed-type payload makes the bounds
//! unavailable rather than reporting an unsound minimum.
//!
//! # Persisted form
//!
//! The payload is a two-field [`Row`] holding the minimum and maximum, reusing
//! the checksum-validated row codec instead of defining a second value format.
//! Availability lives in the metadata flag byte, so a payload whose bound slot is
//! NULL while its flag is set is rejected as corruption rather than read as
//! "no bound".

use crate::column::ColumnType;
use crate::format;
use plomid_core::{ColumnId, ErrorKind, PlomidError, Result};
use plomid_storage::{Field, Row};
use std::cmp::Ordering;
use std::fmt;

/// Number of fields in a persisted statistics payload: min and max.
const STATISTICS_FIELD_COUNT: usize = 2;

/// Orders two non-NULL fields, returning `None` when they are not comparable.
///
/// `String` is ordered by its UTF-8 bytes, which preserves code-point order.
/// `String` and `Bytes` are ordered against each other because both are byte
/// sequences. Mixed variants involving `Integer` are not comparable: the format
/// does not define whether an integer sorts before arbitrary bytes.
#[must_use]
pub fn compare_fields(left: &Field, right: &Field) -> Option<Ordering> {
    match (left, right) {
        (Field::Integer(a), Field::Integer(b)) => Some(a.cmp(b)),
        (Field::Bytes(a), Field::Bytes(b)) => Some(a.cmp(b)),
        (Field::String(a), Field::String(b)) => Some(a.as_bytes().cmp(b.as_bytes())),
        (Field::Bytes(a), Field::String(b)) => Some(a.as_slice().cmp(b.as_bytes())),
        (Field::String(a), Field::Bytes(b)) => Some(a.as_bytes().cmp(b.as_slice())),
        _ => None,
    }
}
/// A borrowed row value together with the logical type of its column.
#[derive(Clone, Copy, Debug)]
pub struct ValueRef<'a> {
    /// Logical type the bytes are interpreted through.
    pub column_type: ColumnType,
    /// Raw persisted bytes of the value.
    pub bytes: &'a [u8],
}

impl<'a> ValueRef<'a> {
    /// Creates a borrowed value of the given column type.
    #[must_use]
    pub fn new(column_type: ColumnType, bytes: &'a [u8]) -> Self {
        Self { column_type, bytes }
    }

    /// Returns true when the bytes are a valid encoding of `column_type`.
    #[must_use]
    pub fn is_well_typed(&self) -> bool {
        match self.column_type {
            ColumnType::Integer => self.bytes.len() == size_of::<i64>(),
            ColumnType::Bytes => true,
            ColumnType::String => std::str::from_utf8(self.bytes).is_ok(),
            ColumnType::Null
            | ColumnType::Bool
            | ColumnType::I16
            | ColumnType::I32
            | ColumnType::F32
            | ColumnType::F64 => false,
        }
    }

    /// Rebuilds the logical field this value denotes.
    #[must_use]
    pub fn materialize(&self) -> Field {
        match self.column_type {
            ColumnType::Integer => match <[u8; 8]>::try_from(self.bytes) {
                Ok(array) => Field::Integer(i64::from_le_bytes(array)),
                Err(_) => Field::Null,
            },
            ColumnType::String => match std::str::from_utf8(self.bytes) {
                Ok(text) => Field::String(text.to_owned()),
                Err(_) => Field::Null,
            },
            ColumnType::Bytes => Field::Bytes(self.bytes.to_vec()),
            ColumnType::Null
            | ColumnType::Bool
            | ColumnType::I16
            | ColumnType::I32
            | ColumnType::F32
            | ColumnType::F64 => Field::Null,
        }
    }
}

/// Orders a borrowed value against a stored bound.
///
/// Returns `None` when the two are not comparable, which the caller treats as
/// "this bound can no longer be proven" rather than as a silent ordering.
#[must_use]
pub fn compare_value_to_field(value: ValueRef<'_>, stored: &Field) -> Option<Ordering> {
    match (value.column_type, stored) {
        (ColumnType::Integer, Field::Integer(rhs)) => {
            let array: [u8; 8] = value.bytes.try_into().ok()?;
            Some(i64::from_le_bytes(array).cmp(rhs))
        }
        (ColumnType::Bytes, Field::Bytes(rhs)) => Some(value.bytes.cmp(rhs.as_slice())),
        (ColumnType::String, Field::String(rhs)) => Some(value.bytes.cmp(rhs.as_bytes())),
        (ColumnType::String, Field::Bytes(rhs)) => Some(value.bytes.cmp(rhs.as_slice())),
        (ColumnType::Bytes, Field::String(rhs)) => Some(value.bytes.cmp(rhs.as_bytes())),
        _ => None,
    }
}

/// Statistics for one column of a segment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnStatistics {
    /// Column these statistics describe.
    pub column_id: ColumnId,
    /// Number of logical rows in the column.
    pub row_count: u64,
    /// Number of rows whose null bit is set.
    pub null_count: u64,
    /// Minimum non-NULL value, when one is held.
    pub min: Option<Field>,
    /// Maximum non-NULL value, when one is held.
    pub max: Option<Field>,
    /// Whether a minimum is meaningful for this column.
    pub min_available: bool,
    /// Whether a maximum is meaningful for this column.
    pub max_available: bool,
}
impl ColumnStatistics {
    /// Creates statistics that track no bounds.
    #[must_use]
    pub fn new(column_id: ColumnId, row_count: u64, null_count: u64) -> Self {
        Self {
            column_id,
            row_count,
            null_count,
            min: None,
            max: None,
            min_available: false,
            max_available: false,
        }
    }

    /// Creates statistics that track bounds when the column type permits it.
    ///
    /// Bounds are tracked only for ordered types, and only when at least one row
    /// is non-NULL, because a column of nothing but NULLs has no minimum.
    #[must_use]
    pub fn with_type(
        column_id: ColumnId,
        row_count: u64,
        null_count: u64,
        column_type: ColumnType,
    ) -> Self {
        let ordered = matches!(
            column_type,
            ColumnType::Integer | ColumnType::Bytes | ColumnType::String
        );
        let track = ordered && null_count < row_count;
        Self {
            column_id,
            row_count,
            null_count,
            min: None,
            max: None,
            min_available: track,
            max_available: track,
        }
    }

    /// Folds one borrowed row value into the tracked bounds.
    ///
    /// This is the flush-path entry point: it compares the raw persisted bytes
    /// of a value against the tracked bounds, so a scan over millions of rows
    /// allocates only when a bound actually has to be replaced. `None` denotes
    /// a NULL row. A value whose encoding does not match its column's type
    /// makes both bounds permanently unavailable.
    pub fn observe_value(&mut self, value: Option<ValueRef<'_>>) {
        if !self.min_available || !self.max_available {
            return;
        }
        let Some(value) = value else {
            return;
        };
        if !value.is_well_typed() {
            return self.invalidate_bounds();
        }
        if let Some(current) = &self.min {
            match compare_value_to_field(value, current) {
                Some(Ordering::Less) => self.min = Some(value.materialize()),
                Some(_) => {}
                None => return self.invalidate_bounds(),
            }
        } else {
            self.min = Some(value.materialize());
        }
        if let Some(current) = &self.max {
            match compare_value_to_field(value, current) {
                Some(Ordering::Greater) => self.max = Some(value.materialize()),
                Some(_) => {}
                None => return self.invalidate_bounds(),
            }
        } else {
            self.max = Some(value.materialize());
        }
    }

    /// Folds one value into the tracked bounds.
    ///
    /// NULL values are ignored. A value that cannot be ordered against a bound
    /// makes both bounds permanently unavailable, so the segment stops claiming
    /// a minimum it can no longer prove.
    pub fn observe(&mut self, value: &Field) {
        if !self.min_available || !self.max_available || matches!(value, Field::Null) {
            return;
        }
        match &self.min {
            Some(current) => match compare_fields(value, current) {
                Some(Ordering::Less) => self.min = Some(value.clone()),
                Some(_) => {}
                None => return self.invalidate_bounds(),
            },
            None => self.min = Some(value.clone()),
        }
        match &self.max {
            Some(current) => match compare_fields(value, current) {
                Some(Ordering::Greater) => self.max = Some(value.clone()),
                Some(_) => {}
                None => return self.invalidate_bounds(),
            },
            None => self.max = Some(value.clone()),
        }
    }

    /// Drops both bounds and marks them unavailable.
    fn invalidate_bounds(&mut self) {
        self.min = None;
        self.max = None;
        self.min_available = false;
        self.max_available = false;
    }

    /// Returns the statistics flag bitmask describing what is held.
    ///
    /// A bound is only advertised when the column both tracks it and actually
    /// holds a value for it, so the flag byte and the payload cannot disagree.
    #[must_use]
    pub fn flags(&self) -> u8 {
        let mut flags = format::STATS_ROW_COUNT_AVAILABLE | format::STATS_NULL_COUNT_AVAILABLE;
        if self.min_available && self.min.is_some() {
            flags |= format::STATS_MIN_AVAILABLE;
        }
        if self.max_available && self.max.is_some() {
            flags |= format::STATS_MAX_AVAILABLE;
        }
        flags
    }

    /// Returns true when a minimum is held.
    #[must_use]
    pub fn min_is_available(&self) -> bool {
        self.min_available && self.min.is_some()
    }

    /// Returns true when a maximum is held.
    #[must_use]
    pub fn max_is_available(&self) -> bool {
        self.max_available && self.max.is_some()
    }

    /// Returns the fraction of rows that are NULL.
    #[must_use]
    pub fn null_fraction(&self) -> f64 {
        if self.row_count == 0 {
            0.0
        } else {
            self.null_count as f64 / self.row_count as f64
        }
    }

    /// Returns the fraction of rows that are non-NULL.
    #[must_use]
    pub fn non_null_fraction(&self) -> f64 {
        1.0 - self.null_fraction()
    }

    /// Returns true when `value` could occur in this column.
    ///
    /// Returns `true` whenever a bound is not held or not comparable, because
    /// pruning may only discard a segment when the statistics prove it cannot
    /// match.
    #[must_use]
    pub fn may_contain(&self, value: &Field) -> bool {
        if let Some(min) = &self.min {
            if self.min_available {
                if let Some(Ordering::Less) = compare_fields(value, min) {
                    return false;
                }
            }
        }
        if let Some(max) = &self.max {
            if self.max_available {
                if let Some(Ordering::Greater) = compare_fields(value, max) {
                    return false;
                }
            }
        }
        true
    }
}
impl ColumnStatistics {
    /// Encodes the persisted statistics payload.
    ///
    /// An unheld bound is persisted as [`Field::Null`] and its flag bit is
    /// omitted, which is what makes the flag byte authoritative on read.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let min = self.min.clone().unwrap_or(Field::Null);
        let max = self.max.clone().unwrap_or(Field::Null);
        Row::new(vec![min, max]).encode()
    }

    /// Encodes the payload together with the flag byte that describes it.
    pub fn encode_with_flags(&self) -> Result<(Vec<u8>, u8)> {
        Ok((self.encode()?, self.flags()))
    }

    /// Decodes a persisted statistics payload into a copy of `self`.
    ///
    /// `self` supplies the row and null counts that the payload does not carry;
    /// `flags` states which bounds the segment claims to hold.
    pub fn decode_with(&self, bytes: &[u8], flags: u8) -> Result<Self> {
        let row = Row::decode(bytes)?;
        let fields = row.fields();
        if fields.len() != STATISTICS_FIELD_COUNT {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "column statistics payload has an unexpected field count",
            ));
        }
        let mut stats = self.clone();
        stats.min_available = flags & format::STATS_MIN_AVAILABLE != 0;
        stats.max_available = flags & format::STATS_MAX_AVAILABLE != 0;
        stats.min = Self::decode_bound(&fields[0], stats.min_available, "minimum")?;
        stats.max = Self::decode_bound(&fields[1], stats.max_available, "maximum")?;
        Ok(stats)
    }

    /// Validates and decodes one persisted bound slot.
    ///
    /// A bound whose flag is set but whose payload is NULL is corruption: the
    /// segment promised a bound it did not write.
    fn decode_bound(field: &Field, available: bool, what: &str) -> Result<Option<Field>> {
        match (available, field) {
            (false, _) => Ok(None),
            (true, Field::Null) => Err(PlomidError::new(
                ErrorKind::Corruption,
                format!("column statistics advertise a {what} whose payload is NULL"),
            )),
            (true, value) => Ok(Some(value.clone())),
        }
    }
}

impl fmt::Display for ColumnStatistics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ColumnStatistics {{ column: {}, rows: {}, nulls: {} ({:.2}%), min: {:?}, max: {:?} }}",
            self.column_id.get(),
            self.row_count,
            self.null_count,
            self.null_fraction() * 100.0,
            self.min,
            self.max
        )
    }
}
/// Statistics for every column of a segment.
///
/// Columns are indexed by identity so that a reader can look up the statistics
/// of a column without scanning the segment's column list.
#[derive(Clone, Debug, Default)]
pub struct SegmentStatistics {
    /// Per-column statistics indexed by column identity.
    columns: Vec<ColumnStatistics>,
}

impl SegmentStatistics {
    /// Creates an empty statistics set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts the statistics of one column.
    pub fn add_column(&mut self, stats: ColumnStatistics) {
        let idx = stats.column_id.get() as usize;
        if idx >= self.columns.len() {
            self.columns
                .resize(idx + 1, ColumnStatistics::new(ColumnId::new(0), 0, 0));
        }
        self.columns[idx] = stats;
    }

    /// Returns the statistics recorded for `column_id`.
    #[must_use]
    pub fn get(&self, column_id: ColumnId) -> Option<&ColumnStatistics> {
        self.columns
            .get(column_id.get() as usize)
            .filter(|stats| stats.column_id == column_id)
    }

    /// Returns every recorded column's statistics, ordered by identity.
    #[must_use]
    pub fn all(&self) -> &[ColumnStatistics] {
        &self.columns
    }

    /// Returns the number of recorded columns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// Returns true when no statistics have been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}
