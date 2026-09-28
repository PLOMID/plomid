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
//! Dictionary encoding for low-cardinality columns.
//!
//! Status codes, tag names, tenant names, host names, and categorical strings
//! repeat heavily, so each distinct value is stored once and every row becomes a
//! fixed-width index into the dictionary. Index widths are bit-packed at the
//! narrowest width the dictionary needs: a two-value dictionary costs one bit per
//! row.
//!
//! ```text
//! body: dict_count[u64]
//!       dict_count * { value_len[u32], value[value_len] }    in index order
//!       index_width[u8]                                      minimal for dict_count
//!       packed indices                                       value_count * width bits
//! ```
//!
//! Both the dictionary and the index width are bounded by the format: a chunk
//! that would need more than `COLUMNAR_MAX_DICTIONARY_ENTRIES` distinct values
//! declines the encoding rather than growing an unbounded dictionary, and the
//! decoder rejects a wider-than-minimal index width, an out-of-range index, or a
//! packed region whose length does not match the declared row count. The
//! decoder walks the packed indices twice instead of collecting them, so a
//! hostile body cannot make it allocate a per-row table.

use crate::encoding::bitpack::{packed_len, width_for_max, BitReader, BitWriter};
use crate::encoding::entries::{append_entry, entry_len, EntryReader};
use crate::layout::{corruption, get_u32, get_u64, to_usize};
use plomid_core::Result;
use std::collections::HashMap;

/// Body header: the dictionary count.
const HEADER_LEN: usize = 8;
/// Bytes one dictionary entry contributes, excluding its value bytes.
const DICTIONARY_ENTRY_LEN: usize = 4;

/// Encodes `raw` as a dictionary, or `None` when the dictionary would be
/// unbounded.
pub(crate) fn encode(raw: &[u8], value_count: u64) -> Result<Option<Vec<u8>>> {
    let mut reader = EntryReader::new(raw, value_count);
    let mut indices: HashMap<&[u8], u32> = HashMap::new();
    let mut dictionary: Vec<&[u8]> = Vec::new();
    let mut index_stream: Vec<u32> = Vec::with_capacity(value_count as usize);
    while let Some(value) = reader.next_entry()? {
        let index = match indices.get(value) {
            Some(index) => *index,
            None => {
                if dictionary.len() >= plomid_core::COLUMNAR_MAX_DICTIONARY_ENTRIES {
                    return Ok(None);
                }
                let index = u32::try_from(dictionary.len())
                    .map_err(|_| corruption("dictionary index exceeds u32"))?;
                dictionary.push(value);
                indices.insert(value, index);
                index
            }
        };
        index_stream.push(index);
    }
    reader.finish()?;

    let width = width_for_max(dictionary.len().saturating_sub(1) as u64);
    let mut body = Vec::new();
    body.extend_from_slice(&(dictionary.len() as u64).to_le_bytes());
    for value in &dictionary {
        let length = u32::try_from(value.len())
            .map_err(|_| corruption("dictionary value exceeds the maximum encodable length"))?;
        body.extend_from_slice(&length.to_le_bytes());
        body.extend_from_slice(value);
    }
    body.push(width as u8);
    let mut writer = BitWriter::with_capacity(packed_len(index_stream.len() * width as usize));
    for index in index_stream {
        writer.write(u64::from(index), width);
    }
    body.extend_from_slice(&writer.finish());
    Ok(Some(body))
}

/// Expands a dictionary body back into the original value stream.
pub(crate) fn decode(body: &[u8], value_count: u64, raw_len: u64) -> Result<Vec<u8>> {
    let dict_count = get_u64(body, 0, "dictionary count")?;
    if dict_count > plomid_core::COLUMNAR_MAX_DICTIONARY_ENTRIES as u64 {
        return Err(corruption(format!(
            "dictionary chunk declares {dict_count} values, more than this format supports"
        )));
    }
    let mut dictionary: Vec<&[u8]> = Vec::with_capacity(dict_count as usize);
    let mut cursor = HEADER_LEN;
    for index in 0..dict_count {
        let length = get_u32(body, cursor, "dictionary value length")?;
        cursor = cursor
            .checked_add(DICTIONARY_ENTRY_LEN)
            .ok_or_else(|| corruption("dictionary entry overflows the body"))?;
        let end = cursor
            .checked_add(length as usize)
            .ok_or_else(|| corruption("dictionary value overflows the body"))?;
        let value = body
            .get(cursor..end)
            .ok_or_else(|| corruption(format!("dictionary value {index} lies outside the body")))?;
        cursor = end;
        dictionary.push(value);
    }
    if dict_count == 0 && (value_count != 0 || raw_len != 0) {
        return Err(corruption(
            "empty dictionary chunk declares rows or raw bytes to reconstruct",
        ));
    }
    let width = u32::from(
        *body
            .get(cursor)
            .ok_or_else(|| corruption("dictionary index width is missing"))?,
    );
    cursor += 1;
    let expected_width = width_for_max(dictionary.len().saturating_sub(1) as u64);
    if width != expected_width {
        return Err(corruption(format!(
            "dictionary index width {width} is not minimal for {} values",
            dictionary.len()
        )));
    }
    let expected_packed = packed_len(
        to_usize(value_count, "value count")?
            .checked_mul(width as usize)
            .ok_or_else(|| corruption("dictionary index bits overflow"))?,
    );
    if body.len() != cursor + expected_packed {
        return Err(corruption(format!(
            "dictionary index region is {} bytes but {value_count} indices at {width} bits need {expected_packed}",
            body.len() - cursor
        )));
    }
    let packed = &body[cursor..];

    // First pass: size the output exactly from the indices themselves.
    let mut total = 0_usize;
    let mut reader = BitReader::new(packed);
    for _ in 0..value_count {
        total = total
            .checked_add(entry_len(lookup(&dictionary, reader.read(width)?)?.len())?)
            .ok_or_else(|| corruption("dictionary expansion overflows"))?;
    }
    if reader.consumed() != expected_packed {
        return Err(corruption(
            "packed dictionary indices are not fully consumed",
        ));
    }
    if total as u64 != raw_len {
        return Err(corruption(format!(
            "dictionary body expands to {total} bytes but the chunk declares {raw_len}"
        )));
    }

    // Second pass: emit. The output buffer is sized from the first pass, so no
    // allocation here depends on an unvalidated length.
    let mut out = Vec::with_capacity(total);
    let mut reader = BitReader::new(packed);
    for _ in 0..value_count {
        append_entry(&mut out, lookup(&dictionary, reader.read(width)?)?)?;
    }
    Ok(out)
}

