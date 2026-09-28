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
//! WAL record framing and checksum validation.
//!
//! One record layout, all integers little-endian:
//!
//! ```text
//! magic[4] = PLW2 | version[u16] | record_type[u16]
//! lsn[u64] | tx_id[u64] | object_id[u64] | payload_len[u32]
//! payload[payload_len] | crc32c[u32]
//! ```
//!
//! The checksum is CRC32C over the 36-byte header plus payload, excluding the
//! trailing checksum field. It uses the shared `crc32c` component
//! (Castagnoli, initial 0xFFFFFFFF, final XOR 0xFFFFFFFF), computed over the
//! serialized bytes without extra copies beyond the encoded frame.
//!
//! Lengths use checked arithmetic and are validated before any allocation.

use plomid_core::{ErrorKind, Lsn, ObjectId, PlomidError, Result, TxId};
use plomid_storage::{compute_checksum, verify_checksum};

/// WAL record-format constants are defined once in `plomid_core::constants`
/// and re-exported here so existing `plomid_wal::` paths keep working.
pub use plomid_core::{
    CHECKSUM_SIZE, MAX_PAYLOAD_SIZE, WAL_HEADER_SIZE, WAL_MAGIC_V2, WAL_RECORD_VERSION,
};

/// WAL operation categories. The persisted value is the explicit `u16` code;
/// the in-memory enum representation is never serialized directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum RecordType {
    /// Transaction or storage operation data.
    Data = 1,
    /// Transaction begin marker.
    Begin = 2,
    /// Transaction commit marker.
    Commit = 3,
    /// Transaction abort marker.
    Abort = 4,
    /// Recovery checkpoint marker.
    Checkpoint = 5,
}

impl RecordType {
    /// Returns the stable persisted code for this record type.
    #[must_use]
    pub const fn code(self) -> u16 {
        self as u16
    }

    pub(crate) fn from_byte(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Data),
            2 => Ok(Self::Begin),
            3 => Ok(Self::Commit),
            4 => Ok(Self::Abort),
            5 => Ok(Self::Checkpoint),
            _ => Err(corruption("unknown WAL record type")),
        }
    }

    pub(crate) fn from_code(value: u16) -> Result<Self> {
        u8::try_from(value)
            .map_err(|_| corruption("unknown WAL record type"))
            .and_then(Self::from_byte)
    }
}

/// One decoded WAL record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    /// Monotonically increasing log sequence number.
    pub lsn: Lsn,
    /// Operation category.
    pub record_type: RecordType,
    /// Owning transaction; zero when appended via the payload-only API.
    pub tx_id: TxId,
    /// Affected object; zero when the writer did not attach one.
    pub object_id: ObjectId,
    /// Opaque operation payload.
    pub payload: Vec<u8>,
}

impl Record {
    /// Creates a record without assigning its LSN.
    #[must_use]
    pub fn new(record_type: RecordType, payload: Vec<u8>) -> Self {
        Self {
            lsn: Lsn::new(0),
            record_type,
            tx_id: TxId::new(0),
            object_id: ObjectId::new(0),
            payload,
        }
    }

    /// Creates a record carrying explicit transaction and object identity.
    #[must_use]
    pub fn with_ids(
        record_type: RecordType,
        tx_id: TxId,
        object_id: ObjectId,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            lsn: Lsn::new(0),
            record_type,
            tx_id,
            object_id,
            payload,
        }
    }
}

pub(crate) fn encode(record: &Record) -> Result<Vec<u8>> {
    let total_len = frame_len_for_payload(record.payload.len())?;
    let payload_len = u32::try_from(record.payload.len())
        .map_err(|_| PlomidError::new(ErrorKind::InvalidArgument, "WAL payload length overflow"))?;
    let mut bytes = Vec::with_capacity(total_len);
    encode_into(record, payload_len, &mut bytes);
    debug_assert_eq!(bytes.len(), total_len);
    Ok(bytes)
}

