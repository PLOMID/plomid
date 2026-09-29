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
//! Column metadata for columnar segments.

use crate::format;
use plomid_core::{ColumnId, Result};
use plomid_storage::Field;
use std::fmt;

/// Logical type of a column in the columnar representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ColumnType {
    /// Column holds only NULLs, or its type is not yet known.
    Null,
    /// A signed 64-bit integer.
    Integer,
    /// Arbitrary binary data.
    Bytes,
    /// UTF-8 variable-length text.
    String,
    /// Reserved for a future boolean encoding.
    Bool,
    /// Reserved for a future 16-bit integer encoding.
    I16,
    /// Reserved for a future 32-bit integer encoding.
    I32,
    /// Reserved for a future single-precision encoding.
    F32,
    /// Reserved for a future double-precision encoding.
    F64,
}

impl ColumnType {
    /// Maps a [`Field`] variant to its column type.
    #[must_use]
    pub fn from_field(field: &Field) -> Self {
        match field {
            Field::Null => ColumnType::Null,
            Field::Integer(_) => ColumnType::Integer,
            Field::Bytes(_) => ColumnType::Bytes,
            Field::String(_) => ColumnType::String,
        }
    }

    /// Returns the format tag byte for this column type.
    ///
    /// Types without a persisted encoding share the NULL tag, which the writer
    /// never emits for a column that carries data.
    #[must_use]
    pub fn tag(&self) -> u8 {
        match self {
            ColumnType::Integer => format::COLUMN_TYPE_INTEGER,
            ColumnType::Bytes => format::COLUMN_TYPE_BYTES,
            ColumnType::String => format::COLUMN_TYPE_STRING,
            ColumnType::Null
            | ColumnType::Bool
            | ColumnType::I16
            | ColumnType::I32
            | ColumnType::F32
            | ColumnType::F64 => format::COLUMN_TYPE_NULL,
        }
    }

    /// Decodes a column type from its format tag byte.
    #[must_use]
    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            format::COLUMN_TYPE_NULL => Some(ColumnType::Null),
            format::COLUMN_TYPE_INTEGER => Some(ColumnType::Integer),
            format::COLUMN_TYPE_BYTES => Some(ColumnType::Bytes),
            format::COLUMN_TYPE_STRING => Some(ColumnType::String),
            _ => None,
        }
    }

    /// Returns the fixed width of a value in bytes, if the type is fixed width.
    #[must_use]
    pub fn fixed_width(&self) -> Option<usize> {
        match self {
            ColumnType::Integer => Some(8),
            _ => None,
        }
    }
}

/// Metadata for one column in a columnar segment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnMetadata {
    /// Stable identity of the column.
    pub column_id: ColumnId,
    /// Logical type of the stored values.
    pub column_type: ColumnType,
    /// Number of logical rows covered by this column.
    pub row_count: u64,
    /// Number of rows whose null bit is set.
    pub null_count: u64,
    /// Number of chunks that store this column's values.
    pub chunk_count: u32,
    /// Index of this column's first chunk within the segment chunk table.
    pub first_chunk: u32,
    /// Absolute offset of this column's null bitmap.
    pub null_bitmap_offset: u64,
    /// Absolute offset of this column's first chunk header.
    pub values_offset: u64,
    /// Total on-disk size of this column's chunks, headers included.
    pub values_size: u64,
    /// Absolute offset of this column's encoded statistics.
    pub statistics_offset: u64,
    /// Encoded statistics size in bytes.
    pub statistics_size: u32,
    /// CRC32C of the encoded statistics payload.
    pub statistics_checksum: u32,
    /// Compression encoding used by this column's chunks.
    pub compression: u8,
    /// Bitmask of the statistics that this column advertises.
    pub stats_flags: u8,
}

impl ColumnMetadata {
    /// Creates metadata for a column with no chunks and no statistics.
    #[must_use]
    pub fn new(column_id: ColumnId, column_type: ColumnType, row_count: u64) -> Self {
        Self {
            column_id,
            column_type,
            row_count,
            null_count: 0,
            chunk_count: 0,
            first_chunk: 0,
            null_bitmap_offset: 0,
            values_offset: 0,
            values_size: 0,
            statistics_offset: 0,
            statistics_size: 0,
            statistics_checksum: 0,
            compression: format::COMPRESSION_NONE_TAG,
            stats_flags: 0,
        }
    }

    /// Returns true when both a minimum and a maximum are persisted.
    #[must_use]
    pub fn has_min_max(&self) -> bool {
        (self.stats_flags & format::STATS_MIN_AVAILABLE) != 0
            && (self.stats_flags & format::STATS_MAX_AVAILABLE) != 0
    }

    /// Returns true when a null count is persisted.
    #[must_use]
    pub fn has_null_count(&self) -> bool {
        (self.stats_flags & format::STATS_NULL_COUNT_AVAILABLE) != 0
    }

    /// Returns true when a row count is persisted.
    #[must_use]
    pub fn has_row_count(&self) -> bool {
        (self.stats_flags & format::STATS_ROW_COUNT_AVAILABLE) != 0
    }

    /// Returns the compression encoding used by this column's chunks.
    #[must_use]
    pub fn chunk_compression(&self) -> Option<crate::chunk::ChunkCompression> {
        crate::chunk::ChunkCompression::from_tag(self.compression)
    }

