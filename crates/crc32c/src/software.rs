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
//! Portable slicing-by-8 CRC32C.
//!
//! Eight 256-entry tables are derived at compile time, so the hot loop costs
//! one table lookup per byte-pair-slice instead of eight conditional shifts.
//! The tables reproduce the same reflected polynomial reduction as the
//! single-table form, therefore results are bit-identical. No unsafe, no
//! allocation, operates directly on `&[u8]`.

#![forbid(unsafe_code)]

const POLYNOMIAL: u32 = 0x82F6_3B78;

/// `TABLES[0]` is the classic byte-indexed CRC32C table (reflected form).
/// `TABLES[k]` is derived from `TABLES[k - 1]` as
/// `(t >> 8) ^ TABLES[0][t & 0xFF]`, which is the standard slicing-by-k
/// construction. Everything is evaluated at compile time.
const TABLES: [[u32; 256]; 8] = {
    let mut tables = [[0_u32; 256]; 8];
    let mut index = 0_usize;
    while index < 256 {
        let mut value = index as u32;
        let mut step = 0_u32;
        while step < 8 {
            let mask = 0_u32.wrapping_sub(value & 1);
            value = (value >> 1) ^ (POLYNOMIAL & mask);
            step += 1;
        }
        tables[0][index] = value;
        index += 1;
    }
    let mut slice = 1_usize;
    while slice < 8 {
        let mut index = 0_usize;
        while index < 256 {
            let previous = tables[slice - 1][index];
            tables[slice][index] = (previous >> 8) ^ tables[0][(previous & 0xFF) as usize];
            index += 1;
        }
        slice += 1;
    }
    tables
};

/// Advances the running state across `bytes`.
#[must_use]
pub fn update(mut crc: u32, bytes: &[u8]) -> u32 {
    let mut rest = bytes;
    // Slicing-by-8: fold 8 bytes per iteration using both half-words.
    while let Some((chunk, tail)) = rest.split_first_chunk::<8>() {
        let word = u64::from_le_bytes(*chunk);
        let low = crc ^ word as u32;
        let high = (word >> 32) as u32;
        crc = TABLES[7][(low & 0xFF) as usize]
            ^ TABLES[6][((low >> 8) & 0xFF) as usize]
            ^ TABLES[5][((low >> 16) & 0xFF) as usize]
            ^ TABLES[4][((low >> 24) & 0xFF) as usize]
            ^ TABLES[3][(high & 0xFF) as usize]
            ^ TABLES[2][((high >> 8) & 0xFF) as usize]
            ^ TABLES[1][((high >> 16) & 0xFF) as usize]
            ^ TABLES[0][((high >> 24) & 0xFF) as usize];
        rest = tail;
    }
    // Slicing-by-4 for the 4-byte aligned remainder.
    while let Some((chunk, tail)) = rest.split_first_chunk::<4>() {
        crc ^= u32::from_le_bytes(*chunk);
        crc = TABLES[3][(crc & 0xFF) as usize]
            ^ TABLES[2][((crc >> 8) & 0xFF) as usize]
            ^ TABLES[1][((crc >> 16) & 0xFF) as usize]
            ^ TABLES[0][((crc >> 24) & 0xFF) as usize];
        rest = tail;
    }
    // Trailing 0..3 bytes.
    for &byte in rest {
        crc = (crc >> 8) ^ TABLES[0][((crc ^ u32::from(byte)) & 0xFF) as usize];
    }
    crc
}

/// Computes the CRC32C checksum of `bytes` in one call.
#[must_use]
pub fn compute(bytes: &[u8]) -> u32 {
    !update(u32::MAX, bytes)
}
