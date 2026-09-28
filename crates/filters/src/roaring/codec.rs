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
//! The persisted Roaring bitmap image.
//!
//! ```text
//! body  = bucket_count:u32 ‖ [ key:u16 ‖ container ]  (bucket_count times)
//! image = body ‖ footer (magic, version, flags, lengths, CRCs)   ← layout::frame
//! ```
//!
//! `bucket_count` bounds the decode loop before a single bucket is read, so a
//! damaged count cannot make the decoder allocate from a bogus length. Each
//! container is self-describing: a type tag selects the codec and its own count
//! must agree with the bytes that follow. Decoding rejects anything the
//! invariants forbid — unordered keys, an empty container, unsorted values,
//! trailing bytes, an unknown tag, or a failed checksum — and hands back the
//! error instead of a partial bitmap.
//!
//! Encoding is deterministic: the same value set always produces the same
//! bytes, because the container families are canonical
//! ([`from_offsets`](super::containers::from_offsets)) and the bucket order is
//! the key order.

use crate::constants::{
    ROARING_CONTAINER_ARRAY, ROARING_CONTAINER_BITMAP, ROARING_CONTAINER_RUN,
    ROARING_FORMAT_VERSION, ROARING_MAGIC,
};
use crate::layout::{self, read_u16, read_u32};
use crate::roaring::container::Container;
use crate::roaring::containers::{array, bitmap, run};
use crate::roaring::{RoaringBitmap, RoaringError};

impl RoaringBitmap {
    /// Encodes the bitmap body (buckets only, without the footer).
    #[must_use]
    pub fn encode_body(&self) -> Vec<u8> {
        let mut out = Vec::new();
        layout::push_u32(&mut out, self.container_count() as u32);
        for (key, container) in self.containers() {
            out.extend_from_slice(&key.to_le_bytes());
            match container {
                Container::Array { values } => array::encode(values, &mut out),
                Container::Bitmap(words) => bitmap::encode(words, &mut out),
                Container::Run { offsets, lengths } => run::encode(offsets, lengths, &mut out),
            }
        }
        out
    }

    /// Decodes a bitmap from its body.
    ///
    /// # Errors
    ///
    /// Returns [`RoaringError::Truncated`] when the image ends early,
    /// [`RoaringError::UnknownContainerType`] for an undefined tag,
    /// [`RoaringError::InvalidKeyOrder`] for unordered bucket keys,
    /// [`RoaringError::EmptyContainer`] for an empty bucket,
    /// [`RoaringError::TrailingBytes`] for bytes after the last bucket, and
    /// the representation errors from the per-container codecs.
    pub fn decode_body(image: &[u8]) -> Result<Self, RoaringError> {
        let buckets = read_u32(image, 0).ok_or(RoaringError::Truncated("bucket count"))?;
        let mut bitmap = Self::new();
        let mut cursor = 4usize;
        let mut previous_key: Option<u16> = None;

        for _ in 0..buckets {
            let key = read_u16(image, cursor).ok_or(RoaringError::Truncated("bucket key"))?;
            cursor += 2;
            if let Some(previous) = previous_key {
                if key <= previous {
                    return Err(RoaringError::InvalidKeyOrder(key));
                }
            }

            let tag = *image
                .get(cursor)
                .ok_or(RoaringError::Truncated("container type"))?;
            let container = decode_container(tag, image, &mut cursor)?;
            if container.is_empty() {
                return Err(RoaringError::EmptyContainer);
            }
            bitmap.push_bucket(key, container);
            previous_key = Some(key);
        }

        if cursor != image.len() {
            return Err(RoaringError::TrailingBytes(cursor, image.len()));
        }
        Ok(bitmap)
    }

    /// Encodes the bitmap into the framed, checksummed image.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        layout::frame(ROARING_MAGIC, ROARING_FORMAT_VERSION, &self.encode_body())
    }

    /// Decodes a bitmap from a framed image.
    ///
    /// # Errors
    ///
    /// Returns [`RoaringError::Corrupt`] when the framing rejects the image
    /// (magic, version, flags, lengths, or checksum), and the
    /// [`decode_body`](Self::decode_body) errors when the body itself is
    /// invalid. The framing checks run first, so body decoding only ever sees
    /// bytes whose checksum matched.
    pub fn deserialize(image: &[u8]) -> Result<Self, RoaringError> {
        let body = layout::unframe(ROARING_MAGIC, ROARING_FORMAT_VERSION, image)
            .map_err(RoaringError::Corrupt)?;
        Self::decode_body(body)
    }
}

