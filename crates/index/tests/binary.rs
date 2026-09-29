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
//! Binary-key handling.
//!
//! ART operates on bytes, so keys must not be assumed to be UTF-8 text. These
//! tests use NUL bytes, all-`0xFF` keys, high-bit bytes, embedded separators,
//! and byte sequences that are invalid UTF-8.

mod common;

use common::{assert_matches, random_key, row, Reference, Rng};
use plomid_index::{ArtIndex, LeafRemove};

#[test]
fn single_byte_boundary_keys() {
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    // 0x00 and 0xFF are the two byte values most likely to be mishandled by an
    // implementation that uses zero as a sentinel.
    for (key, row_id) in [([0u8].as_ref(), 1u64), (&[255u8], 2), (&[1u8], 3)] {
        index.insert(key, row(row_id)).unwrap();
        reference.insert(key, row(row_id));
    }
    assert_matches(&index, &reference);

    assert_eq!(index.lookup_unique(&[0u8]), Some(row(1)));
    assert_eq!(index.lookup_unique(&[255u8]), Some(row(2)));
    assert_eq!(index.lookup_unique(&[1u8]), Some(row(3)));
    // Absent single-byte keys.
    assert_eq!(index.lookup(&[2u8]), None);
    assert_eq!(index.lookup(&[254u8]), None);
}

#[test]
fn nul_bytes_are_ordinary_key_bytes() {
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    // Keys that differ only in NUL placement; a NUL-terminated implementation
    // would confuse all four.
    let keys: [[u8; 3]; 4] = [[0, 1, 0], [0, 0, 1], [1, 0, 0], [0, 0, 0]];
    for (step, key) in keys.iter().enumerate() {
        let row_id = row(step as u64 + 1);
        index.insert(key, row_id).unwrap();
        reference.insert(key, row_id);
    }
    assert_matches(&index, &reference);

    for (step, key) in keys.iter().enumerate() {
        assert_eq!(
            index.lookup_unique(key),
            Some(row(step as u64 + 1)),
            "key {key:?}"
        );
    }
}

#[test]
fn specification_binary_examples() {
    // The exact examples from the specification: [0], [0,1], [255], [0,255,1].
    let mut index = ArtIndex::unique();
    let mut reference = Reference::default();

    let keys: [&[u8]; 4] = [&[0], &[0, 1], &[255], &[0, 255, 1]];
    for (step, key) in keys.iter().enumerate() {
        let row_id = row(step as u64 + 1);
        index.insert(key, row_id).unwrap();
        reference.insert_unique(key, row_id).unwrap();
    }
    assert_matches(&index, &reference);

    for (step, key) in keys.iter().enumerate() {
        assert!(
            index.contains_row(key, row(step as u64 + 1)),
            "key {key:?} must resolve to its own row"
        );
    }

    // Delete one and confirm the reference model follows.
    assert_eq!(index.delete(&[0, 1], row(2)).unwrap(), LeafRemove::Removed);
    reference.delete(&[0, 1], row(2));
    assert!(!index.contains(&[0, 1]));
    assert_matches(&index, &reference);
}

#[test]
fn invalid_utf8_keys_are_accepted() {
    // 0xC0/0x80 are not valid UTF-8; ART must not care.
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    let keys: [&[u8]; 5] = [
        &[0xC0, 0x80],
        &[0xFE, 0xFF],
        &[0x80],
        &[0xED, 0xA0, 0x80],
        &[b'a', 0x80, b'b'],
    ];
    for (step, key) in keys.iter().enumerate() {
        let row_id = row(step as u64 + 10);
        index.insert(key, row_id).unwrap();
        reference.insert(key, row_id);
    }
    assert_matches(&index, &reference);
}

#[test]
fn variable_length_keys_sharing_a_lead_byte() {
    // Every key starts with the same byte, so the root's child is a single
    // subtree that must contain keys of many different lengths.
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    for length in 0..=64usize {
        let mut key = vec![0x7Fu8];
        key.extend(std::iter::repeat_n(0xABu8, length));
        let row_id = row(length as u64 + 1);
        index.insert(&key, row_id).unwrap();
        reference.insert(&key, row_id);
    }
    assert_matches(&index, &reference);

    // Remove every second key, then re-verify the survivors.
    for length in (0..=64usize).step_by(2) {
        let mut key = vec![0x7Fu8];
        key.extend(std::iter::repeat_n(0xABu8, length));
        assert_eq!(
            index.delete(&key, row(length as u64 + 1)).unwrap(),
            LeafRemove::Removed
        );
        reference.delete(&key, row(length as u64 + 1));
    }
    assert_matches(&index, &reference);
}

#[test]
fn long_binary_keys() {
    // 4 KiB keys: long enough that a stack-allocated fixed buffer would fail,
    // and that any accidental key copy becomes visible in the benchmark.
    let mut rng = Rng::new("binary::long_binary_keys");
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    for step in 0..32u64 {
        let key = random_key(&mut rng, 4096);
        index.insert(&key, row(step)).unwrap();
        reference.insert(&key, row(step));
    }
    assert_matches(&index, &reference);

    for (key, _) in reference.pairs() {
        assert!(index.contains(&key));
    }
}

#[test]
fn full_byte_alphabet_at_the_root() {
    // All 256 byte values as single-byte keys: this fills a Node256 at the root
    // and proves no byte value is reserved.
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    for byte in 0..=u8::MAX {
        let key = [byte];
        let row_id = row(u64::from(byte) + 1);
        index.insert(&key, row_id).unwrap();
        reference.insert(&key, row_id);
    }
    assert_matches(&index, &reference);
    assert_eq!(index.len(), 256);

    for byte in 0..=u8::MAX {
        assert!(index.contains(&[byte]), "byte {byte} present");
    }
}
