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
//! Core index operations through the public API: insert, lookup, `contains`,
//! delete, and the empty-key case.
//!
//! Every test ends by comparing the index against the shared reference model
//! ([`common::Reference`]) and by auditing the structure with
//! [`ArtIndex::validate`], so a passing test means "the tree is correct", not
//! merely "this lookup returned the expected row".

mod common;

use common::row;
use plomid_index::{ArtIndex, LeafInsert, LeafRemove};

// ---------------------------------------------------------------------------
// Basic operations
// ---------------------------------------------------------------------------

#[test]
fn empty_index_reports_nothing() {
    let index = ArtIndex::unique();
    assert!(index.is_empty());
    assert_eq!(index.len(), 0);
    assert_eq!(index.key_count(), 0);
    assert_eq!(index.lookup(b"missing"), None);
    assert!(!index.contains(b"missing"));
    assert!(!index.contains(b""));
    assert!(index.entries().is_empty());
    index.validate().expect("an empty tree is valid");
}

#[test]
fn insert_lookup_contains_delete() {
    let mut index = ArtIndex::unique();
    assert_eq!(
        index.insert(b"alpha", row(1)).unwrap(),
        LeafInsert::Inserted
    );
    assert_eq!(index.insert(b"beta", row(2)).unwrap(), LeafInsert::Inserted);

    assert_eq!(index.lookup_unique(b"alpha"), Some(row(1)));
    assert_eq!(index.lookup(b"beta"), Some(&[row(2)][..]));
    assert!(index.contains(b"alpha"));
    assert_eq!(index.len(), 2);
    assert_eq!(index.key_count(), 2);

    assert_eq!(index.delete(b"alpha", row(1)).unwrap(), LeafRemove::Removed);
    assert_eq!(index.lookup(b"alpha"), None);
    assert!(!index.contains(b"alpha"));
    assert_eq!(index.len(), 1);
    index.validate().expect("valid after delete");

    // Deleting the same row twice reports absence the second time.
    assert_eq!(
        index.delete(b"alpha", row(1)).unwrap(),
        LeafRemove::NotFound
    );
    // Deleting a missing key is equally harmless.
    assert_eq!(
        index.delete(b"gamma", row(9)).unwrap(),
        LeafRemove::NotFound
    );
    assert_eq!(
        index.delete(b"beta", row(99)).unwrap(),
        LeafRemove::NotFound
    );
    assert_eq!(index.lookup_unique(b"beta"), Some(row(2)));

    index.validate().expect("valid after misses");
}

#[test]
fn empty_key_is_supported() {
    let mut index = ArtIndex::unique();
    // The empty key is a legitimate byte string and must not be special-cased
    // away by prefix compression.
    assert_eq!(index.insert(b"", row(5)).unwrap(), LeafInsert::Inserted);
    assert!(index.contains(b""));
    assert_eq!(index.lookup_unique(b""), Some(row(5)));

    // A key that extends the empty key coexists with it.
    assert_eq!(index.insert(b"x", row(6)).unwrap(), LeafInsert::Inserted);
    assert!(index.contains(b""));
    assert!(index.contains(b"x"));
    assert_eq!(index.len(), 2);

    // Removing the empty key leaves the longer key untouched.
    assert_eq!(index.delete(b"", row(5)).unwrap(), LeafRemove::Removed);
    assert!(!index.contains(b""));
    assert_eq!(index.lookup_unique(b"x"), Some(row(6)));
    index.validate().expect("valid after empty-key delete");
}

#[test]
fn emptied_tree_collapses_to_an_empty_root() {
    let mut index = ArtIndex::unique();
    for (key, row_id) in [(b"a".as_ref(), 1u64), (b"ab", 2), (b"abc", 3)] {
        index.insert(key, row(row_id)).unwrap();
    }
    // Delete in a different order than insertion, which drives splits, merges
    // and shrinks in the opposite direction.
    assert_eq!(index.delete(b"ab", row(2)).unwrap(), LeafRemove::Removed);
    assert_eq!(index.delete(b"a", row(1)).unwrap(), LeafRemove::Removed);
    assert_eq!(index.delete(b"abc", row(3)).unwrap(), LeafRemove::Removed);

    assert!(index.is_empty());
    assert_eq!(index.entries(), Vec::new());
    assert_eq!(index.root().count(), 0, "no child survives");
    assert!(
        index.root().prefix().is_empty(),
        "prefix metadata is cleared by the final merge"
    );
    index.validate().expect("an emptied tree is valid");
}

#[test]
fn absent_keys_sharing_a_prefix_are_not_confused_with_present_ones() {
    let mut index = ArtIndex::unique();
    index.insert(b"abcdef", row(1)).unwrap();

    // Every strict prefix and every extension of the stored key must miss.
    for key in [
        b"".as_ref(),
        b"a",
        b"ab",
        b"abc",
        b"abcd",
        b"abcde",
        b"abcdefg",
        b"abczzz",
        b"z",
    ] {
        assert_eq!(index.lookup(key), None, "lookup {key:?} must miss");
        assert!(!index.contains(key), "contains {key:?} must miss");
        assert_eq!(
            index.delete(key, row(1)).unwrap(),
            LeafRemove::NotFound,
            "delete {key:?} must miss"
        );
    }
    // The stored key is untouched by the failed operations above.
    assert_eq!(index.lookup_unique(b"abcdef"), Some(row(1)));
    index.validate().expect("valid after misses");
}
