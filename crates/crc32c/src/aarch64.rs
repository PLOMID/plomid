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
//! aarch64 (ARM64) hardware CRC32C via the Armv8.0-A CRC extension.
//!
//! `__crc32cd`, `__crc32cw`, and `__crc32cb` fold 8, 4, and 1 byte respectively,
//! always matching the byte width of the input so no zero-fill bytes leak
//! into the checksum. This module is compiled only for
//! `target_arch = "aarch64"`. CPUs without the CRC extension (rare on
//! server-class parts, but possible) fall back to the software path because
//! [`crate::select`] runs `is_aarch64_feature_detected!("crc")` first.

use core::arch::aarch64::{__crc32cb, __crc32cd, __crc32cw};

/// Advances the running state across `bytes`.
///
/// # Safety boundary
///
/// Every `unsafe` block here reaches an intrinsic whose contract requires the
/// aarch64 `crc` target feature. `lib.rs::select` guarantees it: this function
/// can only be chosen after `is_aarch64_feature_detected!("crc")` returned
/// `true`, so executing these instructions is well-defined. The intrinsics
/// are pure register operations with no memory preconditions.
#[must_use]
pub fn update(mut crc: u32, bytes: &[u8]) -> u32 {
    let mut rest = bytes;
    // Fold 8 bytes at a time.
    while let Some((chunk, tail)) = rest.split_first_chunk::<8>() {
        // SAFETY: the aarch64 CRC extension was confirmed by dispatch before
        // this implementation was selected; the intrinsic has no memory
        // preconditions.
        crc = unsafe { __crc32cd(crc, u64::from_le_bytes(*chunk)) };
        rest = tail;
    }
    // Fold the 4-byte remainder.
    while let Some((chunk, tail)) = rest.split_first_chunk::<4>() {
        // SAFETY: as above; `__crc32cw` folds exactly 4 bytes.
        crc = unsafe { __crc32cw(crc, u32::from_le_bytes(*chunk)) };
        rest = tail;
    }
    // Fold the trailing 0..3 bytes.
    for &byte in rest {
        // SAFETY: as above; `__crc32cb` folds exactly 1 byte.
        crc = unsafe { __crc32cb(crc, byte) };
    }
    crc
}
