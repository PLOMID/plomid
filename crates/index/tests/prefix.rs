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
//! Prefix handling: keys that are prefixes of other keys, long shared prefixes,
//! and unrelated keys that share only part of a prefix.
//!
//! Prefix bugs are the classic way to lose a terminal key, so every test here
//! checks `contains` for *both* the shorter and the longer key, and finishes by
//! comparing the whole index against the reference model.

mod common;

use common::{assert_matches, row, Reference};
use plomid_index::{ArtIndex, LeafRemove};

#[test]
fn ascending_prefix_chain() {
    // The exact example from the specification: a, ab, abc, abcd.
    let mut index = ArtIndex::unique();
    let mut reference = Reference::default();

    for (step, key) in [b"a".as_ref(), b"ab", b"abc", b"abcd"].iter().enumerate() {
        let row_id = row(step as u64 + 1);
        index.insert(key, row_id).unwrap();
        reference.insert_unique(key, row_id).unwrap();
    }
    assert_matches(&index, &reference);

    // Every key in the chain is independently retrievable.
    assert_eq!(index.lookup_unique(b"a"), Some(row(1)));
    assert_eq!(index.lookup_unique(b"ab"), Some(row(2)));
    assert_eq!(index.lookup_unique(b"abc"), Some(row(3)));
    assert_eq!(index.lookup_unique(b"abcd"), Some(row(4)));

    // Removing the innermost key leaves the outer ones intact.
    index.delete(b"abc", row(3)).unwrap();
    reference.delete(b"abc", row(3));
    assert!(!index.contains(b"abc"));
    assert!(index.contains(b"ab"));
    assert!(index.contains(b"abcd"));
    assert_matches(&index, &reference);
}

#[test]
fn descending_prefix_chain() {
    // The reverse insertion order of the same key set: abc, ab, a.
    let mut index = ArtIndex::unique();
    let mut reference = Reference::default();

    for (key, row_id) in [(b"abc".as_ref(), 3u64), (b"ab", 2), (b"a", 1)] {
        index.insert(key, row(row_id)).unwrap();
        reference.insert_unique(key, row(row_id)).unwrap();
    }
    assert_matches(&index, &reference);

    assert_eq!(index.lookup_unique(b"abc"), Some(row(3)));
    assert_eq!(index.lookup_unique(b"ab"), Some(row(2)));
    assert_eq!(index.lookup_unique(b"a"), Some(row(1)));

    // Deleting from the outside in drains the chain completely.
    for (key, row_id) in [(b"a".as_ref(), 1u64), (b"ab", 2), (b"abc", 3)] {
        index.delete(key, row(row_id)).unwrap();
        reference.delete(key, row(row_id));
        assert_matches(&index, &reference);
    }
    assert!(index.is_empty());
}

#[test]
fn interleaved_prefix_chain() {
    // Insertion order chosen so that both splits and in-node extensions happen:
    // the middle key is inserted first, then grown in both directions.
    let mut index = ArtIndex::unique();
    let mut reference = Reference::default();

    for (key, row_id) in [(b"ab".as_ref(), 2u64), (b"a", 1), (b"abcd", 4), (b"abc", 3)] {
        index.insert(key, row(row_id)).unwrap();
        reference.insert_unique(key, row(row_id)).unwrap();
    }
    assert_matches(&index, &reference);

    // Removing the middle keys keeps both the shortest and the longest key.
    index.delete(b"ab", row(2)).unwrap();
    reference.delete(b"ab", row(2));
    index.delete(b"abc", row(3)).unwrap();
    reference.delete(b"abc", row(3));
    assert_matches(&index, &reference);
    assert!(!index.contains(b"ab"));
    assert!(!index.contains(b"abc"));
    assert!(index.contains(b"a"));
    assert!(index.contains(b"abcd"));
}

