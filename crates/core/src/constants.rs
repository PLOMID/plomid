#![forbid(unsafe_code)]
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
//! Single authoritative source for all PLOMID constants.
//!
//! Every crate imports from here: magic bytes, format versions, directory/file
//! names, size constants, default capacities, tag constants, and on-disk field
//! offsets. Changing a constant here changes it everywhere.

// ============================================================================
// Magic bytes
// ============================================================================

pub const DEVICE_MAGIC: [u8; 4] = *b"PLDV";
pub const ALLOCATOR_MAGIC: [u8; 4] = *b"PLEX";
pub const PAGE_MAGIC: [u8; 4] = *b"PLPG";
pub const BLOCK_MAGIC: [u8; 4] = *b"PLBT";
pub const PACK_MAGIC: [u8; 4] = *b"PLPD";
pub const PACK_FOOTER_MAGIC: [u8; 4] = *b"PLPF";
pub const PAGE_MANAGER_MAGIC: [u8; 4] = *b"PLPM";
pub const BLOCK_MANAGER_MAGIC: [u8; 4] = *b"PLBM";
pub const PACK_MANAGER_MAGIC: [u8; 4] = *b"PLPK";
pub const CATALOG_MAGIC: [u8; 4] = *b"PLCT";
pub const CHECKPOINT_MAGIC: [u8; 4] = *b"PLCK";
pub const GENERATION_MAGIC: [u8; 4] = *b"PLGN";
pub const PUBLICATION_POINTER_MAGIC: [u8; 4] = *b"PLPT";
pub const SEGMENT_MAGIC: [u8; 4] = *b"PLWS";
pub const WAL_MAGIC_V2: [u8; 4] = *b"PLW2";
pub const ROW_MAGIC: [u8; 2] = *b"PR";
pub const BTREE_ROOT_MAGIC: [u8; 4] = *b"PLRT";
pub const BTREE_NODE_MAGIC: [u8; 4] = *b"PLBT";
pub const COLUMNAR_MAGIC: [u8; 4] = *b"PLCS";
pub const COLUMNAR_CHUNK_MAGIC: [u8; 4] = *b"PLCC";
pub const COLUMNAR_ENCODING_MAGIC: [u8; 4] = *b"PLCE";

// ============================================================================
// Format versions (on-disk)
// ============================================================================

pub const DEVICE_FORMAT_VERSION: u32 = 1;
pub const ALLOCATOR_FORMAT_VERSION: u32 = 1;
pub const PAGE_FORMAT_VERSION: u32 = 1;
pub const BLOCK_FORMAT_VERSION: u32 = 1;
pub const PACK_FORMAT_VERSION: u32 = 1;
pub const PAGE_MANAGER_VERSION: u32 = 1;
pub const BLOCK_MANAGER_VERSION: u32 = 1;
pub const PACK_MANAGER_VERSION: u32 = 1;
pub const CATALOG_FORMAT_VERSION: u32 = 2;
pub const CHECKPOINT_FORMAT_VERSION: u32 = 1;
pub const GENERATION_FORMAT_VERSION: u32 = 1;
pub const PUBLICATION_POINTER_FORMAT_VERSION: u32 = 1;
pub const SEGMENT_VERSION: u32 = 1;
pub const WAL_RECORD_VERSION: u16 = 2;
pub const ROW_VERSION: u8 = 1;
pub const ROW_VERSION_EXECUTOR: u8 = 2;
// v15: table columns persist their SERIAL/BIGSERIAL flag (recovered column
// definitions must keep allocating sequence values), and indexes persist an
// explicit ordered column list so composite UNIQUE/PK constraints are stored
// as one tuple index rather than one index per column.
pub const CATALOG_VERSION_SQL: u8 = 15;
pub const COLUMNAR_FORMAT_VERSION: u32 = 1;
pub const COLUMNAR_CHUNK_FORMAT_VERSION: u32 = 1;
pub const COLUMNAR_ENCODING_FRAME_VERSION: u8 = 1;

// ============================================================================
// Physical layout sizes
// ============================================================================

