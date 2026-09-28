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
//! Production compression for immutable columnar chunks.
//!
//! Compression happens at exactly one place in the pipeline — after a chunk's
//! values are materialized and before the segment is verified, synced, and
//! published:
//!
//! ```text
//! materialized values -> value encoding -> codec -> frame-validated payload
//! ```
//!
//! # Two layers
//!
//! * A **value encoding** rewrites the `[length][value]` entry stream into a more
//!   compact shape that still reproduces the original bytes exactly. PLOMID
//!   ships four, chosen by measurement against its own workloads rather than for
//!   breadth: [`ColumnEncoding::Raw`] (the version-1 layout, kept as the
//!   baseline), [`ColumnEncoding::DeltaBitpack`] for integer and time-series
//!   columns, [`ColumnEncoding::Dictionary`] for low-cardinality columns, and
//!   [`ColumnEncoding::Rle`] for columns with long runs.
//! * A **codec** is the existing byte-oriented [`ChunkCompression`] — `None` or
//!   the feature-gated ZSTD frame — applied to the encoded bytes. No second
//!   byte-level compressor is introduced.
//!
//! [`ColumnEncoding::Auto`] measures every applicable encoding on the chunk in
//! hand and keeps the smallest stored payload, so a chunk never grows because it
//! was encoded. Ties keep the earlier candidate, and `Raw` is always first, so
//! data that does not benefit keeps the legacy layout byte for byte.
//!
//! # Self-describing framing
//!
//! A chunk that uses a value encoding stores `codec(frame_header || body)`. The
//! frame records everything a reader needs to validate the payload's boundaries
//! without trusting the chunk header that points at it: encoding and codec
//! identities, both versions, the value count, the raw value-stream length, and
//! the body length. The frame lives inside the chunk payload, so the existing
//! CRC32C chunk checksum covers it; it is checked before any body byte is
//! interpreted, and the decoded value stream is then re-validated entry by entry
//! against the frame's declared length.
//!
//! `Raw` chunks carry no frame: their payload is the value stream itself, which
//! is exactly the layout version-1 segments already use, so any segment written
//! before this module existed still decodes.
//!
//! # Safety
//!
//! Every decode allocation is sized from bytes that were already bounds-checked,
//! and every declared length must agree with the bytes actually present. The
//! decoder rejects an unsupported codec or encoding, an unsupported frame or
//! encoding version, non-zero reserved bytes, impossible or inconsistent
//! lengths, truncation, trailing bytes, out-of-range dictionary indices, and
//! corruption, and it never allocates more than the format's declared chunk
//! bounds regardless of what a payload claims.

pub(crate) mod bitpack;
pub(crate) mod delta;
pub(crate) mod dictionary;
pub(crate) mod entries;
pub(crate) mod rle;

use crate::chunk::ChunkCompression;
use crate::compression;
use crate::format;
use crate::layout::{corruption, get_slice, get_u64, get_u8, invalid, put_u64, put_u8, to_usize};
use plomid_core::Result;
use plomid_core::{
    CODEC_VERSION_NONE, CODEC_VERSION_ZSTD, COLUMNAR_ENCODING_FRAME_SIZE,
    COLUMNAR_ENCODING_FRAME_VERSION, COLUMNAR_ENCODING_MAGIC, COLUMNAR_MAX_CHUNK_RAW_SIZE,
    COLUMNAR_MAX_CHUNK_UNCOMPRESSED_SIZE, ENCODING_DELTA_BITPACK_TAG, ENCODING_DICTIONARY_TAG,
    ENCODING_RAW_TAG, ENCODING_RLE_TAG, ENCODING_VERSION_DELTA_BITPACK,
    ENCODING_VERSION_DICTIONARY, ENCODING_VERSION_RAW, ENCODING_VERSION_RLE,
};

/// Value encoding applied to a chunk before its codec runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ColumnEncoding {
    /// Choose the smallest applicable encoding for each chunk by measurement.
    Auto,
    /// Store the value stream verbatim, exactly as version-1 segments do.
    Raw,
    /// Run-length encode the value stream.
    Rle,
    /// Delta-encode and bit-pack fixed-width 64-bit integer values.
    DeltaBitpack,
    /// Store each distinct value once and bit-pack per-row indices.
    Dictionary,
}

impl ColumnEncoding {
    /// Encodings [`ColumnEncoding::Auto`] tries, in deterministic preference
    /// order. Earlier candidates win ties, so `Raw` wins when nothing shrinks.
    pub const CANDIDATES: [ColumnEncoding; 4] = [
        ColumnEncoding::Raw,
        ColumnEncoding::DeltaBitpack,
        ColumnEncoding::Dictionary,
        ColumnEncoding::Rle,
    ];

