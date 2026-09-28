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
//! Bitmap container: a fixed 1024-word (8 KiB) bit set over the offset space.
//!
//! This is the dense representation: one bit per offset, so a bitmap container
//! always occupies [`ROARING_BITMAP_BYTES`] bytes regardless of cardinality.
//! It is chosen when a bucket is too dense for an array and too fragmented for
//! runs. Word `w` covers offsets `w * 64 ..= w * 64 + 63`, least significant
//! bit first; that ordering is what makes ascending iteration and
//! `trailing_zeros`-based scans exact.
//!
//! The persisted payload for a bitmap container is the tag byte plus the fixed
//! [`ROARING_BITMAP_BYTES`] word array — there is no per-container count field,
//! only the tag selects the family.

use crate::constants::{
    ARRAY_MAX_CARDINALITY, BITMAP_WORD_BITS, ROARING_BITMAP_BYTES, ROARING_BITMAP_WORDS,
    ROARING_CONTAINER_BITMAP,
};
use crate::layout::read_u32;
use crate::roaring::container::{Container, RunWord};
use crate::roaring::error::RoaringError;

/// Returns a container of all-zero words.
#[must_use]
pub(crate) fn empty_words() -> Vec<RunWord> {
    vec![0; ROARING_BITMAP_WORDS]
}

/// Persistent length of a bitmap container's payload: the 1024 words, with no
/// count header between the tag and the word data.
#[must_use]
pub(crate) const fn payload_len() -> usize {
    ROARING_BITMAP_BYTES
}

/// Returns the word holding `offset`, or `None` when `words` is too short.
fn split(words: &[RunWord], offset: u16) -> Option<(&RunWord, u32)> {
    let index = usize::from(offset) / BITMAP_WORD_BITS as usize;
    words
        .get(index)
        .map(|word| (word, u32::from(offset) % BITMAP_WORD_BITS))
}

/// Returns whether the bit for `offset` is set.
#[must_use]
pub(crate) fn contains(words: &[RunWord], offset: u16) -> bool {
    match split(words, offset) {
        Some((word, bit)) => word & (1u64 << bit) != 0,
        None => false,
    }
}

/// Sets the bit for `offset`. A word array that is too short is left unchanged.
pub(crate) fn set(words: &mut [RunWord], offset: u16) {
    let index = usize::from(offset) / BITMAP_WORD_BITS as usize;
    let bit = u32::from(offset) % BITMAP_WORD_BITS;
    if let Some(word) = words.get_mut(index) {
        *word |= 1u64 << bit;
    }
}

/// Clears the bit for `offset`. A word array that is too short is left
/// unchanged.
pub(crate) fn clear(words: &mut [RunWord], offset: u16) {
    let index = usize::from(offset) / BITMAP_WORD_BITS as usize;
    let bit = u32::from(offset) % BITMAP_WORD_BITS;
    if let Some(word) = words.get_mut(index) {
        *word &= !(1u64 << bit);
    }
}

/// Returns the number of set bits.
#[must_use]
pub(crate) fn cardinality(words: &[RunWord]) -> u32 {
    words.iter().map(|word| word.count_ones()).sum()
}

/// Encodes a sorted offset list as words.
#[must_use]
pub(crate) fn from_offsets(sorted: &[u16]) -> Vec<RunWord> {
    let mut words = empty_words();
    for offset in sorted {
        set(&mut words, *offset);
    }
    words
}

/// Decodes words into ascending offsets.
#[must_use]
pub(crate) fn to_offsets(words: &[RunWord]) -> Vec<u16> {
    let mut out = Vec::with_capacity(cardinality(words) as usize);
    for (index, word) in words.iter().enumerate() {
        let mut remaining = *word;
        while remaining != 0 {
            let bit = remaining.trailing_zeros();
            // `index * 64 + bit` is at most 65535 for a 1024-word array, so the
            // conversion back to `u16` is exact.
            out.push((index as u32 * BITMAP_WORD_BITS + bit) as u16);
            remaining &= remaining - 1;
        }
    }
    out
}

/// Returns the container that results from setting the bit for `offset`.
#[must_use]
pub(crate) fn insert(words: &[RunWord], offset: u16) -> Container {
    if contains(words, offset) {
        return Container::Bitmap(words.to_vec());
    }
    let mut next = words.to_vec();
    set(&mut next, offset);
    Container::Bitmap(next)
}

