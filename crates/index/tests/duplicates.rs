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
//! Duplicate and uniqueness semantics through the public API.
//!
//! Unique indexes hold at most one RowId per key and reject a second,
//! different RowId with [`ArtError::DuplicateKey`]; non-unique indexes keep a
//! sorted, de-duplicated set of RowIds per key, where a repeated insert of the
//! same RowId is idempotent.

mod common;

use common::{assert_matches, row, Reference};
use plomid_index::{ArtError, ArtIndex, LeafInsert, LeafRemove};

#[test]
fn unique_index_rejects_a_second_row_for_the_same_key() {
    let mut index = ArtIndex::unique();
    let mut reference = Reference::default();

    assert_eq!(
        index.insert(b"user:7", row(7)).unwrap(),
        LeafInsert::Inserted
    );
    reference.insert_unique(b"user:7", row(7)).unwrap();

    // Same RowId again is idempotent, not a conflict.
    assert_eq!(
        index.insert(b"user:7", row(7)).unwrap(),
        LeafInsert::AlreadyPresent
    );
    reference.insert_unique(b"user:7", row(7)).unwrap();

    // A different RowId for the same key is a uniqueness violation.
    assert_eq!(index.insert(b"user:7", row(8)), Err(ArtError::DuplicateKey));
    assert_eq!(
        reference.insert_unique(b"user:7", row(8)),
        Err(ArtError::DuplicateKey)
    );

    // The failed insert changed nothing observable.
    assert_matches(&index, &reference);
    assert_eq!(index.lookup_unique(b"user:7"), Some(row(7)));
    assert_eq!(index.len(), 1);
}

#[test]
fn non_unique_index_collects_many_rows_per_key() {
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    for row_id in [30u64, 10, 20, 10] {
        index.insert(b"tag:rust", row(row_id)).unwrap();
        reference.insert(b"tag:rust", row(row_id));
    }

    assert_matches(&index, &reference);
    assert_eq!(index.key_count(), 1, "four inserts, one key");
    assert_eq!(index.len(), 3, "the repeated RowId 10 counts once");

    // Rows come back sorted, de-duplicated — the secondary-index contract.
    let found: Vec<u64> = index
        .lookup(b"tag:rust")
        .expect("key present")
        .iter()
        .map(|row_id| row_id.get())
        .collect();
    assert_eq!(found, vec![10, 20, 30]);
}

#[test]
fn deleting_one_row_keeps_the_other_rows_for_the_key() {
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    for row_id in [1u64, 2, 3] {
        index.insert(b"shared", row(row_id)).unwrap();
        reference.insert(b"shared", row(row_id));
    }

    // Remove the middle row: the key survives with its remaining rows.
    assert_eq!(
        index.delete(b"shared", row(2)).unwrap(),
        LeafRemove::Removed
    );
    assert!(reference.delete(b"shared", row(2)));
    assert_matches(&index, &reference);
    assert!(index.contains(b"shared"));

    // Re-deleting the same row reports absence.
    assert_eq!(
        index.delete(b"shared", row(2)).unwrap(),
        LeafRemove::NotFound
    );

    // Removing the last two rows removes the key itself.
    assert_eq!(
        index.delete(b"shared", row(1)).unwrap(),
        LeafRemove::Removed
    );
    assert!(reference.delete(b"shared", row(1)));
    assert_eq!(
        index.delete(b"shared", row(3)).unwrap(),
        LeafRemove::Removed
    );
    assert!(reference.delete(b"shared", row(3)));
    assert_matches(&index, &reference);
    assert!(!index.contains(b"shared"));
    assert_eq!(index.lookup(b"shared"), None);
}

#[test]
fn a_freed_unique_key_accepts_a_new_row() {
    // Uniqueness is about what is *currently* stored, not history: after the
    // only row for a key is deleted, the key accepts a different row.
    let mut index = ArtIndex::unique();
    index.insert(b"slot", row(1)).unwrap();
    assert_eq!(index.delete(b"slot", row(1)).unwrap(), LeafRemove::Removed);
    assert!(!index.contains(b"slot"));

    assert_eq!(index.insert(b"slot", row(2)).unwrap(), LeafInsert::Inserted);
    assert_eq!(index.lookup_unique(b"slot"), Some(row(2)));
    index.validate().expect("valid after key reuse");
}

#[test]
fn duplicates_do_not_leak_into_enumeration_or_counts() {
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    for key in [b"dup:a".as_slice(), b"dup:b", b"dup:a"] {
        for row_id in [5u64, 5, 6] {
            index.insert(key, row(row_id)).unwrap();
            reference.insert(key, row(row_id));
        }
    }

    assert_matches(&index, &reference);
    assert_eq!(index.key_count(), 2);
    assert_eq!(index.len(), 4, "two distinct rows under each of two keys");
}