    /// Returns the persisted tag for a concrete encoding, or `None` for
    /// [`ColumnEncoding::Auto`], which is a writer policy and never persisted.
    #[must_use]
    pub fn tag(self) -> Option<u8> {
        match self {
            ColumnEncoding::Auto => None,
            ColumnEncoding::Raw => Some(ENCODING_RAW_TAG),
            ColumnEncoding::Rle => Some(ENCODING_RLE_TAG),
            ColumnEncoding::DeltaBitpack => Some(ENCODING_DELTA_BITPACK_TAG),
            ColumnEncoding::Dictionary => Some(ENCODING_DICTIONARY_TAG),
        }
    }

    /// Decodes a persisted encoding tag.
    #[must_use]
    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            ENCODING_RAW_TAG => Some(ColumnEncoding::Raw),
            ENCODING_RLE_TAG => Some(ColumnEncoding::Rle),
            ENCODING_DELTA_BITPACK_TAG => Some(ColumnEncoding::DeltaBitpack),
            ENCODING_DICTIONARY_TAG => Some(ColumnEncoding::Dictionary),
            _ => None,
        }
    }

    /// Returns the body version this encoding is written in.
    #[must_use]
    pub fn version(self) -> Option<u8> {
        match self {
            ColumnEncoding::Auto => None,
            ColumnEncoding::Raw => Some(ENCODING_VERSION_RAW),
            ColumnEncoding::Rle => Some(ENCODING_VERSION_RLE),
            ColumnEncoding::DeltaBitpack => Some(ENCODING_VERSION_DELTA_BITPACK),
            ColumnEncoding::Dictionary => Some(ENCODING_VERSION_DICTIONARY),
        }
    }

    /// Returns true when the encoding stores a framed body.
    #[must_use]
    pub fn is_framed(self) -> bool {
        !matches!(self, ColumnEncoding::Auto | ColumnEncoding::Raw)
    }

    /// Encodes one value stream, or `None` when the encoding cannot represent it.
    fn encode_body(self, raw: &[u8], value_count: u64) -> Result<Option<Vec<u8>>> {
        match self {
            ColumnEncoding::Auto | ColumnEncoding::Raw => Ok(None),
            ColumnEncoding::Rle => rle::encode(raw, value_count),
            ColumnEncoding::DeltaBitpack => delta::encode(raw, value_count),
            ColumnEncoding::Dictionary => dictionary::encode(raw, value_count),
        }
    }

    /// Decodes one framed body back into the value stream.
    fn decode_body(self, body: &[u8], value_count: u64, raw_len: u64) -> Result<Vec<u8>> {
        match self {
            ColumnEncoding::Auto | ColumnEncoding::Raw => Err(corruption(format!(
                "{self:?} chunks do not carry an encoded body"
            ))),
            ColumnEncoding::Rle => rle::decode(body, value_count, raw_len),
            ColumnEncoding::DeltaBitpack => delta::decode(body, value_count, raw_len),
            ColumnEncoding::Dictionary => dictionary::decode(body, value_count, raw_len),
        }
    }
}

/// Returns the byte-level frame version of a codec.
#[must_use]
pub fn codec_version(codec: ChunkCompression) -> u8 {
    match codec {
        ChunkCompression::None => CODEC_VERSION_NONE,
        ChunkCompression::Zstd => CODEC_VERSION_ZSTD,
    }
}

/// The self-describing header that every encoded chunk payload begins with.
///
/// The frame is stored inside the chunk payload, ahead of the encoded body, so
/// the chunk's CRC32C covers it and a reader can validate the payload's
/// boundaries before interpreting a single body byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncodingFrame {
    /// Version of the frame layout itself.
    pub frame_version: u8,
    /// Value encoding the body was written in.
    pub encoding: ColumnEncoding,
    /// Version of that encoding's body layout.
    pub encoding_version: u8,
    /// Byte codec that was applied to `frame || body`.
    pub codec: ChunkCompression,
    /// Version of that codec's byte-level frame.
    pub codec_version: u8,
    /// Number of logical values (rows) the body reproduces.
    pub value_count: u64,
    /// Length in bytes of the value stream the body expands to.
    pub raw_len: u64,
    /// Length in bytes of the encoded body that follows this frame.
    pub encoded_len: u64,
}

impl EncodingFrame {
    /// Encoded size of a frame in bytes.
    pub const ENCODED_LEN: usize = COLUMNAR_ENCODING_FRAME_SIZE;

