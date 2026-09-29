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
//! CRC32C checksums for persisted pages and WAL records.
//!
//! Thin, allocation-free facade over the isolated [`crc32c`] component. The
//! storage crate never touches CPU-specific code: `#![forbid(unsafe_code)]`
//! holds for this crate, and the hardware/software selection, tables, and
//! intrinsics live entirely inside `crates/crc32c`.
//!
//! Algorithm, polynomial, width, initialization, and finalization are
//! unchanged from the previous in-crate implementation, so persisted page,
//! block, pack, and WAL checksums remain bit-for-bit compatible.

#![forbid(unsafe_code)]

use plomid_core::{ErrorKind, PlomidError, Result};

/// Computes the CRC32C checksum of `bytes`.
#[must_use]
pub fn compute_checksum(bytes: &[u8]) -> u32 {
    crc32c::compute(bytes)
}

/// Computes the CRC32C checksum of `bytes`.
#[must_use]
pub fn compute(bytes: &[u8]) -> u32 {
    crc32c::compute(bytes)
}

/// Returns the running CRC32C state initial value.
#[must_use]
pub fn crc_init() -> u32 {
    crc32c::crc_init()
}

/// Advances `crc` across `bytes` and returns the new running state.
#[must_use]
pub fn crc_update(crc: u32, bytes: &[u8]) -> u32 {
    crc32c::crc_update(crc, bytes)
}

/// Produces the final checksum from a running state.
#[must_use]
pub fn crc_finalize(crc: u32) -> u32 {
    crc32c::crc_finalize(crc)
}

/// Verifies that `bytes` has the expected CRC32C checksum.
pub fn verify_checksum(bytes: &[u8], expected: u32) -> Result<()> {
    crc32c::verify(bytes, expected).map_err(|mismatch| {
        PlomidError::new(
            ErrorKind::Corruption,
            format!(
                "checksum mismatch (expected {expected:#010x}, got {actual:#010x})",
                actual = mismatch.actual
            ),
        )
    })
}

/// Verifies that `bytes` has the expected CRC32C checksum.
pub fn verify(bytes: &[u8], expected: u32) -> Result<()> {
    verify_checksum(bytes, expected)
}

#[cfg(test)]
mod tests {
    use super::{compute_checksum, verify_checksum};
    use plomid_core::ErrorKind;

    #[test]
    fn crc32c_known_answer() {
        assert_eq!(compute_checksum(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn checksum_is_stable() {
        let bytes = b"page header and payload";
        assert_eq!(compute_checksum(bytes), compute_checksum(bytes));
        assert!(verify_checksum(bytes, compute_checksum(bytes)).is_ok());
    }

    #[test]
    fn one_bit_flip_is_detected_as_corruption() {
        let mut bytes = *b"wal record";
        let expected = compute_checksum(&bytes);
        bytes[0] ^= 1;

        let error = verify_checksum(&bytes, expected).expect_err("bit flip must fail verification");
        assert_eq!(error.kind(), ErrorKind::Corruption);
    }
}
