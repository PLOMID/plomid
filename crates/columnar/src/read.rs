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
//! Read path for columnar segments.
//!
//! A [`SegmentReader`] owns the decoded image of a segment. It verifies every
//! checksum the format carries — the header, the body, each column metadata
//! record, each statistics payload, and each chunk — so callers never read a
//! structure without validating it first.
//!
//! Rows are rebuilt lazily: the reader locates the chunks of a column, decodes
//! only the chunk payloads that overlap the requested row range, strips the
//! `u32`-length prefixes the writer wrote, and yields the original value bytes.
//! NULL rows are recovered from the column's null bitmap, which is authoritative.

use crate::chunk::ColumnChunk;
use crate::chunk::{chunk_checksum, ChunkData};
use crate::column::ColumnMetadata;
use crate::format;
use crate::layout::{checksum_of, corruption, to_usize};
use crate::materialization::MaterializedColumn;
use crate::pruning::SegmentPruning;
use crate::segment::ColumnarSegment;
use crate::statistics::{ColumnStatistics, SegmentStatistics};
use plomid_core::{ColumnId, PlomidError, Result};
use plomid_storage::Row;

/// Validates and decodes a persisted columnar segment image.
#[derive(Clone, Debug)]
pub struct SegmentReader {
    /// Decoded header of the segment.
    pub header: crate::segment::SegmentHeader,
    /// Decoded column metadata, in column order.
    pub columns: Vec<ColumnMetadata>,
    /// Decoded chunk table, in column then row order.
    pub chunks: Vec<ColumnChunk>,
    /// Per-column statistics rebuilt from the statistics region.
    pub statistics: crate::statistics::SegmentStatistics,
    /// Number of logical rows in the segment.
    pub row_count: u64,
    /// Pruning metadata (zone maps + BRIN) attached to this segment, if present.
    pub pruning: Option<SegmentPruning>,
}

impl SegmentReader {
    /// Encoded size of a segment header in bytes.
    pub const HEADER_SIZE: usize = format::COLUMNAR_HEADER_SIZE;

    /// Decodes a segment image, verifying every checksum on the way.
    ///
    /// The header checksum is checked first; if the header is damaged nothing
    /// else is trusted. The body checksum is then checked over the metadata,
    /// null-bitmap, and statistics regions. Column metadata records, statistics
    /// payloads, and chunk headers are validated as they are decoded, so a
    /// single corrupt column or chunk is reported precisely.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let header = crate::segment::SegmentHeader::decode(bytes)?;
        let header_end = header.metadata_offset();
        let column_count = header.column_count as usize;
        let metadata_size = column_count
            .checked_mul(format::COLUMNAR_COLUMN_METADATA_SIZE)
            .ok_or_else(|| corruption("column metadata size overflows usize"))?;
        let metadata_end = header_end
            .checked_add(metadata_size)
            .ok_or_else(|| corruption("column metadata region overflows the segment"))?;

        // Body checksum covers metadata + null bitmaps + statistics.
        let chunks_offset_usize = to_usize(header.chunks_offset, "chunks offset")?;
        if chunks_offset_usize < metadata_end {
            return Err(corruption(
                "segment chunk table precedes its metadata table",
            ));
        }
        let body = bytes
            .get(header_end..chunks_offset_usize)
            .ok_or_else(|| corruption("segment body lies outside its image"))?;
        if header.body_checksum != checksum_of(body) {
            return Err(corruption("columnar segment body checksum mismatch"));
        }

        // Decode column metadata records.
        let mut columns = Vec::with_capacity(column_count);
        for col_idx in 0..column_count {
            let base = header_end + col_idx * format::COLUMNAR_COLUMN_METADATA_SIZE;
            let record = bytes
                .get(base..base + format::COLUMNAR_COLUMN_METADATA_SIZE)
                .ok_or_else(|| corruption("column metadata table is truncated"))?;
            columns.push(ColumnMetadata::decode(record)?);
        }

        // Decode the statistics region, one payload per column.
        let mut statistics = SegmentStatistics::new();
        for meta in &columns {
            let stats_bytes = if meta.statistics_size == 0 {
                &b""[..]
            } else {
                let stats_offset = to_usize(meta.statistics_offset, "statistics offset")?;
                let stats_end = stats_offset
                    .checked_add(meta.statistics_size as usize)
                    .ok_or_else(|| corruption("statistics region overflows the segment image"))?;
                bytes
                    .get(stats_offset..stats_end)
                    .ok_or_else(|| corruption("statistics payload lies outside the segment"))?
            };
            if checksum_of(stats_bytes) != meta.statistics_checksum {
                return Err(corruption(format!(
                    "column {} statistics checksum mismatch",
                    meta.column_id.get()
                )));
            }
            // The statistics payload carries the bounds; the row and null counts
            // live in the column metadata record, which is the same record the
            // flag byte was written from.
            let base = ColumnStatistics::new(meta.column_id, meta.row_count, meta.null_count);
            let decoded = base.decode_with(stats_bytes, meta.stats_flags)?;
            statistics.add_column(decoded);
        }

