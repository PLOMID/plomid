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
//! Persistence of pruning metadata as a self-describing segment trailer.
//!
//! The pruning metadata is **appended** to the finished columnar segment
//! image as a trailer. This keeps the pre-existing regions (header, column
//! metadata, null bitmaps, statistics, chunk table) byte-for-byte unchanged —
//! their checksums are computed before the trailer exists and never cover it —
//! while readers that understand the trailer attach the metadata after
//! decoding the body.
//!
//! # Layout (all little-endian, all lengths explicit)
//!
//! ```text
//! [ segment image: header, metadata, bitmaps, statistics, chunk table ] ‖
//! trailer body: range_count × range record:
//!     range header (24 bytes):
//!        0..8   start_row (u64)
//!        8..16  end_row (u64)
//!       16..20  zone_count (u32)
//!       20..24  reserved (u32, must be 0)
//!     zone_count × zone map blobs (see `encode_zone`)
//! ‖ trailer footer (48 bytes, the last 48 bytes of the image):
//!    0..4   magic "PLPM"
//!    4..8   version (u32, = PRUNING_FORMAT_VERSION)
//!    8..12  flags (u32, must be 0 — unknown flags are rejected)
//!   12..16  reserved (u32, must be 0)
//!   16..20  range_count (u32)
//!   20..24  reserved (u32, must be 0)
//!   24..32  body_len (u64)
//!   32..36  body_crc (u32, CRC32C over the trailer body bytes)
//!   36..40  footer_crc (u32, CRC32C over every other footer byte —
//!           footer[..36] ‖ footer[40..48] — so it covers `total_len` too)
//!   40..48  total_len (u64, footer + body)
//! ```
//!
//! # Locating the trailer
//!
//! The footer sits at a *fixed* offset from the end of the image, and it opens
//! with the magic. [`locate_trailer`] therefore decides "metadata present" from
//! the magic alone and then reads `body_len` from the footer to find where the
//! trailer starts — it never has to trust a length field it has not verified.
//! The two length fields are cross-checks: `body_len` must equal the distance
//! from the trailer start to the footer, and `total_len` must equal
//! `body_len + 48`, so a damaged length is reported as corruption instead of
//! being mistaken for "no metadata".
//!
//! [`decode_trailer_at`] is the reader path: the segment decoder already knows
//! where the body ends (the end of the chunk table, derived from
//! integrity-checked chunk framing), so it anchors the trailer there and
//! requires the trailer to run from that anchor to the exact end of the image.
//! [`decode_trailer`] is the same validation for a blob that *is* the trailer
//! image. Both paths require magic, version, flags, reserved bytes, the
//! checksums, both lengths, and the tile-structure check to pass; anything else
//! is an error, never "approximately right" metadata.
//!
//! A trailer that is absent yields `Ok(None)` from both paths: the caller then
//! scans everything, because missing metadata can never justify pruning.
//!
//! Integrity reuses the crate's centralized CRC32C helper
//! ([`crate::layout::checksum_of`], which the whole segment format shares);
//! no second checksum implementation is introduced.
//!
//! # Zone map blob layout (40-byte header + bound payloads)
//!
//! ```text
//!    0..8   column_id (u64)
//!    8      column_type tag (u8)
//!    9      null_state tag (u8: 0 = NO_NULLS, 1 = HAS_NULLS, 2 = ALL_NULLS)
//!   10      flags (u8: 0x01 min present, 0x02 max present — always together)
//!   11      reserved (u8, must be 0)
//!   12..20  start_row (u64)
//!   20..28  row_count (u64)
//!   28..32  min_len (u32)
//!   32..36  max_len (u32)
//!   36..40  zone_crc (u32, CRC32C over header[..36] ‖ min ‖ max)
//!   min_len bytes: minimum bound, encoded as a single-field storage Row
//!   max_len bytes: maximum bound, encoded as a single-field storage Row
//! ```
//!
//! Bounds reuse the existing storage `Row` encoding rather than inventing a
//! second value encoding; a decoder re-validates the pair (`min <= max`,
//! both bounds present or both absent, both decodable and non-NULL).

use super::brin::{BrinIndex, BrinRange};
use super::zonemap::{NullState, ZoneMap};
use crate::column::ColumnType;
use crate::layout::{
    checksum_of, checksum_of_pair, corruption, get_u32, get_u64, get_u8, put_u32, put_u64,
};
use crate::statistics::compare_fields;
use plomid_core::{ColumnId, ErrorKind, PlomidError, Result};
use plomid_storage::{Field, Row};
use std::cmp::Ordering;

/// Magic prefix of the pruning trailer (`"PLPM"`).
pub const PRUNING_MAGIC: [u8; 4] = *b"PLPM";
/// Version of the trailer encoding described in this module.
pub const PRUNING_FORMAT_VERSION: u32 = 1;
/// Trailer header size in bytes.
pub const PRUNING_TRAILER_HEADER_LEN: usize = 48;
/// Zone map header size in bytes.
pub const PRUNING_ZONE_HEADER_LEN: usize = 40;
/// BRIN range header size in bytes.
pub const PRUNING_RANGE_HEADER_LEN: usize = 24;
/// Upper bound on decoded range/zone counts (allocation guard).
pub const MAX_DECODED_RANGES: usize = 1 << 20;
/// Upper bound on decoded zones per range (allocation guard).
pub const MAX_DECODED_ZONES_PER_RANGE: usize = 1 << 20;
/// Upper bound on a single bound payload (allocation guard).
pub const MAX_BOUND_BYTES: usize = 16 * 1024 * 1024;

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 4;
const OFF_FLAGS: usize = 8;
const OFF_RESERVED: usize = 12;
const OFF_RANGE_COUNT: usize = 16;
const OFF_RESERVED2: usize = 20;
const OFF_BODY_LEN: usize = 24;
const OFF_BODY_CRC: usize = 32;
const OFF_HEADER_CRC: usize = 36;
const OFF_TOTAL_LEN: usize = 40;

