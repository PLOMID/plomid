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
//! Bounds-checked little-endian helpers for the columnar format.
//!
//! The columnar format addresses every field by absolute offset, so the helpers
//! here are offset-based rather than cursor-based. Reads are fully checked: a
//! short or malformed segment yields a structured [`ErrorKind::Corruption`]
//! error instead of a panic, which is what lets a damaged segment be rejected
//! during recovery. Writes target buffers that the writer itself sized, so they
//! index directly and never fail.

use plomid_core::{ErrorKind, PlomidError, Result};

/// Builds a corruption error for malformed persisted columnar data.
#[must_use]
pub(crate) fn corruption(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}

/// Builds an invalid-argument error for a rejected request.
#[must_use]
pub(crate) fn invalid(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::InvalidArgument, message)
}

/// Returns the CRC32C checksum of `bytes`.
#[must_use]
pub(crate) fn checksum_of(bytes: &[u8]) -> u32 {
    plomid_storage::compute_checksum(bytes)
}

/// Returns the CRC32C checksum of two concatenated regions.
///
/// Used by chunk framing, whose checksum covers the header prefix and the
/// stored payload without materializing a third buffer.
#[must_use]
pub(crate) fn checksum_of_pair(first: &[u8], second: &[u8]) -> u32 {
    let state = crc32c::crc_init();
    let state = crc32c::crc_update(state, first);
    let state = crc32c::crc_update(state, second);
    crc32c::crc_finalize(state)
}

/// Returns the byte range `[offset, offset + len)` of `bytes`.
pub(crate) fn get_slice<'a>(
    bytes: &'a [u8],
    offset: usize,
    len: usize,
    what: &str,
) -> Result<&'a [u8]> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| corruption(format!("{what} range overflows")))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| corruption(format!("{what} lies outside the segment")))
}

/// Reads a little-endian `u8`.
pub(crate) fn get_u8(bytes: &[u8], offset: usize, what: &str) -> Result<u8> {
    Ok(get_slice(bytes, offset, 1, what)?[0])
}

/// Reads a little-endian `u32`.
pub(crate) fn get_u32(bytes: &[u8], offset: usize, what: &str) -> Result<u32> {
    let slice = get_slice(bytes, offset, 4, what)?;
    let array: [u8; 4] = slice
        .try_into()
        .map_err(|_| corruption(format!("{what} is truncated")))?;
    Ok(u32::from_le_bytes(array))
}

/// Reads a little-endian `u64`.
pub(crate) fn get_u64(bytes: &[u8], offset: usize, what: &str) -> Result<u64> {
    let slice = get_slice(bytes, offset, 8, what)?;
    let array: [u8; 8] = slice
        .try_into()
        .map_err(|_| corruption(format!("{what} is truncated")))?;
    Ok(u64::from_le_bytes(array))
}

/// Writes a little-endian `u8`.
pub(crate) fn put_u8(out: &mut [u8], offset: usize, value: u8) {
    out[offset] = value;
}

/// Writes a little-endian `u32`.
pub(crate) fn put_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes a little-endian `u64`.
pub(crate) fn put_u64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Converts a persisted `u64` to `usize`, rejecting values that cannot index.
pub(crate) fn to_usize(value: u64, what: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| corruption(format!("{what} does not fit in memory")))
}

#[cfg(test)]
mod tests {
    use super::{checksum_of, checksum_of_pair, get_u32, get_u64, put_u32, put_u64, to_usize};
    use plomid_core::ErrorKind;

    #[test]
    fn round_trips_little_endian_values() {
        let mut bytes = [0_u8; 12];
        put_u32(&mut bytes, 0, 0x1234_5678);
        put_u64(&mut bytes, 4, 0x0102_0304_0506_0708);
        assert_eq!(get_u32(&bytes, 0, "value").expect("u32"), 0x1234_5678);
        assert_eq!(
            get_u64(&bytes, 4, "value").expect("u64"),
            0x0102_0304_0506_0708
        );
    }

    #[test]
    fn reads_outside_the_buffer_are_rejected() {
        let bytes = [0_u8; 4];
        let error = get_u64(&bytes, 0, "value").expect_err("truncated must fail");
        assert_eq!(error.kind(), ErrorKind::Corruption);
        assert!(get_u32(&bytes, 1, "value").is_err());
        assert!(get_u32(&bytes, usize::MAX, "value").is_err());
    }

    #[test]
    fn pair_checksum_matches_concatenation() {
        let first = [1_u8, 2, 3];
        let second = [4_u8, 5, 6, 7];
        let mut joined = Vec::new();
        joined.extend_from_slice(&first);
        joined.extend_from_slice(&second);
        assert_eq!(checksum_of_pair(&first, &second), checksum_of(&joined));
    }

    #[test]
    fn oversized_offsets_are_rejected() {
        assert_eq!(to_usize(7, "offset").expect("fits"), 7);
        if usize::BITS < 64 {
            assert!(to_usize(u64::MAX, "offset").is_err());
        }
    }
}