/// Returns the container that results from clearing the bit for `offset`.
///
/// A bitmap that drops to [`ARRAY_MAX_CARDINALITY`] set bits or fewer is
/// handed back as an array, which is the compact representation at that
/// cardinality. The result may be an empty array container; callers drop it.
#[must_use]
pub(crate) fn remove(words: &[RunWord], offset: u16) -> Container {
    if !contains(words, offset) {
        return Container::Bitmap(words.to_vec());
    }
    let mut next = words.to_vec();
    clear(&mut next, offset);
    if cardinality(&next) <= ARRAY_MAX_CARDINALITY {
        Container::Array {
            values: to_offsets(&next),
        }
    } else {
        Container::Bitmap(next)
    }
}

/// Returns the smallest set offset, or `None` when no bit is set.
#[must_use]
pub(crate) fn min_offset(words: &[RunWord]) -> Option<u16> {
    for (index, word) in words.iter().enumerate() {
        if *word != 0 {
            return Some((index as u32 * BITMAP_WORD_BITS + word.trailing_zeros()) as u16);
        }
    }
    None
}

/// Returns the largest set offset, or `None` when no bit is set.
#[must_use]
pub(crate) fn max_offset(words: &[RunWord]) -> Option<u16> {
    for (index, word) in words.iter().enumerate().rev() {
        if *word != 0 {
            let bit = (BITMAP_WORD_BITS - 1) - word.leading_zeros();
            return Some((index as u32 * BITMAP_WORD_BITS + bit) as u16);
        }
    }
    None
}

/// Returns how many bits are set below `offset` (`0..=65536`).
fn count_below(words: &[RunWord], offset: u32) -> u32 {
    let whole = (offset / BITMAP_WORD_BITS) as usize;
    let limit = whole.min(words.len());
    let mut total: u32 = words[..limit].iter().map(|word| word.count_ones()).sum();
    let remainder = offset % BITMAP_WORD_BITS;
    if remainder != 0 {
        if let Some(word) = words.get(whole) {
            total += (word & ((1u64 << remainder) - 1)).count_ones();
        }
    }
    total
}

/// Returns how many set offsets are `<= offset`.
#[must_use]
pub(crate) fn count_less_equal(words: &[RunWord], offset: u16) -> u32 {
    count_below(words, u32::from(offset) + 1)
}

/// Returns how many set offsets fall inside the inclusive range `[start, end]`.
#[must_use]
pub(crate) fn count_in_range(words: &[RunWord], start: u16, end: u16) -> u32 {
    if start > end {
        return 0;
    }
    count_below(words, u32::from(end) + 1) - count_below(words, u32::from(start))
}

/// Appends the container payload (tag, words) to `out`.
pub(crate) fn encode(words: &[RunWord], out: &mut Vec<u8>) {
    out.push(ROARING_CONTAINER_BITMAP);
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
}

/// Checks the bitmap invariant: exactly [`ROARING_BITMAP_WORDS`] words.
///
/// # Errors
///
/// Returns [`RoaringError::Truncated`] when the word count is wrong.
pub(crate) fn validate(words: &[RunWord]) -> Result<(), RoaringError> {
    if words.len() != ROARING_BITMAP_WORDS {
        return Err(RoaringError::Truncated("bitmap words"));
    }
    Ok(())
}

/// Decodes the payload that follows the bitmap tag.
///
/// The payload is exactly [`ROARING_BITMAP_BYTES`] bytes of word data — there
/// is no count field between the tag and the words, only the tag selects the
/// family.
///
/// # Errors
///
/// Returns [`RoaringError::Truncated`] unless the payload is exactly
/// [`ROARING_BITMAP_BYTES`] bytes.
pub(crate) fn decode(payload: &[u8]) -> Result<Vec<RunWord>, RoaringError> {
    if payload.len() != ROARING_BITMAP_BYTES {
        return Err(RoaringError::Truncated("bitmap words"));
    }
    let mut words = Vec::with_capacity(ROARING_BITMAP_WORDS);
    for index in 0..ROARING_BITMAP_WORDS {
        let low = read_u32(payload, index * 8).ok_or(RoaringError::Truncated("bitmap word"))?;
        let high =
            read_u32(payload, index * 8 + 4).ok_or(RoaringError::Truncated("bitmap word"))?;
        words.push(u64::from(low) | (u64::from(high) << 32));
    }
    Ok(words)
}
