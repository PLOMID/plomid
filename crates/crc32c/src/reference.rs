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
//! Obviously-correct bitwise CRC32C reference implementation.
//!
//! This is the original PLOMID implementation, kept verbatim as the
//! correctness oracle for tests and as the "before" baseline in the
//! benchmark. It is intentionally slow (eight conditional shifts per byte)
//! and must never be used on the production hot path. Validated by the
//! canonical vector `CRC32C("123456789") = 0xE3069283`.

#![forbid(unsafe_code)]

const POLYNOMIAL: u32 = 0x82F6_3B78;

/// Advances the running state across `bytes`, one bit-reflection at a time.
#[must_use]
pub fn update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= u32::from(byte);
        let mut bit = 0_u32;
        while bit < 8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (POLYNOMIAL & mask);
            bit += 1;
        }
    }
    crc
}

/// Computes the CRC32C checksum of `bytes` in one call.
#[must_use]
pub fn compute(bytes: &[u8]) -> u32 {
    !update(u32::MAX, bytes)
}