const ZONE_OFF_COLUMN_ID: usize = 0;
const ZONE_OFF_TYPE_TAG: usize = 8;
const ZONE_OFF_NULL_STATE: usize = 9;
const ZONE_OFF_FLAGS: usize = 10;
const ZONE_OFF_RESERVED: usize = 11;
const ZONE_OFF_START_ROW: usize = 12;
const ZONE_OFF_ROW_COUNT: usize = 20;
const ZONE_OFF_MIN_LEN: usize = 28;
const ZONE_OFF_MAX_LEN: usize = 32;
const ZONE_OFF_CRC: usize = 36;

const RANGE_OFF_START_ROW: usize = 0;
const RANGE_OFF_END_ROW: usize = 8;
const RANGE_OFF_ZONE_COUNT: usize = 16;
const RANGE_OFF_RESERVED: usize = 20;

const ZONE_FLAG_MIN_AVAILABLE: u8 = 0x01;
const ZONE_FLAG_MAX_AVAILABLE: u8 = 0x02;
const ZONE_FLAGS_KNOWN: u8 = ZONE_FLAG_MIN_AVAILABLE | ZONE_FLAG_MAX_AVAILABLE;

/// Encodes one zone map into its deterministic little-endian blob.
///
/// Bounds are persisted only as a pair (both or neither): a lone minimum or
/// maximum cannot prove a range empty, so half metadata is worse than none.
/// Each bound is encoded with the existing storage [`Row`] encoding, keeping
/// value representation in one place.
#[must_use]
pub fn encode_zone(zone: &ZoneMap) -> Vec<u8> {
    let bounds = match (&zone.min, &zone.max) {
        (Some(min), Some(max)) => {
            // Encoding a Field as a single-field storage Row cannot fail for
            // the value types pruning supports; if it ever did, dropping the
            // bounds (scan-conservative) beats emitting a truncated payload.
            match (
                Row::new(vec![min.clone()]).encode(),
                Row::new(vec![max.clone()]).encode(),
            ) {
                (Ok(min_row), Ok(max_row)) if !min_row.is_empty() && !max_row.is_empty() => {
                    Some((min_row, max_row))
                }
                _ => None,
            }
        }
        _ => None,
    };
    let mut flags = 0_u8;
    let (min_bytes, max_bytes) = match bounds {
        Some((min_row, max_row)) => {
            flags |= ZONE_FLAG_MIN_AVAILABLE | ZONE_FLAG_MAX_AVAILABLE;
            (min_row, max_row)
        }
        None => (Vec::new(), Vec::new()),
    };
    let mut header = vec![0_u8; PRUNING_ZONE_HEADER_LEN];
    put_u64(&mut header, ZONE_OFF_COLUMN_ID, zone.column_id.get());
    header[ZONE_OFF_TYPE_TAG] = zone.column_type.tag();
    header[ZONE_OFF_NULL_STATE] = zone.null_state.tag();
    header[ZONE_OFF_FLAGS] = flags;
    header[ZONE_OFF_RESERVED] = 0;
    put_u64(&mut header, ZONE_OFF_START_ROW, zone.start_row);
    put_u64(&mut header, ZONE_OFF_ROW_COUNT, zone.row_count);
    put_u32(&mut header, ZONE_OFF_MIN_LEN, min_bytes.len() as u32);
    put_u32(&mut header, ZONE_OFF_MAX_LEN, max_bytes.len() as u32);
    let mut payload = min_bytes;
    payload.extend_from_slice(&max_bytes);
    let crc = checksum_of_pair(&header[..ZONE_OFF_CRC], &payload);
    put_u32(&mut header, ZONE_OFF_CRC, crc);
    header.extend_from_slice(&payload);
    header
}

