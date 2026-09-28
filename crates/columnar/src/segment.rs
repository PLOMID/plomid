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
//! Immutable columnar segments: the in-memory model and the header codec.
//!
//! A segment is addressable from its header alone. The header carries the
//! segment identity, the row and column counts, the offset of the chunk table,
//! and two CRC32C checksums: one over the header itself and one over the whole
//! body. Column metadata records and chunk headers are individually checksummed
//! on top of that, so the pruning path can validate the structures it reads
//! without paying for the payloads it does not read.

use crate::chunk::ColumnChunk;
use crate::column::ColumnMetadata;
use crate::format;
use crate::layout::{checksum_of, corruption, get_u32, get_u64, put_u32, put_u64, to_usize};
use crate::statistics::SegmentStatistics;
use plomid_core::{ErrorKind, GenerationId, PlomidError, Result, SegmentId};
use std::path::PathBuf;

/// Decoded contents of a columnar segment header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentHeader {
    /// Format version of the segment.
    pub format_version: u32,
    /// Number of logical rows in the segment.
    pub row_count: u64,
    /// Number of columns in the segment.
    pub column_count: u32,
    /// Number of chunks in the segment chunk table.
    pub chunk_count: u32,
    /// Total size in bytes of the null-bitmap region.
    pub null_bitmap_bytes: u64,
    /// Generation that published this segment.
    pub generation_id: GenerationId,
    /// Identity of this segment within its generation.
    pub segment_id: SegmentId,
    /// Absolute offset of the first chunk header.
    pub chunks_offset: u64,
    /// CRC32C over the header prefix `[0, COLUMNAR_OFF_CHECKSUM)`.
    pub header_checksum: u32,
    /// CRC32C over the metadata, null-bitmap, statistics, and chunk regions.
    pub body_checksum: u32,
}

impl SegmentHeader {
    /// Encoded size of a segment header in bytes.
    pub const ENCODED_LEN: usize = format::COLUMNAR_HEADER_SIZE;

    /// Creates a zeroed header for a segment with no columns or chunks.
    #[must_use]
    pub fn new(generation_id: GenerationId, segment_id: SegmentId, row_count: u64) -> Self {
        Self {
            format_version: format::COLUMNAR_FORMAT_VERSION,
            row_count,
            column_count: 0,
            chunk_count: 0,
            null_bitmap_bytes: 0,
            generation_id,
            segment_id,
            chunks_offset: 0,
            header_checksum: 0,
            body_checksum: 0,
        }
    }

    /// Encodes the header into its fixed-size little-endian image.
    ///
    /// The header checksum is computed last, over the prefix that excludes the
    /// checksum slot itself.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![0_u8; Self::ENCODED_LEN];
        bytes[format::COLUMNAR_OFF_MAGIC..format::COLUMNAR_OFF_VERSION]
            .copy_from_slice(&format::COLUMNAR_MAGIC);
        put_u32(
            &mut bytes,
            format::COLUMNAR_OFF_VERSION,
            self.format_version,
        );
        put_u64(&mut bytes, format::COLUMNAR_OFF_ROW_COUNT, self.row_count);
        put_u32(
            &mut bytes,
            format::COLUMNAR_OFF_COLUMN_COUNT,
            self.column_count,
        );
        put_u32(
            &mut bytes,
            format::COLUMNAR_OFF_CHUNK_COUNT,
            self.chunk_count,
        );
        put_u64(
            &mut bytes,
            format::COLUMNAR_OFF_NULL_BITMAP_BYTES,
            self.null_bitmap_bytes,
        );
        put_u64(
            &mut bytes,
            format::COLUMNAR_OFF_GENERATION_ID,
            self.generation_id.get(),
        );
        put_u64(
            &mut bytes,
            format::COLUMNAR_OFF_SEGMENT_ID,
            self.segment_id.get(),
        );
        put_u64(
            &mut bytes,
            format::COLUMNAR_OFF_CHUNKS_OFFSET,
            self.chunks_offset,
        );
        put_u32(
            &mut bytes,
            format::COLUMNAR_OFF_BODY_CHECKSUM,
            self.body_checksum,
        );
        let header_checksum = checksum_of(&bytes[..format::COLUMNAR_OFF_CHECKSUM]);
        put_u32(&mut bytes, format::COLUMNAR_OFF_CHECKSUM, header_checksum);
        bytes
    }
}

