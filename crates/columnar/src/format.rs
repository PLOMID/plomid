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
//! Columnar segment binary format.
//!
//! A segment is a self-describing little-endian structure. Every region is
//! located through an absolute offset recorded either in the segment header or
//! in the per-column metadata, so a reader never depends on region ordering and
//! a pruning reader never has to touch a payload it does not need.
//!
//! ```text
//! +-------------------------------------------+
//! | segment header            64 bytes        |  COLUMNAR_HEADER_SIZE
//! +-------------------------------------------+
//! | column metadata           80 * N bytes    |  COLUMNAR_COLUMN_METADATA_SIZE
//! +-------------------------------------------+
//! | null bitmaps              ceil(rows / 8)  |
//! +-------------------------------------------+
//! | column statistics         variable        |
//! +-------------------------------------------+
//! | chunk headers + payloads  variable        |  COLUMNAR_CHUNK_HEADER_SIZE
//! +-------------------------------------------+
//! ```
//!
//! # Integrity
//!
//! Three levels of CRC32C protect a segment:
//!
//! * the **header checksum** covers the header prefix, so identity and layout
//!   can be trusted before any other byte is read;
//! * the **body checksum** covers the metadata, null-bitmap, statistics, and
//!   chunk regions, so a scan that reads only metadata still detects damage to
//!   the bytes it skipped over;
//! * **per-record checksums** cover each column metadata record, each encoded
//!   statistics payload, and each chunk header plus its stored payload, so
//!   corruption is attributable to a specific column and chunk.
//!
//! # Value encoding
//!
//! A column's values are stored as a self-delimiting sequence of entries, one
//! per logical row in ordinal order:
//!
//! ```text
//! entry: length[u32 little-endian] value[length bytes]
//! ```
//!
//! A NULL row contributes a zero length and no value bytes. NULL state is
//! authoritative in the column's null bitmap, so a zero-length entry for a row
//! the bitmap marks non-NULL is an empty (`""` / `[]`) value. This keeps every
//! column decodable without persisting a separate per-row offsets array, and it
//! lets a chunk boundary fall on any row without stranding a value.

pub use plomid_core::{
    CHUNK_OFF_CHECKSUM, CHUNK_OFF_COMPRESSED_SIZE, CHUNK_OFF_COMPRESSION, CHUNK_OFF_ENCODING,
    CHUNK_OFF_FIRST_ROW, CHUNK_OFF_MAGIC, CHUNK_OFF_PAYLOAD, CHUNK_OFF_RESERVED,
    CHUNK_OFF_ROW_COUNT, CHUNK_OFF_UNCOMPRESSED_SIZE, CHUNK_OFF_VERSION,
    COLUMNAR_CHUNK_FORMAT_VERSION, COLUMNAR_CHUNK_HEADER_SIZE, COLUMNAR_CHUNK_MAGIC,
    COLUMNAR_OFF_BODY_CHECKSUM, COLUMNAR_OFF_CHECKSUM, COLUMNAR_OFF_CHUNKS_OFFSET,
    COLUMNAR_OFF_CHUNK_COUNT, COLUMNAR_OFF_COLUMN_COUNT, COLUMNAR_OFF_GENERATION_ID,
    COLUMNAR_OFF_MAGIC, COLUMNAR_OFF_NULL_BITMAP_BYTES, COLUMNAR_OFF_ROW_COUNT,
    COLUMNAR_OFF_SEGMENT_ID, COLUMNAR_OFF_VERSION, COLUMN_META_OFF_CHUNK_COUNT,
    COLUMN_META_OFF_COLUMN_ID, COLUMN_META_OFF_COLUMN_TYPE, COLUMN_META_OFF_COMPRESSION,
    COLUMN_META_OFF_FIRST_CHUNK, COLUMN_META_OFF_NULL_BITMAP, COLUMN_META_OFF_NULL_COUNT,
    COLUMN_META_OFF_RECORD_CHECKSUM, COLUMN_META_OFF_ROW_COUNT, COLUMN_META_OFF_STATISTICS,
    COLUMN_META_OFF_STATISTICS_CHECKSUM, COLUMN_META_OFF_STATISTICS_SIZE,
    COLUMN_META_OFF_STATS_FLAGS, COLUMN_META_OFF_VALUES, COLUMN_META_OFF_VALUES_SIZE,
};