/// Decodes one container starting at `cursor` (which points at the type tag),
/// advancing `cursor` past the tag and the payload it read.
///
/// The payload length is derived from the container's own count field **and**
/// checked against the image before a single value is read, so a damaged count
/// cannot cause an oversized allocation: the slice lookup fails first.
fn decode_container(tag: u8, image: &[u8], cursor: &mut usize) -> Result<Container, RoaringError> {
    let payload_start = (*cursor)
        .checked_add(1)
        .ok_or(RoaringError::Truncated("container"))?;
    match tag {
        ROARING_CONTAINER_ARRAY => {
            let count = read_u32(image, payload_start)
                .ok_or(RoaringError::Truncated("array count"))? as usize;
            let length = array::payload_len(count);
            let payload = payload_slice(image, payload_start, length)?;
            let values = array::decode(payload)?;
            *cursor = payload_start + payload.len();
            Ok(Container::Array { values })
        }
        ROARING_CONTAINER_BITMAP => {
            let length = bitmap::payload_len();
            let payload = payload_slice(image, payload_start, length)?;
            let words = bitmap::decode(payload)?;
            *cursor = payload_start + payload.len();
            Ok(Container::Bitmap(words))
        }
        ROARING_CONTAINER_RUN => {
            let runs = read_u32(image, payload_start).ok_or(RoaringError::Truncated("run count"))?
                as usize;
            let length = run::payload_len(runs);
            let payload = payload_slice(image, payload_start, length)?;
            let (offsets, lengths) = run::decode(payload)?;
            *cursor = payload_start + payload.len();
            Ok(Container::Run { offsets, lengths })
        }
        other => Err(RoaringError::UnknownContainerType(other)),
    }
}

/// Returns the payload slice of exactly `length` bytes at `start`, or
/// [`RoaringError::Truncated`] when the image is too short.
fn payload_slice(image: &[u8], start: usize, length: usize) -> Result<&[u8], RoaringError> {
    let end = start
        .checked_add(length)
        .ok_or(RoaringError::Truncated("container length"))?;
    image
        .get(start..end)
        .ok_or(RoaringError::Truncated("container payload"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a bitmap that forces a bitmap container (cardinality > 4096 in one
    /// bucket), serializes it, deserializes it, and checks that the round-trip is
    /// bit-for-bit identical. This exercises the `bitmap::payload_len()` path in
    /// [`decode_container`] — the decoder asks the bitmap family for its fixed
    /// payload length (8192 bytes, no count header) and slices exactly that many
    /// bytes, rather than assuming a `+4` header that array and run containers
    /// carry.
    #[test]
    fn bitmap_container_round_trip() {
        // 5 000 values in bucket key 1: above the array threshold (4 096) so the
        // canonical representation is a bitmap container.
        let mut bitmap = RoaringBitmap::new();
        for offset in 0..5_000u16 {
            bitmap.insert(u32::from(1u16) << 16 | u32::from(offset));
        }
        assert!(matches!(
            bitmap.container(1),
            Some(crate::roaring::Container::Bitmap(_))
        ));

        let image = bitmap.serialize();
        let decoded = RoaringBitmap::deserialize(&image).expect("round-trip must succeed");
        assert_eq!(decoded, bitmap);

        // Spot-check a couple of values to be sure the payload was decoded, not
        // just that the two images happen to serialize identically.
        assert!(decoded.contains(u32::from(1u16) << 16));
        assert!(decoded.contains(u32::from(1u16) << 16 | 4999));
        assert!(!decoded.contains(u32::from(1u16) << 16 | 5000));
    }

    /// Round-trips an array container to confirm the `+4` count-header path in
    /// [`decode_container`] still works after the codec rewrite.
    #[test]
    fn array_container_round_trip() {
        let mut bitmap = RoaringBitmap::new();
        for offset in [1u16, 5, 9, 12, 42] {
            bitmap.insert(u32::from(3u16) << 16 | u32::from(offset));
        }
        assert!(matches!(
            bitmap.container(3),
            Some(crate::roaring::Container::Array { .. })
        ));

        let image = bitmap.serialize();
        let decoded = RoaringBitmap::deserialize(&image).expect("round-trip must succeed");
        assert_eq!(decoded, bitmap);
    }

    /// Round-trips a run container to confirm the run-count-header path in
    /// [`decode_container`] still works after the codec rewrite.
    #[test]
    fn run_container_round_trip() {
        use crate::roaring::container::Container;
        use crate::roaring::containers::run;

        // 5 000 consecutive values: above the array threshold (4 096) so it won't
        // be an array; a single run costs 4 bytes, far below the 8 KiB bitmap, so
        // the canonical choice is a run container. We construct it via
        // `push_bucket` (which calls `from_offsets` and properly considers runs)
        // rather than incremental `insert()` (which transitions array→bitmap and
        // skips the run check).
        let values: Vec<u16> = (0..5_000).collect();
        let (run_offsets, run_lengths) = run::from_offsets(&values);
        let container = Container::Run {
            offsets: run_offsets,
            lengths: run_lengths,
        };
        let mut bitmap = RoaringBitmap::new();
        bitmap.push_bucket(7, container);

        assert!(matches!(bitmap.container(7), Some(Container::Run { .. })));

        let image = bitmap.serialize();
        let decoded = RoaringBitmap::deserialize(&image).expect("round-trip must succeed");
        assert_eq!(decoded, bitmap);

        assert!(decoded.contains(u32::from(7u16) << 16));
        assert!(decoded.contains(u32::from(7u16) << 16 | 4999));
        assert!(!decoded.contains(u32::from(7u16) << 16 | 5000));
    }
}
