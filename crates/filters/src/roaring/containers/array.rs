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
//! Array container: a sorted `u16` value list.
//!
//! The array representation is the most compact choice for sparse buckets —
//! two bytes per value, so up to [`ARRAY_MAX_CARDINALITY`] values stay smaller
//! than a bitmap container's fixed 8 KiB. Insertion keeps the list sorted;
//! pushing a value past the array threshold hands the bucket over to a bitmap
//! container (never to a run container: a single new value can only add one
//! run, so runs cannot suddenly become the better choice).

use crate::constants::{ARRAY_ENTRY_BYTES, ARRAY_MAX_CARDINALITY};
use crate::layout::{push_u16, push_u32, read_u16, read_u32};
use crate::roaring::container::Container;
use crate::roaring::containers::bitmap;
use crate::roaring::error::RoaringError;

/// Bytes of the container payload: the count (`u32`) and the values.
#[must_use]
pub(crate) const fn payload_len(cardinality: usize) -> usize {
    4 + cardinality * ARRAY_ENTRY_BYTES
}

/// Returns whether `offset` is in `values`.
#[must_use]
pub(crate) fn contains(values: &[u16], offset: u16) -> bool {
    values.binary_search(&offset).is_ok()
}

/// Returns the container that results from adding `offset` to `values`.
#[must_use]
pub(crate) fn insert(values: &[u16], offset: u16) -> Container {
    match values.binary_search(&offset) {
        Ok(_) => Container::Array {
            values: values.to_vec(),
        },
        Err(index) => {
            let mut next = Vec::with_capacity(values.len() + 1);
            next.extend_from_slice(&values[..index]);
            next.push(offset);
            next.extend_from_slice(&values[index..]);
            if next.len() as u32 > ARRAY_MAX_CARDINALITY {
                Container::Bitmap(bitmap::from_offsets(&next))
            } else {
                Container::Array { values: next }
            }
        }
    }
}

/// Returns the container that results from removing `offset` from `values`.
///
/// The result may be an empty array container; callers drop it.
#[must_use]
pub(crate) fn remove(values: &[u16], offset: u16) -> Container {
    match values.binary_search(&offset) {
        Err(_) => Container::Array {
            values: values.to_vec(),
        },
        Ok(index) => {
            let mut next = Vec::with_capacity(values.len() - 1);
            next.extend_from_slice(&values[..index]);
            next.extend_from_slice(&values[index + 1..]);
            Container::Array { values: next }
        }
    }
}

/// Returns the smallest value, or `None` for an empty array.
#[must_use]
pub(crate) fn min_offset(values: &[u16]) -> Option<u16> {
    values.first().copied()
}

/// Returns the largest value, or `None` for an empty array.
#[must_use]
pub(crate) fn max_offset(values: &[u16]) -> Option<u16> {
    values.last().copied()
}

/// Returns how many values are `<= offset`.
#[must_use]
pub(crate) fn count_less_equal(values: &[u16], offset: u16) -> u32 {
    // `partition_point` needs the predicate to be false after the first true,
    // which holds because `values` is sorted.
    values.partition_point(|value| *value <= offset) as u32
}

/// Returns how many values fall inside the inclusive range `[start, end]`.
#[must_use]
pub(crate) fn count_in_range(values: &[u16], start: u16, end: u16) -> u32 {
    if start > end {
        return 0;
    }
    let first = values.partition_point(|value| *value < start);
    let after_last = values.partition_point(|value| *value <= end);
    (after_last - first) as u32
}

/// Checks the array invariants: strictly increasing values.
///
/// # Errors
///
/// Returns [`RoaringError::UnsortedValues`] when two values are equal or out of
/// order.
pub(crate) fn validate(values: &[u16]) -> Result<(), RoaringError> {
    if values.windows(2).any(|window| window[0] >= window[1]) {
        return Err(RoaringError::UnsortedValues);
    }
    Ok(())
}

/// Appends the container payload (tag, count, values) to `out`.
pub(crate) fn encode(values: &[u16], out: &mut Vec<u8>) {
    out.push(crate::constants::ROARING_CONTAINER_ARRAY);
    push_u32(out, values.len() as u32);
    for value in values {
        push_u16(out, *value);
    }
}

/// Decodes the payload that follows the array tag.
///
/// # Errors
///
/// Returns [`RoaringError::Truncated`] when the count does not match the
/// payload length, and the [`validate`] errors when the values are unsorted.
pub(crate) fn decode(payload: &[u8]) -> Result<Vec<u16>, RoaringError> {
    let count = read_u32(payload, 0).ok_or(RoaringError::Truncated("array count"))? as usize;
    if payload.len() != payload_len(count) {
        return Err(RoaringError::Truncated("array values"));
    }
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let offset = 4 + index * ARRAY_ENTRY_BYTES;
        values.push(read_u16(payload, offset).ok_or(RoaringError::Truncated("array value"))?);
    }
    validate(&values)?;
    Ok(values)
}
