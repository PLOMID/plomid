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
//! Delta + bit-packed encoding for fixed-width 64-bit integer columns.
//!
//! Time-series and analytical integer columns are dominated by one of two
//! shapes: near-monotonic sequences (timestamps, sequence numbers, keys) and
//! small-valued repeats. Both collapse under delta-then-pack, and a time series
//! sampled on a regular interval collapses completely: the encoding stores the
//! first value, the smallest delta in the chunk, and then bit-packs each
//! `delta - min_delta` at the narrowest width the chunk needs. A constant step
//! therefore costs zero bits per row and a varying-but-narrow step costs only
//! its variation.
//!
//! ```text
//! body: width[u8]                    bits per packed delta, 0..=64
//!       min_delta[i64 little-endian]
//!       first[i64 little-endian]     both present only when value_count > 0
//!       packed (delta - min_delta)   (value_count - 1) * width bits, LSB-first
//! ```
//!
//! The encoding applies only when every value entry is exactly eight bytes wide,
//! which is precisely the persisted form of an integer column. Every value is
//! treated as an `i64` bit pattern and all delta arithmetic wraps, so
//! `i64::MIN`/`i64::MAX` and arbitrary bit patterns round-trip exactly rather
//! than being rejected or saturating.

use crate::encoding::bitpack::{packed_len, width_for_max, BitReader, BitWriter};
use crate::encoding::entries::{append_entry, entry_len, EntryReader};
use crate::layout::{corruption, get_u64, to_usize};
use plomid_core::Result;

/// Fixed width of an encoded integer value, mirroring the persisted form.
pub(crate) const INTEGER_WIDTH: usize = 8;
/// Body header: the bit width byte, the minimum delta, and the first value.
const HEADER_LEN: usize = 1 + INTEGER_WIDTH + INTEGER_WIDTH;

/// Encodes an integer value stream, or `None` when it is not fixed-width.
pub(crate) fn encode(raw: &[u8], value_count: u64) -> Result<Option<Vec<u8>>> {
    let mut reader = EntryReader::new(raw, value_count);
    let mut first: Option<i64> = None;
    let mut previous = 0_i64;
    let mut deltas: Vec<i64> = Vec::new();
    while let Some(value) = reader.next_entry()? {
        let array: [u8; INTEGER_WIDTH] = match value.try_into() {
            Ok(array) => array,
            Err(_) => return Ok(None),
        };
        let current = i64::from_le_bytes(array);
        match first {
            None => first = Some(current),
            Some(_) => deltas.push(current.wrapping_sub(previous)),
        }
        previous = current;
    }
    reader.finish()?;
    let Some(first) = first else {
        // An empty chunk carries only the width byte: there is no value to lead
        // with and no delta to pack.
        return Ok(Some(vec![0_u8]));
    };

    // Smallest delta, so a regular series packs to zero bits per row.
    let min_delta = deltas.iter().copied().min().unwrap_or(0);
    let mut width_max = 0_u64;
    for delta in &deltas {
        width_max = width_max.max((*delta as u64).wrapping_sub(min_delta as u64));
    }
    let width = width_for_max(width_max);

    let packed_bits = packed_len(deltas.len() * width as usize);
    let mut body = Vec::with_capacity(HEADER_LEN + packed_bits);
    body.push(width as u8);
    body.extend_from_slice(&min_delta.to_le_bytes());
    body.extend_from_slice(&first.to_le_bytes());
    let mut writer = BitWriter::with_capacity(packed_bits);
    for delta in deltas {
        writer.write((delta as u64).wrapping_sub(min_delta as u64), width);
    }
    body.extend_from_slice(&writer.finish());
    Ok(Some(body))
}

