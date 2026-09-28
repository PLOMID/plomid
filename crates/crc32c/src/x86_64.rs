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
//! x86_64 hardware CRC32C via the SSE4.2 `CRC32` instructions.
//!
//! Only `_mm_crc32_u64`, `_mm_crc32_u32`, and `_mm_crc32_u8` are used, each
//! exactly matching the byte width of the data being folded, so no zero-fill
//! bytes can leak into the checksum. This module is compiled only for
//! `target_arch = "x86_64"`.

use core::arch::x86_64::{_mm_crc32_u32, _mm_crc32_u64, _mm_crc32_u8};

/// Advances the running state across `bytes`.
///
/// # Safety boundary
///
/// Every `unsafe` block here reaches a CPU intrinsic whose contract requires
/// the SSE4.2 feature. `lib.rs::select` guarantees it: this function can only
/// be chosen after `is_x86_feature_detected!("sse4.2")` returned `true`, so
/// executing these instructions is well-defined. No other preconditions
/// exist — the intrinsics are pure register operations and cannot violate
/// memory safety.
#[must_use]
pub fn update(mut crc: u32, bytes: &[u8]) -> u32 {
    let mut rest = bytes;
    // Fold 8 bytes at a time.
    while let Some((chunk, tail)) = rest.split_first_chunk::<8>() {
        // SAFETY: the SSE4.2 CPU feature was confirmed by dispatch before
        // this implementation was selected; the intrinsic has no memory
        // preconditions.
        crc = unsafe { _mm_crc32_u64(crc as u64, u64::from_le_bytes(*chunk)) } as u32;
        rest = tail;
    }
    // Fold the 4-byte remainder.
    while let Some((chunk, tail)) = rest.split_first_chunk::<4>() {
        // SAFETY: as above; `_mm_crc32_u32` folds exactly 4 bytes.
        crc = unsafe { _mm_crc32_u32(crc, u32::from_le_bytes(*chunk)) };
        rest = tail;
    }
    // Fold the trailing 0..3 bytes.
    for &byte in rest {
        // SAFETY: as above; `_mm_crc32_u8` folds exactly 1 byte.
        crc = unsafe { _mm_crc32_u8(crc, byte) };
    }
    crc
}
