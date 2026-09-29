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
//! Column chunks: compressed/uncompressed value blocks within a column.
//!
//! Chunks never split a value entry, so each chunk is independently decodable.
//! Chunks are stored in ascending row order and the [`ColumnChunk::first_row`]
//! of the first chunk of a column is always zero.

use crate::encoding::ColumnEncoding;
use crate::format;
use plomid_core::{ColumnId, ErrorKind, PlomidError, Result};

/// Compression encoding for a chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkCompression {
    /// Payload is stored verbatim.
    None,
    /// Payload is a ZSTD frame.
    Zstd,
}

impl ChunkCompression {
    /// Returns the format tag byte for this encoding.
    #[must_use]
    pub fn tag(&self) -> u8 {
        match self {
            ChunkCompression::None => format::COMPRESSION_NONE_TAG,
            ChunkCompression::Zstd => format::COMPRESSION_ZSTD_TAG,
        }
    }

    /// Decodes an encoding from its format tag byte.
    #[must_use]
    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            format::COMPRESSION_NONE_TAG => Some(ChunkCompression::None),
            format::COMPRESSION_ZSTD_TAG => Some(ChunkCompression::Zstd),
            _ => None,
        }
    }
}

/// A chunk of column values with the metadata required to decode it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnChunk {
    /// Column this chunk belongs to.
    pub column_id: ColumnId,
    /// Value encoding applied to the stored payload.
    ///
    /// Byte 9 of the chunk header is reserved in the version-1 layout, so a
    /// segment written before value encodings existed reads back as
    /// [`ColumnEncoding::Raw`] and decodes unchanged.
    pub encoding: ColumnEncoding,
    /// Compression encoding of the stored payload.
    pub compression: ChunkCompression,
    /// Ordinal of the first row covered by this chunk.
    pub first_row: u64,
    /// Number of rows covered by this chunk.
    pub row_count: u64,
    /// Absolute offset of the compressed payload in the segment.
    pub file_offset: u64,
    /// Stored (compressed) payload size in bytes.
    pub compressed_size: u64,
    /// Size in bytes of what the codec was given, before compression.
    ///
    /// For a framed chunk this is the encoding frame plus the encoded body; for
    /// a [`ColumnEncoding::Raw`] chunk it is the value stream itself.
    pub uncompressed_size: u64,
    /// CRC32C checksum of the stored payload.
    pub checksum: u32,
}

impl ColumnChunk {
    /// Creates chunk metadata.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        column_id: ColumnId,
        encoding: ColumnEncoding,
        compression: ChunkCompression,
        first_row: u64,
        row_count: u64,
        file_offset: u64,
        compressed_size: u64,
        uncompressed_size: u64,
        checksum: u32,
    ) -> Self {
        Self {
            column_id,
            encoding,
            compression,
            first_row,
            row_count,
            file_offset,
            compressed_size,
            uncompressed_size,
            checksum,
        }
    }

    /// Returns true when this chunk stores a compressed payload.
    #[must_use]
    pub fn is_compressed(&self) -> bool {
        self.compression != ChunkCompression::None
    }

    /// Returns true when this chunk carries a value-encoding frame.
    #[must_use]
    pub fn is_encoded(&self) -> bool {
        self.encoding.is_framed()
    }

    /// Returns the last row ordinal covered by this chunk, exclusive.
    #[must_use]
    pub fn end_row(&self) -> u64 {
        self.first_row.saturating_add(self.row_count)
    }

    /// Returns the number of bytes this chunk occupies, header included.
    #[must_use]
    pub fn stored_size(&self) -> u64 {
        crate::format::COLUMNAR_CHUNK_HEADER_SIZE as u64 + self.compressed_size
    }
}

/// Decoded fields of a chunk header.
///
/// The chunk header checksum covers the header prefix *and* the stored payload,
/// so it can only be verified once the payload has been read. [`Self`] therefore
/// carries the framing fields alone; the checksum is validated by the reader
/// when it loads the payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkHeaderFields {
    /// Value encoding of the stored payload.
    pub encoding: ColumnEncoding,
    /// Compression encoding of the stored payload.
    pub compression: ChunkCompression,
    /// Ordinal of the first row covered by this chunk.
    pub first_row: u64,
    /// Number of rows covered by this chunk.
    pub row_count: u64,
    /// Absolute offset of the stored payload.
    pub payload_offset: u64,
    /// Stored (compressed) payload size in bytes.
    pub compressed_size: u64,
    /// Decoded payload size in bytes.
    pub uncompressed_size: u64,
    /// CRC32C checksum of the header prefix and stored payload.
    pub checksum: u32,
}

impl ColumnChunk {
    /// Encoded size of a chunk header in bytes.
    pub const HEADER_LEN: usize = crate::format::COLUMNAR_CHUNK_HEADER_SIZE;

