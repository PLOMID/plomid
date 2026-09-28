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
//! Key model for the ART index.
//!
//! ART operates on arbitrary byte strings. It never assumes keys are UTF-8
//! text, and it never assumes a fixed length: keys may be empty, short, long,
//! or variable-length, and may contain any byte value including `0x00`.
//!
//! # Where key encoding lives
//!
//! Encoding SQL values into bytes is deliberately *not* part of this crate.
//! The SQL layer already owns that concern
//! (`plomid_executor::index::index_value_bytes` builds a
//! `[tag][length][payload]` encoding for the transactional KV index path).
//! That encoder stays where it is: ART consumes whatever bytes the caller
//! produced, so a later planner/executor integration can reuse the exact same
//! encoding for both index implementations.
//!
//! This module therefore only contains:
//!
//! * [`ArtKey`] — the borrowed byte-string key type used by ART operations.
//! * [`ByteKey`] — an owned, ordered, hashable key wrapper used by callers
//!   that need to keep keys in maps (dedupe sets, reference models).
//! * [`common_prefix_len`] — the prefix helper shared by traversal and tests.
//! * [`encode_u64_be`] / [`encode_i64_ordered`] — order-preserving numeric
//!   encodings, because byte-wise ART ordering is only SQL-correct when the
//!   encoding is order preserving.

use std::borrow::Borrow;

/// The borrowed key type accepted by ART operations.
///
/// Keys are plain byte slices so that binary keys, empty keys, and
/// variable-length keys are all representable without conversion.
pub type ArtKey = [u8];

/// Returns the length of the longest common prefix of `left` and `right`.
///
/// Comparison proceeds eight bytes at a time to keep long common prefixes
/// (for example keys sharing a timestamp prefix) cheap. The implementation is
/// fully safe: it reads bytes through `u64::from_be_bytes` on slices that
/// were length-checked first.
#[must_use]
pub fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
    let limit = left.len().min(right.len());
    let mut offset = 0;

    // Word-at-a-time comparison over the region where both sides still have
    // at least eight bytes remaining.
    while offset + 8 <= limit {
        let left_word = u64::from_be_bytes([
            left[offset],
            left[offset + 1],
            left[offset + 2],
            left[offset + 3],
            left[offset + 4],
            left[offset + 5],
            left[offset + 6],
            left[offset + 7],
        ]);
        let right_word = u64::from_be_bytes([
            right[offset],
            right[offset + 1],
            right[offset + 2],
            right[offset + 3],
            right[offset + 4],
            right[offset + 5],
            right[offset + 6],
            right[offset + 7],
        ]);
        if left_word != right_word {
            // The first differing byte is the count of matching high bytes in
            // the XOR, which `leading_zeros` gives directly.
            return offset + (left_word ^ right_word).leading_zeros() as usize / 8;
        }
        offset += 8;
    }

    while offset < limit && left[offset] == right[offset] {
        offset += 1;
    }
    offset
}

/// Encodes an unsigned integer as big-endian bytes.
///
/// Big-endian byte order is order preserving, which is what a byte-wise
/// ordered structure such as ART requires for equality and range semantics.
#[must_use]
pub fn encode_u64_be(value: u64) -> [u8; 8] {
    value.to_be_bytes()
}

/// Encodes a signed integer as an order-preserving byte string.
///
/// The sign bit is flipped so negative values sort before non-negative values
/// under unsigned byte comparison.
#[must_use]
pub fn encode_i64_ordered(value: i64) -> [u8; 8] {
    (value as u64 ^ (1u64 << 63)).to_be_bytes()
}

/// An owned byte-string key.
///
/// `ByteKey` exists for callers that need to keep keys around — dedupe sets,
/// invariant reports, and the randomized reference model used by the tests.
/// It implements [`Borrow<[u8]>`] so a `HashMap<ByteKey, _>` can be probed
/// with a plain `&[u8]` without allocating an owned key.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteKey(Vec<u8>);

impl ByteKey {
    /// Creates a key from any byte source.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Creates a key from an unsigned integer using an order-preserving encoding.
    #[must_use]
    pub fn from_u64_be(value: u64) -> Self {
        Self(encode_u64_be(value).to_vec())
    }