/// Decodes one zone map from the start of `bytes`, returning the map and the
/// number of bytes consumed.
///
/// Every field is validated: unknown type tags, unknown null states, unknown
/// flag bits, nonzero reserved bytes, half-present bounds, oversized or
/// undecodable payloads, NULL bounds, and `min > max` (or mutually
/// incomparable bounds) are all corruption. A decoder error means the
/// metadata is untrustworthy, never "use what you could read".
pub fn decode_zone(bytes: &[u8]) -> Result<(ZoneMap, usize)> {
    if bytes.len() < PRUNING_ZONE_HEADER_LEN {
        return Err(corruption("pruning zone map header is truncated"));
    }
    let header = &bytes[..PRUNING_ZONE_HEADER_LEN];
    let column_id = ColumnId::new(get_u64(
        header,
        ZONE_OFF_COLUMN_ID,
        "pruning zone column id",
    )?);
    let type_tag = get_u8(header, ZONE_OFF_TYPE_TAG, "pruning zone type tag")?;
    let column_type = ColumnType::from_tag(type_tag).ok_or_else(|| {
        corruption(format!(
            "pruning zone map has unknown column type tag {type_tag}"
        ))
    })?;
    let null_state = NullState::from_tag(get_u8(
        header,
        ZONE_OFF_NULL_STATE,
        "pruning zone null state",
    )?)?;
    let flags = get_u8(header, ZONE_OFF_FLAGS, "pruning zone flags")?;
    if flags & !ZONE_FLAGS_KNOWN != 0 {
        return Err(corruption(format!(
            "pruning zone map carries unknown flags {flags:#04x}"
        )));
    }
    if get_u8(header, ZONE_OFF_RESERVED, "pruning zone reserved")? != 0 {
        return Err(corruption("pruning zone reserved field is nonzero"));
    }
    let start_row = get_u64(header, ZONE_OFF_START_ROW, "pruning zone start row")?;
    let row_count = get_u64(header, ZONE_OFF_ROW_COUNT, "pruning zone row count")?;
    let min_len = get_u32(header, ZONE_OFF_MIN_LEN, "pruning zone min length")? as usize;
    let max_len = get_u32(header, ZONE_OFF_MAX_LEN, "pruning zone max length")? as usize;
    if min_len > MAX_BOUND_BYTES || max_len > MAX_BOUND_BYTES {
        return Err(corruption("pruning zone bound length is impossibly large"));
    }
    // Bounds travel as a pair: one flag without the other, payloads without
    // flags, or flags without payloads are all malformed.
    let min_available = flags & ZONE_FLAG_MIN_AVAILABLE != 0;
    let max_available = flags & ZONE_FLAG_MAX_AVAILABLE != 0;
    if min_available != max_available {
        return Err(corruption("pruning zone map advertises only one bound"));
    }
    if !min_available && (min_len != 0 || max_len != 0) {
        return Err(corruption(
            "pruning zone map without bounds carries bound payloads",
        ));
    }
    if min_available && (min_len == 0 || max_len == 0) {
        return Err(corruption("pruning zone map bound payload is empty"));
    }
    let total = PRUNING_ZONE_HEADER_LEN
        .checked_add(min_len)
        .and_then(|total| total.checked_add(max_len))
        .ok_or_else(|| corruption("pruning zone map length overflows"))?;
    if bytes.len() < total {
        return Err(corruption("pruning zone map bound payload is truncated"));
    }
    let payload = &bytes[PRUNING_ZONE_HEADER_LEN..total];
    let stored_crc = get_u32(header, ZONE_OFF_CRC, "pruning zone checksum")?;
    if checksum_of_pair(&header[..ZONE_OFF_CRC], payload) != stored_crc {
        return Err(corruption("pruning zone map checksum mismatch"));
    }
    let (min, max) = if min_available {
        let min = decode_bound(
            &bytes[PRUNING_ZONE_HEADER_LEN..PRUNING_ZONE_HEADER_LEN + min_len],
            "minimum",
        )?;
        let max = decode_bound(&bytes[PRUNING_ZONE_HEADER_LEN + min_len..total], "maximum")?;
        (min, max)
    } else {
        (None, None)
    };
    // A builder never emits crossed bounds; a decoder refuses them.
    if let (Some(min), Some(max)) = (&min, &max) {
        match compare_fields(min, max) {
            Some(Ordering::Greater) => {
                return Err(corruption("pruning zone map minimum exceeds its maximum"));
            }
            None => {
                return Err(corruption(
                    "pruning zone map bounds are not mutually comparable",
                ));
            }
            _ => {}
        }
    }
    Ok((
        ZoneMap {
            column_id,
            column_type,
            start_row,
            row_count,
            null_state,
            min,
            max,
        },
        total,
    ))
}

/// Decodes one bound payload: a single-field, non-NULL storage `Row`.
fn decode_bound(bytes: &[u8], which: &str) -> Result<Option<Field>> {
    let row = Row::decode(bytes).map_err(|error| {
        PlomidError::new(
            error.kind(),
            format!("pruning zone map {which} is not a decodable row payload: {error}"),
        )
    })?;
    if row.fields().len() != 1 {
        return Err(corruption(format!(
            "pruning zone map {which} has an unexpected field count"
        )));
    }
    let field = row.into_fields().pop().unwrap_or(Field::Null);
    if matches!(field, Field::Null) {
        return Err(corruption(format!(
            "pruning zone map {which} payload is NULL"
        )));
    }
    Ok(Some(field))
}

/// Encodes the pruning trailer for a segment's metadata.
///
/// The body holds one range record per BRIN range; each record embeds that
/// range's zone-map blobs. `body_crc` covers the body only, `footer_crc`
/// covers every other footer byte, and `total_len` is the very last field of
/// the image — so a reader holding only bytes can locate the trailer from the
/// end without any out-of-band length, while a reader that already knows where
/// the segment body ends can anchor on that offset instead.
#[must_use]
pub fn encode_trailer(brin: &BrinIndex) -> Vec<u8> {
    let mut out = Vec::new();
    for range in &brin.ranges {
        let mut header = vec![0_u8; PRUNING_RANGE_HEADER_LEN];
        put_u64(&mut header, RANGE_OFF_START_ROW, range.start_row);
        put_u64(&mut header, RANGE_OFF_END_ROW, range.end_row);
        put_u32(
            &mut header,
            RANGE_OFF_ZONE_COUNT,
            range.zone_maps.len() as u32,
        );
        header[RANGE_OFF_RESERVED..RANGE_OFF_RESERVED + 4].copy_from_slice(&[0; 4]);
        out.extend_from_slice(&header);
        for zone in &range.zone_maps {
            out.extend_from_slice(&encode_zone(zone));
        }
    }
    let body_len = out.len();
    let body_crc = checksum_of(&out);
    let total_len = (PRUNING_TRAILER_HEADER_LEN as u64).saturating_add(body_len as u64);
    let mut footer = vec![0_u8; PRUNING_TRAILER_HEADER_LEN];
    footer[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&PRUNING_MAGIC);
    put_u32(&mut footer, OFF_VERSION, PRUNING_FORMAT_VERSION);
    put_u32(&mut footer, OFF_FLAGS, 0);
    footer[OFF_RESERVED..OFF_RESERVED + 4].copy_from_slice(&[0; 4]);
    put_u32(&mut footer, OFF_RANGE_COUNT, brin.ranges.len() as u32);
    footer[OFF_RESERVED2..OFF_RESERVED2 + 4].copy_from_slice(&[0; 4]);
    put_u64(&mut footer, OFF_BODY_LEN, body_len as u64);
    put_u32(&mut footer, OFF_BODY_CRC, body_crc);
    // `total_len` lives at the end of the image, followed only by the header
    // checksum that covers it. A corrupt length is therefore always reported
    // as corruption, never mistaken for "this image has no metadata".
    put_u64(&mut footer, OFF_TOTAL_LEN, total_len);
    let crc = framing_checksum(&footer);
    put_u32(&mut footer, OFF_HEADER_CRC, crc);
    out.extend_from_slice(&footer);
    out
}