    /// Encodes the frame into its fixed-size little-endian image.
    #[must_use]
    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut bytes = [0_u8; Self::ENCODED_LEN];
        bytes[plomid_core::ENCODING_FRAME_OFF_MAGIC..plomid_core::ENCODING_FRAME_OFF_FRAME_VERSION]
            .copy_from_slice(&COLUMNAR_ENCODING_MAGIC);
        put_u8(
            &mut bytes,
            plomid_core::ENCODING_FRAME_OFF_FRAME_VERSION,
            self.frame_version,
        );
        put_u8(
            &mut bytes,
            plomid_core::ENCODING_FRAME_OFF_ENCODING,
            self.encoding.tag().unwrap_or(ENCODING_RAW_TAG),
        );
        put_u8(
            &mut bytes,
            plomid_core::ENCODING_FRAME_OFF_ENCODING_VERSION,
            self.encoding_version,
        );
        put_u8(
            &mut bytes,
            plomid_core::ENCODING_FRAME_OFF_CODEC,
            self.codec.tag(),
        );
        put_u8(
            &mut bytes,
            plomid_core::ENCODING_FRAME_OFF_CODEC_VERSION,
            self.codec_version,
        );
        put_u64(
            &mut bytes,
            plomid_core::ENCODING_FRAME_OFF_VALUE_COUNT,
            self.value_count,
        );
        put_u64(
            &mut bytes,
            plomid_core::ENCODING_FRAME_OFF_RAW_LEN,
            self.raw_len,
        );
        put_u64(
            &mut bytes,
            plomid_core::ENCODING_FRAME_OFF_ENCODED_LEN,
            self.encoded_len,
        );
        bytes
    }

    /// Decodes and validates a frame image.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let frame = get_slice(bytes, 0, Self::ENCODED_LEN, "encoding frame")?;
        if frame
            [plomid_core::ENCODING_FRAME_OFF_MAGIC..plomid_core::ENCODING_FRAME_OFF_FRAME_VERSION]
            != COLUMNAR_ENCODING_MAGIC
        {
            return Err(corruption("encoding frame magic does not match"));
        }
        let frame_version = get_u8(
            frame,
            plomid_core::ENCODING_FRAME_OFF_FRAME_VERSION,
            "encoding frame version",
        )?;
        if frame_version != COLUMNAR_ENCODING_FRAME_VERSION {
            return Err(plomid_core::PlomidError::new(
                plomid_core::ErrorKind::Unsupported,
                format!("unsupported encoding frame version {frame_version}"),
            ));
        }
        let encoding_tag = get_u8(
            frame,
            plomid_core::ENCODING_FRAME_OFF_ENCODING,
            "encoding tag",
        )?;
        let encoding = ColumnEncoding::from_tag(encoding_tag)
            .ok_or_else(|| corruption(format!("unknown value encoding tag {encoding_tag}")))?;
        let codec_tag = get_u8(frame, plomid_core::ENCODING_FRAME_OFF_CODEC, "codec tag")?;
        let codec = ChunkCompression::from_tag(codec_tag)
            .ok_or_else(|| corruption(format!("unknown codec tag {codec_tag}")))?;
        for offset in [
            plomid_core::ENCODING_FRAME_OFF_RESERVED0,
            plomid_core::ENCODING_FRAME_OFF_RESERVED1,
            plomid_core::ENCODING_FRAME_OFF_RESERVED2,
        ] {
            if get_u8(frame, offset, "reserved byte")? != 0 {
                return Err(corruption("encoding frame reserved byte is not zero"));
            }
        }
        Ok(Self {
            frame_version,
            encoding,
            encoding_version: get_u8(
                frame,
                plomid_core::ENCODING_FRAME_OFF_ENCODING_VERSION,
                "encoding version",
            )?,
            codec,
            codec_version: get_u8(
                frame,
                plomid_core::ENCODING_FRAME_OFF_CODEC_VERSION,
                "codec version",
            )?,
            value_count: get_u64(
                frame,
                plomid_core::ENCODING_FRAME_OFF_VALUE_COUNT,
                "value count",
            )?,
            raw_len: get_u64(frame, plomid_core::ENCODING_FRAME_OFF_RAW_LEN, "raw length")?,
            encoded_len: get_u64(
                frame,
                plomid_core::ENCODING_FRAME_OFF_ENCODED_LEN,
                "encoded length",
            )?,
        })
    }

    /// Checks this frame against the framing that referenced it.
    fn validate_against(
        &self,
        encoding: ColumnEncoding,
        codec: ChunkCompression,
        value_count: u64,
        declared_uncompressed: u64,
    ) -> Result<()> {
        if self.encoding != encoding {
            return Err(corruption(format!(
                "encoding frame declares {:?} but the chunk header declares {encoding:?}",
                self.encoding
            )));
        }
        if self.codec != codec {
            return Err(corruption(format!(
                "encoding frame declares codec {:?} but the chunk header declares {codec:?}",
                self.codec
            )));
        }
        if self.encoding_version != encoding.version().unwrap_or(self.encoding_version) {
            return Err(plomid_core::PlomidError::new(
                plomid_core::ErrorKind::Unsupported,
                format!(
                    "unsupported {:?} body version {}",
                    self.encoding, self.encoding_version
                ),
            ));
        }
        if self.codec_version != codec_version(codec) {
            return Err(plomid_core::PlomidError::new(
                plomid_core::ErrorKind::Unsupported,
                format!("unsupported {codec:?} codec version {}", self.codec_version),
            ));
        }
        if self.value_count != value_count {
            return Err(corruption(format!(
                "encoding frame holds {} values but the chunk header declares {value_count}",
                self.value_count
            )));
        }
        let body_len = declared_uncompressed
            .checked_sub(Self::ENCODED_LEN as u64)
            .ok_or_else(|| corruption("chunk is smaller than its encoding frame"))?;
        if self.encoded_len != body_len {
            return Err(corruption(format!(
                "encoding frame declares {} body bytes but {body_len} are framed",
                self.encoded_len
            )));
        }
        if self.raw_len > COLUMNAR_MAX_CHUNK_RAW_SIZE {
            return Err(corruption(format!(
                "encoding frame declares {} value-stream bytes, beyond this format's limit",
                self.raw_len
            )));
        }
        // Every framed encoding reproduces at least a four-byte length prefix per
        // value, so a value count above this bound is impossible for a well-formed
        // body. Rejecting it keeps every later allocation bounded by `raw_len`.
        let minimum = u64::try_from(format::COLUMNAR_VALUE_LENGTH_PREFIX)
            .map_err(|_| corruption("value length prefix does not fit in u64"))?
            .checked_mul(value_count)
            .ok_or_else(|| corruption("declared value count overflows"))?;
        if self.raw_len < minimum {
            return Err(corruption(format!(
                "encoding frame declares {} value-stream bytes for {value_count} values",
                self.raw_len
            )));
        }
        Ok(())
    }
}