pub const PAGE_SIZE: u64 = 16 * 1024;
pub const PAGE_SIZE_BYTES: usize = 16 * 1024;
pub const BLOCK_SIZE: u64 = 256 * 1024;
pub const BLOCK_SIZE_USIZE: usize = 256 * 1024;
pub const EXTENT_SIZE: u64 = 64 * 1024 * 1024;
pub const PACK_TARGET_SIZE: u64 = 16 * 1024 * 1024 * 1024;
pub const COLUMNAR_CHUNK_TARGET_SIZE: u64 = 16 * 1024 * 1024;
pub const PAGES_PER_BLOCK: u64 = BLOCK_SIZE / PAGE_SIZE;
pub const PAGES_PER_BLOCK_USIZE: usize = 16;
pub const BLOCKS_PER_EXTENT: u64 = EXTENT_SIZE / BLOCK_SIZE;
pub const PAGES_PER_EXTENT: u64 = EXTENT_SIZE / PAGE_SIZE;

// ============================================================================
// Encoded structure sizes and header lengths
// ============================================================================

pub const PAGE_HEADER_SIZE: usize = 4 + 4 + 4 + 8 + 8 + 8 + 4 + 4 + 4;
pub const PAGE_HEADER_CHECKSUM_OFFSET: usize = 44;
pub const PAGE_TRAILER_SIZE: usize = 4;
pub const PAGE_DATA_SIZE: usize = PAGE_SIZE_BYTES - PAGE_HEADER_SIZE - PAGE_TRAILER_SIZE;
pub const BLOCK_METADATA_SIZE: usize = 8 + 8 + 8 + (8 * PAGES_PER_BLOCK_USIZE);
pub const PACK_HEADER_SIZE: usize = 4 + 4 + 8 + 8 + 8 + 8 + 8 + 4 + 4;
pub const PACK_FOOTER_SIZE: usize = 4 + 4 + 8 + 8 + 8 + 8 + 8 + 4;
pub const BLOCK_DIRECTORY_ENTRY_SIZE: usize = 8 + 8 + 8 + 8;

pub const DEVICE_HEADER_LEN: u64 = BLOCK_SIZE;
pub const SNAPSHOT_PAGE_LEN: u64 = BLOCK_SIZE;
pub const SNAPSHOT_PREFIX_LEN: u64 = 40;
pub const SNAPSHOT_ENTRY_LEN: u64 = 24;
pub const BLOCK_MANAGER_HEADER_LEN: u64 = BLOCK_SIZE;
pub const PAGE_MANAGER_HEADER_LEN: u64 = PAGE_SIZE;
pub const PACK_HEADER_LEN: u64 = PAGE_SIZE;
pub const PACK_MANAGER_BLOCK_LEN: usize = BLOCK_SIZE_USIZE;
pub const PACK_FOOTER_LEN: u64 = PAGE_SIZE;
pub const PACK_TARGET_BLOCKS: u64 = 1 << 16;
pub const PACK_DIRECTORY_ENTRY_LEN: u64 = 16;

pub const SEGMENT_HEADER_SIZE: usize = 32;
pub const WAL_HEADER_SIZE: usize = 36;
pub const MAX_PAYLOAD_SIZE: usize = 64 * 1024 * 1024;
pub const CHECKSUM_SIZE: usize = 4;
pub const LOCATION_SIZE: usize = 16;

pub const CHECKPOINT_HEADER_SIZE: usize = 64;
pub const MAX_METADATA_SIZE: usize = 4 * 1024 * 1024;
pub const CATALOG_HEADER_SIZE: usize = 72;
pub const CATALOG_CHECKSUMMED_PREFIX_LEN: usize = 56;
pub const MIN_CATALOG_RECORD_LEN: usize = 52;
pub const PHYSICAL_REFERENCE_LEN: usize = 57;
pub const SCHEMA_COLUMN_LEN: usize = 12;
pub const GENERATION_HEADER_SIZE: usize = 96;
pub const GENERATION_CHECKSUMMED_PREFIX_LEN: usize = 80;
pub const PUBLICATION_POINTER_SIZE: usize = 72;
pub const POINTER_CHECKSUMMED_PREFIX_LEN: usize = 48;

