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
//! Bounds-checked little-endian helpers and the footer shared by every
//! persisted filter image.
//!
//! Every filter image in this crate is a **body** followed by a fixed 40-byte
//! **footer**, in the same style as the columnar pruning trailer: the footer is
//! self-describing (magic + format version), explicit (counts and lengths are
//! stored, never inferred), and integrity-checked (CRC32C over the body and
//! over the footer itself).
//!
//! ```text
//! [ body: structure-specific records ] ‖ footer (40 bytes, the last 40):
//!    0..4   magic (u8[4])      "PLRB" (Roaring) / "PLXF" (XOR filter)
//!    4..8   version (u32)      ROARING_FORMAT_VERSION / XOR_FILTER_FORMAT_VERSION
//!    8..12  flags (u32)        must be 0 — unknown flags are rejected
//!   12..16  reserved (u32)     must be 0
//!   16..24  body_len (u64)     length of the body in bytes
//!   24..28  body_crc (u32)     CRC32C over the body bytes
//!   28..32  footer_crc (u32)   CRC32C over footer[..28] ‖ footer[32..40]
//!   32..40  total_len (u64)    body_len + 40
//! ```
//!
//! [`frame`] appends that footer; [`unframe`] validates it and hands back the
//! body. Both length fields are cross-checks — `total_len` must equal the image
//! length and `body_len` must equal `total_len - 40` — so a damaged length is
//! reported as corruption instead of silently moving the body boundary. The
//! footer carries its own checksum, so a decoder rejects a damaged footer
//! *before* it believes a single length inside it.
//!
//! Unknown magic, version, flags, reserved bytes, and every checksum mismatch
//! are errors: a filter image is never "approximately right".
//!
//! CRC32C comes from the existing [`plomid_storage`] facade (which wraps the
//! isolated `crc32c` component); no second checksum implementation is
//! introduced.

use crate::constants::{ROARING_FOOTER_LEN, XOR_FOOTER_LEN};

/// Length of the footer that terminates every persisted filter image.
pub(crate) const FOOTER_LEN: usize = ROARING_FOOTER_LEN;

/// Compile-time proof that both filter kinds share one footer framing.
const _: () = assert!(ROARING_FOOTER_LEN == XOR_FOOTER_LEN);

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 4;
const OFF_FLAGS: usize = 8;
const OFF_RESERVED: usize = 12;
const OFF_BODY_LEN: usize = 16;
const OFF_BODY_CRC: usize = 24;
const OFF_FOOTER_CRC: usize = 28;
const OFF_TOTAL_LEN: usize = 32;

/// Returns the CRC32C checksum of `bytes`.
#[must_use]
pub(crate) fn checksum_of(bytes: &[u8]) -> u32 {
    crc32c::compute(bytes)
}

/// Returns the CRC32C checksum of two concatenated regions.
///
/// Used for the footer checksum, which covers the footer bytes around the
/// checksum field without materializing a third buffer.
#[must_use]
pub(crate) fn checksum_of_pair(first: &[u8], second: &[u8]) -> u32 {
    let state = crc32c::crc_init();
    let state = crc32c::crc_update(state, first);
    let state = crc32c::crc_update(state, second);
    crc32c::crc_finalize(state)
}

/// Copies `N` bytes at `offset`, or `None` when they are not all present.
fn bytes_at<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    let slice = bytes.get(offset..end)?;
    let mut buffer = [0u8; N];
    buffer.copy_from_slice(slice);
    Some(buffer)
}

/// Reads a little-endian `u16` at `offset`, or `None` when it is not present.
#[must_use]
pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    bytes_at::<2>(bytes, offset).map(u16::from_le_bytes)
}

/// Reads a little-endian `u32` at `offset`, or `None` when it is not present.
#[must_use]
pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes_at::<4>(bytes, offset).map(u32::from_le_bytes)
}

/// Reads a little-endian `u64` at `offset`, or `None` when it is not present.
#[must_use]
pub(crate) fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes_at::<8>(bytes, offset).map(u64::from_le_bytes)
}

/// Appends a little-endian `u16` to `out`.
pub(crate) fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Appends a little-endian `u32` to `out`.
pub(crate) fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Appends a little-endian `u64` to `out`.
pub(crate) fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Writes a little-endian `u32` into a fixed buffer.
///
/// A buffer too short for the field is left untouched: writers in this crate
/// size their own buffers, so this is a belt-and-braces guard, not an error
/// path.
fn put_u32(out: &mut [u8], offset: usize, value: u32) {
    let Some(end) = offset.checked_add(4) else {
        return;
    };
    let Some(slice) = out.get_mut(offset..end) else {
        return;
    };
    slice.copy_from_slice(&value.to_le_bytes());
}

/// Writes a little-endian `u64` into a fixed buffer.
///
/// Behaves like [`put_u32`]: a short buffer is left untouched.
fn put_u64(out: &mut [u8], offset: usize, value: u64) {
    let Some(end) = offset.checked_add(8) else {
        return;
    };
    let Some(slice) = out.get_mut(offset..end) else {
        return;
    };
    slice.copy_from_slice(&value.to_le_bytes());
}

