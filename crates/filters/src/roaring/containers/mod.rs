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
//! Per-representation container codecs and value algorithms.
//!
//! Each sub-module owns one container family end to end — its memory layout,
//! membership test, mutation, counting, validation, and byte-level codec — so
//! the three representations never share state or assumptions:
//!
//! * [`array`] — sorted `u16` list, sparse buckets.
//! * [`bitmap`] — 1 024 `u64` words, dense buckets.
//! * [`run`] — `(start, offset-length)` pairs, clustered buckets.
//!
//! Set *arithmetic* deliberately does not live per representation. A container
//! is first expanded to sorted offsets, the operation runs once in
//! [`offsets`](super::offsets), and the result is re-encoded by
//! [`from_offsets`] into the most compact family. That is what makes results
//! canonical and keeps one implementation per operation instead of nine.

use crate::constants::{ARRAY_MAX_CARDINALITY, ROARING_BITMAP_BYTES, RUN_PAIR_BYTES};
use crate::roaring::container::Container;
use crate::roaring::offsets;

pub(crate) mod array;
pub(crate) mod bitmap;
pub(crate) mod run;
pub(crate) mod set_ops;

pub(crate) use set_ops::{
    difference, intersect, intersects, is_subset, symmetric_difference, union,
};

/// Returns the container holding exactly the given sorted offsets.
///
/// This is the single canonical-representation rule of the crate:
///
/// 1. empty offsets produce an **empty array** container (callers drop it);
/// 2. cardinality `<= ARRAY_MAX_CARDINALITY` produces an **array**;
/// 3. otherwise, runs win only while they stay smaller than a bitmap
///    (`runs * 4 < 8192`), i.e. while the value list is still clustered;
/// 4. everything else is a **bitmap**.
///
/// A dense bucket therefore becomes a bitmap even when its offsets happen to
/// form few runs but cost more than 8 KiB as runs.
#[must_use]
pub(crate) fn from_offsets(sorted: &[u16]) -> Container {
    if sorted.is_empty() {
        return Container::Array { values: Vec::new() };
    }
    if sorted.len() as u32 <= ARRAY_MAX_CARDINALITY {
        return Container::Array {
            values: sorted.to_vec(),
        };
    }
    let runs = offsets::count_runs(sorted) as usize;
    if RUN_PAIR_BYTES * runs < ROARING_BITMAP_BYTES {
        let (run_starts, run_lengths) = run::from_offsets(sorted);
        return Container::Run {
            offsets: run_starts,
            lengths: run_lengths,
        };
    }
    Container::Bitmap(bitmap::from_offsets(sorted))
}

#[cfg(test)]
mod tests {
    use super::from_offsets;
    use crate::roaring::container::Container;

    /// Compares two containers by content, so a bucket stored as a bitmap and
    /// the same bucket stored as runs count as equal.
    fn same_values(left: &Container, right: &Container) -> bool {
        left.cardinality() == right.cardinality() && left.offsets() == right.offsets()
    }

    #[test]
    fn sparse_offsets_become_an_array() {
        let container = from_offsets(&[1, 5, 9]);
        assert!(matches!(container, Container::Array { .. }));
    }

    #[test]
    fn dense_offsets_become_a_bitmap() {
        // 7 000 scattered values: 7 000 runs of one, so runs would cost 28 KiB
        // — more than a bitmap — and the canonical choice is a bitmap.
        let values: Vec<u16> = (0..7_000).map(|v| (v * 3) as u16).collect();
        let container = from_offsets(&values);
        assert!(matches!(container, Container::Bitmap(_)));
    }

    #[test]
    fn clustered_offsets_become_runs() {
        // 9 000 values that form exactly ten runs of 900 values each:
        // runs cost 40 bytes, far less than 8 KiB, so the canonical choice is
        // a run container.
        let mut values = Vec::new();
        for block in 0..10 {
            for offset in 0..900 {
                values.push((block * 1000 + offset) as u16);
            }
        }
    }

    #[test]
    fn values_survive_the_representation_choice() {
        let values: Vec<u16> = (0..4096u32).map(|value| value as u16).collect();
        let container = from_offsets(&values);
        assert_eq!(container.offsets(), values);
        // The same values also compare equal when stored as a bitmap, which is
        // what makes the canonical choice safe under set operations.
        let bitmap = Container::Bitmap(super::bitmap::from_offsets(&values));
        assert!(same_values(&container, &bitmap));
    }
}