pub const COLUMNAR_HEADER_SIZE: usize = 64;
pub const COLUMNAR_CHECKSUMMED_PREFIX_LEN: usize = 56;
pub const COLUMNAR_COLUMN_METADATA_SIZE: usize = 80;
pub const COLUMNAR_CHUNK_HEADER_SIZE: usize = 56;
/// Width of the little-endian length prefix in front of a columnar value.
pub const COLUMNAR_VALUE_LENGTH_SIZE: usize = 4;
/// Encoded size of the self-describing chunk encoding frame.
pub const COLUMNAR_ENCODING_FRAME_SIZE: usize = 40;

pub const BTREE_NO_PAGE: u64 = u64::MAX;
pub const BTREE_LEAF: u8 = 1;
pub const BTREE_INTERNAL: u8 = 2;

pub const BLOCK_MANAGER_HDR_COUNT: usize = 40;
pub const BLOCK_MANAGER_HDR_CHECKSUM: usize = 44;
pub const PACK_MANAGER_HDR_COUNT: usize = 56;
pub const PACK_MANAGER_HDR_CHECKSUM: usize = 60;
pub const PACK_MANAGER_FTR_COUNT: usize = 40;
pub const PACK_MANAGER_FTR_CHECKSUM: usize = 44;
pub const PACK_MANAGER_DIRECTORY_FREE: u64 = u64::MAX;
pub const PAGE_MANAGER_HDR_VERSION_END: usize = 8;
pub const PAGE_MANAGER_HDR_PAGE_SIZE_END: usize = 16;
pub const PAGE_MANAGER_HDR_NEXT_ID_END: usize = 24;
pub const PAGE_MANAGER_HDR_SLOT_COUNT_END: usize = 32;
pub const PAGE_MANAGER_HDR_GENERATION_END: usize = 40;
pub const PAGE_MANAGER_HDR_CHECKSUM_END: usize = 44;
pub const DEVICE_HDR_CHECKSUMMED_END: usize = 52;
pub const DEVICE_HDR_CHECKSUM_END: usize = 56;

// ============================================================================
// Capacity limits
// ============================================================================

pub const MAX_DEVICE_CAPACITY: u64 = 1u64 << 48;
pub const MIN_DEVICE_CAPACITY: u64 = BLOCK_SIZE + BLOCK_SIZE + 64 * 1024 * 1024;
pub const MAX_CATALOG_RECORDS: usize = 1 << 20;
pub const MAX_SCHEMA_COLUMNS: usize = 1 << 12;
pub const MAX_RETAINED_GENERATIONS: usize = 1 << 20;
pub const MAX_PHYSICAL_REFERENCES: usize = 1 << 20;
pub const MAX_GENERATION_REFERENCES: usize = 1 << 20;
pub const MAX_PUBLICATION_OBJECTS: usize = 1 << 16;
/// Hard bound on the bytes a chunk hands to its codec, and on what a codec may
/// produce. Bounds every decode allocation so a malformed chunk header or codec
/// frame can never ask for unbounded memory.
pub const COLUMNAR_MAX_CHUNK_UNCOMPRESSED_SIZE: u64 = 1 << 30;
/// Hard bound on the decoded value-stream length of one chunk.
pub const COLUMNAR_MAX_CHUNK_RAW_SIZE: u64 = 1 << 30;
/// Maximum distinct values a dictionary-encoded chunk may hold.
pub const COLUMNAR_MAX_DICTIONARY_ENTRIES: usize = 1 << 16;
pub const MAX_FRONTEND_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

// ============================================================================
// Directory and file names
// ============================================================================

pub const CATALOG_DIR_NAME: &str = "catalog";
pub const CHECKPOINT_DIR_NAME: &str = "checkpoints";
pub const WAL_DIR_NAME: &str = "wal";
pub const CHECKPOINT_MARKER_NAME: &str = "checkpoint";
pub const PUBLICATION_POINTER_FILE_NAME: &str = "CURRENT";
pub const CATALOG_TMP_SUFFIX: &str = ".tmp";
pub const CHECKPOINT_TMP_SUFFIX: &str = ".tmp";
pub const CATALOG_FILE_PREFIX: &str = "catalog-";
pub const CATALOG_FILE_SUFFIX: &str = ".cat";
pub const CHECKPOINT_FILE_PREFIX: &str = "checkpoint-";
pub const CHECKPOINT_FILE_SUFFIX: &str = ".ckpt";
pub const WAL_NEW_PREFIX: &str = "WAL-";
pub const WAL_NEW_SUFFIX: &str = ".dat";
pub const CATALOG_GENERATION_DIGITS: usize = 20;
pub const CHECKPOINT_GENERATION_DIGITS: usize = 20;