    /// Creates a key from a signed integer using an order-preserving encoding.
    #[must_use]
    pub fn from_i64_ordered(value: i64) -> Self {
        Self(encode_i64_ordered(value).to_vec())
    }

    /// Returns the bytes of this key.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// Consumes this key, returning the owned byte vector.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }

    /// Returns the key length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns true when the key has no bytes.
    ///
    /// Empty keys are legal for ART: the empty key lives in the root's
    /// terminal slot and requires no special casing in traversal.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Appends one byte.
    pub fn push(&mut self, byte: u8) {
        self.0.push(byte);
    }

    /// Appends a byte slice.
    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

impl AsRef<[u8]> for ByteKey {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Borrow<[u8]> for ByteKey {
    fn borrow(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl From<Vec<u8>> for ByteKey {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&[u8]> for ByteKey {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl From<&str> for ByteKey {
    fn from(text: &str) -> Self {
        Self(text.as_bytes().to_vec())
    }
}
#[cfg(test)]
mod tests {
    use super::{common_prefix_len, encode_i64_ordered, encode_u64_be, ByteKey};

    #[test]
    fn common_prefix_handles_prefix_relationships() {
        assert_eq!(common_prefix_len(b"abc", b"abcd"), 3);
        assert_eq!(common_prefix_len(b"abcd", b"abc"), 3);
        assert_eq!(common_prefix_len(b"abc", b"abc"), 3);
        assert_eq!(common_prefix_len(b"", b"abc"), 0);
        assert_eq!(common_prefix_len(b"", b""), 0);
        assert_eq!(common_prefix_len(b"foo", b"foobar"), 3);
        assert_eq!(common_prefix_len(b"foo", b"bar"), 0);
    }

    #[test]
    fn common_prefix_spans_word_boundaries() {
        let left = b"0123456789abcdefghij";
        let right = b"0123456789abcdXfghij";
        // 14 matching bytes: the first ten, then four more ("abcd") before the
        // bytes diverge ('e' vs 'X').
        assert_eq!(common_prefix_len(left, right), 14);
        assert_eq!(&left[..14], &right[..14]);
        assert_ne!(left[14], right[14]);
    }

    #[test]
    fn binary_keys_compare_byte_wise() {
        assert_eq!(common_prefix_len(&[0u8], &[0u8, 1]), 1);
        assert_eq!(common_prefix_len(&[0u8, 255, 1], &[0u8, 255, 2]), 2);
        assert_eq!(common_prefix_len(&[255u8], &[0u8]), 0);
    }

    #[test]
    fn numeric_encodings_are_order_preserving() {
        assert!(encode_u64_be(1) < encode_u64_be(2));
        assert!(encode_i64_ordered(-5) < encode_i64_ordered(3));
        assert!(encode_i64_ordered(i64::MIN) < encode_i64_ordered(-1));
    }

    #[test]
    fn byte_key_behaves_like_a_byte_string() {
        let key = ByteKey::from("abc");
        assert_eq!(key.as_bytes(), b"abc");
        assert_eq!(key.len(), 3);
        assert!(!key.is_empty());
        assert!(ByteKey::default().is_empty());
        assert_eq!(ByteKey::from_u64_be(7).as_bytes(), &encode_u64_be(7));

        let mut map = std::collections::HashMap::new();
        map.insert(key.clone(), 1u32);
        // `Borrow<[u8]>` lets a borrowed key probe an owned-key map.
        assert_eq!(map.get(b"abc".as_slice()), Some(&1));
    }

    #[test]
    fn byte_keys_sort_like_bytes() {
        // `sort` on a fixed array avoids a heap allocation for this tiny case
        // (and satisfies the `useless_vec` lint).
        let mut keys = [ByteKey::from("b"), ByteKey::from("a"), ByteKey::from("ab")];
        keys.sort();
        let rendered: Vec<&[u8]> = keys.iter().map(ByteKey::as_bytes).collect();
        assert_eq!(rendered, vec![b"a".as_slice(), b"ab", b"b"]);
    }
}
