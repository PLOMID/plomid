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
//! Run container: run-length encoded offsets.
//!
//! A run is a pair `(start, length)` where `length` is the **number of offsets
//! after `start`**, so run `i` covers `offsets[i] ..= offsets[i] + lengths[i]`
//! inclusive. Defining the field as an offset rather than a count is what lets
//! a container whose run covers all 65 536 offsets be represented at all: a
//! count of 65 536 does not fit the per-run `u16` field, while the offset
//! 65 535 does.
//!
//! Runs are stored in the canonical form produced by
//! [`from_offsets`]: starts strictly increasing, and consecutive runs separated
//! by at least one missing offset (adjacent runs would have been merged). A run
//! container is only chosen while its `4 * runs` bytes stay below a bitmap's
//! fixed 8 KiB, so its run list is bounded by 2 048 pairs in practice.
//!
//! The codec does not expose a separate encoding-length helper: the persisted
//! format writes the tag byte + run count + pairs, and the length is derived
//! from [`RUN_PAIR_BYTES`] at encode time exactly like the other families do
//! with [`ROARING_BITMAP_BYTES`] and [`ARRAY_ENTRY_BYTES`].

use crate::constants::{ARRAY_MAX_CARDINALITY, ROARING_CONTAINER_RUN, RUN_PAIR_BYTES};
use crate::layout::{push_u32, read_u16, read_u32};
use crate::roaring::container::Container;
use crate::roaring::error::RoaringError;

/// Bytes of the container payload: the count (`u32`) plus the pairs.
#[must_use]
pub(crate) const fn payload_len(runs: usize) -> usize {
    4 + runs * RUN_PAIR_BYTES
}

/// Returns the last offset covered by run `index`.
fn run_end(offsets: &[u16], lengths: &[u16], index: usize) -> u32 {
    u32::from(offsets[index]) + u32::from(lengths[index])
}

/// Returns whether `offset` is covered by the runs (assumes parallel arrays).
#[must_use]
pub(crate) fn contains(offsets: &[u16], lengths: &[u16], offset: u16) -> bool {
    let starts = offsets.partition_point(|start| *start <= offset);
    starts > 0 && run_end(offsets, lengths, starts - 1) >= u32::from(offset)
}

/// Returns a run container covering exactly the given sorted offsets.
#[must_use]
pub(crate) fn from_offsets(sorted: &[u16]) -> (Vec<u16>, Vec<u16>) {
    let mut offsets = Vec::new();
    let mut lengths = Vec::new();
    let mut index = 0usize;
    while index < sorted.len() {
        let start = sorted[index];
        let mut end = index;
        while end + 1 < sorted.len() && sorted[end + 1] == sorted[end] + 1 {
            end += 1;
        }
        offsets.push(start);
        lengths.push((u32::from(sorted[end]) - u32::from(start)) as u16);
        index = end + 1;
    }
    (offsets, lengths)
}

/// Expands the runs into ascending offsets.
#[must_use]
pub(crate) fn to_offsets(offsets: &[u16], lengths: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(count(offsets, lengths) as usize);
    for (start, length) in offsets.iter().zip(lengths) {
        let end = u32::from(*start) + u32::from(*length);
        for value in u32::from(*start)..=end {
            out.push(value as u16);
        }
    }
    out
}

/// Returns the runs unchanged as a container.
fn unchanged(offsets: &[u16], lengths: &[u16]) -> Container {
    Container::Run {
        offsets: offsets.to_vec(),
        lengths: lengths.to_vec(),
    }
}

/// Returns the container that results from adding `offset` to the runs.
///
/// Adjacent runs are merged and a new single-offset run is inserted in sorted
/// position, so the canonical form is preserved by construction.
#[must_use]
pub(crate) fn insert(offsets: &[u16], lengths: &[u16], offset: u16) -> Container {
    let position = offsets.partition_point(|start| *start <= offset);
    if position > 0 && run_end(offsets, lengths, position - 1) >= u32::from(offset) {
        return unchanged(offsets, lengths);
    }
    let merges_left =
        position > 0 && run_end(offsets, lengths, position - 1) + 1 == u32::from(offset);
    let merges_right =
        position < offsets.len() && u32::from(offsets[position]) == u32::from(offset) + 1;

    let mut next_offsets = offsets.to_vec();
    let mut next_lengths = lengths.to_vec();
    match (merges_left, merges_right) {
        (true, true) => {
            // `position - 1` and `position` are joined by `offset`.
            let left = position - 1;
            let end = run_end(offsets, lengths, position);
            next_lengths[left] = (end - u32::from(offsets[left])) as u16;
            next_offsets.remove(position);
            next_lengths.remove(position);
        }
        (true, false) => next_lengths[position - 1] += 1,
        (false, true) => {
            next_offsets[position] = offset;
            next_lengths[position] += 1;
        }
        (false, false) => {
            next_offsets.insert(position, offset);
            next_lengths.insert(position, 0);
        }
    }
    Container::Run {
        offsets: next_offsets,
        lengths: next_lengths,
    }
}