/// Serializes `record` into `out` without allocating a frame.
///
/// The append hot path reuses one caller-owned buffer across a batch, so a
/// 12k-record WAL replay seed pays one allocation instead of one allocation,
/// one length check, and one checksum pass per record. The on-disk bytes are
/// identical to [`encode`]: callers must clear the buffer before each frame.
pub(crate) fn encode_into(record: &Record, payload_len: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&WAL_MAGIC_V2);
    out.extend_from_slice(&WAL_RECORD_VERSION.to_le_bytes());
    out.extend_from_slice(&record.record_type.code().to_le_bytes());
    out.extend_from_slice(&record.lsn.get().to_le_bytes());
    out.extend_from_slice(&record.tx_id.get().to_le_bytes());
    out.extend_from_slice(&record.object_id.get().to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(&record.payload);
    let checksum = compute_checksum(out);
    out.extend_from_slice(&checksum.to_le_bytes());
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Record> {
    if bytes.len() < WAL_HEADER_SIZE + CHECKSUM_SIZE {
        return Err(corruption("WAL record is truncated"));
    }
    if bytes[..4] != WAL_MAGIC_V2 {
        return Err(corruption("invalid WAL record magic"));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != WAL_RECORD_VERSION {
        return Err(corruption("unsupported WAL record version"));
    }
    let record_type = RecordType::from_code(u16::from_le_bytes([bytes[6], bytes[7]]))?;
    let payload_len = usize::try_from(u32::from_le_bytes([
        bytes[32], bytes[33], bytes[34], bytes[35],
    ]))
    .map_err(|_| corruption("WAL payload length is invalid"))?;
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(corruption("WAL payload exceeds safety limit"));
    }
    let expected_len = frame_len_for_payload(payload_len)?;
    if bytes.len() != expected_len {
        return Err(corruption("WAL record length does not match frame"));
    }
    let expected_checksum = u32::from_le_bytes([
        bytes[expected_len - 4],
        bytes[expected_len - 3],
        bytes[expected_len - 2],
        bytes[expected_len - 1],
    ]);
    verify_checksum(&bytes[..expected_len - 4], expected_checksum)?;
    let lsn = u64::from_le_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| corruption("invalid WAL LSN"))?,
    );
    if lsn == 0 {
        return Err(corruption("WAL record has an invalid LSN"));
    }
    Ok(Record {
        lsn: Lsn::new(lsn),
        record_type,
        tx_id: TxId::new(u64::from_le_bytes(
            bytes[16..24]
                .try_into()
                .map_err(|_| corruption("invalid WAL transaction ID"))?,
        )),
        object_id: ObjectId::new(u64::from_le_bytes(
            bytes[24..32]
                .try_into()
                .map_err(|_| corruption("invalid WAL object ID"))?,
        )),
        payload: bytes[WAL_HEADER_SIZE..expected_len - CHECKSUM_SIZE].to_vec(),
    })
}

pub(crate) fn frame_len(header: &[u8]) -> Result<usize> {
    if header.len() < 4 {
        return Err(corruption("WAL header is truncated"));
    }
    if header.len() != WAL_HEADER_SIZE {
        return Err(corruption("WAL header is truncated"));
    }
    if header[..4] != WAL_MAGIC_V2 {
        return Err(corruption("invalid WAL record magic"));
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != WAL_RECORD_VERSION {
        return Err(corruption("unsupported WAL record version"));
    }
    RecordType::from_code(u16::from_le_bytes([header[6], header[7]]))?;
    let payload_len = usize::try_from(u32::from_le_bytes([
        header[32], header[33], header[34], header[35],
    ]))
    .map_err(|_| corruption("WAL payload length is invalid"))?;
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(corruption("WAL payload exceeds safety limit"));
    }
    frame_len_for_payload(payload_len)
}

fn corruption(message: &'static str) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}

/// Total framed length of a version 2 record with `payload_len` bytes.
pub fn frame_len_for_payload(payload_len: usize) -> Result<usize> {
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(PlomidError::new(
            ErrorKind::InvalidArgument,
            "WAL payload is too large",
        ));
    }
    WAL_HEADER_SIZE
        .checked_add(payload_len)
        .and_then(|length| length.checked_add(CHECKSUM_SIZE))
        .ok_or_else(|| PlomidError::new(ErrorKind::InvalidArgument, "WAL record is too large"))
}