// ============================================================================
// On-disk field offsets — catalog header
// ============================================================================

pub const CATALOG_OFF_VERSION: usize = 4;
pub const CATALOG_OFF_HEADER_LEN: usize = 8;
pub const CATALOG_OFF_CATALOG_VERSION: usize = 12;
pub const CATALOG_OFF_CATALOG_GENERATION: usize = 20;
pub const CATALOG_OFF_STORAGE_GENERATION: usize = 28;
pub const CATALOG_OFF_CHECKPOINT_LSN: usize = 36;
pub const CATALOG_OFF_PUBLICATION: usize = 44;
pub const CATALOG_OFF_RECORD_COUNT: usize = 48;
pub const CATALOG_OFF_RECORDS_LEN: usize = 52;
pub const CATALOG_OFF_CHECKSUM: usize = 56;
pub const CATALOG_OFF_RESERVED: usize = 60;

// ============================================================================
// On-disk field offsets — checkpoint header
// ============================================================================

pub const CHECKPOINT_OFF_VERSION: usize = 4;
pub const CHECKPOINT_OFF_HEADER_LEN: usize = 8;
pub const CHECKPOINT_OFF_CKPT_GEN: usize = 12;
pub const CHECKPOINT_OFF_CKPT_LSN: usize = 20;
pub const CHECKPOINT_OFF_STORAGE_GEN: usize = 28;
pub const CHECKPOINT_OFF_CATALOG_GEN: usize = 36;
pub const CHECKPOINT_OFF_META_LEN: usize = 44;
pub const CHECKPOINT_OFF_CHECKSUM: usize = 48;
pub const CHECKPOINT_OFF_RESERVED: usize = 52;
pub const CHECKPOINT_CHECKSUMMED_PREFIX_LEN: usize = 48;

// ============================================================================
// On-disk field offsets — generation header
// ============================================================================

pub const GENERATION_OFF_VERSION: usize = 4;
pub const GENERATION_OFF_HEADER_LEN: usize = 8;
pub const GENERATION_OFF_GENERATION_ID: usize = 12;
pub const GENERATION_OFF_OBJECT_ID: usize = 20;
pub const GENERATION_OFF_CATALOG_VERSION: usize = 28;
pub const GENERATION_OFF_PUBLICATION_GENERATION: usize = 36;
pub const GENERATION_OFF_STORAGE_GENERATION: usize = 44;
pub const GENERATION_OFF_CHECKPOINT_LSN: usize = 52;
pub const GENERATION_OFF_PREVIOUS_GENERATION: usize = 60;
pub const GENERATION_OFF_REFERENCE_COUNT: usize = 68;
pub const GENERATION_OFF_REFERENCES_LEN: usize = 72;
pub const GENERATION_OFF_STATE: usize = 76;
pub const GENERATION_OFF_CHECKSUM: usize = 80;
pub const GENERATION_OFF_RESERVED: usize = 84;

// ============================================================================
// On-disk field offsets — publication pointer
// ============================================================================

pub const POINTER_OFF_VERSION: usize = 4;
pub const POINTER_OFF_HEADER_LEN: usize = 8;
pub const POINTER_OFF_CATALOG_GENERATION: usize = 12;
pub const POINTER_OFF_CATALOG_VERSION: usize = 20;
pub const POINTER_OFF_STORAGE_GENERATION: usize = 28;
pub const POINTER_OFF_CHECKPOINT_LSN: usize = 36;
pub const POINTER_OFF_STATE: usize = 44;
pub const POINTER_OFF_CHECKSUM: usize = 48;
pub const POINTER_OFF_RESERVED: usize = 52;

// ============================================================================
// On-disk field offsets — columnar segment header
// ============================================================================

