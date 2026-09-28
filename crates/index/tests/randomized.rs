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
//! Randomized testing against a reference model.
//!
//! A deterministic pseudo-random generator (from `tests/common`) drives mixed
//! insert/lookup/delete workloads on both a unique and a non-unique index.
//! After every operation the index is checked against a `HashMap<Key,
//! BTreeSet<RowId>>` reference model: pair counts, per-key lookups, and full
//! enumeration order. Structural invariants run inside `assert_matches` on
//! every check, so node growth, shrink, splits, and merges are all exercised
//! under random shapes.

mod common;

use std::collections::{BTreeSet, HashMap};

use common::{assert_matches, random_key_over, row, Reference, Rng};
use plomid_index::{ArtError, ArtIndex, LeafInsert, LeafRemove};

/// One scripted step of the randomized workload.
#[derive(Clone, Debug)]
enum Operation {
    /// Insert a (possibly new) RowId under a (possibly new) key.
    Insert { key: Vec<u8>, row_value: u64 },
    /// Delete a (possibly absent) RowId from a (possibly absent) key.
    Delete { key: Vec<u8>, row_value: u64 },
    /// Look up a (possibly absent) key. Checks lookup and membership only.
    Probe { key: Vec<u8> },
}

/// Runs a mixed workload against both index kinds, checking the reference
/// model after every mutating step and probing reads throughout.
///
/// Keys have random lengths in `0..=max_key_len` over `alphabet`, so empty
/// keys, prefix-nested keys, and single-byte keys all appear. A plain
/// `HashMap<Vec<u8>, BTreeSet<u64>>` shadows the ordered [`Reference`] model
/// as a second opinion: it is updated by independent code and must agree with
/// both the index and the reference on every probe.
fn run_workload(seed: &str, steps: usize, alphabet: &[u8], max_key_len: usize, row_space: u64) {
    let mut rng = Rng::new(seed);
    let mut script = Vec::with_capacity(steps);
    for _ in 0..steps {
        let roll = rng.below(100);
        let key_len = rng.below(max_key_len + 1);
        let key = random_key_over(&mut rng, key_len, alphabet);
        let row_value = rng.below(row_space as usize) as u64;
        script.push(if roll < 55 {
            Operation::Insert { key, row_value }
        } else if roll < 80 {
            Operation::Delete { key, row_value }
        } else {
            Operation::Probe { key }
        });
    }

    for kind_unique in [true, false] {
        let mut index = if kind_unique {
            ArtIndex::unique()
        } else {
            ArtIndex::non_unique()
        };
        let mut reference = Reference::default();
        // Second opinion, maintained by independent code: key -> set of rows.
        let mut shadow: HashMap<Vec<u8>, BTreeSet<u64>> = HashMap::new();

        for operation in &script {
            match operation {
                Operation::Insert { key, row_value } => {
                    let row_id = row(*row_value);
                    let outcome = if kind_unique {
                        // The reference model and the index must agree on the
                        // uniqueness decision, including idempotent repeats. A
                        // rejected duplicate leaves the model untouched, so the
                        // shadow map only records an accepted insert.
                        let expected = reference.insert_unique(key, row_id).map(|is_new| {
                            if is_new {
                                LeafInsert::Inserted
                            } else {
                                LeafInsert::AlreadyPresent
                            }
                        });
                        let found = index.insert(key, row_id);
                        assert_eq!(found, expected, "unique insert {key:?} → {row_value}");
                        match found {
                            Ok(outcome) => {
                                // The reference (and the assertion above) confirms
                                // the models agree, so an accepted insert is
                                // also safe to record in the shadow map.
                                if outcome == LeafInsert::Inserted {
                                    shadow.entry(key.clone()).or_default().insert(row_id.get());
                                }
                                outcome
                            }
                            Err(ArtError::DuplicateKey) => {
                                // Both sides rejected the duplicate; nothing is
                                // recorded and the models stay in agreement.
                                assert!(
                                    expected.is_err(),
                                    "index and reference must reject together"
                                );
                                return assert_matches(&index, &reference);
                            }
                            Err(other) => panic!("unexpected insert error: {other:?}"),
                        }
                    } else {
                        let is_new = reference.insert(key, row_id);
                        let outcome = index
                            .insert(key, row_id)
                            .expect("non-unique insert never fails");
                        // `is_new` and the reported outcome must match.
                        assert_eq!(outcome == LeafInsert::Inserted, is_new);
                        shadow.entry(key.clone()).or_default().insert(row_id.get());
                        outcome
                    };
                    // A repeat insert must not inflate the pair count.
                    if outcome == LeafInsert::AlreadyPresent {
                        assert_matches(&index, &reference);
                    }
                }
                Operation::Delete { key, row_value } => {
                    let row_id = row(*row_value);
                    let expected = reference.delete(key, row_id);
                    let found = index.delete(key, row_id).expect("delete never fails");
                    assert_eq!(
                        found == LeafRemove::Removed,
                        expected,
                        "delete {key:?} → {row_value}"
                    );
                    if let std::collections::hash_map::Entry::Occupied(mut slot) =
                        shadow.entry(key.clone())
                    {
                        slot.get_mut().remove(&row_id.get());
                        if slot.get().is_empty() {
                            slot.remove();
                        }
                    }
                }
                Operation::Probe { key } => {
                    let expected = reference.lookup(key);
                    let found: Option<Vec<u64>> = index
                        .lookup(key)
                        .map(|rows| rows.iter().map(|id| id.get()).collect());
                    assert_eq!(found, expected, "probe {key:?}");
                    assert_eq!(index.contains(key), expected.is_some());
                    // The shadow map agrees with both models.
                    assert_eq!(
                        expected,
                        shadow.get(key).map(|rows| rows.iter().copied().collect()),
                        "shadow probe {key:?}"
                    );
                    continue;
                }
            }
            assert_matches(&index, &reference);
        }
    }
}

