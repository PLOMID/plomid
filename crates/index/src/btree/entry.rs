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
//! Public entry and range-bound types for the B+Tree index.
//!
//! The tree's internal node/entry formats live in [`super::node`]; this module
//! holds the caller-facing shapes returned by search and range scans.

use plomid_core::RowId;

/// One visible index entry: an encoded key and its ordered RowId set.
///
/// For a unique index the set always contains exactly one RowId. RowIds are
/// logical references only — never physical offsets — so the authoritative
/// row data stays in the row/storage layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexEntry {
    /// Encoded, order-preserving key bytes.
    pub key: Vec<u8>,
    /// Row references for this key, sorted ascending.
    pub row_ids: Vec<RowId>,
}

/// Inclusive or exclusive range boundary for [`BTreeIndex::range_scan`].
///
/// `Unbounded` opens the range on that side. Boundaries follow the usual
/// half-open algebra, so `(start, end]`, `[start, end)`, `[start, end]`,
/// `(start, end)`, and both unbounded forms are all expressible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Bound {
    /// Include keys equal to this value.
    Included(Vec<u8>),
    /// Exclude keys equal to this value.
    Excluded(Vec<u8>),
    /// No boundary on this side.
    Unbounded,
}

impl Bound {
    /// Returns the boundary key when the bound is bounded.
    #[must_use]
    pub fn key(&self) -> Option<&[u8]> {
        match self {
            Bound::Included(key) | Bound::Excluded(key) => Some(key),
            Bound::Unbounded => None,
        }
    }

    /// Returns true when the boundary value itself is inside the range.
    #[must_use]
    pub fn is_inclusive(&self) -> bool {
        matches!(self, Bound::Included(_))
    }

    /// True when the range begins unbounded.
    #[must_use]
    pub fn is_unbounded_start(start: &Bound) -> bool {
        matches!(start, Bound::Unbounded)
    }

    /// True when the range ends unbounded.
    #[must_use]
    pub fn is_unbounded_end(end: &Bound) -> bool {
        matches!(end, Bound::Unbounded)
    }
}