pub const COLUMNAR_OFF_MAGIC: usize = 0;
pub const COLUMNAR_OFF_VERSION: usize = 4;
pub const COLUMNAR_OFF_ROW_COUNT: usize = 8;
pub const COLUMNAR_OFF_COLUMN_COUNT: usize = 16;
pub const COLUMNAR_OFF_CHUNK_COUNT: usize = 20;
pub const COLUMNAR_OFF_NULL_BITMAP_BYTES: usize = 24;
pub const COLUMNAR_OFF_GENERATION_ID: usize = 32;
pub const COLUMNAR_OFF_SEGMENT_ID: usize = 40;
pub const COLUMNAR_OFF_CHUNKS_OFFSET: usize = 48;
pub const COLUMNAR_OFF_CHECKSUM: usize = 56;
/// CRC32C over the metadata, null-bitmap, and statistics regions.
pub const COLUMNAR_OFF_BODY_CHECKSUM: usize = 60;

// ============================================================================
// On-disk field offsets — columnar column metadata
// ============================================================================

pub const COLUMN_META_OFF_COLUMN_ID: usize = 0;
pub const COLUMN_META_OFF_COLUMN_TYPE: usize = 8;
pub const COLUMN_META_OFF_COMPRESSION: usize = 9;
pub const COLUMN_META_OFF_STATS_FLAGS: usize = 10;
pub const COLUMN_META_OFF_FIRST_CHUNK: usize = 12;
pub const COLUMN_META_OFF_ROW_COUNT: usize = 16;
pub const COLUMN_META_OFF_NULL_COUNT: usize = 24;
pub const COLUMN_META_OFF_CHUNK_COUNT: usize = 32;
pub const COLUMN_META_OFF_NULL_BITMAP: usize = 36;
pub const COLUMN_META_OFF_VALUES: usize = 44;
pub const COLUMN_META_OFF_VALUES_SIZE: usize = 52;
pub const COLUMN_META_OFF_STATISTICS: usize = 60;
pub const COLUMN_META_OFF_STATISTICS_SIZE: usize = 68;
pub const COLUMN_META_OFF_STATISTICS_CHECKSUM: usize = 72;
pub const COLUMN_META_OFF_RECORD_CHECKSUM: usize = 76;

// ============================================================================
// On-disk field offsets — columnar chunk header
// ============================================================================

pub const CHUNK_OFF_MAGIC: usize = 0;
pub const CHUNK_OFF_VERSION: usize = 4;
pub const CHUNK_OFF_COMPRESSION: usize = 8;
pub const CHUNK_OFF_FIRST_ROW: usize = 10;
pub const CHUNK_OFF_ROW_COUNT: usize = 18;
pub const CHUNK_OFF_PAYLOAD: usize = 26;
pub const CHUNK_OFF_COMPRESSED_SIZE: usize = 34;
pub const CHUNK_OFF_UNCOMPRESSED_SIZE: usize = 42;
/// CRC32C over the chunk header prefix `[0, 50)` followed by the stored payload.
pub const CHUNK_OFF_CHECKSUM: usize = 50;
pub const CHUNK_OFF_RESERVED: usize = 54;
/// Value encoding of a chunk, one of the `ENCODING_*_TAG` values.
pub const CHUNK_OFF_ENCODING: usize = 9;

// ============================================================================
// On-disk field offsets — columnar chunk encoding frame
// ============================================================================

pub const ENCODING_FRAME_OFF_MAGIC: usize = 0;
pub const ENCODING_FRAME_OFF_FRAME_VERSION: usize = 4;
pub const ENCODING_FRAME_OFF_ENCODING: usize = 5;
pub const ENCODING_FRAME_OFF_ENCODING_VERSION: usize = 6;
pub const ENCODING_FRAME_OFF_CODEC: usize = 7;
pub const ENCODING_FRAME_OFF_CODEC_VERSION: usize = 8;
pub const ENCODING_FRAME_OFF_RESERVED0: usize = 9;
pub const ENCODING_FRAME_OFF_RESERVED1: usize = 10;
pub const ENCODING_FRAME_OFF_VALUE_COUNT: usize = 12;
pub const ENCODING_FRAME_OFF_RAW_LEN: usize = 20;
pub const ENCODING_FRAME_OFF_ENCODED_LEN: usize = 28;
pub const ENCODING_FRAME_OFF_RESERVED2: usize = 36;

// ============================================================================
// On-disk field offsets — row / WAL record
// ============================================================================