impl SegmentHeader {
    /// Decodes and validates a segment header from the start of `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let prefix = bytes
            .get(..Self::ENCODED_LEN)
            .ok_or_else(|| corruption("columnar segment is shorter than its header"))?;
        if prefix[format::COLUMNAR_OFF_MAGIC..format::COLUMNAR_OFF_VERSION]
            != format::COLUMNAR_MAGIC
        {
            return Err(corruption("columnar segment magic does not match"));
        }
        let header_checksum = get_u32(prefix, format::COLUMNAR_OFF_CHECKSUM, "header checksum")?;
        if header_checksum != checksum_of(&prefix[..format::COLUMNAR_OFF_CHECKSUM]) {
            return Err(corruption("columnar segment header checksum mismatch"));
        }
        let format_version = get_u32(prefix, format::COLUMNAR_OFF_VERSION, "format version")?;
        if format_version != format::COLUMNAR_FORMAT_VERSION {
            return Err(PlomidError::new(
                ErrorKind::Unsupported,
                format!("unsupported columnar segment format version {format_version}"),
            ));
        }
        Ok(Self {
            format_version,
            row_count: get_u64(prefix, format::COLUMNAR_OFF_ROW_COUNT, "row count")?,
            column_count: get_u32(prefix, format::COLUMNAR_OFF_COLUMN_COUNT, "column count")?,
            chunk_count: get_u32(prefix, format::COLUMNAR_OFF_CHUNK_COUNT, "chunk count")?,
            null_bitmap_bytes: get_u64(
                prefix,
                format::COLUMNAR_OFF_NULL_BITMAP_BYTES,
                "null bitmap bytes",
            )?,
            generation_id: GenerationId::new(get_u64(
                prefix,
                format::COLUMNAR_OFF_GENERATION_ID,
                "generation id",
            )?),
            segment_id: SegmentId::new(get_u64(
                prefix,
                format::COLUMNAR_OFF_SEGMENT_ID,
                "segment id",
            )?),
            chunks_offset: get_u64(prefix, format::COLUMNAR_OFF_CHUNKS_OFFSET, "chunks offset")?,
            header_checksum,
            body_checksum: get_u32(prefix, format::COLUMNAR_OFF_BODY_CHECKSUM, "body checksum")?,
        })
    }

    /// Returns the offset at which the column metadata table starts.
    #[must_use]
    pub fn metadata_offset(&self) -> usize {
        Self::ENCODED_LEN
    }

    /// Returns the size in bytes of the column metadata table.
    #[must_use]
    pub fn metadata_bytes(&self) -> usize {
        self.column_count as usize * format::COLUMNAR_COLUMN_METADATA_SIZE
    }

    /// Returns the offset at which the null-bitmap region starts.
    #[must_use]
    pub fn null_bitmap_offset(&self) -> usize {
        self.metadata_offset() + self.metadata_bytes()
    }

    /// Returns the offset of the chunk table as an in-memory index.
    pub fn chunks_offset_usize(&self) -> Result<usize> {
        to_usize(self.chunks_offset, "chunks offset")
    }

    /// Returns the byte range that the segment body checksum covers.
    pub fn body_range(&self) -> Result<std::ops::Range<usize>> {
        let start = self.metadata_offset();
        let end = self.chunks_offset_usize()?;
        if end < start {
            return Err(corruption(
                "columnar segment chunk table precedes its metadata table",
            ));
        }
        Ok(start..end)
    }
}
/// An immutable columnar segment.
#[derive(Clone, Debug)]
pub struct ColumnarSegment {
    /// Generation that published this segment.
    pub generation_id: GenerationId,
    /// Identity of this segment within its generation.
    pub segment_id: SegmentId,
    /// Location of the segment on disk, when it has been persisted.
    pub path: PathBuf,
    /// Per-column metadata in column order.
    pub columns: Vec<ColumnMetadata>,
    /// Chunk table in ascending column and row order.
    pub chunks: Vec<ColumnChunk>,
    /// Per-column statistics.
    pub statistics: SegmentStatistics,
    /// Number of logical rows in the segment.
    pub row_count: u64,
    /// Format version of the segment.
    pub format_version: u32,
    /// Whether the segment has been decoded from persisted bytes.
    pub is_readable: bool,
}

impl ColumnarSegment {
    /// Creates a segment image that has not yet been persisted or decoded.
    #[must_use]
    pub fn new(
        generation_id: GenerationId,
        segment_id: SegmentId,
        path: PathBuf,
        columns: Vec<ColumnMetadata>,
        chunks: Vec<ColumnChunk>,
        statistics: SegmentStatistics,
        row_count: u64,
    ) -> Self {
        Self {
            generation_id,
            segment_id,
            path,
            columns,
            chunks,
            statistics,
            row_count,
            format_version: format::COLUMNAR_FORMAT_VERSION,
            is_readable: false,
        }
    }

    /// Returns the number of columns in the segment.
    #[must_use]
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Returns the metadata of `column_id`, if the segment holds that column.
    ///
    /// Column metadata is indexed by ordinal, so a non-contiguous column
    /// identity is reported as absent rather than mismatched.
    #[must_use]
    pub fn column(&self, column_id: plomid_core::ColumnId) -> Option<&ColumnMetadata> {
        self.columns
            .get(column_id.get() as usize)
            .filter(|meta| meta.column_id == column_id)
    }

    /// Returns the chunks of `column_id` in row order.
    #[must_use]
    pub fn column_chunks(&self, column_id: plomid_core::ColumnId) -> Vec<&ColumnChunk> {
        self.chunks
            .iter()
            .filter(|chunk| chunk.column_id == column_id)
            .collect()
    }

    /// Returns the statistics of `column_id`.
    #[must_use]
    pub fn column_statistics(
        &self,
        column_id: plomid_core::ColumnId,
    ) -> Option<&crate::statistics::ColumnStatistics> {
        self.statistics.get(column_id)
    }