    /// Encodes this chunk's framing into its fixed-size chunk header.
    ///
    /// The checksum covers the header prefix and the stored payload, so it is
    /// passed in rather than recomputed here.
    #[must_use]
    pub fn encode_header(&self) -> [u8; Self::HEADER_LEN] {
        use crate::layout::{put_u32, put_u64, put_u8};
        let mut header = [0_u8; Self::HEADER_LEN];
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
            self.compression.tag(),
        );
        put_u8(
            &mut header,
            format::CHUNK_OFF_ENCODING,
            self.encoding.tag().unwrap_or(format::ENCODING_RAW_TAG),
        );
        put_u64(&mut header, format::CHUNK_OFF_FIRST_ROW, self.first_row);
        put_u64(&mut header, format::CHUNK_OFF_ROW_COUNT, self.row_count);
        put_u64(&mut header, format::CHUNK_OFF_PAYLOAD, self.file_offset);
        put_u64(
            &mut header,
            format::CHUNK_OFF_COMPRESSED_SIZE,
            self.compressed_size,
        );
        put_u64(
            &mut header,
            format::CHUNK_OFF_UNCOMPRESSED_SIZE,
            self.uncompressed_size,
        );
        put_u32(&mut header, format::CHUNK_OFF_CHECKSUM, self.checksum);
        header
    }

    /// Returns a copy of this chunk framing with a recomputed checksum.
    #[must_use]
    pub fn with_checksum(mut self, checksum: u32) -> Self {
        self.checksum = checksum;
        self
    }

    /// Decodes and validates a chunk header located at `header_offset`.
    ///
    /// Returns the framing fields together with the absolute offset of the next
    /// chunk header, which is the first byte after this chunk's payload.
    pub fn decode_header(bytes: &[u8], header_offset: u64) -> Result<(ChunkHeaderFields, u64)> {
        use crate::layout::{corruption, get_slice, get_u32, get_u64, get_u8};
        let header = get_slice(
            bytes,
            usize::try_from(header_offset)
                .map_err(|_| corruption("chunk header offset overflows"))?,
            Self::HEADER_LEN,
            "chunk header",
        )?;
        if header[format::CHUNK_OFF_MAGIC..format::CHUNK_OFF_VERSION]
            != format::COLUMNAR_CHUNK_MAGIC
        {
            return Err(corruption("chunk header magic does not match"));
        }
        let version = get_u32(header, format::CHUNK_OFF_VERSION, "chunk format version")?;
        if version != format::COLUMNAR_CHUNK_FORMAT_VERSION {
            return Err(PlomidError::new(
                ErrorKind::Unsupported,
                format!("unsupported column chunk format version {version}"),
            ));
        }
        let tag = get_u8(header, format::CHUNK_OFF_COMPRESSION, "chunk compression")?;
        let compression = ChunkCompression::from_tag(tag)
            .ok_or_else(|| corruption(format!("unknown chunk compression tag {tag}")))?;
        let encoding_tag = get_u8(header, format::CHUNK_OFF_ENCODING, "chunk encoding")?;
        let encoding = ColumnEncoding::from_tag(encoding_tag)
            .ok_or_else(|| corruption(format!("unknown chunk encoding tag {encoding_tag}")))?;
        let payload_offset = get_u64(header, format::CHUNK_OFF_PAYLOAD, "chunk payload offset")?;
        let expected_payload = header_offset + Self::HEADER_LEN as u64;
        if payload_offset != expected_payload {
            return Err(corruption(format!(
                "chunk payload offset {payload_offset} does not follow its header at {expected_payload}"
            )));
        }
        let compressed_size = get_u64(
            header,
            format::CHUNK_OFF_COMPRESSED_SIZE,
            "chunk compressed size",
        )?;
        let fields = ChunkHeaderFields {
            compression,
            encoding,
            first_row: get_u64(header, format::CHUNK_OFF_FIRST_ROW, "chunk first row")?,
            row_count: get_u64(header, format::CHUNK_OFF_ROW_COUNT, "chunk row count")?,
            payload_offset,
            compressed_size,
            uncompressed_size: get_u64(
                header,
                format::CHUNK_OFF_UNCOMPRESSED_SIZE,
                "chunk uncompressed size",
            )?,
            checksum: get_u32(header, format::CHUNK_OFF_CHECKSUM, "chunk checksum")?,
        };
        let next = payload_offset
            .checked_add(compressed_size)
            .ok_or_else(|| corruption("chunk payload end overflows"))?;
        Ok((fields, next))
    }
}

/// Returns the CRC32C checksum framing a chunk header and its stored payload.
///
/// `checksummed_prefix` is the header prefix the caller passes: every header
/// byte that precedes the checksum field. The checksum therefore covers the
/// framing fields followed by the stored payload, and excludes the checksum
/// field itself, so a chunk can be verified without trusting the value under
/// verification. The segment header and the column metadata records use the same
/// convention.
#[must_use]
pub fn chunk_checksum(checksummed_prefix: &[u8], payload: &[u8]) -> u32 {
    crate::layout::checksum_of_pair(checksummed_prefix, payload)
}

/// A chunk payload that has been read back into memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkData {
    /// Metadata of the chunk this payload belongs to.
    pub chunk: ColumnChunk,
    /// Decoded (uncompressed) payload bytes.
    pub data: Vec<u8>,
    /// Whether the payload checksum has been verified.
    pub verified: bool,
}

impl ChunkData {
    /// Wraps decoded payload bytes with their chunk metadata.
    #[must_use]
    pub fn new(chunk: ColumnChunk, data: Vec<u8>) -> Self {
        Self {
            chunk,
            data,
            verified: false,
        }
    }

    /// Returns the decoded payload.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Returns the decoded payload mutably.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Consumes the payload and returns its bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    /// Records that the payload checksum matched.
    pub fn mark_verified(&mut self) {
        self.verified = true;
    }
}