/// Expands a delta body back into the original value stream.
pub(crate) fn decode(body: &[u8], value_count: u64, raw_len: u64) -> Result<Vec<u8>> {
    let expected_raw = entry_len(INTEGER_WIDTH)?
        .checked_mul(to_usize(value_count, "value count")?)
        .ok_or_else(|| corruption("integer chunk length overflows"))?;
    if expected_raw as u64 != raw_len {
        return Err(corruption(format!(
            "integer chunk declares {raw_len} raw bytes but {value_count} fixed-width values need {expected_raw}"
        )));
    }
    if value_count == 0 {
        if body != [0_u8] {
            return Err(corruption("empty integer chunk body is malformed"));
        }
        return Ok(Vec::new());
    }

    let width = u32::from(
        *body
            .first()
            .ok_or_else(|| corruption("delta body is empty but the chunk declares values"))?,
    );
    if width > u64::BITS {
        return Err(corruption(format!("delta bit width {width} exceeds 64")));
    }
    let min_delta = get_u64(body, 1, "delta minimum")? as i64;
    let first = get_u64(body, 1 + INTEGER_WIDTH, "delta first value")? as i64;
    let delta_count = value_count - 1;
    let expected_packed = packed_len(
        to_usize(delta_count, "delta count")?
            .checked_mul(width as usize)
            .ok_or_else(|| corruption("delta bit length overflows"))?,
    );
    if body.len() != HEADER_LEN + expected_packed {
        return Err(corruption(format!(
            "delta body is {} bytes but {delta_count} deltas at {width} bits need {}",
            body.len(),
            HEADER_LEN + expected_packed
        )));
    }

    let mut out = Vec::with_capacity(expected_raw);
    let mut previous = first;
    append_entry(&mut out, &previous.to_le_bytes())?;
    let mut reader = BitReader::new(&body[HEADER_LEN..]);
    for _ in 0..delta_count {
        let delta = min_delta.wrapping_add(reader.read(width)? as i64);
        previous = previous.wrapping_add(delta);
        append_entry(&mut out, &previous.to_le_bytes())?;
    }
    if reader.consumed() != expected_packed {
        return Err(corruption("packed deltas are not fully consumed"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};
    use crate::encoding::entries::{append_entry, validate_stream};
    use plomid_core::ErrorKind;

    fn int_stream(values: &[i64]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            append_entry(&mut out, &value.to_le_bytes()).expect("append");
        }
        out
    }

    fn body_of(values: &[i64]) -> (Vec<u8>, Vec<u8>) {
        let raw = int_stream(values);
        let body = encode(&raw, values.len() as u64)
            .expect("encode")
            .expect("applicable");
        (raw, body)
    }

    fn round_trip(values: &[i64]) {
        let (raw, body) = body_of(values);
        let back = decode(&body, values.len() as u64, raw.len() as u64).expect("decode");
        assert_eq!(back, raw);
        validate_stream(&back, values.len() as u64).expect("well formed");
    }

    #[test]
    fn round_trips_monotonic_repeated_and_wide_values() {
        round_trip(&[0, 1, 2, 3, 4, 5]);
        round_trip(&[7; 32]);
        round_trip(&[i64::MIN, i64::MAX, 0, -1, 1]);
        round_trip(&[-5]);
        round_trip(&[]);
        let random: Vec<i64> = (0..256)
            .map(|i| (i as i64).wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as i64) ^ i64::MIN)
            .collect();
        round_trip(&random);
    }

    #[test]
    fn constant_steps_need_no_delta_bits() {
        let values: Vec<i64> = (0..1_000).collect();
        let (raw, body) = body_of(&values);
        assert_eq!(body[0], 0);
        assert_eq!(body.len(), 17);
        assert!(body.len() * 50 < raw.len());
    }

    #[test]
    fn irregular_steps_need_bits_only_for_their_spread() {
        // Deltas are 1, 2 and 3: min_delta is 1, so the widest adjusted delta is
        // 2, which fits in two bits.
        let mut values = vec![0_i64];
        for step in [1_i64, 2, 3].iter().cycle().take(255) {
            values.push(values[values.len() - 1] + *step);
        }
        let (raw, body) = body_of(&values);
        assert_eq!(body[0], 2);
        assert_eq!(body.len(), 17 + (255 * 2_usize).div_ceil(8));
        assert!(body.len() < raw.len() / 4);
    }

    #[test]
    fn constant_columns_need_no_delta_bits() {
        let (_, body) = body_of(&vec![42_i64; 100]);
        assert_eq!(body[0], 0);
        assert_eq!(body.len(), 17);
    }

    #[test]
    fn declines_variable_width_entries() {
        let mut stream = Vec::new();
        append_entry(&mut stream, b"abc").expect("append");
        assert!(encode(&stream, 1).expect("encode").is_none());
    }

    #[test]
    fn rejects_wrong_widths_lengths_and_truncation() {
        let (raw, body) = body_of(&[1, 2, 3]);
        assert!(decode(&body, 3, raw.len() as u64 + 1).is_err());
        assert!(decode(&body, 2, raw.len() as u64).is_err());
        assert!(decode(&body[..body.len() - 1], 3, raw.len() as u64).is_err());
        let mut wide = body.clone();
        wide[0] = 65;
        assert_eq!(
            decode(&wide, 3, raw.len() as u64)
                .expect_err("width")
                .kind(),
            ErrorKind::Corruption
        );
        assert!(decode(&[], 1, 12).is_err());
        assert!(decode(&[0_u8], 1, 12).is_err());
    }
}