pub const ROW_OFF_VERSION: usize = 2;
pub const ROW_HEADER_SIZE: usize = 12;
pub const ROW_OFF_FLAGS: usize = 3;
pub const ROW_OFF_FIELD_COUNT: usize = 4;
pub const ROW_OFF_NULL_BITMAP_BYTES: usize = 6;
pub const ROW_OFF_TOTAL_LENGTH: usize = 8;
pub const WAL_V2_OFF_MAGIC: usize = 0;
pub const WAL_V2_OFF_VERSION: usize = 4;
pub const WAL_V2_OFF_LSN: usize = 8;
pub const WAL_V2_OFF_RECORD_TYPE: usize = 16;
pub const WAL_V2_OFF_PAYLOAD_LEN: usize = 18;
pub const WAL_V2_OFF_CHECKSUM: usize = 20;
pub const WAL_V2_OFF_PAYLOAD: usize = 24;

// ============================================================================
// Tag constants
// ============================================================================

pub const ROW_INTEGER_TAG: u8 = 1;
pub const ROW_BYTES_TAG: u8 = 2;
pub const ROW_STRING_TAG: u8 = 3;

// ============================================================================
// Tag constants — columnar columns, chunks, and statistics
// ============================================================================

pub const COLUMN_NULL_TAG: u8 = 0;
pub const COLUMN_INTEGER_TAG: u8 = 1;
pub const COLUMN_BYTES_TAG: u8 = 2;
pub const COLUMN_STRING_TAG: u8 = 3;
pub const COMPRESSION_NONE_TAG: u8 = 0;
pub const COMPRESSION_ZSTD_TAG: u8 = 1;
/// Codec versions. A tag identifies the codec; the version identifies the
/// byte-level frame layout that codec produced.
pub const CODEC_VERSION_NONE: u8 = 1;
pub const CODEC_VERSION_ZSTD: u8 = 1;
/// Value encodings applied before the codec. `RAW` means the chunk payload is
/// the value stream itself, exactly as version-1 segments have always stored it.
pub const ENCODING_RAW_TAG: u8 = 0;
pub const ENCODING_RLE_TAG: u8 = 1;
pub const ENCODING_DELTA_BITPACK_TAG: u8 = 2;
pub const ENCODING_DICTIONARY_TAG: u8 = 3;
/// Encoding versions, one per encoding tag.
pub const ENCODING_VERSION_RAW: u8 = 1;
pub const ENCODING_VERSION_RLE: u8 = 1;
pub const ENCODING_VERSION_DELTA_BITPACK: u8 = 1;
pub const ENCODING_VERSION_DICTIONARY: u8 = 1;
pub const STATS_MIN_AVAILABLE: u8 = 0x01;
pub const STATS_MAX_AVAILABLE: u8 = 0x02;
pub const STATS_NULL_COUNT_AVAILABLE: u8 = 0x04;
pub const STATS_ROW_COUNT_AVAILABLE: u8 = 0x08;
pub const STATS_ALL_AVAILABLE: u8 = STATS_MIN_AVAILABLE
    | STATS_MAX_AVAILABLE
    | STATS_NULL_COUNT_AVAILABLE
    | STATS_ROW_COUNT_AVAILABLE;
pub const WAL_PUT_OP: u8 = 1;
pub const WAL_DELETE_OP: u8 = 2;

// ============================================================================
// Tag constants — SQL catalog expression encoding
// ============================================================================

