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
//! Deterministic little-endian bit packing.
//!
//! Bits fill the output least-significant-bit first: bit `i` of the packed
//! stream lives at byte `i / 8`, bit `i % 8`. That single rule is what makes the
//! encoding reproducible across machines and is asserted by the round-trip
//! tests.
//!
//! The reader is fully bounds-checked: every read either succeeds or reports a
//! structured corruption error, so a truncated or malformed packed region can
//! never panic or read past its buffer.

use crate::layout::corruption;
use plomid_core::Result;

/// Returns the number of bytes needed to hold `bits` bits.
#[must_use]
pub(crate) fn packed_len(bits: usize) -> usize {
    bits.div_ceil(8)
}

/// Returns the bits required to represent every value in `0..=max`, or `0` when
/// `max == 0` (a single distinct value needs no index bits at all).
#[must_use]
pub(crate) fn width_for_max(max: u64) -> u32 {
    if max == 0 {
        0
    } else {
        u64::BITS - max.leading_zeros()
    }
}

/// Appends fixed-width values to a little-endian, LSB-first bit stream.
#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    out: Vec<u8>,
    current: u8,
    used: u32,
}

impl BitWriter {
    /// Creates an empty writer.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::with_capacity(0)
    }

    /// Creates a writer that will hold `bytes` packed bytes before reallocating.
    #[must_use]
    pub(crate) fn with_capacity(bytes: usize) -> Self {
        Self {
            out: Vec::with_capacity(bytes),
            current: 0,
            used: 0,
        }
    }

    /// Appends the low `width` bits of `value`.
    ///
    /// `width` must not exceed 64. Higher bits of `value` are ignored, so a
    /// caller that over-supplies a value still writes the declared field width.
    pub(crate) fn write(&mut self, value: u64, width: u32) {
        debug_assert!(width <= u64::BITS);
        let mut remaining = width;
        let mut rest = value;
        while remaining > 0 {
            let take = (8 - self.used).min(remaining);
            let mask = (1_u64 << take) - 1;
            let bits = (rest & mask) as u8;
            self.current |= bits << self.used;
            self.used += take;
            rest >>= take;
            remaining -= take;
            if self.used == 8 {
                self.out.push(self.current);
                self.current = 0;
                self.used = 0;
            }
        }
    }

    /// Flushes the trailing partial byte and returns the packed stream.
    #[must_use]
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            self.out.push(self.current);
        }
        self.out
    }
}

/// Reads fixed-width values from a little-endian, LSB-first bit stream.
#[derive(Debug)]
pub(crate) struct BitReader<'a> {
    bytes: &'a [u8],
    position: usize,
    used: u32,
}

impl<'a> BitReader<'a> {
    /// Creates a reader over `bytes`.
    #[must_use]
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            used: 0,
        }
    }

    /// Reads the next `width` bits, rejecting a stream that ends early.
    pub(crate) fn read(&mut self, width: u32) -> Result<u64> {
        let mut remaining = width;
        let mut shift = 0_u32;
        let mut out = 0_u64;
        while remaining > 0 {
            let available = 8 - self.used;
            if available == 0 {
                self.position += 1;
                self.used = 0;
                continue;
            }
            let byte = *self
                .bytes
                .get(self.position)
                .ok_or_else(|| corruption("packed bit stream is truncated"))?;
            let take = available.min(remaining);
            let mask = ((1_u16 << take) - 1) as u8;
            let bits = u64::from((byte >> self.used) & mask);
            out |= bits << shift;
            shift += take;
            self.used += take;
            remaining -= take;
        }
        Ok(out)
    }

    /// Returns the number of whole bytes consumed so far.
    #[must_use]
    pub(crate) fn consumed(&self) -> usize {
        if self.used == 0 {
            self.position
        } else {
            self.position + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{packed_len, width_for_max, BitReader, BitWriter};
    use plomid_core::ErrorKind;

    #[test]
    fn packs_and_unpacks_lsb_first() {
        let mut writer = BitWriter::new();
        writer.write(0b1, 1);
        writer.write(0b10, 2);
        writer.write(0x1FF, 9);
        let bytes = writer.finish();
        // bit0 = 1; bits1..3 = 0b10; bits3..12 = 0b1_1111_1111
        assert_eq!(bytes.len(), 2);
        assert_eq!(bytes[0], 0b1111_1101);
        assert_eq!(bytes[1], 0b0000_1111);
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read(1).expect("bit"), 0b1);
        assert_eq!(reader.read(2).expect("bits"), 0b10);
        assert_eq!(reader.read(9).expect("bits"), 0x1FF);
        assert_eq!(reader.consumed(), bytes.len());
    }

    #[test]
    fn round_trips_every_width() {
        for width in 0..=64_u32 {
            let limit = if width == 0 {
                0
            } else {
                (1_u64 << width.min(63)) - 1
            };
            let values = [0_u64, 1, limit, limit / 3];
            let mut writer = BitWriter::new();
            for value in values {
                writer.write(value, width);
            }
            let bytes = writer.finish();
            assert_eq!(bytes.len(), packed_len(values.len() * width as usize));
            let mut reader = BitReader::new(&bytes);
            for value in values {
                assert_eq!(reader.read(width).expect("read") & limit, value & limit);
            }
            assert_eq!(reader.consumed(), bytes.len());
        }
    }

    #[test]
    fn zero_width_writes_nothing() {
        let mut writer = BitWriter::new();
        writer.write(u64::MAX, 0);
        assert!(writer.finish().is_empty());
    }

    #[test]
    fn reading_past_the_end_is_corruption() {
        let mut reader = BitReader::new(&[0xFF, 0x00]);
        assert_eq!(reader.read(8).expect("first byte"), 0xFF);
        assert_eq!(reader.read(8).expect("second byte"), 0x00);
        let error = reader.read(1).expect_err("truncated stream");
        assert_eq!(error.kind(), ErrorKind::Corruption);
        assert!(BitReader::new(&[]).read(1).is_err());
    }

    #[test]
    fn width_for_max_reports_minimal_bits() {
        assert_eq!(width_for_max(0), 0);
        assert_eq!(width_for_max(1), 1);
        assert_eq!(width_for_max(2), 2);
        assert_eq!(width_for_max(3), 2);
        assert_eq!(width_for_max(u64::MAX), 64);
    }
}