#[test]
fn foo_prefix_family() {
    // foo, foobar, foobarbaz: a long shared prefix with a terminal key in the
    // middle of the chain.
    let mut index = ArtIndex::unique();
    let mut reference = Reference::default();

    for (key, row_id) in [
        (b"foo".as_ref(), 10u64),
        (b"foobar", 20),
        (b"foobarbaz", 30),
    ] {
        index.insert(key, row(row_id)).unwrap();
        reference.insert_unique(key, row(row_id)).unwrap();
    }
    assert_matches(&index, &reference);

    assert_eq!(index.lookup_unique(b"foo"), Some(row(10)));
    assert_eq!(index.lookup_unique(b"foobar"), Some(row(20)));
    assert_eq!(index.lookup_unique(b"foobarbaz"), Some(row(30)));
    // Near misses differing only in the last byte must not match.
    assert_eq!(index.lookup(b"fooba"), None);
    assert_eq!(index.lookup(b"foobarba"), None);
    assert_eq!(index.lookup(b"foobarbazz"), None);

    // Delete the middle key: the long prefix must be re-split, not corrupted.
    index.delete(b"foobar", row(20)).unwrap();
    reference.delete(b"foobar", row(20));
    assert_matches(&index, &reference);
    assert!(index.contains(b"foo"));
    assert!(index.contains(b"foobarbaz"));
    assert_eq!(
        index.delete(b"foobarbaz", row(30)).unwrap(),
        LeafRemove::Removed
    );
    reference.delete(b"foobarbaz", row(30));
    assert_matches(&index, &reference);
}

#[test]
fn unrelated_keys_with_partial_shared_prefix() {
    // Keys that share a lead-in but diverge: the node must keep both branches
    // and neither may shadow the other.
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    let pairs: [(&[u8], u64); 8] = [
        (b"prefix-one", 1),
        (b"prefix-two", 2),
        (b"prefixed", 3),
        (b"prefix", 4),
        (b"pre", 5),
        (b"pref", 6),
        (b"unrelated", 7),
        (b"p", 8),
    ];
    for (key, row_id) in pairs {
        index.insert(key, row(row_id)).unwrap();
        reference.insert(key, row(row_id));
    }
    assert_matches(&index, &reference);

    for (key, row_id) in pairs {
        assert_eq!(index.lookup_unique(key), Some(row(row_id)), "key {key:?}");
    }
    // A key sharing the whole lead-in but continuing differently is absent.
    assert_eq!(index.lookup(b"prefix-three"), None);
    assert_eq!(index.lookup(b"pr"), None);
}

#[test]
fn long_common_prefix_is_compressed_and_preserved() {
    // A 512-byte shared prefix forces deep path compression; the two keys then
    // differ only in their final byte.
    let mut shared = vec![b'x'; 512];
    let mut left = shared.clone();
    left.push(b'a');
    shared.push(b'b');
    let right = shared;

    let mut index = ArtIndex::non_unique();
    index.insert(&left, row(1)).unwrap();
    index.insert(&right, row(2)).unwrap();

    assert_eq!(index.lookup_unique(&left), Some(row(1)));
    assert_eq!(index.lookup_unique(&right), Some(row(2)));
    assert_eq!(index.key_count(), 2);

    // Introducing a key that ends *inside* the compressed prefix splits it.
    let middle = vec![b'x'; 256];
    index.insert(&middle, row(3)).unwrap();
    assert_eq!(index.lookup_unique(&middle), Some(row(3)));
    assert_eq!(index.lookup_unique(&left), Some(row(1)));
    assert_eq!(index.lookup_unique(&right), Some(row(2)));
    index
        .validate()
        .expect("valid after a split inside a long prefix");

    // Removing the middle key merges the halves again.
    index.delete(&middle, row(3)).unwrap();
    assert!(!index.contains(&middle));
    assert_eq!(index.lookup_unique(&left), Some(row(1)));
    assert_eq!(index.lookup_unique(&right), Some(row(2)));
    index.validate().expect("valid after re-merging");
}

#[test]
fn every_prefix_of_a_chain_is_retrievable() {
    // Insert all prefixes of one long key, then verify retrieval for all of
    // them, including the shortest (single byte) and the empty key.
    let key: Vec<u8> = b"0123456789abcdef".to_vec();
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    index.insert(b"", row(0)).unwrap();
    reference.insert(b"", row(0));
    for length in 1..=key.len() {
        let prefix = &key[..length];
        let row_id = row(length as u64);
        index.insert(prefix, row_id).unwrap();
        reference.insert(prefix, row_id);
    }
    assert_matches(&index, &reference);

    // Delete from the longest to the shortest: each removal steals a terminal
    // slot, leaving a pure chain that must merge back down to a single node.
    for length in (1..=key.len()).rev() {
        let prefix = &key[..length];
        let row_id = row(length as u64);
        assert_eq!(index.delete(prefix, row_id).unwrap(), LeafRemove::Removed);
        reference.delete(prefix, row_id);
        assert!(!index.contains(prefix), "prefix {prefix:?} removed");
        assert_matches(&index, &reference);
    }
    assert_eq!(index.len(), 1, "only the empty key is left");
    assert!(index.contains(b""));
    index.delete(b"", row(0)).unwrap();
    assert!(index.is_empty());
}
