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
//! Shared harness for the ART integration tests.
//!
//! This module lives in `tests/common/` on purpose: files in a subdirectory of
//! `tests/` are not compiled as their own test binaries, so several small test
//! files can share one reference model without duplicating it.
//!
//! Two independent reference models are used so a structural bug cannot hide
//! behind an implementation of the same idea:
//!
//! * [`Reference`] — an ordered `key → set of RowIds` model (compared exactly,
//!   including enumeration order and counts).
//! * a plain `HashMap<Vec<u8>, BTreeSet<u64>>` — used by the randomized test as
//!   a second opinion on lookup and membership.
//!
//! Randomized tests are deterministic: the PRNG is seeded from a test-provided
//! string, so any failure can be replayed exactly.

// Not every test file uses every helper; that is expected and not a defect.
#![allow(dead_code)]

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use plomid_core::RowId;
use plomid_index::{ArtError, ArtIndex};

/// Small helper so tests read like `row(7)` instead of `RowId::new(7)`.
pub fn row(value: u64) -> RowId {
    RowId::new(value)
}

/// Reference model: `key → set of RowIds`, exactly what an index must contain.
#[derive(Default)]
pub struct Reference {
    map: BTreeMap<Vec<u8>, BTreeSet<u64>>,
}

impl Reference {
    /// Mirrors [`ArtIndex::insert`] for a non-unique index.
    pub fn insert(&mut self, key: &[u8], row_id: RowId) -> bool {
        self.map
            .entry(key.to_vec())
            .or_default()
            .insert(row_id.get())
    }

    /// Mirrors [`ArtIndex::insert`] for a unique index: a second, different row
    /// for the same key is rejected.
    pub fn insert_unique(&mut self, key: &[u8], row_id: RowId) -> Result<bool, ArtError> {
        let rows = self.map.entry(key.to_vec()).or_default();
        if rows.contains(&row_id.get()) {
            return Ok(false);
        }
        if !rows.is_empty() {
            return Err(ArtError::DuplicateKey);
        }
        rows.insert(row_id.get());
        Ok(true)
    }

    /// Mirrors [`ArtIndex::delete`].
    pub fn delete(&mut self, key: &[u8], row_id: RowId) -> bool {
        let Some(rows) = self.map.get_mut(key) else {
            return false;
        };
        let removed = rows.remove(&row_id.get());
        if rows.is_empty() {
            self.map.remove(key);
        }
        removed
    }

    /// The RowIds the model holds for `key`, in ascending order.
    pub fn lookup(&self, key: &[u8]) -> Option<Vec<u64>> {
        self.map.get(key).map(|rows| rows.iter().copied().collect())
    }

    /// Number of `key → RowId` pairs.
    pub fn pair_count(&self) -> usize {
        self.map.values().map(BTreeSet::len).sum()
    }

    /// Number of distinct keys.
    pub fn key_count(&self) -> usize {
        self.map.len()
    }

    /// Returns true when `key` is present in the model.
    pub fn contains(&self, key: &[u8]) -> bool {
        self.map.contains_key(key)
    }

    /// Every `key → RowId` pair in ascending key, then ascending RowId order.
    pub fn pairs(&self) -> Vec<(Vec<u8>, u64)> {
        self.map
            .iter()
            .flat_map(|(key, rows)| rows.iter().map(move |row_id| (key.clone(), *row_id)))
            .collect()
    }

    /// Distinct keys, ascending. Used to drive lookups for absent keys too.
    pub fn keys(&self) -> Vec<Vec<u8>> {
        self.map.keys().cloned().collect()
    }
}

/// Asserts the index agrees with the reference model in every observable way.
///
/// This is the central check used by every test: structural invariants, counts,
/// per-key lookup, membership, and full enumeration order.
pub fn assert_matches(index: &ArtIndex, reference: &Reference) {
    index
        .validate()
        .unwrap_or_else(|violation| panic!("invariant violated: {violation}"));

    assert_eq!(index.len(), reference.pair_count(), "pair count");
    assert_eq!(index.key_count(), reference.key_count(), "key count");

    for (key, expected) in reference.pairs() {
        assert!(
            index.contains_row(&key, row(expected)),
            "contains_row for {key:?} → {expected}"
        );
    }

    for key in reference.keys() {
        let found = index.lookup(&key);
        let expected = reference.lookup(&key);
        let found: Option<Vec<u64>> =
            found.map(|rows| rows.iter().map(|row_id| row_id.get()).collect());
        assert_eq!(found, expected, "lookup for key {key:?}");
        assert!(index.contains(&key), "contains for key {key:?}");
    }

    let enumerated: Vec<(Vec<u8>, u64)> = index
        .entries()
        .into_iter()
        .map(|entry| (entry.key().to_vec(), entry.row_id().get()))
        .collect();
    assert_eq!(enumerated, reference.pairs(), "enumeration order");
}

/// Deterministic pseudo-random generator.
///
/// A tiny xorshift keeps the randomized tests reproducible (failures replay
/// exactly) and dependency-free, which matters more here than statistical
/// quality. `DefaultHasher` seeds the state from the caller's string so two
/// tests do not walk the same key sequence.
pub struct Rng(u64);

impl Rng {
    /// Creates a generator whose sequence is fixed by `seed`.
    pub fn new(seed: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        seed.hash(&mut hasher);
        // Any non-zero state works for xorshift; `| 1` guarantees that.
        Self(hasher.finish() | 1)
    }

    /// Returns the next pseudo-random `u64`.
    pub fn next_u64(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Returns a pseudo-random value in `0..bound` (`0` when `bound == 0`).
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// Returns a pseudo-random byte.
    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 24) as u8
    }
}

/// Builds a pseudo-random key of `len` bytes from the full byte range.
pub fn random_key(rng: &mut Rng, len: usize) -> Vec<u8> {
    (0..len).map(|_| rng.byte()).collect()
}

/// Builds a pseudo-random key drawn from a restricted alphabet.
///
/// A small alphabet creates long common prefixes and heavy prefix nesting,
/// which is the case path compression must get right.
pub fn random_key_over(rng: &mut Rng, len: usize, alphabet: &[u8]) -> Vec<u8> {
    (0..len)
        .map(|_| alphabet[rng.below(alphabet.len())])
        .collect()
}