/// Returns the framing checksum over every footer field except the checksum
/// itself: `footer[..OFF_HEADER_CRC]` ‖ `footer[OFF_TOTAL_LEN..]`.
///
/// Covering the trailing `total_len` matters: that field is what a reader uses
/// to find the trailer, so it must never be usable while damaged.
#[must_use]
fn framing_checksum(footer: &[u8]) -> u32 {
    checksum_of_pair(
        &footer[..OFF_HEADER_CRC],
        &footer[OFF_TOTAL_LEN..PRUNING_TRAILER_HEADER_LEN],
    )
}

/// Locates the trailer bounds `(header_start, total_len)` in `bytes`, or
/// `None` when the image does not end with a structurally plausible trailer.
///
/// This is a cheap pre-check, not validation: the caller still verifies
/// magic, versions, checksums, and lengths afterwards.
fn locate_trailer(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.len() < PRUNING_TRAILER_HEADER_LEN {
        return None;
    }
    // The footer is the last 48 bytes of the image and opens with the magic, so
    // "metadata present" is decided from a fixed offset; `total_len` (the last
    // eight bytes) is only used to find where the trailer *starts*.
    let footer_start = bytes.len() - PRUNING_TRAILER_HEADER_LEN;
    if bytes[footer_start..footer_start + 4] != PRUNING_MAGIC {
        return None;
    }
    let total_len = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().ok()?) as usize;
    if total_len < PRUNING_TRAILER_HEADER_LEN || total_len > bytes.len() {
        return None; // structurally impossible: callers must not trust it
    }
    Some((bytes.len() - total_len, total_len))
}

/// Returns true when `bytes` ends with a structurally plausible trailer.
#[must_use]
pub fn has_trailer(bytes: &[u8]) -> bool {
    locate_trailer(bytes).is_some()
}

/// Strips the trailer from `bytes` when present; otherwise returns `bytes`.
///
/// Useful for readers that need the pre-trailer image (for example to
/// checksum a region without the trailer).
#[must_use]
pub fn strip_trailer(bytes: &[u8]) -> &[u8] {
    match locate_trailer(bytes) {
        Some((header_start, _)) => &bytes[..header_start],
        None => bytes,
    }
}

/// Decodes the pruning trailer appended to a segment image.
///
/// * `Ok(None)` — no trailer: metadata unavailable, the caller scans
///   everything (the ordinary state for pre-existing segments).
/// * `Ok(Some(_))` — a fully validated trailer: ranges tile the segment
///   exactly, every zone map decodes and passes its checksum.
/// * `Err(_)` — trailer bytes are present but malformed, truncated,
///   versioned unknown, or fail a checksum. Such metadata must never be
///   trusted; callers fail the read like any other corrupt persisted
///   structure.
///
/// This entry point locates the trailer from the last eight bytes of the
/// image (the trailing `total_len` field). Readers that already know where the
/// segment body ends should prefer [`decode_trailer_at`], which does not have
/// to trust a value it has not yet verified.
pub fn decode_trailer(bytes: &[u8]) -> Result<Option<BrinIndex>> {
    if bytes.len() < PRUNING_TRAILER_HEADER_LEN {
        return Ok(None);
    }
    let footer_start = bytes.len() - PRUNING_TRAILER_HEADER_LEN;
    if bytes[footer_start..footer_start + 4] != PRUNING_MAGIC {
        // No magic: this image carries no metadata at all.
        return Ok(None);
    }
    // Magic present ⇒ metadata *claims* to be here. Every subsequent failure —
    // including an impossible length — is corruption, never "absent".
    let total_len = get_u64(bytes, bytes.len() - 8, "pruning trailer total length")?;
    let total_len = usize::try_from(total_len)
        .map_err(|_| corruption("pruning trailer total length does not fit this platform"))?;
    if total_len < PRUNING_TRAILER_HEADER_LEN || total_len > bytes.len() {
        return Err(corruption(
            "pruning trailer total length does not fit the segment image",
        ));
    }
    match decode_trailer_at(bytes, bytes.len() - total_len)? {
        Some(index) => Ok(Some(index)),
        None => Err(corruption(
            "pruning trailer anchors at the end of its image",
        )),
    }
}

/// Decodes the trailer whose region starts exactly at `trailer_start`.
///
/// `trailer_start` is the first byte after the segment body — the offset the
/// reader already derived from integrity-checked chunk framing. Because the
/// footer is the *last* 48 bytes of the image, anchoring on that trusted
/// offset means the footer's `total_len` is cross-checked against a value the
/// reader computed itself, so a damaged length can never be mistaken for
/// "no metadata":
///
/// * `trailer_start == bytes.len()` — the image ends with its body: no
///   trailer, scan everything.
/// * otherwise the bytes after the body must be a *complete, valid* trailer
///   (body followed by a 48-byte footer) that ends exactly at the end of the
///   image. Anything else is corruption.
pub fn decode_trailer_at(bytes: &[u8], trailer_start: usize) -> Result<Option<BrinIndex>> {
    if trailer_start > bytes.len() {
        return Err(corruption(
            "pruning trailer start lies outside the segment image",
        ));
    }
    if trailer_start == bytes.len() {
        // No trailing bytes at all: metadata is simply absent.
        return Ok(None);
    }
    let footer_start = bytes
        .len()
        .checked_sub(PRUNING_TRAILER_HEADER_LEN)
        .filter(|start| *start >= trailer_start)
        .ok_or_else(|| corruption("pruning trailer footer is truncated"))?;
    let footer = &bytes[footer_start..];
    let index = decode_trailer_region(bytes, trailer_start, footer)?;
    Ok(Some(index))
}

