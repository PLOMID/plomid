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
//! Sorted-offset merge primitives.
//!
//! Every container set operation is defined here, once, over plain sorted
//! `u16` offsets. Container representations (`Array`, `Bitmap`, `Run`) are
//! *storage* choices only: they decode to sorted offsets, the operation runs on
//! those offsets, and the result is re-encoded by
//! [`containers::from_offsets`](super::containers::from_offsets) into the most
//! compact representation. That keeps one implementation per set operation
//! instead of one per representation pair, and it makes the result canonical —
//! the same value set always produces the same container family.
//!
//! All functions require **strictly increasing** inputs (the invariant every
//! container maintains) and produce strictly increasing output.

/// Returns the number of maximal consecutive runs in `sorted`.
///
/// Used by the canonical representation choice: a value list that packs into
/// few runs is cheaper as a run container than as a bitmap.
#[must_use]
pub(crate) fn count_runs(sorted: &[u16]) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let mut runs = 1u32;
    for window in sorted.windows(2) {
        // `wrapping` arithmetic would be wrong here: offsets are real values,
        // so a gap of exactly one means the run continues.
        if window[1] != window[0] + 1 {
            runs += 1;
        }
    }
    runs
}

/// Returns the union of two sorted offset lists.
#[must_use]
pub(crate) fn union(left: &[u16], right: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut i, mut j) = (0usize, 0usize);
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => {
                out.push(left[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(right[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(left[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&left[i..]);
    out.extend_from_slice(&right[j..]);
    out
}

/// Returns the intersection of two sorted offset lists.
#[must_use]
pub(crate) fn intersect(left: &[u16], right: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(left.len().min(right.len()));
    let (mut i, mut j) = (0usize, 0usize);
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(left[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Returns the values of `left` that are not in `right`.
#[must_use]
pub(crate) fn difference(left: &[u16], right: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(left.len());
    let (mut i, mut j) = (0usize, 0usize);
    while i < left.len() {
        if j >= right.len() {
            out.extend_from_slice(&left[i..]);
            break;
        }
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => {
                out.push(left[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Returns the values that are in exactly one of the two sorted lists.
#[must_use]
pub(crate) fn symmetric_difference(left: &[u16], right: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut i, mut j) = (0usize, 0usize);
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => {
                out.push(left[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(right[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&left[i..]);
    out.extend_from_slice(&right[j..]);
    out
}

/// Returns the values in `[start, end]`, assuming `start <= end`.
///
/// The range is inclusive on both ends and covers at most one container.
#[must_use]
pub(crate) fn range_offsets(start: u16, end: u16) -> Vec<u16> {
    if start > end {
        return Vec::new();
    }
    // `end - start + 1` is at most 65536, so the length always fits a `u32`.
    let len = u32::from(end) - u32::from(start) + 1;
    let mut out = Vec::with_capacity(len as usize);
    for value in start..=end {
        out.push(value);
    }
    out
}

/// Returns whether every value of `left` also appears in `right`.
#[must_use]
pub(crate) fn is_subset(left: &[u16], right: &[u16]) -> bool {
    let (mut i, mut j) = (0usize, 0usize);
    while i < left.len() {
        if j >= right.len() || right[j] > left[i] {
            return false;
        }
        if right[j] == left[i] {
            i += 1;
        }
        j += 1;
    }
    true
}

/// Returns whether the two sorted lists share at least one value.
#[must_use]
pub(crate) fn overlaps(left: &[u16], right: &[u16]) -> bool {
    let (mut i, mut j) = (0usize, 0usize);
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}
