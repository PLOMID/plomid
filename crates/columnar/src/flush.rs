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
//! Flush pipeline: materialized rows to a persisted columnar segment.
//!
//! The writer lays a segment out in one pass, in the order the format declares:
//! header, column metadata, null bitmaps, column statistics, then chunk headers
//! and payloads. Two properties make the output trustworthy:
//!
//! * **Chunk boundaries follow row boundaries.** A column's encoded values are
//!   self-delimiting, and the writer cuts chunks only at row entry boundaries,
//!   so no chunk ever splits a value and every chunk decodes in isolation.
//! * **Offsets are recorded, never predicted.** Every offset in the header and
//!   in the column metadata is taken from the buffer the writer has actually
//!   filled, so header, metadata, and payload cannot disagree.
//!
//! After the payloads are written the writer seals the segment: it computes the
//! body checksum over the metadata, null-bitmap, and statistics regions, then
//! the header checksum over the header prefix, and records both. A segment is
//! therefore never observable in a state where its checksums do not cover the
//! bytes beside them.

use crate::chunk::{chunk_checksum, ChunkCompression, ColumnChunk};
use crate::column::{ColumnMetadata, ColumnType};
use crate::compression;
use crate::encoding::{self, ColumnEncoding};
use crate::format;
use crate::layout::{checksum_of, corruption, invalid, put_u32, put_u64, put_u8};
use crate::materialization::{materialize_columns, MaterializedColumn};
use crate::segment::{ColumnarSegment, SegmentHeader};
use crate::statistics::{ColumnStatistics, SegmentStatistics, ValueRef};
use plomid_core::{GenerationId, Result, SegmentId};
use plomid_storage::Row;

/// Tunables for the columnar flush path.
#[derive(Clone, Debug)]
pub struct FlushConfig {
    /// Value encoding applied to every chunk.
    ///
    /// [`ColumnEncoding::Auto`] measures each chunk and keeps the smallest
    /// applicable encoding, so encoding never grows a chunk.
    pub encoding: ColumnEncoding,
    /// Byte codec applied after the value encoding.
    pub compression: ChunkCompression,
    /// Target uncompressed size of a single chunk in bytes.
    pub chunk_target_size: u64,
    /// Whether per-column minimum and maximum statistics are derived.
    pub collect_statistics: bool,
    /// Whether segment and per-record checksums are computed and verified.
    pub verify_checksums: bool,
    /// Rows per BRIN range when pruning metadata is collected.
    /// Zero disables pruning metadata.
    pub brin_rows_per_range: u64,
    /// Whether pruning metadata (zone maps + BRIN) is collected and persisted.
    pub collect_pruning: bool,
}

impl Default for FlushConfig {
    fn default() -> Self {
        Self {
            encoding: ColumnEncoding::Auto,
            compression: ChunkCompression::None,
            chunk_target_size: format::CHUNK_TARGET_SIZE,
            collect_statistics: true,
            verify_checksums: true,
            brin_rows_per_range: crate::pruning::DEFAULT_BRIN_ROWS_PER_RANGE,
            collect_pruning: true,
        }
    }
}

impl FlushConfig {
    /// Returns a configuration that compresses chunks with ZSTD.
    #[must_use]
    pub fn with_zstd() -> Self {
        Self {
            compression: ChunkCompression::Zstd,
            ..Self::default()
        }
    }