/// One chunk that has been value-encoded and passed through a codec.
#[derive(Clone, Debug)]
pub struct EncodedChunk {
    /// Encoding the stored payload uses.
    pub encoding: ColumnEncoding,
    /// Version of that encoding's body layout.
    pub encoding_version: u8,
    /// Codec applied to the encoded bytes.
    pub codec: ChunkCompression,
    /// Version of the codec's byte-level frame.
    pub codec_version: u8,
    /// Number of logical values (rows) the payload holds.
    pub value_count: u64,
    /// Length in bytes of the value stream the payload reproduces.
    pub raw_len: u64,
    /// Length in bytes of what the codec was given.
    pub uncompressed_len: u64,
    /// Length in bytes of the encoded body, `0` for [`ColumnEncoding::Raw`].
    pub encoded_len: u64,
    /// Bytes to persist for this chunk.
    pub payload: Vec<u8>,
}

impl EncodedChunk {
    /// Returns true when the payload carries an encoding frame.
    #[must_use]
    pub fn is_framed(&self) -> bool {
        self.encoding.is_framed()
    }

    /// Returns the number of bytes this chunk stores.
    #[must_use]
    pub fn stored_len(&self) -> usize {
        self.payload.len()
    }
}

/// Value-encodes and codecs one chunk's value stream.
///
/// `raw` is the chunk's value stream (`[length][value]` entries in row order)
/// and `value_count` is the exact number of entries it must hold. The returned
/// payload is the exact byte range to persist in the chunk table.
///
/// Encoding choice is per chunk and never grows it:
///
/// * [`ColumnEncoding::Raw`] stores the value stream verbatim;
/// * [`ColumnEncoding::Auto`] measures every applicable encoding and keeps the
///   smallest payload;
/// * a specific encoding prefers itself, and falls back to
///   [`ColumnEncoding::Raw`] when it does not apply to the chunk (a
///   `DeltaBitpack` setting over a string column, for example) or when applying
///   it would store more bytes than the raw stream would.
///
/// Ties keep the earlier candidate, and `Raw` is always last resort, so a column
/// whose values do not fit the configured encoding is still storable rather than
/// failing the flush.
pub fn encode_chunk(
    raw: &[u8],
    value_count: u64,
    encoding: ColumnEncoding,
    codec: ChunkCompression,
) -> Result<EncodedChunk> {
    entries::validate_stream(raw, value_count)?;
    if !compression::is_compression_available(codec) {
        return Err(invalid(
            "the requested chunk codec is not available in this build",
        ));
    }

    let preferred = [encoding, ColumnEncoding::Raw];
    let candidates: &[ColumnEncoding] = match encoding {
        ColumnEncoding::Auto => &ColumnEncoding::CANDIDATES,
        ColumnEncoding::Raw => &preferred[..1],
        _ => &preferred,
    };

    let mut best: Option<EncodedChunk> = None;
    for candidate in candidates {
        let Some(encoded) = try_candidate(raw, value_count, *candidate, codec)? else {
            continue;
        };
        // Strictly smaller wins, so the first candidate of equal size is kept
        // and `Raw` — always first for `Auto`, always last for a specific
        // encoding — is the encoding of record whenever nothing wins.
        if best
            .as_ref()
            .is_none_or(|current| encoded.payload.len() < current.payload.len())
        {
            best = Some(encoded);
        }
    }
    best.ok_or_else(|| {
        invalid(format!(
            "the requested {encoding:?} encoding cannot represent this chunk"
        ))
    })
}