        // Decode the chunk table. Chunks are stored as [header][payload] pairs
        // beginning at the chunk table offset. `decode_header` validates the
        // framing and returns the offset of the next header, which is the first
        // byte after this chunk's stored payload.
        let mut chunks = Vec::with_capacity(header.chunk_count as usize);
        let mut position = chunks_offset_usize;
        for chunk_idx in 0..header.chunk_count as usize {
            let (fields, next) =
                ColumnChunk::decode_header(bytes, position as u64).map_err(|error| {
                    // Keep the error kind: an unsupported framing version stays
                    // distinguishable from damaged bytes.
                    PlomidError::new(error.kind(), format!("chunk {chunk_idx} header: {error}"))
                })?;
            let prefix_end = position
                .checked_add(format::CHUNK_OFF_CHECKSUM)
                .ok_or_else(|| corruption("chunk header prefix overflows usize"))?;
            let prefix = bytes
                .get(position..prefix_end)
                .ok_or_else(|| corruption(format!("chunk {chunk_idx} header is truncated")))?;
            let payload_pos = to_usize(fields.payload_offset, "chunk payload offset")?;
            let compressed_end = payload_pos
                .checked_add(fields.compressed_size as usize)
                .ok_or_else(|| corruption("chunk payload overflows the segment image"))?;
            let compressed = bytes
                .get(payload_pos..compressed_end)
                .ok_or_else(|| corruption("chunk payload lies outside the segment image"))?;
            if chunk_checksum(prefix, compressed) != fields.checksum {
                return Err(corruption(format!(
                    "chunk {chunk_idx} payload checksum mismatch"
                )));
            }

            chunks.push(ColumnChunk::new(
                ColumnId::new(0), // attributed from metadata below
                fields.encoding,
                fields.compression,
                fields.first_row,
                fields.row_count,
                fields.payload_offset,
                fields.compressed_size,
                fields.uncompressed_size,
                fields.checksum,
            ));
            position = to_usize(next, "chunk table position")?;
        }

        // Attribute each chunk to its column. A column with N chunks owns the
        // next N chunk-table slots; column_id comes from the metadata record,
        // which is the source of truth.
        let mut slot = 0_usize;
        for meta in &columns {
            for _ in 0..meta.chunk_count {
                if slot < chunks.len() {
                    chunks[slot].column_id = meta.column_id;
                }
                slot += 1;
            }
        }
        if slot != chunks.len() {
            return Err(corruption(
                "chunk table length disagrees with column metadata",
            ));
        }

        // Pruning metadata, if any, begins exactly where the chunk table ends.
        // That offset comes from integrity-checked chunk framing, so a damaged
        // trailer length can never masquerade as "no metadata present".
        let trailer_start = position;
        let pruning =
            crate::pruning::decode_trailer_at(bytes, trailer_start)?.map(SegmentPruning::from_brin);

