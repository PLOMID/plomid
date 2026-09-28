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
//! Node shrink transitions through the public API.
//!
//! The growth side is covered in `growth_shrink.rs`; these tests drive the
//! reverse path: fill a node until it grows, delete enough entries to reach
//! the shrink threshold, and verify that every surviving key is still
//! retrievable and every deleted key stays absent.

mod common;

use common::{assert_matches, row, Reference};
use plomid_index::{ArtIndex, LeafRemove};

/// Keys of the form `prefix + [byte]`, which make one node hold one child per
/// varying byte and therefore drive its family.
fn family_keys(prefix: &[u8], count: usize) -> Vec<Vec<u8>> {
    (0..count)
        .map(|index| {
            let mut key = prefix.to_vec();
            key.push(index as u8);
            key
        })
        .collect()
}

/// Inserts `keys` (RowId == position + 1) and returns the reference model.
fn build(index: &mut ArtIndex, keys: &[Vec<u8>]) -> Reference {
    let mut reference = Reference::default();
    for (position, key) in keys.iter().enumerate() {
        let row_id = row(position as u64 + 1);
        index.insert(key, row_id).unwrap();
        reference.insert(key, row_id);
    }
    reference
}

/// Deletes `keys[start..end]` (RowId == position + 1) from both structures.
fn remove_range(index: &mut ArtIndex, reference: &mut Reference, keys: &[Vec<u8>], start: usize) {
    for (offset, key) in keys[start..].iter().enumerate() {
        let row_id = row(start as u64 + offset as u64 + 1);
        assert_eq!(
            index.delete(key, row_id).unwrap(),
            LeafRemove::Removed,
            "key {key:?} must delete"
        );
        assert!(reference.delete(key, row_id), "model must agree");
    }
}

#[test]
fn node256_shrinks_to_node48_and_keeps_every_survivor() {
    let mut index = ArtIndex::unique();
    // 49 keys push the inner fanout node into Node256.
    let keys = family_keys(b"sh", 49);
    let mut reference = build(&mut index, &keys);
    // Delete the last key: 48 children remain, which is the Node256 shrink
    // threshold (see `ART_NODE256_SHRINK`).
    remove_range(&mut index, &mut reference, &keys, 48);
    assert_matches(&index, &reference);

    // The deleted key is absent; every survivor is present with its exact row.
    assert!(!index.contains(&keys[48]));
    for key in &keys[..48] {
        assert!(index.contains(key), "survivor {key:?} lost");
    }
    assert_eq!(index.key_count(), 48);
}

#[test]
fn node48_shrinks_to_node16_and_node16_shrinks_to_node4() {
    let mut index = ArtIndex::unique();
    let keys = family_keys(b"mi", 49);
    let mut reference = build(&mut index, &keys);

    // Node256 → (delete to 48) → Node48 territory → delete down to 16, which
    // is the Node48 shrink threshold (`ART_NODE48_SHRINK`).
    remove_range(&mut index, &mut reference, &keys, 16);
    assert_matches(&index, &reference);
    assert_eq!(index.key_count(), 16);
    for key in &keys[..16] {
        assert!(index.contains(key), "survivor {key:?} lost");
    }
    for key in &keys[16..] {
        assert!(!index.contains(key), "deleted key {key:?} resurrected");
    }

    // Delete down to 4, the Node16 shrink threshold (`ART_NODE16_SHRINK`).
    // Keep key 0's row in the model while removing keys 4..16.
    for (offset, key) in keys[4..16].iter().enumerate() {
        let row_id = row(4 + offset as u64 + 1);
        assert_eq!(
            index.delete(key, row_id).unwrap(),
            LeafRemove::Removed,
            "key {key:?} must delete"
        );
        assert!(reference.delete(key, row_id), "model must agree");
    }
    assert_matches(&index, &reference);
    assert_eq!(index.key_count(), 4);
    for key in &keys[..4] {
        assert!(index.contains(key), "survivor {key:?} lost");
    }
}

#[test]
fn repeated_grow_shrink_cycles_never_lose_entries() {
    // Grow to Node256, shrink back, grow again: a node must survive the round
    // trip with identical contents.
    let mut index = ArtIndex::unique();
    let keys = family_keys(b"cy", 49);
    let mut reference = build(&mut index, &keys);

    // Shrink to 10 survivors.
    remove_range(&mut index, &mut reference, &keys, 10);
    assert_matches(&index, &reference);
    assert_eq!(index.key_count(), 10);

    // Grow again with fresh keys in the same family (new RowIds continue the
    // sequence; the model tracks both generations).
    for (offset, byte) in (100u8..120).enumerate() {
        let mut key = b"cy".to_vec();
        key.push(byte);
        let row_id = row(1_000 + offset as u64);
        index.insert(&key, row_id).unwrap();
        reference.insert(&key, row_id);
    }
    assert_matches(&index, &reference);
    assert_eq!(index.key_count(), 30);

    // Shrink once more; survivors from both generations must remain.
    for (offset, byte) in (100u8..120).enumerate() {
        let mut key = b"cy".to_vec();
        key.push(byte);
        let row_id = row(1_000 + offset as u64);
        assert_eq!(
            index.delete(&key, row_id).unwrap(),
            LeafRemove::Removed,
            "key {key:?} must delete"
        );
        assert!(reference.delete(&key, row_id), "model must agree");
    }
    assert_matches(&index, &reference);
    assert_eq!(index.key_count(), 10);
}