    /// Returns this configuration with a specific value encoding.
    #[must_use]
    pub fn with_encoding(mut self, encoding: ColumnEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// Returns this configuration with a specific byte codec.
    #[must_use]
    pub fn with_compression(mut self, compression: ChunkCompression) -> Self {
        self.compression = compression;
        self
    }

    /// Returns the effective chunk target in bytes.
    ///
    /// The target is clamped to hold at least one value entry, so a nonsensical
    /// configuration degrades to one row per chunk instead of failing or
    /// producing empty chunks.
    fn effective_chunk_target(&self) -> usize {
        usize::try_from(self.chunk_target_size)
            .unwrap_or(usize::MAX)
            .max(format::COLUMNAR_VALUE_LENGTH_PREFIX + 1)
    }

    /// Rejects configurations this build cannot honour.
    fn validate(&self) -> Result<()> {
        if !compression::is_compression_available(self.compression) {
            return Err(invalid(
                "the requested chunk compression is not available in this build",
            ));
        }
        if self.chunk_target_size > plomid_core::COLUMNAR_MAX_CHUNK_UNCOMPRESSED_SIZE {
            return Err(invalid(format!(
                "chunk target of {} bytes exceeds the {}-byte chunk limit",
                self.chunk_target_size,
                plomid_core::COLUMNAR_MAX_CHUNK_UNCOMPRESSED_SIZE
            )));
        }
        Ok(())
    }
}

/// A finished segment together with the exact bytes that encode it.
#[derive(Clone, Debug)]
pub struct FlushedSegment {
    /// The decoded segment model, ready to be published.
    pub segment: ColumnarSegment,
    /// The sealed on-disk image of the segment.
    pub bytes: Vec<u8>,
    /// The header that was written into `bytes`.
    pub header: SegmentHeader,
}
/// One column's encoded values, statistics, and chosen chunk boundaries.
struct PreparedColumn {
    /// Materialized column this preparation came from.
    materialized: MaterializedColumn,
    /// Encoded values in the persisted segment layout.
    encoded: Vec<u8>,
    /// Encoded entry boundaries with `row_count + 1` entries.
    bounds: Vec<usize>,
    /// Statistics folded from the column, or `None` when not collected.
    statistics: Option<ColumnStatistics>,
    /// Rows covered by each planned chunk.
    chunk_rows: Vec<(usize, usize)>,
    /// Null bitmap exactly `null_bitmap_size(row_count)` bytes wide.
    null_bitmap: Vec<u8>,
    /// Column metadata with every offset still unset.
    metadata: ColumnMetadata,
}

/// Groups chunk boundaries so each chunk stays near `target` bytes.
///
/// Boundaries are taken between rows, never inside a row's entry, so a chunk
/// always decodes on its own. Every returned range is non-empty, which
/// guarantees the loop terminates for any target.
fn plan_chunk_rows(bounds: &[usize], target: usize) -> Vec<(usize, usize)> {
    let row_count = bounds.len().saturating_sub(1);
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    while start < row_count {
        let mut end = start;
        let mut size = 0_usize;
        while end < row_count {
            let entry = bounds[end + 1] - bounds[end];
            if end > start && size + entry > target {
                break;
            }
            size += entry;
            end += 1;
            if size >= target {
                break;
            }
        }
        ranges.push((start, end));
        start = end;
    }
    ranges
}

/// Encodes the null bitmap of a column at exactly the persisted width.
///
/// The materializer sizes its bitmap from the row count it was created with, so
/// this is a consistency assertion rather than a conversion: a mismatch would
/// mean the column and the segment disagree about how many rows exist.
fn persisted_null_bitmap(column: &MaterializedColumn, row_count: u64) -> Result<Vec<u8>> {
    let expected = format::null_bitmap_size(row_count as usize);
    if column.null_bitmap.len() != expected {
        return Err(invalid(format!(
            "column {} null bitmap is {} bytes but {expected} are required for {row_count} rows",
            column.column_id.get(),
            column.null_bitmap.len()
        )));
    }
    Ok(column.null_bitmap.clone())
}

/// Folds one column's values into its statistics.
///
/// Bounds are only tracked for columns the statistics layer deems orderable, so
/// a type that cannot be ordered reports no minimum or maximum rather than a
/// fabricated one.
fn collect_column_statistics(
    column: &MaterializedColumn,
    row_count: u64,
    null_count: u64,
) -> Result<ColumnStatistics> {
    let mut stats =
        ColumnStatistics::with_type(column.column_id, row_count, null_count, column.column_type);
    for row_idx in 0..row_count as usize {
        let value = if column.is_null(row_idx) {
            None
        } else {
            let bytes = column.value_bytes(row_idx)?;
            Some(ValueRef::new(column.column_type, bytes))
        };
        stats.observe_value(value);
    }
    Ok(stats)
}

impl PreparedColumn {
    /// Encodes one column's values and chooses its chunk boundaries.
    ///
    /// The encoded value stream is a sequence of `<u32 length><bytes>` entries,
    /// one per row in ordinal order, exactly as the format persists them. The
    /// `bounds` array records the byte offset of every row's entry so the
    /// planner can cut chunks between rows without decoding the stream.
    fn prepare(column: &MaterializedColumn, row_count: u64, config: &FlushConfig) -> Result<Self> {
        let encoded = column.encoded_values()?;
        let row_count_usize = row_count as usize;

        let mut bounds = Vec::with_capacity(row_count_usize + 1);
        let mut cursor = 0_usize;
        bounds.push(0);
        for row_idx in 0..row_count_usize {
            let entry_len = if column.is_null(row_idx) {
                0
            } else {
                column.lengths[row_idx] as usize
            };
            cursor = cursor
                .checked_add(format::COLUMNAR_VALUE_LENGTH_PREFIX)
                .and_then(|sum| sum.checked_add(entry_len))
                .ok_or_else(|| corruption("column value stream length overflows"))?;
            bounds.push(cursor);
        }
        if cursor != encoded.len() {
            return Err(corruption(format!(
                "column {} encoded length {} disagrees with derived offset {}",
                column.column_id.get(),
                encoded.len(),
                cursor
            )));
        }

        let target = config.effective_chunk_target();
        let chunk_rows = plan_chunk_rows(&bounds, target);
        let null_bitmap = persisted_null_bitmap(column, row_count)?;
        let statistics = if config.collect_statistics {
            let null_count = column.null_count();
            Some(collect_column_statistics(column, row_count, null_count)?)
        } else {
            None
        };

        Ok(Self {
            materialized: column.clone(),
            encoded,
            bounds,
            statistics,
            chunk_rows,
            null_bitmap,
            metadata: ColumnMetadata::new(column.column_id, column.column_type, row_count),
        })
    }

