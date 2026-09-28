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
//! Stable binary encoding for OLTP rows stored in page payloads.
//!
//! Layout version 1 is little-endian and consists of:
//!
//! ```text
//! header       12 bytes: magic[2], version[1], flags[1], field_count[u16],
//!                         null_bitmap_bytes[u16], total_length[u32]
//! null bitmap  ceil(field_count / 8) bytes; bit N means field N is NULL
//! fields       non-NULL fields in ordinal order:
//!              tag[u8] + i64[u64] for Integer
//!              tag[u8] + length[u32] + bytes for Bytes/String
//! ```
//!
//! `String` values are UTF-8. Flags are reserved and must be zero in version 1.
//! The encoded row is intended to fit in [`crate::PAGE_DATA_SIZE`]; overflow
//! rows and external storage are deferred to a later format revision.

use plomid_core::{ErrorKind, PlomidError, Result};

// Row-encoding constants are defined once in `plomid_core::constants`; the
// module-local names below keep the body of this file unchanged.
use plomid_core::{
    ROW_BYTES_TAG as BYTES_TAG, ROW_HEADER_SIZE as HEADER_SIZE, ROW_INTEGER_TAG as INTEGER_TAG,
    ROW_MAGIC as MAGIC, ROW_STRING_TAG as STRING_TAG, ROW_VERSION as VERSION,
};

/// One logical row field supported by the initial on-disk format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Field {
    /// SQL NULL.
    Null,
    /// A signed 64-bit integer.
    Integer(i64),
    /// Arbitrary binary data.
    Bytes(Vec<u8>),
    /// UTF-8 variable-length text.
    String(String),
}

/// A sequence of fields encoded in ordinal order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    fields: Vec<Field>,
}

impl Row {
    /// Creates a row from its fields.
    #[must_use]
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    /// Returns the row fields in ordinal order.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Consumes the row and returns its fields.
    #[must_use]
    pub fn into_fields(self) -> Vec<Field> {
        self.fields
    }

    /// Encodes this row using the version 1 layout documented above.
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode(self)
    }

    /// Decodes and validates one complete encoded row.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        decode(bytes)
    }
}

/// Encodes a row into an owned byte buffer.
pub fn encode(row: &Row) -> Result<Vec<u8>> {
    let field_count = u16::try_from(row.fields.len())
        .map_err(|_| PlomidError::new(ErrorKind::InvalidArgument, "row has too many fields"))?;
    let bitmap_bytes = usize::from(field_count).div_ceil(8);
    let bitmap_bytes_u16 = u16::try_from(bitmap_bytes).map_err(|_| {
        PlomidError::new(ErrorKind::InvalidArgument, "row null bitmap is too large")
    })?;
    let mut output = vec![0; HEADER_SIZE + bitmap_bytes];
    output[..2].copy_from_slice(&MAGIC);
    output[2] = VERSION;
    output[4..6].copy_from_slice(&field_count.to_le_bytes());
    output[6..8].copy_from_slice(&bitmap_bytes_u16.to_le_bytes());

    for (index, field) in row.fields.iter().enumerate() {
        match field {
            Field::Null => {
                output[HEADER_SIZE + index / 8] |= 1 << (index % 8);
            }
            Field::Integer(value) => {
                output.push(INTEGER_TAG);
                output.extend_from_slice(&value.to_le_bytes());
            }
            Field::Bytes(value) => append_variable(&mut output, BYTES_TAG, value)?,
            Field::String(value) => append_variable(&mut output, STRING_TAG, value.as_bytes())?,
        }
    }

    let total_length = u32::try_from(output.len())
        .map_err(|_| PlomidError::new(ErrorKind::InvalidArgument, "encoded row is too large"))?;
    output[8..12].copy_from_slice(&total_length.to_le_bytes());
    Ok(output)
}

fn append_variable(output: &mut Vec<u8>, tag: u8, value: &[u8]) -> Result<()> {
    let length = u32::try_from(value.len())
        .map_err(|_| PlomidError::new(ErrorKind::InvalidArgument, "variable field is too large"))?;
    output.push(tag);
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(value);
    Ok(())
}