        Ok(Self {
            header,
            columns,
            chunks,
            statistics,
            row_count: header.row_count,
            pruning,
        })
    }

    /// Returns the metadata of `column_id`, if present.
    #[must_use]
    pub fn column(&self, column_id: ColumnId) -> Option<&ColumnMetadata> {
        self.columns
            .get(column_id.get() as usize)
            .filter(|meta| meta.column_id == column_id)
    }

    /// Returns the chunks of `column_id` in row order.
    #[must_use]
    pub fn column_chunks(&self, column_id: ColumnId) -> Vec<&ColumnChunk> {
        self.chunks
            .iter()
            .filter(|chunk| chunk.column_id == column_id)
            .collect()
    }

    /// Returns the statistics logged for `column_id`.
    #[must_use]
    pub fn column_statistics(&self, column_id: ColumnId) -> Option<&ColumnStatistics> {
        self.statistics.get(column_id)
    }

    /// Decodes a single chunk's payload into an owned [`ChunkData`].
    ///
    /// The stored payload was integrity-checked against its framing checksum
    /// when the segment was decoded, so this decompresses rather than
    /// re-checksums. Decoding is delegated to the encoding layer, which enforces
    /// the format's allocation bounds, requires every declared length to agree
    /// with the bytes actually present, validates the chunk's encoding frame
    /// against this chunk's framing, and re-checks the decoded value stream
    /// entry by entry before it is handed to a caller.
    pub fn read_chunk(&self, bytes: &[u8], chunk: &ColumnChunk) -> Result<ChunkData> {
        let payload_pos = to_usize(chunk.file_offset, "chunk payload offset")?;
        let compressed_end = payload_pos
            .checked_add(chunk.compressed_size as usize)
            .ok_or_else(|| corruption("chunk payload overflows the segment image"))?;
        let compressed = bytes
            .get(payload_pos..compressed_end)
            .ok_or_else(|| corruption("chunk payload lies outside the segment image"))?;
        let value_stream = crate::encoding::decode_chunk(
            compressed,
            chunk.encoding,
            chunk.compression,
            chunk.uncompressed_size,
            chunk.row_count,
        )?;
        let mut data = ChunkData::new(chunk.clone(), value_stream);
        data.mark_verified();
        Ok(data)
    }

    /// Materializes the rows in `[start_row, end_row)` by scanning columns.
    ///
    /// Only the chunks overlapping the requested range are decoded, so a narrow
    /// scan skips chunks it never touches.
    pub fn read_rows(
        &self,
        bytes: &[u8],
        start_row: u64,
        end_row: u64,
        column_ids: &[ColumnId],
    ) -> Result<Vec<Row>> {
        let end_row = end_row.min(self.row_count);
        let start_row = start_row.min(end_row);
        let count = (end_row - start_row) as usize;

        let mut columns: Vec<MaterializedColumn> = Vec::with_capacity(column_ids.len());
        for &column_id in column_ids {
            let meta = self
                .column(column_id)
                .ok_or_else(|| corruption(format!("unknown column {}", column_id.get())))?;
            columns.push(self.read_column(bytes, meta, start_row, end_row)?);
        }

        let mut rows = Vec::with_capacity(count);
        for row_idx in 0..count {
            let mut fields = Vec::with_capacity(columns.len());
            for col in &columns {
                fields.push(col.get_field(row_idx));
            }
            rows.push(Row::new(fields));
        }
        Ok(rows)
    }

    /// Materializes one column's value entries for rows `[start_row, end_row)`.
    ///
    /// Chunks are decoded lazily and only the ones overlapping the range are
    /// touched; the resulting [`MaterializedColumn`] reuses the writer's
    /// in-memory layout, so the value-extraction helpers apply unchanged.
    pub fn read_column(
        &self,
        bytes: &[u8],
        meta: &ColumnMetadata,
        start_row: u64,
        end_row: u64,
    ) -> Result<MaterializedColumn> {
        let row_count = end_row.saturating_sub(start_row) as usize;
        let mut null_bitmap = vec![0_u8; format::null_bitmap_size(row_count)];
        let mut values = Vec::new();
        let mut offsets = vec![0_u32; row_count];
        let mut lengths = vec![0_u32; row_count];

        let null_offset = to_usize(meta.null_bitmap_offset, "null bitmap offset")?;
        let null_len = format::null_bitmap_size(meta.row_count as usize);
        let null_bytes = bytes
            .get(null_offset..null_offset + null_len)
            .ok_or_else(|| corruption("column null bitmap lies outside the segment image"))?;

        for chunk in self.column_chunks(meta.column_id) {
            if chunk.end_row() <= start_row || chunk.first_row >= end_row {
                continue;
            }
            let data = self.read_chunk(bytes, chunk)?;
            let payload = data.as_slice();

            let row_begin = chunk.first_row.max(start_row);
            let row_end = chunk.end_row().min(end_row);
            let mut cursor = 0_usize;
            for _ in 0..row_begin.saturating_sub(chunk.first_row) {
                let length = read_entry_length(payload, &mut cursor)?;
                cursor = cursor
                    .checked_add(length as usize)
                    .ok_or_else(|| corruption("chunk entry overflows its frame"))?;
            }
            for out_row in 0..(row_end - row_begin) as usize {
                let length = read_entry_length(payload, &mut cursor)?;
                let value = payload
                    .get(cursor..cursor + length as usize)
                    .ok_or_else(|| corruption("chunk value lies outside its entry"))?;
                let absolute = row_begin + out_row as u64;
                let dest = (absolute - start_row) as usize;
                offsets[dest] = values.len() as u32;
                lengths[dest] = length;
                values.extend_from_slice(value);
                cursor += length as usize;

                let byte = absolute as usize / 8;
                let bit = absolute as usize % 8;
                if null_bytes.get(byte).is_some_and(|b| b & (1 << bit) != 0) {
                    let dest_byte = dest / 8;
                    let dest_bit = dest % 8;
                    if let Some(slot) = null_bitmap.get_mut(dest_byte) {
                        *slot |= 1 << dest_bit;
                    }
                }
            }
        }

        Ok(MaterializedColumn {
            column_id: meta.column_id,
            column_type: meta.column_type,
            row_count: row_count as u64,
            null_bitmap,
            values,
            offsets,
            lengths,
        })
    }

    /// Returns the decoded segment model.
    #[must_use]
    pub fn into_segment(self) -> ColumnarSegment {
        ColumnarSegment::new(
            self.header.generation_id,
            self.header.segment_id,
            std::path::PathBuf::new(),
            self.columns,
            self.chunks,
            self.statistics,
            self.row_count,
        )
    }
}

/// Reads a little-endian `u32` length prefix from `cursor`.
fn read_entry_length(payload: &[u8], cursor: &mut usize) -> Result<u32> {
    let prefix = payload
        .get(*cursor..*cursor + format::COLUMNAR_VALUE_LENGTH_PREFIX)
        .ok_or_else(|| corruption("chunk value length prefix is truncated"))?;
    *cursor += format::COLUMNAR_VALUE_LENGTH_PREFIX;
    Ok(u32::from_le_bytes(prefix.try_into().map_err(|_| {
        corruption("chunk value length prefix is truncated")
    })?))
}