/// Shared strict decoder for a trailer occupying `bytes[trailer_start..]`,
/// with its 48-byte footer passed as `footer` (the last 48 bytes of `bytes`).
///
/// Validates the footer framing, then the body, and requires both lengths to
/// agree with the region the caller located.
fn decode_trailer_region(bytes: &[u8], trailer_start: usize, footer: &[u8]) -> Result<BrinIndex> {
    if footer[OFF_MAGIC..OFF_MAGIC + 4] != PRUNING_MAGIC {
        return Err(corruption("pruning trailer magic mismatch"));
    }
    let version = get_u32(footer, OFF_VERSION, "pruning trailer version")?;
    if version != PRUNING_FORMAT_VERSION {
        return Err(PlomidError::new(
            ErrorKind::Unsupported,
            format!("unsupported pruning trailer version {version}"),
        ));
    }
    if get_u32(footer, OFF_FLAGS, "pruning trailer flags")? != 0 {
        return Err(corruption("pruning trailer carries unknown flags"));
    }
    if footer[OFF_RESERVED..OFF_RESERVED + 4] != [0; 4]
        || footer[OFF_RESERVED2..OFF_RESERVED2 + 4] != [0; 4]
    {
        return Err(corruption("pruning trailer reserved field is nonzero"));
    }
    // The framing checksum covers `total_len` as well as every other footer
    // field, so a damaged length is corruption, never "no metadata".
    let stored_footer_crc = get_u32(footer, OFF_HEADER_CRC, "pruning trailer footer checksum")?;
    if framing_checksum(footer) != stored_footer_crc {
        return Err(corruption("pruning trailer footer checksum mismatch"));
    }
    let range_count = get_u32(footer, OFF_RANGE_COUNT, "pruning trailer range count")? as usize;
    if range_count > MAX_DECODED_RANGES {
        return Err(corruption(
            "pruning trailer range count is impossibly large",
        ));
    }
    let body_len = get_u64(footer, OFF_BODY_LEN, "pruning trailer body length")? as usize;
    let total_len_u64 = get_u64(footer, OFF_TOTAL_LEN, "pruning trailer total length")?;
    let total_len = usize::try_from(total_len_u64)
        .map_err(|_| corruption("pruning trailer total length does not fit this platform"))?;
    if body_len
        .checked_add(PRUNING_TRAILER_HEADER_LEN)
        .is_none_or(|expected| expected != total_len)
    {
        return Err(corruption(
            "pruning trailer body length disagrees with its total length",
        ));
    }
    // The trailer must be exactly the bytes after the segment body: the
    // caller-derived anchor is the independent check on `total_len`.
    let trailer_end = trailer_start
        .checked_add(total_len)
        .ok_or_else(|| corruption("pruning trailer length overflows the image"))?;
    if trailer_end != bytes.len() {
        return Err(corruption(
            "pruning trailer does not end at the end of the segment image",
        ));
    }
    let body = bytes
        .get(trailer_start..trailer_start + body_len)
        .ok_or_else(|| corruption("pruning trailer body lies outside the segment image"))?;
    if body.len() != body_len {
        return Err(corruption("pruning trailer body is truncated"));
    }
    let expected_body_crc = get_u32(footer, OFF_BODY_CRC, "pruning trailer body checksum")?;
    if checksum_of(body) != expected_body_crc {
        return Err(corruption("pruning trailer body checksum mismatch"));
    }
    let mut ranges = Vec::with_capacity(range_count.min(4096));
    let mut cursor = 0_usize;
    for _ in 0..range_count {
        let remaining = body
            .get(cursor..)
            .ok_or_else(|| corruption("pruning trailer range is truncated"))?;
        let (range, consumed) = decode_range(remaining)?;
        ranges.push(range);
        cursor += consumed;
    }
    if cursor != body.len() {
        return Err(corruption("pruning trailer has trailing body bytes"));
    }
    // The index must tile the segment it describes; otherwise the metadata
    // is inconsistent with itself and unusable for pruning.
    let row_count = ranges.iter().map(|range| range.end_row).max().unwrap_or(0);
    let rows_per_range = ranges
        .first()
        .map(|range| range.row_count().max(1))
        .unwrap_or(1);
    let index = BrinIndex::from_ranges(row_count, rows_per_range, ranges);
    index.validate()?;
    Ok(index)
}