pub const EXPR_COLUMN_REF: u8 = 0;
pub const EXPR_LITERAL: u8 = 1;
pub const EXPR_STAR: u8 = 2;
pub const EXPR_EQUAL: u8 = 3;
pub const EXPR_NOT_EQUAL: u8 = 4;
pub const EXPR_LESS: u8 = 5;
pub const EXPR_LESS_OR_EQUAL: u8 = 6;
pub const EXPR_GREATER: u8 = 7;
pub const EXPR_GREATER_OR_EQUAL: u8 = 8;
pub const EXPR_IS_NULL: u8 = 9;
pub const EXPR_IS_NOT_NULL: u8 = 10;
pub const EXPR_AND: u8 = 11;
pub const EXPR_OR: u8 = 12;
pub const EXPR_NOT: u8 = 13;
pub const EXPR_ADD: u8 = 14;
pub const EXPR_SUBTRACT: u8 = 15;
pub const EXPR_MULTIPLY: u8 = 16;
pub const EXPR_DIVIDE: u8 = 17;
pub const EXPR_MODULO: u8 = 18;
pub const EXPR_NEGATE: u8 = 19;
pub const EXPR_FUNCTION_CALL: u8 = 20;
pub const EXPR_IN: u8 = 21;
pub const EXPR_BETWEEN: u8 = 22;
pub const EXPR_LIKE: u8 = 23;
pub const EXPR_CASE: u8 = 24;
pub const EXPR_COALESCE: u8 = 25;
pub const EXPR_NULLIF: u8 = 26;
pub const EXPR_EXISTS: u8 = 27;
pub const EXPR_IS_DISTINCT_FROM: u8 = 28;
pub const EXPR_SCALAR_SUBQUERY: u8 = 29;
pub const EXPR_WINDOW_FUNCTION: u8 = 30;
pub const EXPR_CONCAT: u8 = 31;
pub const EXPR_BIT_AND: u8 = 32;
pub const EXPR_BIT_OR: u8 = 33;
pub const EXPR_BIT_XOR: u8 = 34;
pub const EXPR_SHIFT_LEFT: u8 = 35;
pub const EXPR_SHIFT_RIGHT: u8 = 36;
pub const EXPR_TYPE_CAST: u8 = 37;
pub const EXPR_CAST: u8 = 38;
pub const EXPR_JSON_ARROW: u8 = 39;
pub const EXPR_ARRAY_INDEX: u8 = 40;

// ============================================================================
// Filter format constants
// ============================================================================

/// Magic bytes for persistent Roaring-style bitmaps ("PLRB").
pub const ROARING_MAGIC: [u8; 4] = *b"PLRB";
/// Format version for persistent Roaring bitmaps.
pub const ROARING_FORMAT_VERSION: u32 = 1;
/// Container type tag: sorted array of u16 values (sparse).
pub const ROARING_CONTAINER_ARRAY: u8 = 0;
/// Container type tag: full 65536-bit bitmap (dense).
pub const ROARING_CONTAINER_BITMAP: u8 = 1;
/// Container type tag: run-length encoded (start, length) pairs.
pub const ROARING_CONTAINER_RUN: u8 = 2;
/// Maximum number of containers (full 16-bit key space).
pub const ROARING_MAX_CONTAINERS: usize = 1 << 16;
/// Maximum array-container entries (standard Roaring threshold).
pub const ROARING_ARRAY_LAZY_MAX: usize = 4096;
/// Bitmap container size in bytes (65536 bits / 8).
pub const ROARING_BITMAP_BYTES: usize = 8192;
/// Number of 64-bit words in a bitmap container (65536 bits / 64).
pub const ROARING_BITMAP_WORDS: usize = 1024;
/// Fixed footer size appended after a serialized Roaring bitmap body.
pub const ROARING_FOOTER_LEN: usize = 40;

/// Magic bytes for persistent XOR filters ("PLXF").
pub const XOR_FILTER_MAGIC: [u8; 4] = *b"PLXF";
/// Format version for persistent XOR filters.
pub const XOR_FILTER_FORMAT_VERSION: u32 = 1;
/// Number of fingerprint bits per slot (XOR filter parameter).
pub const XOR_FINGERPRINT_BITS: u32 = 8;
/// Modulus for 8-bit fingerprints (256).
pub const XOR_FINGERPRINT_MODULUS: u64 = 1u64 << XOR_FINGERPRINT_BITS;
/// Number of hash functions in the XOR filter (always 3).
pub const XOR_HASH_COUNT: usize = 3;
/// Slot-count numerator: m = ceil(n * XOR_ALPHA_NUM / XOR_ALPHA_DEN) + XOR_SLOTS_SLACK.
///
/// This is the *inverse* load factor, and it must stay above the peelability
/// threshold of a random 3-uniform hypergraph. Peeling succeeds only while
/// `m/n > 11/9 ≈ 1.2222`; below that threshold the 2-core never empties and
/// construction fails for every seed. `27/20 = 1.35` sits comfortably above the
/// threshold, which makes each attempt succeed with overwhelming probability
/// (measured: 127/128 seeds at n = 1000, 128/128 at n = 10 000) instead of
/// merely likely.
pub const XOR_ALPHA_NUM: u64 = 27;
/// Slot-count denominator; see [`XOR_ALPHA_NUM`].
pub const XOR_ALPHA_DEN: u64 = 20;
/// Additive slot slack added after the proportional term.
///
/// The proportional term alone is unreliable for small key sets, where rounding
/// to a multiple of three is too coarse to control the load: at n = 64 the
/// proportional term yields a load of 0.74 and only ~61% of seeds peel. A fixed
/// 32-slot cushion fixes the small-`n` regime while costing nothing relative to
/// the filter size once `n` grows (it is 0.2% of a 10 000-element filter).
pub const XOR_SLOTS_SLACK: usize = 32;
/// Maximum XOR filter fingerprint slots (256 M slots ≈ 256 MiB).
pub const XOR_MAX_SLOTS: usize = 1 << 28;
/// Maximum elements the XOR filter can hold.
pub const XOR_MAX_ELEMENTS: usize =
    (XOR_MAX_SLOTS * XOR_ALPHA_DEN as usize) / XOR_ALPHA_NUM as usize;