/// Produces one candidate encoding, or `None` when it does not apply.
fn try_candidate(
    raw: &[u8],
    value_count: u64,
    encoding: ColumnEncoding,
    codec: ChunkCompression,
) -> Result<Option<EncodedChunk>> {
    let raw_len = u64::try_from(raw.len())
        .map_err(|_| corruption("chunk value stream length exceeds u64"))?;
    if encoding == ColumnEncoding::Raw {
        check_uncompressed_bound(raw_len)?;
        let payload = compression::compress(raw, codec)?;
        return Ok(Some(EncodedChunk {
            encoding,
            encoding_version: encoding.version().unwrap_or(ENCODING_VERSION_RAW),
            codec,
            codec_version: codec_version(codec),
            value_count,
            raw_len,
            uncompressed_len: raw_len,
            encoded_len: 0,
            payload,
        }));
    }

    let Some(body) = encoding.encode_body(raw, value_count)? else {
        return Ok(None);
    };
    let encoded_len =
        u64::try_from(body.len()).map_err(|_| corruption("encoded body length exceeds u64"))?;
    let frame = EncodingFrame {
        frame_version: COLUMNAR_ENCODING_FRAME_VERSION,
        encoding,
        encoding_version: encoding
            .version()
            .ok_or_else(|| invalid("only concrete encodings produce framed chunks"))?,
        codec,
        codec_version: codec_version(codec),
        value_count,
        raw_len,
        encoded_len,
    };
    let mut uncompressed = Vec::with_capacity(EncodingFrame::ENCODED_LEN + body.len());
    uncompressed.extend_from_slice(&frame.encode());
    uncompressed.extend_from_slice(&body);
    let uncompressed_len = u64::try_from(uncompressed.len())
        .map_err(|_| corruption("framed payload length exceeds u64"))?;
    check_uncompressed_bound(uncompressed_len)?;
    let payload = compression::compress(&uncompressed, codec)?;
    Ok(Some(EncodedChunk {
        encoding,
        encoding_version: frame.encoding_version,
        codec,
        codec_version: frame.codec_version,
        value_count,
        raw_len,
        uncompressed_len,
        encoded_len,
        payload,
    }))
}

/// Rejects a payload that would exceed the format's per-chunk bound.
fn check_uncompressed_bound(length: u64) -> Result<()> {
    if length > COLUMNAR_MAX_CHUNK_UNCOMPRESSED_SIZE {
        return Err(invalid(format!(
            "chunk payload of {length} bytes exceeds the {COLUMNAR_MAX_CHUNK_UNCOMPRESSED_SIZE}-byte chunk limit"
        )));
    }
    Ok(())
}