/// Decodes one BRIN range record from the start of `bytes`.
fn decode_range(bytes: &[u8]) -> Result<(BrinRange, usize)> {
    if bytes.len() < PRUNING_RANGE_HEADER_LEN {
        return Err(corruption("BRIN range header is truncated"));
    }
    let header = &bytes[..PRUNING_RANGE_HEADER_LEN];
    let start_row = get_u64(header, RANGE_OFF_START_ROW, "BRIN range start row")?;
    let end_row = get_u64(header, RANGE_OFF_END_ROW, "BRIN range end row")?;
    let zone_count = get_u32(header, RANGE_OFF_ZONE_COUNT, "BRIN range zone count")? as usize;
    if header[RANGE_OFF_RESERVED..RANGE_OFF_RESERVED + 4] != [0; 4] {
        return Err(corruption("BRIN range reserved field is nonzero"));
    }
    if end_row < start_row {
        return Err(corruption("BRIN range ends before it starts"));
    }
    if zone_count > MAX_DECODED_ZONES_PER_RANGE {
        return Err(corruption("BRIN range zone count is impossibly large"));
    }
    let mut cursor = PRUNING_RANGE_HEADER_LEN;
    let mut zone_maps = Vec::with_capacity(zone_count.min(64));
    for _ in 0..zone_count {
        let remaining = bytes
            .get(cursor..)
            .ok_or_else(|| corruption("BRIN zone is truncated"))?;
        let (zone, consumed) = decode_zone(remaining)?;
        // Structural containment: a zone that escapes its range is corrupt,
        // never "approximately right".
        if zone.row_count > 0 && (zone.start_row < start_row || zone.end_row() > end_row) {
            return Err(corruption("BRIN zone escapes its range"));
        }
        zone_maps.push(zone);
        cursor += consumed;
    }
    Ok((BrinRange::new(start_row, end_row, zone_maps), cursor))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pruning::zonemap::ZoneMapBuilder;

    /// A representative small index: two ranges, two columns each.
    fn sample_index() -> BrinIndex {
        let mut ranges = Vec::new();
        for range_idx in 0_u64..2 {
            let start = range_idx * 4;
            let mut zones = Vec::new();
            for column in 0_u64..2 {
                let mut builder = ZoneMapBuilder::new(ColumnId::new(column), ColumnType::Integer);
                for row in start..start + 4 {
                    builder.observe(row, Some(Field::Integer(row as i64)));
                }
                zones.push(builder.finish());
            }
            ranges.push(BrinRange::new(start, start + 4, zones));
        }
        BrinIndex::from_ranges(8, 4, ranges)
    }

    fn sample_bytes() -> Vec<u8> {
        encode_trailer(&sample_index())
    }

    /// Length of the pre-trailer body in the synthetic images below.
    fn body_len() -> usize {
        128
    }

    /// Assembles `body ‖ trailer` the way the flush pipeline does.
    fn image_with_trailer() -> Vec<u8> {
        let mut image = vec![0xAB_u8; body_len()];
        image.extend_from_slice(&sample_bytes());
        image
    }

    /// Rewrites every checksum the format carries so that a byte-level mutation
    /// reaches the *structural* validators instead of being caught by the
    /// checksums. Without this, corruption tests would only exercise the CRC
    /// paths and never the semantic ones.
    ///
    /// Only checksums are rewritten: lengths, counts, versions, flags, and
    /// payload bytes are preserved exactly as the mutation left them, so a
    /// tampered length still disagrees with its counterpart and a tampered
    /// count still describes the wrong structure. The walk is fully
    /// bounds-checked and stops at the first unparseable record instead of
    /// panicking, so mutating a length to `u32::MAX` cannot crash the helper.
    ///
    /// `bytes` is one whole trailer region: `body ‖ footer`.
    fn reseal(bytes: &mut [u8]) {
        if bytes.len() < PRUNING_TRAILER_HEADER_LEN {
            return;
        }
        let footer_start = bytes.len() - PRUNING_TRAILER_HEADER_LEN;
        let mut cursor = 0_usize;
        while cursor < footer_start {
            let header = match bytes.get(cursor..cursor + PRUNING_RANGE_HEADER_LEN) {
                Some(header) => header,
                None => break,
            };
            let zone_count = match header
                .get(RANGE_OFF_ZONE_COUNT..RANGE_OFF_ZONE_COUNT + 4)
                .and_then(|slot| slot.try_into().ok())
                .map(u32::from_le_bytes)
            {
                Some(count) => count.min(MAX_DECODED_ZONES_PER_RANGE as u32) as usize,
                None => break,
            };
            // When the header claims more zones than the cap, the decoder
            // rejects the count; keep the claim intact and stop here so the
            // structural validator is what fires.
            let claimed = u32::from_le_bytes(
                header[RANGE_OFF_ZONE_COUNT..RANGE_OFF_ZONE_COUNT + 4]
                    .try_into()
                    .unwrap_or([0; 4]),
            ) as usize;
            if claimed != zone_count {
                break;
            }
            cursor += PRUNING_RANGE_HEADER_LEN;
            let mut stop = false;
            for _ in 0..zone_count {
                let zone_header = match bytes.get(cursor..cursor + PRUNING_ZONE_HEADER_LEN) {
                    Some(header) => header,
                    None => {
                        stop = true;
                        break;
                    }
                };
                let min_len = u32::from_le_bytes(
                    zone_header[ZONE_OFF_MIN_LEN..ZONE_OFF_MIN_LEN + 4]
                        .try_into()
                        .unwrap_or([0xFF; 4]),
                ) as usize;
                let max_len = u32::from_le_bytes(
                    zone_header[ZONE_OFF_MAX_LEN..ZONE_OFF_MAX_LEN + 4]
                        .try_into()
                        .unwrap_or([0xFF; 4]),
                ) as usize;
                let zone_len = match PRUNING_ZONE_HEADER_LEN
                    .checked_add(min_len)
                    .and_then(|sum| sum.checked_add(max_len))
                {
                    Some(len) => len,
                    None => {
                        stop = true;
                        break;
                    }
                };
                let (prefix, payload) = match (
                    bytes.get(cursor..cursor + ZONE_OFF_CRC),
                    bytes.get(cursor + PRUNING_ZONE_HEADER_LEN..cursor + zone_len),
                ) {
                    (Some(prefix), Some(payload)) => (prefix, payload),
                    _ => {
                        stop = true;
                        break;
                    }
                };
                let crc = checksum_of_pair(prefix, payload);
                if let Some(slot) = bytes.get_mut(cursor + ZONE_OFF_CRC..cursor + ZONE_OFF_CRC + 4)
                {
                    slot.copy_from_slice(&crc.to_le_bytes());
                } else {
                    stop = true;
                    break;
                }
                cursor += zone_len;
                if cursor > footer_start {
                    stop = true;
                    break;
                }
            }
            if stop {
                break;
            }
        }
        let body_crc = checksum_of(&bytes[..footer_start]);
        if let Some(footer) = bytes.get_mut(footer_start..) {
            put_u32(footer, OFF_BODY_CRC, body_crc);
            let crc = framing_checksum(footer);
            put_u32(footer, OFF_HEADER_CRC, crc);
        }
    }

    /// Applies `mutate` to the trailer of a fresh image, then reseals it so the
    /// structural validators are what reject the result.
    fn tampered(mutate: impl FnOnce(&mut [u8])) -> Vec<u8> {
        let mut image = image_with_trailer();
        let start = body_len();
        mutate(&mut image[start..]);
        reseal(&mut image[start..]);
        image
    }

    /// Offset of the first zone blob inside the trailer region.
    fn first_zone_offset() -> usize {
        PRUNING_RANGE_HEADER_LEN
    }

    /// Offset of the footer inside the trailer region.
    fn footer_offset() -> usize {
        sample_bytes().len() - PRUNING_TRAILER_HEADER_LEN
    }

    /// Offset of the second range record inside the trailer body.
    fn second_range_offset() -> usize {
        PRUNING_RANGE_HEADER_LEN + 2 * (PRUNING_ZONE_HEADER_LEN + 16)
    }

    fn expect_corruption(bytes: &[u8], trailer_start: usize, what: &str) {
        match decode_trailer_at(bytes, trailer_start) {
            Err(error) => assert_eq!(error.kind(), ErrorKind::Corruption, "{what}"),
            Ok(other) => panic!("{what}: expected corruption, decoded {other:?}"),
        }
    }
    #[test]
    fn trailer_round_trips_through_both_entry_points() {
        let image = image_with_trailer();
        let expected = sample_index();
        assert!(has_trailer(&image));
        assert_eq!(strip_trailer(&image).len(), body_len());
        assert_eq!(
            decode_trailer_at(&image, body_len())
                .expect("anchored")
                .expect("present"),
            expected
        );
        assert_eq!(
            decode_trailer(&image).expect("tail").expect("present"),
            expected
        );
        let tail = &image[image.len() - 8..];
        assert_eq!(
            u64::from_le_bytes(tail.try_into().unwrap()) as usize,
            sample_bytes().len()
        );
    }

    #[test]
    fn image_without_trailer_reports_absent_metadata() {
        let body = vec![0xCD_u8; body_len()];
        assert!(!has_trailer(&body));
        assert_eq!(strip_trailer(&body), &body[..]);
        assert!(decode_trailer_at(&body, body_len())
            .expect("absent")
            .is_none());
        assert!(decode_trailer(&body).expect("absent").is_none());
        // A body too short to hold even a trailer header is still "absent".
        assert!(decode_trailer_at(&body[..8], 8).expect("absent").is_none());
        assert!(decode_trailer(&body[..8]).expect("absent").is_none());
    }

    #[test]
    fn truncation_is_corruption_not_absent_metadata() {
        let image = image_with_trailer();
        for cut in 1..=PRUNING_TRAILER_HEADER_LEN {
            let truncated = &image[..image.len() - cut];
            expect_corruption(truncated, body_len(), &format!("cut {cut}"));
        }
    }

    #[test]
    fn corrupted_total_length_is_reported_not_swallowed() {
        // If the length anchor is damaged, the tail-based entry point must not
        // conclude "this image has no metadata": that would silently disable
        // pruning for a segment whose metadata is present but untrustworthy.
        let mut image = image_with_trailer();
        let len = image.len();
        let bogus = u64::from_le_bytes(image[len - 8..].try_into().unwrap()) + 8;
        image[len - 8..].copy_from_slice(&bogus.to_le_bytes());
        assert!(decode_trailer(&image).is_err(), "tail lookup must reject");
        expect_corruption(&image, body_len(), "anchored, long total_len");

        let mut image = image_with_trailer();
        let len = image.len();
        image[len - 8..].copy_from_slice(&0_u64.to_le_bytes());
        assert!(decode_trailer(&image).is_err());
        expect_corruption(&image, body_len(), "anchored, zero total_len");
    }

    #[test]
    fn trailer_must_end_exactly_at_the_end_of_the_image() {
        let mut image = image_with_trailer();
        image.push(0); // one trailing byte past the trailer
        assert!(decode_trailer_at(&image, body_len()).is_err());
        // A trailer start before the body end means the metadata does not
        // describe the image it claims to.
        let image = image_with_trailer();
        assert!(decode_trailer_at(&image, body_len() - 1).is_err());
        assert!(decode_trailer_at(&image, image.len() + 1).is_err());
    }

    #[test]
    fn checksum_corruption_in_either_region_is_rejected() {
        // A single flipped byte in the body, without any repair, must fail.
        let mut image = image_with_trailer();
        let target = body_len() + PRUNING_TRAILER_HEADER_LEN + PRUNING_RANGE_HEADER_LEN;
        image[target] ^= 0xFF;
        assert!(decode_trailer_at(&image, body_len()).is_err());
        // So must a flipped byte in the header.
        let mut image = image_with_trailer();
        image[body_len() + footer_offset() + OFF_RANGE_COUNT] ^= 0xFF;
        assert!(decode_trailer_at(&image, body_len()).is_err());
        let mut image = image_with_trailer();
        image[body_len() + footer_offset() + OFF_RESERVED] ^= 0xFF;
        assert!(decode_trailer_at(&image, body_len()).is_err());
    }

    #[test]
    fn unknown_version_is_unsupported_not_trusted() {
        let image = tampered(|trailer| {
            put_u32(
                trailer,
                footer_offset() + OFF_VERSION,
                PRUNING_FORMAT_VERSION + 1,
            );
        });
        let error = decode_trailer_at(&image, body_len()).expect_err("version");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn unknown_header_flags_and_nonzero_reserved_bits_are_rejected() {
        expect_corruption(
            &tampered(|trailer| put_u32(trailer, footer_offset() + OFF_FLAGS, 1)),
            body_len(),
            "flags",
        );
        expect_corruption(
            &tampered(|trailer| put_u32(trailer, footer_offset() + OFF_RESERVED2, 0xDEAD)),
            body_len(),
            "reserved2",
        );
    }

    #[test]
    fn malformed_range_counts_are_rejected() {
        expect_corruption(
            &tampered(|trailer| put_u32(trailer, footer_offset() + OFF_RANGE_COUNT, 0)),
            body_len(),
            "under-counted ranges",
        );
        expect_corruption(
            &tampered(|trailer| put_u32(trailer, footer_offset() + OFF_RANGE_COUNT, u32::MAX)),
            body_len(),
            "impossibly large range count",
        );
        expect_corruption(
            &tampered(|trailer| {
                put_u32(trailer, RANGE_OFF_ZONE_COUNT, u32::MAX);
            }),
            body_len(),
            "impossibly large zone count",
        );
    }

    #[test]
    fn body_length_disagreeing_with_total_length_is_rejected() {
        expect_corruption(
            &tampered(|trailer| {
                let len = footer_offset();
                let body_len =
                    get_u64(&trailer[len..], OFF_BODY_LEN, "body len").expect("read") + 1;
                put_u64(&mut trailer[len..], OFF_BODY_LEN, body_len);
            }),
            body_len(),
            "body length mismatch",
        );
    }

    #[test]
    fn impossible_range_tiling_is_rejected() {
        // A gap: the second range is nudged forward, leaving row 3 uncovered.
        let image = tampered(|trailer| {
            put_u64(trailer, second_range_offset() + RANGE_OFF_START_ROW, 5);
        });
        expect_corruption(&image, body_len(), "gapped tiling");
        // An overlap: the second range starts before the first ends.
        let image = tampered(|trailer| {
            put_u64(trailer, second_range_offset() + RANGE_OFF_START_ROW, 1);
        });
        expect_corruption(&image, body_len(), "overlapping tiling");
        // A range that ends before it starts.
        let image = tampered(|trailer| put_u64(trailer, RANGE_OFF_END_ROW, 0));
        expect_corruption(&image, body_len(), "inverted range");
    }

    #[test]
    fn invalid_zone_type_tag_and_null_state_are_rejected() {
        expect_corruption(
            &tampered(|trailer| trailer[first_zone_offset() + ZONE_OFF_TYPE_TAG] = 0xEE),
            body_len(),
            "invalid type tag",
        );
        for bad_state in [3_u8, 0x7F, 0xFF] {
            expect_corruption(
                &tampered(move |trailer| {
                    trailer[first_zone_offset() + ZONE_OFF_NULL_STATE] = bad_state;
                }),
                body_len(),
                "invalid null state",
            );
        }
    }

    #[test]
    fn unknown_zone_flags_and_reserved_bits_are_rejected() {
        expect_corruption(
            &tampered(|trailer| {
                trailer[first_zone_offset() + ZONE_OFF_FLAGS] |= 0x80;
            }),
            body_len(),
            "unknown zone flags",
        );
        // Half-present bounds are worse than none: a lone minimum cannot prove
        // a range empty, so the pair must be all-or-nothing.
        expect_corruption(
            &tampered(|trailer| {
                trailer[first_zone_offset() + ZONE_OFF_FLAGS] ^= ZONE_FLAG_MAX_AVAILABLE;
            }),
            body_len(),
            "half-present bounds",
        );
        expect_corruption(
            &tampered(|trailer| {
                trailer[first_zone_offset() + ZONE_OFF_RESERVED] = 1;
            }),
            body_len(),
            "nonzero zone reserved",
        );
    }

    #[test]
    fn crossed_bounds_are_rejected() {
        // Rewrite the minimum bound payload to a value above the maximum while
        // keeping its length (Integer bounds are fixed width), then reseal so
        // the semantic check is what fires.
        let high = Row::new(vec![Field::Integer(9_999)])
            .encode()
            .expect("encode");
        let image = tampered(move |trailer| {
            let base = first_zone_offset() + PRUNING_ZONE_HEADER_LEN;
            trailer[base..base + high.len()].copy_from_slice(&high);
        });
        expect_corruption(&image, body_len(), "min > max");
    }

    #[test]
    fn truncated_or_oversized_bound_payloads_are_rejected() {
        expect_corruption(
            &tampered(|trailer| {
                put_u32(trailer, first_zone_offset() + ZONE_OFF_MIN_LEN, u32::MAX);
            }),
            body_len(),
            "oversized min bound",
        );
        expect_corruption(
            &tampered(|trailer| {
                put_u32(trailer, first_zone_offset() + ZONE_OFF_MAX_LEN, u32::MAX);
            }),
            body_len(),
            "oversized max bound",
        );
        // A declared-but-absent payload (present bits set, lengths zero).
        expect_corruption(
            &tampered(|trailer| {
                put_u32(trailer, first_zone_offset() + ZONE_OFF_MIN_LEN, 0);
            }),
            body_len(),
            "empty bound payload",
        );
    }

    #[test]
    fn null_bound_payloads_are_rejected() {
        let null_bound = Row::new(vec![Field::Null]).encode().expect("encode");
        let image = tampered(move |trailer| {
            let base = first_zone_offset() + PRUNING_ZONE_HEADER_LEN;
            trailer[base..base + null_bound.len()].copy_from_slice(&null_bound);
        });
        expect_corruption(&image, body_len(), "NULL bound");
    }

    #[test]
    fn undecodable_bound_payloads_are_rejected() {
        let image = tampered(|trailer| {
            let base = first_zone_offset() + PRUNING_ZONE_HEADER_LEN;
            // Scramble the encoded value without changing the payload length.
            for byte in &mut trailer[base..base + 16] {
                *byte ^= 0xA5;
            }
        });
        let result = decode_trailer_at(&image, body_len());
        assert!(result.is_err(), "scrambled bound must not decode");
        assert_eq!(result.expect_err("err").kind(), ErrorKind::Corruption);
    }

    #[test]
    fn zone_escaping_its_range_is_rejected() {
        // A zone map whose rows fall outside the range that owns it is
        // corruption: the summary would not describe the rows it is used to
        // prune.
        let image = tampered(|trailer| {
            put_u64(trailer, first_zone_offset() + ZONE_OFF_ROW_COUNT, 4);
            put_u64(trailer, first_zone_offset() + ZONE_OFF_START_ROW, 3);
        });
        expect_corruption(&image, body_len(), "zone escapes range");
    }

    #[test]
    fn corrupted_zone_checksum_is_rejected() {
        // A flipped zone CRC, without any repair, must fail. (Resealing would
        // recompute the CRC, so this test mutates without resealing.)
        let mut image = image_with_trailer();
        let target = body_len() + first_zone_offset() + ZONE_OFF_CRC;
        image[target] ^= 0xFF;
        expect_corruption(&image, body_len(), "zone crc");
    }

    #[test]
    fn trailing_body_bytes_are_rejected() {
        // `range_count` smaller than the body's actual record count leaves
        // unread bytes: never "approximately right" metadata.
        let image = tampered(|trailer| {
            // Reduce the header's range count by one without shortening the
            // body, so the decoder must notice the leftovers.
            put_u32(trailer, footer_offset() + OFF_RANGE_COUNT, 1);
        });
        expect_corruption(&image, body_len(), "trailing body bytes");
    }
}