    /// Number of rows this column covers.
    fn row_count(&self) -> u64 {
        self.materialized.row_count
    }

    /// Number of rows covered by the chunk at `chunk_index`.
    fn chunk_row_count(&self, chunk_index: usize) -> u64 {
        let (start, end) = self.chunk_rows[chunk_index];
        (end - start) as u64
    }

    /// Byte range of the chunk at `chunk_index` within the encoded stream.
    fn chunk_byte_range(&self, chunk_index: usize) -> std::ops::Range<usize> {
        let (start_row, end_row) = self.chunk_rows[chunk_index];
        self.bounds[start_row]..self.bounds[end_row]
    }
}

/// Encodes a 56-byte chunk header.
///
/// The checksum slot is left zeroed here; the caller fills it in after the
/// payload is chosen, because the checksum covers both the header prefix and
/// the stored payload.
fn encode_chunk_header(
    encoding: ColumnEncoding,
    compression: ChunkCompression,
    first_row: u64,
    row_count: u64,
    payload_offset: u64,
    compressed_size: u64,
    uncompressed_size: u64,
) -> Vec<u8> {
    let mut header = vec![0_u8; format::COLUMNAR_CHUNK_HEADER_SIZE];
    header[format::CHUNK_OFF_MAGIC..format::CHUNK_OFF_VERSION]
        .copy_from_slice(&format::COLUMNAR_CHUNK_MAGIC);
    put_u32(
        &mut header,
        format::CHUNK_OFF_VERSION,
        format::COLUMNAR_CHUNK_FORMAT_VERSION,
    );
    put_u8(
        &mut header,
        format::CHUNK_OFF_COMPRESSION,
        compression.tag(),
    );
    put_u8(
        &mut header,
        format::CHUNK_OFF_ENCODING,
        encoding.tag().unwrap_or(format::ENCODING_RAW_TAG),
    );
    put_u64(&mut header, format::CHUNK_OFF_FIRST_ROW, first_row);
    put_u64(&mut header, format::CHUNK_OFF_ROW_COUNT, row_count);
    put_u64(&mut header, format::CHUNK_OFF_PAYLOAD, payload_offset);
    put_u64(
        &mut header,
        format::CHUNK_OFF_COMPRESSED_SIZE,
        compressed_size,
    );
    put_u64(
        &mut header,
        format::CHUNK_OFF_UNCOMPRESSED_SIZE,
        uncompressed_size,
    );
    header
}
/// Writes a finished columnar segment to memory.
///
/// The rows are materialized into columns, each column is encoded and chunked,
/// and the result is laid out as a single integrity-checked byte image:
///
/// ```text
/// header | column metadata | null bitmaps | statistics | chunk headers + payloads
/// ```
///
/// The chunk table begins at a fixed offset recorded in the header, and the
/// body checksum covers the metadata, null bitmaps, and statistics so a reader
/// can validate the structures it scans without touching a payload. Each chunk
/// carries its own checksum over its header prefix and stored payload.
pub fn flush(
    rows: &[Row],
    column_types: &[ColumnType],
    generation_id: GenerationId,
    segment_id: SegmentId,
    config: &FlushConfig,
) -> Result<FlushedSegment> {
    config.validate()?;

    let row_count = rows.len() as u64;
    let materialized = materialize_columns(rows, column_types)?;
    let prepared: Vec<PreparedColumn> = materialized
        .iter()
        .map(|col| PreparedColumn::prepare(col, row_count, config))
        .collect::<Result<Vec<_>>>()?;

    let column_count = prepared.len();
    let null_bitmap_total: usize = prepared.iter().map(|p| p.null_bitmap.len()).sum();
    let chunk_count: u64 = prepared.iter().map(|p| p.chunk_rows.len() as u64).sum();

    // Encode each column's statistics payload up front so every region's size is
    // known and each metadata record can point at its statistics.
    let mut stats_payloads: Vec<Option<Vec<u8>>> = Vec::with_capacity(column_count);
    let mut stats_checksums: Vec<u32> = Vec::with_capacity(column_count);
    let mut total_stats_bytes = 0_usize;
    for prepared_col in &prepared {
        if let Some(ref stats) = prepared_col.statistics {
            let (payload, _flags) = stats.encode_with_flags()?;
            total_stats_bytes = total_stats_bytes
                .checked_add(payload.len())
                .ok_or_else(|| corruption("statistics region length overflows"))?;
            stats_checksums.push(checksum_of(&payload));
            stats_payloads.push(Some(payload));
        } else {
            stats_checksums.push(0);
            stats_payloads.push(None);
        }
    }

    let header_size = format::COLUMNAR_HEADER_SIZE;
    let col_meta_size = column_count * format::COLUMNAR_COLUMN_METADATA_SIZE;
    let metadata_end = header_size
        .checked_add(col_meta_size)
        .ok_or_else(|| corruption("column metadata region length overflows"))?;
    let null_bitmap_offset = metadata_end;
    let statistics_region_start = null_bitmap_offset
        .checked_add(null_bitmap_total)
        .ok_or_else(|| corruption("null bitmap region length overflows"))?;
    let chunks_offset = statistics_region_start
        .checked_add(total_stats_bytes)
        .ok_or_else(|| corruption("statistics region length overflows"))?;

    // Per-column null-bitmap and statistics offsets (column order).
    let mut null_offsets: Vec<u64> = Vec::with_capacity(column_count);
    let mut stats_offsets: Vec<u64> = Vec::with_capacity(column_count);
    let mut running_null = null_bitmap_offset as u64;
    let mut running_stats = statistics_region_start as u64;
    for prepared_col in &prepared {
        null_offsets.push(running_null);
        running_null = running_null
            .checked_add(prepared_col.null_bitmap.len() as u64)
            .expect("null bitmap offset fits");
    }
    for (idx, payload) in stats_payloads.iter().enumerate() {
        let offset = if payload.is_some() {
            let placed = running_stats;
            running_stats = running_stats
                .checked_add(payload.as_ref().map_or(0, Vec::len) as u64)
                .expect("statistics offset fits");
            placed
        } else {
            0
        };
        stats_offsets.push(offset);
        let _ = idx;
    }

    // Reserve the header, then build the column metadata records.
    let mut out: Vec<u8> = vec![0; header_size];

    let mut columns_metadata: Vec<ColumnMetadata> = Vec::with_capacity(column_count);
    for (col_idx, prepared_col) in prepared.iter().enumerate() {
        let mut meta = prepared_col.metadata.clone();
        meta.row_count = prepared_col.row_count();
        meta.null_count = prepared_col.materialized.null_count();
        meta.null_bitmap_offset = null_offsets[col_idx];
        meta.chunk_count = prepared_col.chunk_rows.len() as u32;
        meta.statistics_offset = stats_offsets[col_idx];
        meta.statistics_size = stats_payloads[col_idx].as_ref().map_or(0, |payload| {
            u32::try_from(payload.len()).expect("stats fit in u32")
        });
        meta.statistics_checksum = stats_checksums[col_idx];
        meta.stats_flags = prepared_col
            .statistics
            .as_ref()
            .map_or(0, |stats| stats.flags());
        columns_metadata.push(meta);
    }

    // Assign contiguous first_chunk indices (column order) and write the records.
    let mut next_chunk: u32 = 1;
    for meta in &mut columns_metadata {
        meta.first_chunk = next_chunk - 1;
        next_chunk = next_chunk
            .checked_add(meta.chunk_count)
            .expect("chunk count fits in u32");
    }
    for meta in &columns_metadata {
        out.extend_from_slice(&meta.encode());
    }

    // Write the null-bitmap region, then the statistics region. A column with no
    // statistics contributes no bytes and keeps its zero offset.
    for prepared_col in &prepared {
        out.extend_from_slice(&prepared_col.null_bitmap);
    }
    for (col_idx, payload) in stats_payloads.iter().enumerate() {
        if let Some(payload) = payload {
            let expected = stats_checksums[col_idx];
            if expected != checksum_of(payload) {
                return Err(corruption(format!(
                    "column {col_idx} statistics checksum changed before write"
                )));
            }
            out.extend_from_slice(payload);
        }
    }

    // Emit chunk headers and payloads in column order, building the chunk table.
    // As we go we record each column's values offset and byte size so the
    // metadata records can be patched once the chunk table is sealed.
    let mut chunks: Vec<ColumnChunk> = Vec::with_capacity(chunk_count as usize);
    let mut column_values: Vec<(u64, u64)> = vec![(0, 0); column_count];
    for (col_idx, prepared_col) in prepared.iter().enumerate() {
        let column_values_offset = out.len() as u64;
        let mut column_values_size = 0_u64;
        let mut accumulated_rows = 0_u64;
        for chunk_index in 0..prepared_col.chunk_rows.len() {
            let byte_range = prepared_col.chunk_byte_range(chunk_index);
            let raw = &prepared_col.encoded[byte_range];
            let first_row = accumulated_rows;
            let rows_in_chunk = prepared_col.chunk_row_count(chunk_index);
            // Value encoding and codec happen here, once, on the chunk's exact
            // byte range: the encoding decides the bytes and the frame about to
            // be written into the chunk header.
            let encoded =
                encoding::encode_chunk(raw, rows_in_chunk, config.encoding, config.compression)?;
            let payload = &encoded.payload;
            let compressed_size = u64::try_from(payload.len())
                .map_err(|_| corruption("chunk payload exceeds u64"))?;
            let uncompressed_size = encoded.uncompressed_len;

            let mut chunk_header = encode_chunk_header(
                encoded.encoding,
                encoded.codec,
                first_row,
                rows_in_chunk,
                0,
                compressed_size,
                uncompressed_size,
            );
            // The header answers where its payload begins: the header this
            // buffer is about to append, followed immediately by the payload.
            let payload_offset = (out.len() as u64)
                .checked_add(format::COLUMNAR_CHUNK_HEADER_SIZE as u64)
                .ok_or_else(|| corruption("chunk payload offset overflows u64"))?;
            put_u64(&mut chunk_header, format::CHUNK_OFF_PAYLOAD, payload_offset);
            let chunk_checksum =
                chunk_checksum(&chunk_header[..format::CHUNK_OFF_CHECKSUM], payload);
            put_u32(
                &mut chunk_header,
                format::CHUNK_OFF_CHECKSUM,
                chunk_checksum,
            );

            out.extend_from_slice(&chunk_header);
            out.extend_from_slice(payload);

            chunks.push(ColumnChunk::new(
                prepared_col.materialized.column_id,
                encoded.encoding,
                encoded.codec,
                first_row,
                rows_in_chunk,
                payload_offset,
                compressed_size,
                uncompressed_size,
                chunk_checksum,
            ));
            column_values_size += chunk_header.len() as u64 + compressed_size;
            accumulated_rows += rows_in_chunk;
        }
        column_values[col_idx] = (column_values_offset, column_values_size);
    }

    // Patch each column's values offset and size into its metadata record.
    for (col_idx, (values_offset, values_size)) in column_values.iter().enumerate() {
        let record_offset = header_size + col_idx * format::COLUMNAR_COLUMN_METADATA_SIZE;
        put_u64(
            &mut out,
            record_offset + format::COLUMN_META_OFF_VALUES,
            *values_offset,
        );
        put_u64(
            &mut out,
            record_offset + format::COLUMN_META_OFF_VALUES_SIZE,
            *values_size,
        );
        // The record checksum covers the prefix it lives in; recompute it so
        // the patched offsets stay covered by the recorded checksum.
        let checksum = checksum_of(
            &out[record_offset..record_offset + format::COLUMN_META_OFF_RECORD_CHECKSUM],
        );
        put_u32(
            &mut out,
            record_offset + format::COLUMN_META_OFF_RECORD_CHECKSUM,
            checksum,
        );
    }

    // Seal the segment header now that every region offset is known.
    let mut header = SegmentHeader::new(generation_id, segment_id, row_count);
    header.column_count = column_count as u32;
    header.chunk_count = chunks.len() as u32;
    header.null_bitmap_bytes = null_bitmap_total as u64;
    header.chunks_offset = chunks_offset as u64;
    let body_end = chunks_offset;
    header.body_checksum = checksum_of(&out[header_size..body_end]);
    let header_bytes = header.encode();
    out[..header_size].copy_from_slice(&header_bytes);

    // Build pruning metadata if requested.
    let pruning: Option<crate::pruning::SegmentPruning> =
        if config.collect_pruning && config.brin_rows_per_range > 0 {
            let pruning = crate::pruning::build_segment_pruning(
                &materialized,
                row_count,
                config.brin_rows_per_range,
            );
            // Verify the built metadata is valid.
            pruning.validate()?;
            Some(pruning)
        } else {
            None
        };

    // Append pruning trailer if we built it.
    if let Some(pr) = &pruning {
        let brin = pr.brin.as_ref().unwrap();
        let trailer_bytes = crate::pruning::encode_trailer(brin);
        out.extend_from_slice(&trailer_bytes);
    }

    // Assemble the decoded model.
    let mut segment_statistics = SegmentStatistics::new();
    for prepared_col in &prepared {
        if let Some(stats) = &prepared_col.statistics {
            segment_statistics.add_column(stats.clone());
        }
    }
    let segment = ColumnarSegment::new(
        generation_id,
        segment_id,
        std::path::PathBuf::new(),
        columns_metadata,
        chunks,
        segment_statistics,
        row_count,
    );

    debug_assert_eq!(header.encode().len(), header_size);
    debug_assert_eq!(segment.column_count(), column_count);
    debug_assert_eq!(
        segment_chunks_total(&segment) as usize,
        prepared.iter().map(|p| p.chunk_rows.len()).sum::<usize>()
    );
    let _ = format::STATS_ALL_AVAILABLE;
    Ok(FlushedSegment {
        segment,
        bytes: out,
        header,
    })
}

/// Sums every column's chunk count from a decoded segment.
fn segment_chunks_total(segment: &ColumnarSegment) -> u64 {
    segment
        .columns
        .iter()
        .map(|meta| u64::from(meta.chunk_count))
        .sum()
}