/// Column type tags under their columnar names.
pub use plomid_core::{
    COLUMN_BYTES_TAG as COLUMN_TYPE_BYTES, COLUMN_INTEGER_TAG as COLUMN_TYPE_INTEGER,
    COLUMN_NULL_TAG as COLUMN_TYPE_NULL, COLUMN_STRING_TAG as COLUMN_TYPE_STRING,
};

/// Compression tags under their columnar names.
pub use plomid_core::{
    COMPRESSION_NONE_TAG as COMPRESSION_NONE, COMPRESSION_ZSTD_TAG as COMPRESSION_ZSTD,
};

/// Width of the little-endian length prefix in front of each value entry.
pub use plomid_core::COLUMNAR_VALUE_LENGTH_SIZE as COLUMNAR_VALUE_LENGTH_PREFIX;

pub use plomid_core::{
    COLUMNAR_CHECKSUMMED_PREFIX_LEN, COLUMNAR_COLUMN_METADATA_SIZE, COLUMNAR_ENCODING_FRAME_SIZE,
    COLUMNAR_ENCODING_FRAME_VERSION, COLUMNAR_ENCODING_MAGIC, COLUMNAR_FORMAT_VERSION,
    COLUMNAR_HEADER_SIZE, COLUMNAR_MAGIC, COMPRESSION_NONE_TAG, COMPRESSION_ZSTD_TAG,
    ENCODING_DELTA_BITPACK_TAG, ENCODING_DICTIONARY_TAG, ENCODING_RAW_TAG, ENCODING_RLE_TAG,
    STATS_ALL_AVAILABLE, STATS_MAX_AVAILABLE, STATS_MIN_AVAILABLE, STATS_NULL_COUNT_AVAILABLE,
    STATS_ROW_COUNT_AVAILABLE,
};

/// Default target on-disk size of a published segment.
pub const TARGET_SEGMENT_SIZE: u64 = 512 * 1024 * 1024;

/// Default target uncompressed size of a single chunk.
pub const CHUNK_TARGET_SIZE: u64 = 16 * 1024 * 1024;

/// Returns the persisted null-bitmap width for `row_count` rows.
#[must_use]
pub fn null_bitmap_size(row_count: usize) -> usize {
    row_count.div_ceil(8)
}

/// Returns true when `bitmap` marks `row_idx` as NULL.
///
/// An index outside the bitmap is reported as non-NULL: the bitmap is sized
/// from the column's row count, so a row beyond it is a caller error that must
/// not be answered with a fabricated NULL.
#[must_use]
pub fn null_bitmap_is_null(bitmap: &[u8], row_idx: usize) -> bool {
    let byte_idx = row_idx / 8;
    let bit_idx = row_idx % 8;
    bitmap
        .get(byte_idx)
        .is_some_and(|byte| (byte & (1 << bit_idx)) != 0)
}

#[cfg(test)]
mod tests {
    use super::{null_bitmap_is_null, null_bitmap_size};

    #[test]
    fn null_bitmap_size_rounds_up_to_whole_bytes() {
        assert_eq!(null_bitmap_size(0), 0);
        assert_eq!(null_bitmap_size(1), 1);
        assert_eq!(null_bitmap_size(8), 1);
        assert_eq!(null_bitmap_size(9), 2);
    }

    #[test]
    fn null_bits_are_little_endian_within_each_byte() {
        let bitmap = [0b0000_0101, 0b1000_0000];
        assert!(null_bitmap_is_null(&bitmap, 0));
        assert!(!null_bitmap_is_null(&bitmap, 1));
        assert!(null_bitmap_is_null(&bitmap, 2));
        assert!(null_bitmap_is_null(&bitmap, 15));
    }

    #[test]
    fn indices_outside_the_bitmap_are_not_null() {
        let bitmap = [0xFF_u8];
        assert!(!null_bitmap_is_null(&bitmap, 8));
        assert!(!null_bitmap_is_null(&bitmap, usize::MAX));
        assert!(!null_bitmap_is_null(&[], 0));
    }
}
