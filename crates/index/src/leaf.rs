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
//! Leaf values: the logical RowId references stored under one key.
//!
//! A leaf either holds an empty posting list (the key is absent) or one or
//! more logical [`RowId`] values (the key is present). The same
//! representation therefore serves both index kinds:
//!
//! * **unique index** — at most one RowId per key; a second, *different*
//!   RowId is rejected as a duplicate-key conflict.
//! * **non-unique index** — many RowIds per key.
//!
//! Only logical RowIds are stored. No page id, file offset, buffer pointer, or
//! memory address ever appears here, so an ART stays valid when the storage
//! layer relocates a row.
//!
//! # Representation
//!
//! Values are kept sorted ascending. That gives deterministic iteration order
//! (useful for tests, benchmarks, and stable SQL result ordering), makes
//! duplicate detection a binary search, and keeps the common single-value case
//! to one `Vec` allocation. This is intentionally a plain `Vec<RowId>` rather
//! than a dedicated posting-list subsystem: the representation is small and
//! can be replaced (for example by page-backed lists for very hot keys) without
//! touching traversal code, because traversal only uses the methods below.

use plomid_core::RowId;

use crate::error::ArtError;

/// Outcome of inserting a RowId into a leaf.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafInsert {
    /// The RowId was not previously present and has been added.
    Inserted,
    /// The RowId was already present; the leaf was left unchanged.
    AlreadyPresent,
}

/// Outcome of removing a RowId from a leaf.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafRemove {
    /// The RowId was present and has been removed.
    Removed,
    /// The RowId was not present; the leaf was left unchanged.
    NotFound,
}

/// The logical RowIds stored for a single key.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeafValues {
    values: Vec<RowId>,
}

impl LeafValues {
    /// Creates a leaf holding a single RowId.
    #[must_use]
    pub fn single(row_id: RowId) -> Self {
        Self {
            values: vec![row_id],
        }
    }

    /// Creates a leaf from a set of RowIds, sorting and de-duplicating them.
    #[must_use]
    pub fn from_rows(rows: impl IntoIterator<Item = RowId>) -> Self {
        let mut values: Vec<RowId> = rows.into_iter().collect();
        values.sort_unstable();
        values.dedup();
        Self { values }
    }

    /// Returns the number of RowIds stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns true when no RowId is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns true when `row_id` is present.
    #[must_use]
    pub fn contains(&self, row_id: RowId) -> bool {
        self.values.binary_search(&row_id).is_ok()
    }

    /// Returns the first (lowest) RowId, if any.
    ///
    /// A unique-index lookup uses this to avoid copying the vector.
    #[must_use]
    pub fn first(&self) -> Option<RowId> {
        self.values.first().copied()
    }

    /// Iterates over the stored RowIds in ascending order.
    pub fn iter(&self) -> std::slice::Iter<'_, RowId> {
        self.values.iter()
    }

    /// Borrows the RowIds as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[RowId] {
        self.values.as_slice()
    }

    /// Inserts a RowId, allowing multiple RowIds per key (non-unique index).
    ///
    /// Inserting an already present RowId is idempotent and reported as
    /// [`LeafInsert::AlreadyPresent`]; an existing entry is never silently
    /// overwritten.
    pub fn insert(&mut self, row_id: RowId) -> LeafInsert {
        match self.values.binary_search(&row_id) {
            Ok(_) => LeafInsert::AlreadyPresent,
            Err(position) => {
                self.values.insert(position, row_id);
                LeafInsert::Inserted
            }
        }
    }

    /// Inserts a RowId into a unique leaf.
    ///
    /// Re-inserting the *same* RowId is idempotent. Inserting a *different*
    /// RowId for a key that is already occupied is a duplicate-key conflict,
    /// mapped onto the engine's existing `PL-CONFLICT` error kind.
    pub fn insert_unique(&mut self, row_id: RowId) -> Result<LeafInsert, ArtError> {
        match self.values.first() {
            None => {
                self.values.push(row_id);
                Ok(LeafInsert::Inserted)
            }
            Some(existing) if *existing == row_id => Ok(LeafInsert::AlreadyPresent),
            Some(_) => Err(ArtError::DuplicateKey),
        }
    }

    /// Removes a RowId, keeping the leaf's sort order.
    pub fn remove(&mut self, row_id: RowId) -> LeafRemove {
        match self.values.binary_search(&row_id) {
            Ok(position) => {
                self.values.remove(position);
                LeafRemove::Removed
            }
            Err(_) => LeafRemove::NotFound,
        }
    }

    /// Approximate heap memory attributable to this leaf, in bytes.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.values.capacity() * std::mem::size_of::<RowId>()
    }
}
#[cfg(test)]
mod tests {
    use super::{LeafInsert, LeafRemove, LeafValues};
    use crate::error::ArtError;
    use plomid_core::RowId;

    fn rid(value: u64) -> RowId {
        RowId::new(value)
    }

    #[test]
    fn non_unique_leaf_keeps_every_row() {
        let mut leaf = LeafValues::default();
        assert_eq!(leaf.insert(rid(9)), LeafInsert::Inserted);
        assert_eq!(leaf.insert(rid(3)), LeafInsert::Inserted);
        assert_eq!(leaf.insert(rid(7)), LeafInsert::Inserted);
        // A duplicate RowId is idempotent, never a silent overwrite.
        assert_eq!(leaf.insert(rid(7)), LeafInsert::AlreadyPresent);

        assert_eq!(leaf.len(), 3);
        let rows: Vec<u64> = leaf.iter().map(|row| row.get()).collect();
        // Sorted ascending for deterministic iteration.
        assert_eq!(rows, vec![3, 7, 9]);
    }

    #[test]
    fn unique_leaf_rejects_a_second_distinct_row() {
        let mut leaf = LeafValues::default();
        assert_eq!(leaf.insert_unique(rid(1)), Ok(LeafInsert::Inserted));
        assert_eq!(leaf.insert_unique(rid(1)), Ok(LeafInsert::AlreadyPresent));
        assert_eq!(leaf.insert_unique(rid(2)), Err(ArtError::DuplicateKey));
        assert_eq!(leaf.as_slice(), &[rid(1)]);
    }

    #[test]
    fn removal_reports_missing_rows() {
        let mut leaf = LeafValues::from_rows([rid(1), rid(2), rid(1)]);
        assert_eq!(leaf.as_slice(), &[rid(1), rid(2)]);
        assert_eq!(leaf.remove(rid(1)), LeafRemove::Removed);
        assert_eq!(leaf.remove(rid(1)), LeafRemove::NotFound);
        assert_eq!(leaf.remove(rid(2)), LeafRemove::Removed);
        assert!(leaf.is_empty());
        assert_eq!(leaf.first(), None);
    }

    #[test]
    fn single_leaf_reports_membership_without_copying() {
        let leaf = LeafValues::single(rid(42));
        assert_eq!(leaf.first(), Some(rid(42)));
        assert!(leaf.contains(rid(42)));
        assert!(!leaf.contains(rid(43)));
        assert_eq!(leaf.memory_bytes(), std::mem::size_of::<RowId>());
    }
}
