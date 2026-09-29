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
//! Node growth and shrinkage through the public API.
//!
//! The node families are internal, so these tests force transitions with key
//! shapes (a fixed prefix plus one varying byte) and then observe the resulting
//! families through [`ArtIndex::stats`]. Every transition is followed by a full
//! "all previously inserted keys are still retrievable" sweep, because the
//! interesting failure mode is not the family itself but a lost entry.

mod common;

use common::{assert_matches, row, Reference};
use plomid_index::ArtIndex;

/// Family indices in [`plomid_index::ArtStats::node_counts`].
const NODE4: usize = 0;
const NODE16: usize = 1;
const NODE48: usize = 2;
const NODE256: usize = 3;

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

/// Inserts `keys` (RowId == position) and returns the reference model.
fn build(index: &mut ArtIndex, keys: &[Vec<u8>]) -> Reference {
    let mut reference = Reference::default();
    for (position, key) in keys.iter().enumerate() {
        let row_id = row(position as u64 + 1);
        index.insert(key, row_id).unwrap();
        reference.insert(key, row_id);
    }
    reference
}

#[test]
fn node4_grows_to_node16_at_the_fifth_child() {
    let mut index = ArtIndex::unique();
    // Four children still fit in a Node4.
    let keys = family_keys(b"gb", 4);
    let reference = build(&mut index, &keys);
    // Representation note: a key's tail bytes and terminal RowIds live in the
    // key's own value node, so four keys mean the root, one inner Node4, and
    // four value nodes — six Node4s in total.
    assert_eq!(
        index.stats().node_counts[NODE4],
        6,
        "root + inner + 4 value nodes"
    );
    assert_eq!(index.stats().node_counts[NODE16], 0);
    assert_matches(&index, &reference);

    // The fifth child has to grow the inner node.
    let keys = family_keys(b"gb", 5);
    let mut reference = reference;
    let row_id = row(5);
    index.insert(&keys[4], row_id).unwrap();
    reference.insert(&keys[4], row_id);

    let stats = index.stats();
    // The root stays a Node4 and each of the five keys keeps its own value
    // node; only the inner fanout node changed family.
    assert_eq!(
        stats.node_counts[NODE4], 6,
        "root + 5 value nodes stay Node4"
    );
    assert_eq!(stats.node_counts[NODE16], 1);
    assert_eq!(stats.node_counts[NODE48], 0, "no overshoot");
    assert_matches(&index, &reference);
}

#[test]
fn node16_grows_to_node48_at_the_seventeenth_child() {
    let mut index = ArtIndex::unique();
    let keys = family_keys(b"gc", 16);
    let reference = build(&mut index, &keys);
    assert_eq!(index.stats().node_counts[NODE16], 1);
    assert_matches(&index, &reference);

    let keys = family_keys(b"gc", 17);
    let mut reference = reference;
    index.insert(&keys[16], row(17)).unwrap();
    reference.insert(&keys[16], row(17));

    let stats = index.stats();
    assert_eq!(stats.node_counts[NODE48], 1);
    assert_eq!(stats.node_counts[NODE16], 0, "no overshoot");
    assert_eq!(stats.node_counts[NODE256], 0, "no overshoot");
    assert_matches(&index, &reference);
}

#[test]
fn node48_grows_to_node256_at_the_forty_ninth_child() {
    let mut index = ArtIndex::unique();
    let keys = family_keys(b"gd", 48);
    let reference = build(&mut index, &keys);
    assert_eq!(index.stats().node_counts[NODE48], 1);
    assert_matches(&index, &reference);

    let keys = family_keys(b"gd", 49);
    let mut reference = reference;
    index.insert(&keys[48], row(49)).unwrap();
    reference.insert(&keys[48], row(49));

    let stats = index.stats();
    assert_eq!(stats.node_counts[NODE256], 1);
    assert_eq!(stats.node_counts[NODE48], 0, "no overshoot");
    assert_matches(&index, &reference);
}

#[test]
fn every_growth_step_keeps_all_earlier_keys() {
    // A single keyspace that crosses all three growth points, verifying after
    // every insert that nothing was lost or altered.
    let mut index = ArtIndex::unique();
    let mut reference = Reference::default();

    for count in 1..=70usize {
        let key = {
            let mut key = b"grow".to_vec();
            key.push(count as u8);
            key
        };
        let row_id = row(count as u64);
        index.insert(&key, row_id).unwrap();
        reference.insert_unique(&key, row_id).unwrap();

        // Re-check the whole keyspace, not just the new key.
        for (earlier, expected) in reference.pairs() {
            assert!(
                index.contains_row(&earlier, row(expected)),
                "after {count} inserts, key {earlier:?} lost"
            );
        }
        index.validate().expect("valid at every growth step");
    }
    assert_matches(&index, &reference);
    assert_eq!(
        index.stats().node_counts[NODE256],
        1,
        "crossed every family"
    );
}

#[test]
fn deep_node_grows_while_preserving_long_prefixes() {
    // A long shared prefix keeps the growing node deep in the tree; the prefix
    // must survive the three family changes untouched.
    let prefix = b"the-quick-brown-fox-jumps";
    let mut index = ArtIndex::non_unique();
    let keys = family_keys(prefix, 64);
    let reference = build(&mut index, &keys);

    assert_eq!(index.stats().node_counts[NODE256], 1);
    assert_matches(&index, &reference);

    for key in &keys {
        assert!(index.contains(key), "key {key:?} survived deep growth");
        assert!(key.starts_with(prefix));
    }
}