/// Resolves one packed index, rejecting an index outside the dictionary.
fn lookup<'a>(dictionary: &[&'a [u8]], index: u64) -> Result<&'a [u8]> {
    let index = to_usize(index, "dictionary index")?;
    dictionary
        .get(index)
        .copied()
        .ok_or_else(|| corruption(format!("dictionary index {index} is out of range")))
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};
    use crate::encoding::entries::{append_entry, validate_stream};
    use plomid_core::ErrorKind;

    fn stream(values: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            append_entry(&mut out, value).expect("append");
        }
        out
    }

    fn body_of(values: &[&[u8]]) -> (Vec<u8>, Vec<u8>) {
        let raw = stream(values);
        let body = encode(&raw, values.len() as u64)
            .expect("encode")
            .expect("applicable");
        (raw, body)
    }

    #[test]
    fn round_trips_low_cardinality_columns() {
        for values in [
            vec![&b"red"[..], b"green", b"red", b"blue", b"red"],
            vec![&b""[..], b"", b"x"],
            vec![&b"only"[..]],
            vec![],
        ] {
            let (raw, body) = body_of(&values);
            let back = decode(&body, values.len() as u64, raw.len() as u64).expect("decode");
            assert_eq!(back, raw);
            validate_stream(&back, values.len() as u64).expect("well formed");
        }
    }

    #[test]
    fn two_distinct_values_cost_one_bit_per_row() {
        let values: Vec<&[u8]> = (0..1_000)
            .map(|i| if i % 2 == 0 { &b"y"[..] } else { &b"n"[..] })
            .collect();
        let (raw, body) = body_of(&values);
        // dict_count(8) + two five-byte entries, then the width byte.
        assert_eq!(body[18], 1);
        assert!(body.len() < raw.len() / 2);
    }

    #[test]
    fn rejects_a_non_minimal_index_width() {
        let (raw, body) = body_of(&[b"a", b"b"]);
        let mut wide = body.clone();
        wide[18] = 8;
        assert_eq!(
            decode(&wide, 2, raw.len() as u64)
                .expect_err("width")
                .kind(),
            ErrorKind::Corruption
        );
    }

    #[test]
    fn rejects_an_index_outside_the_dictionary() {
        // A hand-built body for three values whose middle index is 3, which no
        // three-entry dictionary can hold.
        let mut body = Vec::new();
        body.extend_from_slice(&3_u64.to_le_bytes());
        for value in [&b"a"[..], b"b", b"c"] {
            body.extend_from_slice(&1_u32.to_le_bytes());
            body.extend_from_slice(value);
        }
        body.push(2); // width for three entries
        body.push(0x1C); // indices 0, 3, 1 at two bits each, LSB-first
        let error = decode(&body, 3, 15).expect_err("index");
        assert_eq!(error.kind(), ErrorKind::Corruption);
        assert!(error.to_string().contains("out of range"));
    }

    #[test]
    fn rejects_truncation_trailing_and_wrong_lengths() {
        let (raw, body) = body_of(&[b"a", b"b", b"a"]);
        assert!(decode(&body[..body.len() - 1], 3, raw.len() as u64).is_err());
        let mut padded = body.clone();
        padded.push(0);
        assert!(decode(&padded, 3, raw.len() as u64).is_err());
        assert!(decode(&body, 3, raw.len() as u64 + 4).is_err());
        assert!(decode(&body, 4, raw.len() as u64).is_err());
        assert!(decode(&[], 0, 0).is_err());
    }

    #[test]
    fn declines_a_dictionary_beyond_the_configured_cap() {
        let mut raw = Vec::new();
        for index in 0..=plomid_core::COLUMNAR_MAX_DICTIONARY_ENTRIES {
            append_entry(&mut raw, format!("v{index}").as_bytes()).expect("append");
        }
        assert!(encode(
            &raw,
            plomid_core::COLUMNAR_MAX_DICTIONARY_ENTRIES as u64 + 1
        )
        .expect("encode")
        .is_none());
    }
}