/// Decodes and validates one complete encoded row.
pub fn decode(bytes: &[u8]) -> Result<Row> {
    if bytes.len() < HEADER_SIZE {
        return Err(corruption("row header is truncated"));
    }
    if bytes[..2] != MAGIC {
        return Err(corruption("invalid row magic"));
    }
    if bytes[2] != VERSION {
        return Err(corruption("unsupported row format version"));
    }
    if bytes[3] != 0 {
        return Err(corruption("row contains unknown flags"));
    }
    let field_count = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
    let bitmap_bytes = usize::from(u16::from_le_bytes([bytes[6], bytes[7]]));
    let expected_bitmap_bytes = field_count.div_ceil(8);
    if bitmap_bytes != expected_bitmap_bytes {
        return Err(corruption("row null bitmap length is invalid"));
    }
    let total_length = usize::try_from(u32::from_le_bytes([
        bytes[8], bytes[9], bytes[10], bytes[11],
    ]))
    .map_err(|_| corruption("row length is invalid"))?;
    if total_length != bytes.len() {
        return Err(corruption("row length does not match input"));
    }
    let fields_start = HEADER_SIZE
        .checked_add(bitmap_bytes)
        .ok_or_else(|| corruption("row header length overflow"))?;
    if fields_start > bytes.len() {
        return Err(corruption("row null bitmap is truncated"));
    }
    let bitmap = &bytes[HEADER_SIZE..fields_start];
    let mut cursor = fields_start;
    let mut fields = Vec::with_capacity(field_count);
    for index in 0..field_count {
        if bitmap[index / 8] & (1 << (index % 8)) != 0 {
            fields.push(Field::Null);
            continue;
        }
        let tag = take_u8(bytes, &mut cursor)?;
        match tag {
            INTEGER_TAG => {
                let raw = take_exact(bytes, &mut cursor, 8)?;
                fields.push(Field::Integer(i64::from_le_bytes(
                    raw.try_into()
                        .map_err(|_| corruption("invalid integer field"))?,
                )));
            }
            BYTES_TAG => fields.push(Field::Bytes(take_variable(bytes, &mut cursor)?)),
            STRING_TAG => {
                let value = take_variable(bytes, &mut cursor)?;
                let value = String::from_utf8(value)
                    .map_err(|_| corruption("string field is not valid UTF-8"))?;
                fields.push(Field::String(value));
            }
            _ => return Err(corruption("unknown row field tag")),
        }
    }
    if cursor != bytes.len() {
        return Err(corruption("row contains trailing bytes"));
    }
    Ok(Row { fields })
}

fn take_u8(bytes: &[u8], cursor: &mut usize) -> Result<u8> {
    let value = *bytes
        .get(*cursor)
        .ok_or_else(|| corruption("row field tag is truncated"))?;
    *cursor += 1;
    Ok(value)
}

fn take_exact<'a>(bytes: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8]> {
    let end = cursor
        .checked_add(length)
        .ok_or_else(|| corruption("row field length overflow"))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or_else(|| corruption("row field is truncated"))?;
    *cursor = end;
    Ok(value)
}

fn take_variable(bytes: &[u8], cursor: &mut usize) -> Result<Vec<u8>> {
    let raw_length = take_exact(bytes, cursor, 4)?;
    let length = usize::try_from(u32::from_le_bytes(
        raw_length
            .try_into()
            .map_err(|_| corruption("invalid field length"))?,
    ))
    .map_err(|_| corruption("field length is invalid"))?;
    Ok(take_exact(bytes, cursor, length)?.to_vec())
}