/// Returns the container that results from removing `offset` from the runs.
///
/// A run that loses its interior is split in two. When the remaining
/// cardinality drops to [`ARRAY_MAX_CARDINALITY`] or below the result is handed
/// back as an array, which is the compact representation at that cardinality.
#[must_use]
pub(crate) fn remove(offsets: &[u16], lengths: &[u16], offset: u16) -> Container {
    let position = offsets.partition_point(|start| *start <= offset);
    if position == 0 || run_end(offsets, lengths, position - 1) < u32::from(offset) {
        return unchanged(offsets, lengths);
    }
    let index = position - 1;
    let start = u32::from(offsets[index]);
    let end = run_end(offsets, lengths, index);
    let target = u32::from(offset);

    let mut next_offsets = offsets.to_vec();
    let mut next_lengths = lengths.to_vec();
    if end == start {
        next_offsets.remove(index);
        next_lengths.remove(index);
    } else if target == start {
        next_offsets[index] += 1;
        next_lengths[index] -= 1;
    } else if target == end {
        next_lengths[index] -= 1;
    } else {
        next_lengths[index] = (target - 1 - start) as u16;
        next_offsets.insert(index + 1, (target + 1) as u16);
        next_lengths.insert(index + 1, (end - (target + 1)) as u16);
    }

    if count(&next_offsets, &next_lengths) <= ARRAY_MAX_CARDINALITY {
        Container::Array {
            values: to_offsets(&next_offsets, &next_lengths),
        }
    } else {
        Container::Run {
            offsets: next_offsets,
            lengths: next_lengths,
        }
    }
}

/// Returns the smallest covered offset, or `None` for no runs.
#[must_use]
pub(crate) fn min_offset(offsets: &[u16]) -> Option<u16> {
    offsets.first().copied()
}

/// Returns the largest covered offset, or `None` for no runs.
#[must_use]
pub(crate) fn max_offset(offsets: &[u16], lengths: &[u16]) -> Option<u16> {
    if offsets.is_empty() {
        return None;
    }
    Some(run_end(offsets, lengths, offsets.len() - 1) as u16)
}

/// Returns how many covered offsets are `<= offset`.
#[must_use]
pub(crate) fn count_less_equal(offsets: &[u16], lengths: &[u16], offset: u16) -> u32 {
    let limit = u32::from(offset);
    let mut total = 0u32;
    for (start, length) in offsets.iter().zip(lengths) {
        let start = u32::from(*start);
        if start > limit {
            break;
        }
        total += (limit - start).min(u32::from(*length)) + 1;
    }
    total
}

/// Returns how many covered offsets fall inside the inclusive range
/// `[start, end]`.
#[must_use]
pub(crate) fn count_in_range(offsets: &[u16], lengths: &[u16], start: u16, end: u16) -> u32 {
    if start > end {
        return 0;
    }
    let (low, high) = (u32::from(start), u32::from(end));
    let mut total = 0u32;
    for (run_start, run_length) in offsets.iter().zip(lengths) {
        let first = u32::from(*run_start).max(low);
        let last = (u32::from(*run_start) + u32::from(*run_length)).min(high);
        if first <= last {
            total += last - first + 1;
        }
    }
    total
}

/// Checks the run invariants: parallel arrays, ordered and separated runs, and
/// every run inside the container's offset space.
///
/// # Errors
///
/// Returns [`RoaringError::MismatchedKeysAndContainers`] when the two arrays
/// differ in length, [`RoaringError::ValueOutOfRange`] when a run ends past
/// offset 65 535, and [`RoaringError::UnsortedValues`] when starts are
/// unordered or two runs are adjacent or overlapping (they must have been
/// merged).
pub(crate) fn validate(offsets: &[u16], lengths: &[u16]) -> Result<(), RoaringError> {
    if offsets.len() != lengths.len() {
        return Err(RoaringError::MismatchedKeysAndContainers);
    }
    for (index, (start, length)) in offsets.iter().zip(lengths).enumerate() {
        if u32::from(*start) + u32::from(*length) > u32::from(u16::MAX) {
            return Err(RoaringError::ValueOutOfRange(*start));
        }
        if index > 0 {
            let previous_end = run_end(offsets, lengths, index - 1);
            if u32::from(*start) <= previous_end + 1 {
                return Err(RoaringError::UnsortedValues);
            }
        }
    }
    Ok(())
}

/// Appends the container payload (tag, run count, pairs) to `out`.
pub(crate) fn encode(offsets: &[u16], lengths: &[u16], out: &mut Vec<u8>) {
    out.push(ROARING_CONTAINER_RUN);
    push_u32(out, offsets.len() as u32);
    for (start, length) in offsets.iter().zip(lengths) {
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
    }
}

/// Decodes the payload that follows the run tag.
///
/// # Errors
///
/// Returns [`RoaringError::Truncated`] when the run count does not match the
/// payload length, and the [`validate`] errors when a pair is invalid.
pub(crate) fn decode(payload: &[u8]) -> Result<(Vec<u16>, Vec<u16>), RoaringError> {
    let runs = read_u32(payload, 0).ok_or(RoaringError::Truncated("run count"))? as usize;
    let expected = 4 + runs * RUN_PAIR_BYTES;
    if payload.len() != expected {
        return Err(RoaringError::Truncated("run pairs"));
    }
    let mut offsets = Vec::with_capacity(runs);
    let mut lengths = Vec::with_capacity(runs);
    for index in 0..runs {
        let pair = 4 + index * RUN_PAIR_BYTES;
        offsets.push(read_u16(payload, pair).ok_or(RoaringError::Truncated("run start"))?);
        lengths.push(read_u16(payload, pair + 2).ok_or(RoaringError::Truncated("run length"))?);
    }
    validate(&offsets, &lengths)?;
    Ok((offsets, lengths))
}

/// Returns the number of offsets covered by the runs.
#[must_use]
pub(crate) fn count(offsets: &[u16], lengths: &[u16]) -> u32 {
    debug_assert_eq!(offsets.len(), lengths.len(), "run arrays must be parallel");
    lengths.iter().map(|length| u32::from(*length) + 1).sum()
}