    /// Encodes this record into its fixed-size little-endian image.
    ///
    /// The record checksum covers the record prefix, so a reader can validate
    /// a column's metadata before trusting any offset it carries.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        use crate::layout::{checksum_of, put_u32, put_u64, put_u8};
        let mut bytes = vec![0_u8; format::COLUMNAR_COLUMN_METADATA_SIZE];
        put_u64(
            &mut bytes,
            format::COLUMN_META_OFF_COLUMN_ID,
            self.column_id.get(),
        );
        put_u8(
            &mut bytes,
            format::COLUMN_META_OFF_COLUMN_TYPE,
            self.column_type.tag(),
        );
        put_u8(
            &mut bytes,
            format::COLUMN_META_OFF_COMPRESSION,
            self.compression,
        );
        put_u8(
            &mut bytes,
            format::COLUMN_META_OFF_STATS_FLAGS,
            self.stats_flags,
        );
        put_u32(
            &mut bytes,
            format::COLUMN_META_OFF_FIRST_CHUNK,
            self.first_chunk,
        );
        put_u64(
            &mut bytes,
            format::COLUMN_META_OFF_ROW_COUNT,
            self.row_count,
        );
        put_u64(
            &mut bytes,
            format::COLUMN_META_OFF_NULL_COUNT,
            self.null_count,
        );
        put_u32(
            &mut bytes,
            format::COLUMN_META_OFF_CHUNK_COUNT,
            self.chunk_count,
        );
        put_u64(
            &mut bytes,
            format::COLUMN_META_OFF_NULL_BITMAP,
            self.null_bitmap_offset,
        );
        put_u64(
            &mut bytes,
            format::COLUMN_META_OFF_VALUES,
            self.values_offset,
        );
        put_u64(
            &mut bytes,
            format::COLUMN_META_OFF_VALUES_SIZE,
            self.values_size,
        );
        put_u64(
            &mut bytes,
            format::COLUMN_META_OFF_STATISTICS,
            self.statistics_offset,
        );
        put_u32(
            &mut bytes,
            format::COLUMN_META_OFF_STATISTICS_SIZE,
            self.statistics_size,
        );
        put_u32(
            &mut bytes,
            format::COLUMN_META_OFF_STATISTICS_CHECKSUM,
            self.statistics_checksum,
        );
        let checksum = checksum_of(&bytes[..format::COLUMN_META_OFF_RECORD_CHECKSUM]);
        put_u32(
            &mut bytes,
            format::COLUMN_META_OFF_RECORD_CHECKSUM,
            checksum,
        );
        bytes
    }

    /// Decodes and validates a fixed-size column metadata record.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        use crate::layout::{checksum_of, corruption, get_u32, get_u64, get_u8};
        let record = bytes
            .get(..format::COLUMNAR_COLUMN_METADATA_SIZE)
            .ok_or_else(|| corruption("column metadata record is truncated"))?;
        let stored = get_u32(
            record,
            format::COLUMN_META_OFF_RECORD_CHECKSUM,
            "column metadata checksum",
        )?;
        if stored != checksum_of(&record[..format::COLUMN_META_OFF_RECORD_CHECKSUM]) {
            return Err(corruption("column metadata checksum mismatch"));
        }
        let type_tag = get_u8(record, format::COLUMN_META_OFF_COLUMN_TYPE, "column type")?;
        let column_type = ColumnType::from_tag(type_tag)
            .ok_or_else(|| corruption(format!("unknown column type tag {type_tag}")))?;
        let compression_tag = get_u8(
            record,
            format::COLUMN_META_OFF_COMPRESSION,
            "column compression",
        )?;
        if crate::chunk::ChunkCompression::from_tag(compression_tag).is_none() {
            return Err(corruption(format!(
                "unknown column compression tag {compression_tag}"
            )));
        }
        Ok(Self {
            column_id: ColumnId::new(get_u64(
                record,
                format::COLUMN_META_OFF_COLUMN_ID,
                "column id",
            )?),
            column_type,
            row_count: get_u64(record, format::COLUMN_META_OFF_ROW_COUNT, "row count")?,
            null_count: get_u64(record, format::COLUMN_META_OFF_NULL_COUNT, "null count")?,
            chunk_count: get_u32(record, format::COLUMN_META_OFF_CHUNK_COUNT, "chunk count")?,
            first_chunk: get_u32(record, format::COLUMN_META_OFF_FIRST_CHUNK, "first chunk")?,
            null_bitmap_offset: get_u64(
                record,
                format::COLUMN_META_OFF_NULL_BITMAP,
                "null bitmap offset",
            )?,
            values_offset: get_u64(record, format::COLUMN_META_OFF_VALUES, "values offset")?,
            values_size: get_u64(record, format::COLUMN_META_OFF_VALUES_SIZE, "values size")?,
            statistics_offset: get_u64(
                record,
                format::COLUMN_META_OFF_STATISTICS,
                "statistics offset",
            )?,
            statistics_size: get_u32(
                record,
                format::COLUMN_META_OFF_STATISTICS_SIZE,
                "statistics size",
            )?,
            statistics_checksum: get_u32(
                record,
                format::COLUMN_META_OFF_STATISTICS_CHECKSUM,
                "statistics checksum",
            )?,
            compression: compression_tag,
            stats_flags: get_u8(
                record,
                format::COLUMN_META_OFF_STATS_FLAGS,
                "statistics flags",
            )?,
        })
    }
}

impl fmt::Display for ColumnMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ColumnMetadata {{ column_id: {}, type: {:?}, rows: {}, nulls: {}, chunks: {}, compression: {} }}",
            self.column_id.get(),
            self.column_type,
            self.row_count,
            self.null_count,
            self.chunk_count,
            self.compression
        )
    }
}