fn corruption(message: &'static str) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, Field, Row};
    use plomid_core::ErrorKind;

    #[test]
    fn round_trips_scalars_strings_bytes_and_nulls() {
        let row = Row::new(vec![
            Field::Integer(-42),
            Field::Null,
            Field::Bytes(vec![0, 1, 2, 255]),
            Field::String("PLOMID storage".to_owned()),
        ]);
        let encoded = encode(&row).expect("row encoding must succeed");
        let decoded = decode(&encoded).expect("row decoding must succeed");
        assert_eq!(decoded, row);
    }

    #[test]
    fn truncated_rows_fail_with_corruption() {
        let encoded = Row::new(vec![Field::Integer(7), Field::String("value".to_owned())])
            .encode()
            .expect("row encoding must succeed");
        for length in 0..encoded.len() {
            let error = decode(&encoded[..length]).expect_err("truncated row must fail");
            assert_eq!(error.kind(), ErrorKind::Corruption);
        }
    }

    #[test]
    fn invalid_tag_fails_with_corruption() {
        let mut encoded = Row::new(vec![Field::Integer(7)])
            .encode()
            .expect("row encoding must succeed");
        encoded[12] = 99;
        let error = decode(&encoded).expect_err("invalid tag must fail");
        assert_eq!(error.kind(), ErrorKind::Corruption);
    }

    #[test]
    fn empty_variable_width_fields_round_trip() {
        let row = Row::new(vec![
            Field::Bytes(Vec::new()),
            Field::String(String::new()),
            Field::Bytes(vec![0]),
        ]);
        let encoded = encode(&row).expect("row encoding must succeed");
        assert_eq!(decode(&encoded).expect("row decoding must succeed"), row);
        // Empty values are length-prefixed zero, not omitted.
        assert!(encoded.len() > 12 + 1);
    }

    #[test]
    fn all_null_and_empty_rows_round_trip() {
        let all_null = Row::new(vec![
            Field::Null,
            Field::Null,
            Field::Null,
            Field::Null,
            Field::Null,
            Field::Null,
            Field::Null,
            Field::Null,
            Field::Null,
        ]);
        let encoded = encode(&all_null).expect("row encoding must succeed");
        assert_eq!(
            decode(&encoded).expect("row decoding must succeed"),
            all_null
        );

        let empty = Row::new(Vec::new());
        let encoded = encode(&empty).expect("row encoding must succeed");
        assert_eq!(encoded.len(), 12);
        assert_eq!(decode(&encoded).expect("row decoding must succeed"), empty);
    }

    #[test]
    fn maximum_supported_values_round_trip() {
        let wide_text = "x".repeat(4_096);
        let wide_bytes = vec![0xAB; 8_192];
        let row = Row::new(vec![
            Field::Integer(i64::MIN),
            Field::Integer(i64::MAX),
            Field::Integer(0),
            Field::String(wide_text),
            Field::Bytes(wide_bytes),
        ]);
        let encoded = encode(&row).expect("row encoding must succeed");
        assert_eq!(decode(&encoded).expect("row decoding must succeed"), row);
    }

    #[test]
    fn encoding_is_deterministic() {
        let row = Row::new(vec![
            Field::Integer(11),
            Field::Null,
            Field::String("deterministic".to_owned()),
            Field::Bytes(vec![1, 2, 3]),
        ]);
        let first = encode(&row).expect("row encoding must succeed");
        let second = encode(&row).expect("row encoding must succeed");
        assert_eq!(first, second);
        // Re-encoding a decoded row reproduces the identical byte string.
        let decoded = decode(&first).expect("row decoding must succeed");
        assert_eq!(encode(&decoded).expect("row encoding must succeed"), first);
    }

    #[test]
    fn malformed_headers_fail_with_corruption() {
        let encoded = Row::new(vec![Field::Integer(7)])
            .encode()
            .expect("row encoding must succeed");

        let mut bad_magic = encoded.clone();
        bad_magic[0] = 0x00;
        assert_corruption(&bad_magic);

        let mut bad_version = encoded.clone();
        bad_version[2] = 0x7F;
        assert_corruption(&bad_version);

        let mut bad_flags = encoded.clone();
        bad_flags[3] = 0x01;
        assert_corruption(&bad_flags);

        let mut bad_bitmap = encoded.clone();
        bad_bitmap[6] = 0x02;
        assert_corruption(&bad_bitmap);

        let mut bad_length = encoded.clone();
        bad_length[8] = 0xFF;
        assert_corruption(&bad_length);
    }

    #[test]
    fn trailing_and_overlong_payloads_fail_with_corruption() {
        let mut trailing = Row::new(vec![Field::Integer(7)])
            .encode()
            .expect("row encoding must succeed");
        trailing.push(0x00);
        let length = u32::try_from(trailing.len()).expect("length");
        trailing[8..12].copy_from_slice(&length.to_le_bytes());
        assert_corruption(&trailing);

        let mut overlong = Row::new(vec![Field::Bytes(vec![1, 2, 3])])
            .encode()
            .expect("row encoding must succeed");
        let inflated = u32::MAX.to_le_bytes();
        overlong[14..18].copy_from_slice(&inflated);
        assert_corruption(&overlong);
        let inflated = (u32::MAX / 2).to_le_bytes();
        overlong[14..18].copy_from_slice(&inflated);
        assert_corruption(&overlong);
    }

    #[test]
    fn non_utf8_string_fields_fail_with_corruption() {
        let mut encoded = Row::new(vec![Field::String("value".to_owned())])
            .encode()
            .expect("row encoding must succeed");
        encoded[18] = 0xFF;
        assert_corruption(&encoded);
    }

    #[test]
    fn excessive_field_counts_fail_with_invalid_argument() {
        let row = Row::new(vec![Field::Null; usize::from(u16::MAX) + 1]);
        let error = encode(&row).expect_err("too many fields must fail");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    fn assert_corruption(bytes: &[u8]) {
        let error = decode(bytes).expect_err("malformed row must fail");
        assert_eq!(error.kind(), ErrorKind::Corruption);
    }
}