/// Wraps `body` in the shared footer, returning the complete image.
#[must_use]
pub(crate) fn frame(magic: [u8; 4], version: u32, body: &[u8]) -> Vec<u8> {
    let body_crc = checksum_of(body);
    let body_len = u64::try_from(body.len()).unwrap_or(u64::MAX);
    let total_len = u64::try_from(body.len() + FOOTER_LEN).unwrap_or(u64::MAX);

    let mut footer = [0u8; FOOTER_LEN];
    footer[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&magic);
    put_u32(&mut footer, OFF_VERSION, version);
    put_u32(&mut footer, OFF_FLAGS, 0);
    put_u32(&mut footer, OFF_RESERVED, 0);
    put_u64(&mut footer, OFF_BODY_LEN, body_len);
    put_u32(&mut footer, OFF_BODY_CRC, body_crc);
    put_u64(&mut footer, OFF_TOTAL_LEN, total_len);
    // The footer checksum covers every footer byte except the checksum field
    // itself, so `total_len` is protected too.
    let footer_crc = checksum_of_pair(&footer[..OFF_FOOTER_CRC], &footer[OFF_FOOTER_CRC + 4..]);
    put_u32(&mut footer, OFF_FOOTER_CRC, footer_crc);

    let mut image = Vec::with_capacity(body.len() + FOOTER_LEN);
    image.extend_from_slice(body);
    image.extend_from_slice(&footer);
    image
}

/// Validates the footer of `image` and returns the body it frames.
///
/// # Errors
///
/// Returns a static description when the image is shorter than a footer, when
/// the magic, version, flags, or reserved bytes are not exactly what this
/// format writes, when the two length fields disagree with the image, or when
/// either checksum does not match. Callers map the description onto their own
/// error type; a filter is never decoded from a footer that failed any check.
pub(crate) fn unframe(magic: [u8; 4], version: u32, image: &[u8]) -> Result<&[u8], &'static str> {
    let Some(footer_start) = image.len().checked_sub(FOOTER_LEN) else {
        return Err("image shorter than the filter footer");
    };
    let footer = &image[footer_start..];

    if footer[OFF_MAGIC..OFF_MAGIC + 4] != magic {
        return Err("filter magic mismatch");
    }
    if read_u32(footer, OFF_VERSION) != Some(version) {
        return Err("unknown filter format version");
    }
    if read_u32(footer, OFF_FLAGS) != Some(0) {
        return Err("unknown filter flags");
    }
    if read_u32(footer, OFF_RESERVED) != Some(0) {
        return Err("non-zero reserved footer field");
    }

    let stored_footer_crc =
        read_u32(footer, OFF_FOOTER_CRC).ok_or("truncated filter footer checksum")?;
    if checksum_of_pair(&footer[..OFF_FOOTER_CRC], &footer[OFF_FOOTER_CRC + 4..])
        != stored_footer_crc
    {
        return Err("filter footer checksum mismatch");
    }

    let body_len = read_u64(footer, OFF_BODY_LEN).ok_or("truncated filter body length")?;
    let total_len = read_u64(footer, OFF_TOTAL_LEN).ok_or("truncated filter total length")?;
    let body_len = usize::try_from(body_len).map_err(|_| "filter body length out of range")?;
    let Some(total) = body_len.checked_add(FOOTER_LEN) else {
        return Err("filter length overflow");
    };
    if total != image.len() || usize::try_from(total_len).ok() != Some(total) {
        return Err("filter length fields disagree with the image");
    }

    let (body, _) = image.split_at(body_len);
    let stored_body_crc = read_u32(footer, OFF_BODY_CRC).ok_or("truncated filter body checksum")?;
    if checksum_of(body) != stored_body_crc {
        return Err("filter body checksum mismatch");
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{frame, unframe, FOOTER_LEN};

    const MAGIC: [u8; 4] = *b"PLTB";
    const VERSION: u32 = 1;

    /// Builds a framed body so damage can be injected at known offsets.
    fn framed() -> Vec<u8> {
        frame(MAGIC, VERSION, b"filter body bytes")
    }

    #[test]
    fn round_trip_returns_the_body() {
        let image = framed();
        assert_eq!(image.len(), "filter body bytes".len() + FOOTER_LEN);
        let body = unframe(MAGIC, VERSION, &image).expect("well-formed image must decode");
        assert_eq!(body, b"filter body bytes");
    }

    #[test]
    fn empty_body_is_framed_and_recovered() {
        let image = frame(MAGIC, VERSION, &[]);
        assert_eq!(image.len(), FOOTER_LEN);
        assert_eq!(unframe(MAGIC, VERSION, &image), Ok(&[][..]));
    }

    #[test]
    fn wrong_magic_is_rejected() {
        assert!(unframe(*b"PLXX", VERSION, &framed()).is_err());
    }

    #[test]
    fn unknown_version_is_rejected() {
        assert!(unframe(MAGIC, VERSION + 1, &framed()).is_err());
    }

    #[test]
    fn every_truncated_length_is_rejected() {
        let image = framed();
        for cut in 1..=FOOTER_LEN {
            let short = &image[..image.len() - cut];
            assert!(unframe(MAGIC, VERSION, short).is_err(), "cut {cut}");
        }
    }

    #[test]
    fn any_single_byte_flip_is_detected() {
        let image = framed();
        for index in 0..image.len() {
            let mut damaged = image.clone();
            damaged[index] ^= 0x40;
            assert!(
                unframe(MAGIC, VERSION, &damaged).is_err(),
                "flip at byte {index} must be rejected"
            );
        }
    }
}