    /// Returns true when the segment has been decoded from persisted bytes.
    #[must_use]
    pub fn is_readable(&self) -> bool {
        self.is_readable
    }

    /// Returns the total stored size of every chunk payload.
    #[must_use]
    pub fn stored_payload_bytes(&self) -> u64 {
        self.chunks.iter().map(|chunk| chunk.compressed_size).sum()
    }

    /// Returns the total uncompressed size of every chunk payload.
    #[must_use]
    pub fn uncompressed_payload_bytes(&self) -> u64 {
        self.chunks
            .iter()
            .map(|chunk| chunk.uncompressed_size)
            .sum()
    }

    /// Returns the compression ratio of the segment, or `1.0` when empty.
    #[must_use]
    pub fn compression_ratio(&self) -> f64 {
        let stored = self.stored_payload_bytes();
        let raw = self.uncompressed_payload_bytes();
        if raw == 0 {
            1.0
        } else {
            stored as f64 / raw as f64
        }
    }
}

/// Assembles a [`ColumnarSegment`], verifying its internal consistency.
#[derive(Clone, Debug)]
pub struct SegmentBuilder {
    generation_id: GenerationId,
    segment_id: SegmentId,
    path: PathBuf,
    columns: Vec<ColumnMetadata>,
    chunks: Vec<ColumnChunk>,
    statistics: SegmentStatistics,
    row_count: u64,
}

impl SegmentBuilder {
    /// Creates a builder for a segment published by `generation_id`.
    #[must_use]
    pub fn new(generation_id: GenerationId, segment_id: SegmentId) -> Self {
        Self {
            generation_id,
            segment_id,
            path: PathBuf::new(),
            columns: Vec::new(),
            chunks: Vec::new(),
            statistics: SegmentStatistics::new(),
            row_count: 0,
        }
    }

    /// Sets the number of logical rows in the segment.
    #[must_use]
    pub fn row_count(mut self, row_count: u64) -> Self {
        self.row_count = row_count;
        self
    }

    /// Sets the on-disk location of the segment.
    #[must_use]
    pub fn path(mut self, path: PathBuf) -> Self {
        self.path = path;
        self
    }

    /// Appends one column's metadata.
    #[must_use]
    pub fn add_column(mut self, metadata: ColumnMetadata) -> Self {
        self.columns.push(metadata);
        self
    }

    /// Appends one chunk to the chunk table.
    #[must_use]
    pub fn add_chunk(mut self, chunk: ColumnChunk) -> Self {
        self.chunks.push(chunk);
        self
    }

    /// Records one column's statistics.
    #[must_use]
    pub fn add_statistics(mut self, stats: crate::statistics::ColumnStatistics) -> Self {
        self.statistics.add_column(stats);
        self
    }

    /// Validates the accumulated segment and returns it.
    ///
    /// A segment is only publishable when every column's metadata agrees with
    /// the chunk table it points at: the chunk range must lie inside the table,
    /// the chunks must append in row order, and the rows a column claims must
    /// equal the rows its chunks cover.
    pub fn build(self) -> Result<ColumnarSegment> {
        self.validate()?;
        Ok(ColumnarSegment {
            generation_id: self.generation_id,
            segment_id: self.segment_id,
            path: self.path,
            columns: self.columns,
            chunks: self.chunks,
            statistics: self.statistics,
            row_count: self.row_count,
            format_version: format::COLUMNAR_FORMAT_VERSION,
            is_readable: false,
        })
    }

    /// Verifies the cross-references between column metadata and chunks.
    fn validate(&self) -> Result<()> {
        let chunk_count = self.chunks.len();
        for (ordinal, meta) in self.columns.iter().enumerate() {
            let first = meta.first_chunk as usize;
            let count = meta.chunk_count as usize;
            let end = first
                .checked_add(count)
                .ok_or_else(|| corruption(format!("column {ordinal} chunk range overflows")))?;
            if end > chunk_count {
                return Err(corruption(format!(
                    "column {ordinal} references chunks {first}..{end} but the table holds {chunk_count}"
                )));
            }
            let mut covered_rows = 0_u64;
            for (offset, chunk) in self.chunks[first..end].iter().enumerate() {
                if chunk.column_id != meta.column_id {
                    return Err(corruption(format!(
                        "chunk {} of column {ordinal} belongs to column {}",
                        first + offset,
                        chunk.column_id.get()
                    )));
                }
                if offset > 0 {
                    let previous = &self.chunks[first + offset - 1];
                    if chunk.first_row != previous.end_row() {
                        return Err(corruption(format!(
                            "chunk {} of column {ordinal} starts at row {} but the previous chunk ends at row {}",
                            first + offset,
                            chunk.first_row,
                            previous.end_row()
                        )));
                    }
                }
                covered_rows = covered_rows.saturating_add(chunk.row_count);
            }
            if count > 0 && covered_rows != meta.row_count {
                return Err(corruption(format!(
                    "column {ordinal} claims {} rows but its chunks cover {covered_rows}",
                    meta.row_count
                )));
            }
        }
        Ok(())
    }
}