/// Fixed footer size appended after a serialized XOR filter body.
pub const XOR_FOOTER_LEN: usize = 40;
/// Maximum construction retries with different seeds before giving up.
pub const XOR_MAX_BUILD_ATTEMPTS: u32 = 5;

// ============================================================================
// Default capacities and configuration
// ============================================================================

pub const DEFAULT_POOL_CAPACITY: usize = 1024;

/// Rotation size of an active **storage** segment (the pack/segment file a
/// storage manager keeps open and grows) in bytes.
///
/// This is the value the running database uses: `LifecycleConfig::default()`
/// feeds it to `StorageManager::create`/`open`, so it is the single authority
/// for how large a physical storage segment grows before a new one is placed.
/// It is intentionally larger than one extent (64 MiB per `EXTENT_SIZE`) is
/// not required to be, and small enough that segment creation stays cheap on
/// slow media (SD cards, network volumes).
pub const DEFAULT_STORAGE_SEGMENT_SIZE_BYTES: u64 = 8 * 1024 * 1024;

/// Rotation size of a **WAL** segment file in bytes.
///
/// This is the value the running database uses: `LifecycleConfig::default()`
/// feeds it to `SegmentedWal::create`/`open`, so it is the single authority
/// for WAL rotation, for how much WAL recovery must replay at most between
/// checkpoints, and for how large a retained WAL segment can be on disk.
pub const DEFAULT_WAL_SEGMENT_SIZE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_SUBQUERY_DEPTH: usize = 24;
pub const MAX_RECURSION_ITERATIONS: usize = 1_000_000;
pub const ROWID_META_PREFIX: &str = "_plomid_meta:rowid:";

// ============================================================================
// Type system constants
// ============================================================================

pub const FIRST_USER_OID: u32 = 16_384;
pub const NO_TYPEMOD: i32 = -1;
pub const NUMERIC_MAX_PRECISION: u16 = 1000;
pub const NUMERIC_MAX_SCALE: u16 = 16383;
pub const MAX_LENGTH: u32 = 10_485_760;
pub const MAX_TIME_PRECISION: u16 = 6;
pub const POSTGRES_EPOCH_JDATE: i32 = 10_957;
pub const USECS_PER_SEC: i64 = 1_000_000;
pub const USECS_PER_DAY: i64 = 86_400 * USECS_PER_SEC;
pub const MONTH_DAYS: [i32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

// ============================================================================
// Transaction constants
// ============================================================================

pub const INITIAL_TXN_ID: u64 = 1;
pub const INITIAL_COMMIT_TIMESTAMP: u64 = 1;
pub const MAX_TXN_ID: u64 = u64::MAX - 1;
pub const MAX_COMMIT_TIMESTAMP: u64 = u64::MAX - 1;

// ============================================================================
// CLI / server defaults
// ============================================================================

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 5432;
pub const DEFAULT_USER: &str = "plomid";
pub const DEFAULT_DATABASE: &str = "plomid";
pub const DEFAULT_DATA_DIR: &str = "./data";

// ============================================================================
// SCRAM constants
// ============================================================================

pub const SCRAM_HMAC_BLOCK: usize = 64;
pub const SCRAM_HASH_LEN: usize = 32;
pub const SCRAM_BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