#[test]
fn mixed_workload_over_a_small_key_space() {
    // Two letters over lengths 0..=4 force constant collisions, prefix
    // nesting, duplicate inserts, and repeated deletes of the same rows.
    run_workload("art/small-alphabet", 2_000, b"ab", 4, 12);
}

#[test]
fn mixed_workload_over_a_wide_key_space() {
    // All 256 byte values spread keys across many subtrees, forcing
    // Node16/Node48 growth while probes keep hitting absent keys.
    const FULL: [u8; 256] = {
        let mut table = [0u8; 256];
        let mut byte = 0usize;
        while byte < 256 {
            table[byte] = byte as u8;
            byte += 1;
        }
        table
    };
    run_workload("art/full-alphabet", 3_000, &FULL, 6, 500);
}

#[test]
fn insert_heavy_workload_with_long_shared_prefixes() {
    // Keys share the three-byte prefix "pre" by construction, so almost every
    // insert walks the same compressed prefix before fanning out on the tail.
    // Inserts and deletes churn over a small row space, which keeps the tree
    // near the Node4/Node16 boundary and repeatedly exercises growth and
    // shrink.
    let mut rng = Rng::new("art/long-prefix");
    let mut index = ArtIndex::non_unique();
    let mut reference = Reference::default();

    for step in 0..3_000u64 {
        let mut key = b"pre".to_vec();
        let tail_len = 1 + rng.below(5);
        key.extend(random_key_over(&mut rng, tail_len, b"abcdef"));
        let row_id = row(rng.below(40) as u64);

        if step % 4 == 3 {
            // Every fourth step deletes, so the tree churns instead of
            // growing monotonically.
            let removed_expected = reference.delete(&key, row_id);
            let removed = index.delete(&key, row_id).expect("delete never fails");
            assert_eq!(removed == LeafRemove::Removed, removed_expected);
        } else {
            reference.insert(&key, row_id);
            index
                .insert(&key, row_id)
                .expect("non-unique insert never fails");
        }

        if step % 250 == 0 {
            assert_matches(&index, &reference);
        }
    }
    assert_matches(&index, &reference);
}