/// Decodes a persisted chunk payload back into its value stream.
///
/// `declared_uncompressed` and `value_count` come from the chunk header that
/// referenced the payload. Every bound is enforced before it is used: the
/// declared length is limited by the format, the codec is asked to produce at
/// most that many bytes, the resulting length must match the declaration
/// exactly, the frame (when present) must agree with the header on encoding,
/// codec, both versions, value count, and body length, and the decoded value
/// stream must then validate entry by entry against the frame's declared length.
pub fn decode_chunk(
    payload: &[u8],
    encoding: ColumnEncoding,
    codec: ChunkCompression,
    declared_uncompressed: u64,
    value_count: u64,
) -> Result<Vec<u8>> {
    if declared_uncompressed > COLUMNAR_MAX_CHUNK_UNCOMPRESSED_SIZE {
        return Err(corruption(format!(
            "chunk declares {declared_uncompressed} bytes, beyond this format's limit"
        )));
    }
    let declared = to_usize(declared_uncompressed, "chunk uncompressed size")?;
    let bytes = compression::decompress(payload, declared_uncompressed, codec)?;
    if bytes.len() != declared {
        return Err(corruption(format!(
            "chunk decompressed to {} bytes but declares {declared}",
            bytes.len()
        )));
    }

    if encoding == ColumnEncoding::Raw {
        entries::validate_stream(&bytes, value_count)?;
        return Ok(bytes);
    }
    if !encoding.is_framed() {
        return Err(invalid(
            "an automatic encoding cannot be decoded: a chunk must persist a concrete encoding",
        ));
    }

    let frame = EncodingFrame::decode(&bytes)?;
    frame.validate_against(encoding, codec, value_count, declared_uncompressed)?;
    let body = bytes
        .get(EncodingFrame::ENCODED_LEN..)
        .ok_or_else(|| corruption("chunk is smaller than its encoding frame"))?;
    let raw = encoding.decode_body(body, value_count, frame.raw_len)?;
    if raw.len() as u64 != frame.raw_len {
        return Err(corruption(format!(
            "encoding body expanded to {} bytes but the frame declares {}",
            raw.len(),
            frame.raw_len
        )));
    }
    entries::validate_stream(&raw, value_count)?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::entries::append_entry;
    use plomid_core::ErrorKind;

    fn stream(values: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            append_entry(&mut out, value).expect("append");
        }
        out
    }

    fn int_stream(values: &[i64]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            append_entry(&mut out, &value.to_le_bytes()).expect("append");
        }
        out
    }

    fn bytes_stream(values: &[Vec<u8>]) -> Vec<u8> {
        stream(&values.iter().map(Vec::as_slice).collect::<Vec<&[u8]>>())
    }

    /// Builds an unframed payload whose frame carries caller-chosen fields.
    fn craft(encoding: ColumnEncoding, value_count: u64, raw_len: u64, body: &[u8]) -> Vec<u8> {
        let frame = EncodingFrame {
            frame_version: COLUMNAR_ENCODING_FRAME_VERSION,
            encoding,
            encoding_version: encoding.version().expect("concrete encoding"),
            codec: ChunkCompression::None,
            codec_version: codec_version(ChunkCompression::None),
            value_count,
            raw_len,
            encoded_len: body.len() as u64,
        };
        let mut out = Vec::new();
        out.extend_from_slice(&frame.encode());
        out.extend_from_slice(body);
        out
    }

    /// Datasets exercising the shapes the encodings target, with their row counts.
    fn datasets() -> Vec<(&'static str, Vec<u8>, u64)> {
        let low_cardinality: Vec<&[u8]> = (0..512)
            .map(|i| {
                if i % 3 == 0 {
                    &b"alpha"[..]
                } else {
                    &b"beta"[..]
                }
            })
            .collect();
        let long_strings = bytes_stream(&vec![vec![b'q'; 256]; 64]);
        let random_bytes = bytes_stream(
            &(0..256u32)
                .map(|seed| {
                    let mut state = seed.wrapping_mul(2_654_435_761);
                    let mut bytes = Vec::with_capacity(16);
                    for _ in 0..16 {
                        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        bytes.push((state >> 24) as u8);
                    }
                    bytes
                })
                .collect::<Vec<Vec<u8>>>(),
        );
        vec![
            ("empty", stream(&[]), 0),
            ("one", stream(&[b"x"]), 1),
            (
                "monotonic-integers",
                int_stream(&(0..512).collect::<Vec<i64>>()),
                512,
            ),
            ("constant-integers", int_stream(&vec![9_i64; 512]), 512),
            ("low-cardinality-strings", stream(&low_cardinality), 512),
            ("long-strings", long_strings, 64),
            ("random-bytes", random_bytes, 256),
        ]
    }

    #[test]
    fn auto_never_grows_a_chunk_and_round_trips_exactly() {
        for (name, raw, count) in datasets() {
            let encoded = encode_chunk(&raw, count, ColumnEncoding::Auto, ChunkCompression::None)
                .expect("encode");
            assert!(
                encoded.payload.len() <= raw.len(),
                "{name} grew from {} to {} bytes",
                raw.len(),
                encoded.payload.len()
            );
            let back = decode_chunk(
                &encoded.payload,
                encoded.encoding,
                encoded.codec,
                encoded.uncompressed_len,
                count,
            )
            .expect("decode");
            assert_eq!(back, raw, "{name} did not round trip");
        }
    }

    #[test]
    fn auto_is_deterministic() {
        for (name, raw, count) in datasets() {
            let first = encode_chunk(&raw, count, ColumnEncoding::Auto, ChunkCompression::None)
                .expect("encode");
            let second = encode_chunk(&raw, count, ColumnEncoding::Auto, ChunkCompression::None)
                .expect("encode");
            assert_eq!(first.encoding, second.encoding, "{name}");
            assert_eq!(first.payload, second.payload, "{name}");
        }
    }

    #[test]
    fn every_encoding_round_trips_when_it_applies() {
        let integers = int_stream(&(0..300).collect::<Vec<i64>>());
        for encoding in [
            ColumnEncoding::Raw,
            ColumnEncoding::Rle,
            ColumnEncoding::DeltaBitpack,
            ColumnEncoding::Dictionary,
            ColumnEncoding::Auto,
        ] {
            let encoded =
                encode_chunk(&integers, 300, encoding, ChunkCompression::None).expect("encode");
            let back = decode_chunk(
                &encoded.payload,
                encoded.encoding,
                encoded.codec,
                encoded.uncompressed_len,
                encoded.value_count,
            )
            .expect("decode");
            assert_eq!(back, integers, "{encoding:?}");
        }
    }

    #[test]
    fn explicit_encodings_fall_back_to_raw_when_they_do_not_apply() {
        let strings = stream(&[b"a", b"b"]);
        let encoded = encode_chunk(
            &strings,
            2,
            ColumnEncoding::DeltaBitpack,
            ChunkCompression::None,
        )
        .expect("encode");
        assert_eq!(encoded.encoding, ColumnEncoding::Raw);
        assert_eq!(encoded.payload, strings);
        // The requested encoding is used where it does apply.
        let integers = int_stream(&(0..300).collect::<Vec<i64>>());
        let encoded = encode_chunk(
            &integers,
            300,
            ColumnEncoding::DeltaBitpack,
            ChunkCompression::None,
        )
        .expect("encode");
        assert_eq!(encoded.encoding, ColumnEncoding::DeltaBitpack);
    }

    #[test]
    fn an_unavailable_codec_is_refused() {
        if !crate::compression::is_compression_available(ChunkCompression::Zstd) {
            let error = encode_chunk(
                &int_stream(&[1]),
                1,
                ColumnEncoding::Auto,
                ChunkCompression::Zstd,
            )
            .expect_err("zstd is not built in");
            assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        }
    }

    #[test]
    fn raw_chunks_keep_the_version_one_layout() {
        let raw = int_stream(&[4, 5, 6]);
        let encoded =
            encode_chunk(&raw, 3, ColumnEncoding::Raw, ChunkCompression::None).expect("encode");
        assert_eq!(encoded.encoding, ColumnEncoding::Raw);
        assert!(!encoded.is_framed());
        assert_eq!(encoded.payload, raw);
        assert_eq!(encoded.uncompressed_len, raw.len() as u64);
        assert_eq!(encoded.encoded_len, 0);
        assert_eq!(encoded.raw_len, raw.len() as u64);
    }

    #[test]
    fn framed_chunks_record_their_encoding_codec_and_lengths() {
        let raw = int_stream(&(0..64).collect::<Vec<i64>>());
        let encoded =
            encode_chunk(&raw, 64, ColumnEncoding::Auto, ChunkCompression::None).expect("encode");
        assert!(encoded.is_framed());
        assert_eq!(encoded.encoding, ColumnEncoding::DeltaBitpack);
        let frame = EncodingFrame::decode(&encoded.payload).expect("frame");
        assert_eq!(frame.encoding, ColumnEncoding::DeltaBitpack);
        assert_eq!(frame.encoding_version, ENCODING_VERSION_DELTA_BITPACK);
        assert_eq!(frame.codec, ChunkCompression::None);
        assert_eq!(frame.value_count, 64);
        assert_eq!(frame.raw_len, raw.len() as u64);
        assert_eq!(frame.encoded_len, encoded.encoded_len);
        assert_eq!(
            encoded.uncompressed_len,
            EncodingFrame::ENCODED_LEN as u64 + frame.encoded_len
        );
    }

    /// A valid framed payload for mutation tests. With the `None` codec the
    /// stored payload is the frame followed by the body, so frame offsets are
    /// directly addressable.
    fn framed_sample() -> (Vec<u8>, u64, u64) {
        let raw = int_stream(&(0..32).collect::<Vec<i64>>());
        let encoded = encode_chunk(
            &raw,
            32,
            ColumnEncoding::DeltaBitpack,
            ChunkCompression::None,
        )
        .expect("encode");
        (encoded.payload, 32, encoded.uncompressed_len)
    }

    fn decode_sample(payload: &[u8], declared: u64) -> Result<Vec<u8>> {
        decode_chunk(
            payload,
            ColumnEncoding::DeltaBitpack,
            ChunkCompression::None,
            declared,
            32,
        )
    }

    #[test]
    fn rejects_a_declared_length_that_disagrees_with_the_payload() {
        let (payload, _, declared) = framed_sample();
        assert!(decode_sample(&payload, declared + 1).is_err());
        assert!(decode_sample(&payload, declared - 1).is_err());
        assert!(decode_sample(&payload, 0).is_err());
    }

    #[test]
    fn rejects_truncation_trailing_bytes_and_an_oversized_declaration() {
        let (payload, _, declared) = framed_sample();
        let truncated = &payload[..payload.len() - 1];
        assert!(decode_sample(truncated, declared).is_err());
        let mut padded = payload.clone();
        padded.push(0);
        assert!(decode_sample(&padded, declared).is_err());
        let error = decode_chunk(
            &payload,
            ColumnEncoding::DeltaBitpack,
            ChunkCompression::None,
            plomid_core::COLUMNAR_MAX_CHUNK_UNCOMPRESSED_SIZE + 1,
            32,
        )
        .expect_err("oversized declaration");
        assert_eq!(error.kind(), ErrorKind::Corruption);
    }

    #[test]
    fn rejects_an_unsupported_frame_version() {
        let (mut payload, _, declared) = framed_sample();
        payload[plomid_core::ENCODING_FRAME_OFF_FRAME_VERSION] = 9;
        let error = decode_sample(&payload, declared).expect_err("version");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn rejects_unknown_frame_encoding_and_codec_tags() {
        let (mut payload, _, declared) = framed_sample();
        payload[plomid_core::ENCODING_FRAME_OFF_ENCODING] = 0x7F;
        assert_eq!(
            decode_sample(&payload, declared)
                .expect_err("encoding")
                .kind(),
            ErrorKind::Corruption
        );
        let (mut payload, _, declared) = framed_sample();
        payload[plomid_core::ENCODING_FRAME_OFF_CODEC] = 0x7F;
        assert_eq!(
            decode_sample(&payload, declared).expect_err("codec").kind(),
            ErrorKind::Corruption
        );
    }

    #[test]
    fn rejects_non_zero_reserved_frame_bytes() {
        for offset in [
            plomid_core::ENCODING_FRAME_OFF_RESERVED0,
            plomid_core::ENCODING_FRAME_OFF_RESERVED1,
            plomid_core::ENCODING_FRAME_OFF_RESERVED2,
        ] {
            let (mut payload, _, declared) = framed_sample();
            payload[offset] = 1;
            assert_eq!(
                decode_sample(&payload, declared)
                    .expect_err("reserved")
                    .kind(),
                ErrorKind::Corruption,
                "reserved byte at {offset}"
            );
        }
    }

    #[test]
    fn rejects_a_frame_that_disagrees_with_its_chunk_header() {
        let (payload, _, declared) = framed_sample();
        // The chunk header says RLE while the frame says delta+bitpack.
        let error = decode_chunk(
            &payload,
            ColumnEncoding::Rle,
            ChunkCompression::None,
            declared,
            32,
        )
        .expect_err("encoding mismatch");
        assert_eq!(error.kind(), ErrorKind::Corruption);
        // The chunk header's row count disagrees with the frame's value count.
        let error = decode_chunk(
            &payload,
            ColumnEncoding::DeltaBitpack,
            ChunkCompression::None,
            declared,
            31,
        )
        .expect_err("value count mismatch");
        assert_eq!(error.kind(), ErrorKind::Corruption);
    }

    #[test]
    fn rejects_impossible_frame_lengths_and_encodings() {
        let (payload, _, declared) = framed_sample();
        let body_len_at = plomid_core::ENCODING_FRAME_OFF_ENCODED_LEN;
        let mut wrong_body = payload.clone();
        wrong_body[body_len_at..body_len_at + 8].copy_from_slice(&1_u64.to_le_bytes());
        assert!(decode_sample(&wrong_body, declared).is_err());
    }

    #[test]
    fn rejects_impossible_frame_raw_lengths() {
        let raw = int_stream(&(0..32).collect::<Vec<i64>>());
        let body = crate::encoding::delta::encode(&raw, 32)
            .expect("encode")
            .expect("body");
        let oversized = craft(ColumnEncoding::DeltaBitpack, 32, u64::MAX, &body);
        assert_eq!(
            decode_sample(&oversized, oversized.len() as u64)
                .expect_err("raw length beyond the format limit")
                .kind(),
            ErrorKind::Corruption
        );
        // 32 values need at least 128 raw bytes; four is impossible.
        let impossible = craft(ColumnEncoding::DeltaBitpack, 32, 4, &body);
        assert!(decode_sample(&impossible, impossible.len() as u64).is_err());
    }

    #[test]
    fn rejects_automatic_encoding_in_persisted_framing() {
        let (payload, _, declared) = framed_sample();
        let error = decode_chunk(
            &payload,
            ColumnEncoding::Auto,
            ChunkCompression::None,
            declared,
            32,
        )
        .expect_err("auto is not persistable");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn rejects_a_value_count_that_disagrees_with_the_value_stream() {
        let raw = int_stream(&[1, 2, 3]);
        // Three entries encoded as two values: the stream itself is well formed,
        // so only the count check can catch the disagreement.
        let error = encode_chunk(&raw, 2, ColumnEncoding::Auto, ChunkCompression::None)
            .expect_err("stream and count disagree");
        assert_eq!(error.kind(), ErrorKind::Corruption);
    }
}
