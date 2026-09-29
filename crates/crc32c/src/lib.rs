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
//! Isolated CRC32C (Castagnoli) component for PLOMID.
//!
//! # Safety boundary
//!
//! The PLOMID storage crate keeps `#![forbid(unsafe_code)]`; it only consumes
//! the safe API below. This component is the single place in the workspace
//! permitted to reach CPU-specific CRC instructions (`_mm_crc32_u{8,32,64}`
//! on x86_64, `__crc32{b,w,d}` on aarch64). The crate-level lint override in
//! `Cargo.toml` exists solely for that purpose; the portable software path
//! (`software::`) re-forbids `unsafe` internally, so `unsafe` blocks appear
//! only inside `x86_64::` and `aarch64::`, each guarded by a CPU-feature check
//! performed *before* the implementation is ever selected.
//!
//! # Compatibility
//!
//! Algorithm, polynomial, width, and initialization/finalization semantics are
//! byte-for-byte identical to the previous in-crate implementation:
//!
//! ```text
//! CRC32C / Castagnoli, reflected polynomial 0x82F63B78
//! initial state 0xFFFFFFFF, final XOR 0xFFFFFFFF
//! CRC32C("123456789") = 0xE3069283
//! ```
//!
//! Persisted checksums are unchanged; the streaming state used by
//! [`crc_init`]/[`crc_update`]/[`crc_finalize`] is the same running value as
//! before, so page and WAL layouts are untouched.
//!
//! # Dispatch
//!
//! CPU feature detection happens once, outside the inner loop, and the chosen
//! implementation is cached in a [`OnceLock`] as a plain function pointer.
//! No allocation occurs on the checksum path; every function operates
//! directly on `&[u8]`.

/// Portable software fallback; also the correctness oracle's fast
/// counterpart. `doc(hidden)`: production callers use the dispatching API.
#[doc(hidden)]
pub mod software;

/// Bitwise reference implementation (validation oracle and benchmark
/// baseline). `doc(hidden)`: intentionally slow, never for production use.
#[doc(hidden)]
pub mod reference;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

use std::sync::OnceLock;

/// Shape of an implementation's streaming update step.
type UpdateFn = fn(u32, &[u8]) -> u32;

/// Selected implementation, resolved once on first use.
static UPDATE: OnceLock<UpdateFn> = OnceLock::new();

/// Picks the fastest implementation supported by this CPU. Called at most
/// once per process; feature detection is never inside the data loop.
#[cfg(target_arch = "x86_64")]
fn select() -> UpdateFn {
    if std::arch::is_x86_feature_detected!("sse4.2") {
        x86_64::update
    } else {
        software::update
    }
}

/// Picks the fastest implementation supported by this CPU. Called at most
/// once per process; feature detection is never inside the data loop.
#[cfg(target_arch = "aarch64")]
fn select() -> UpdateFn {
    // The `crc` feature is the Armv8.0-A CRC-32 checksum extension (ARM64
    // CRC32C). Not every aarch64 CPU implements it; Apple Silicon and
    // server parts such as Graviton do.
    if std::arch::is_aarch64_feature_detected!("crc") {
        aarch64::update
    } else {
        software::update
    }
}

/// Other targets always use the portable software path.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn select() -> UpdateFn {
    software::update
}

#[inline]
fn dispatch() -> UpdateFn {
    *UPDATE.get_or_init(select)
}

/// Returns the running CRC32C state initial value.
#[must_use]
pub fn crc_init() -> u32 {
    u32::MAX
}

/// Advances `crc` across `bytes` and returns the new running state.
///
/// Feed contiguous segments in order in any number of calls; finish with
/// [`crc_finalize`]. Streaming must produce the same value as one-shot
/// [`compute`] over the concatenation of the segments.
#[must_use]
pub fn crc_update(crc: u32, bytes: &[u8]) -> u32 {
    dispatch()(crc, bytes)
}

/// Produces the final checksum from a running state.
#[must_use]
pub fn crc_finalize(crc: u32) -> u32 {
    !crc
}

/// Computes the CRC32C checksum of `bytes` in one call.
#[must_use]
pub fn compute(bytes: &[u8]) -> u32 {
    crc_finalize(crc_update(crc_init(), bytes))
}

/// Describes a failed checksum verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mismatch {
    /// Checksum the caller expected to find.
    pub expected: u32,
    /// Checksum actually computed from the bytes.
    pub actual: u32,
}

/// Verifies that `bytes` has the expected CRC32C checksum.
///
/// Callers translate [`Mismatch`] into their own corruption error kind; this
/// component never panics, ignores, repairs, or downgrades a mismatch.
pub fn verify(bytes: &[u8], expected: u32) -> Result<(), Mismatch> {
    let actual = compute(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(Mismatch { expected, actual })
    }
}

/// Reports whether the selected implementation uses CPU CRC instructions.
///
/// Purely informational (used by tests and diagnostics); the checksum result
/// never depends on it.
#[must_use]
pub fn hardware_accelerated() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("sse4.2")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("crc")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// Test and benchmark hook: the hardware update function when this CPU
/// supports CRC instructions, `None` otherwise. Never used by production
/// callers, who go through [`crc_update`].
#[doc(hidden)]
#[must_use]
pub fn dispatch_hw() -> Option<UpdateFn> {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse4.2") {
            return Some(x86_64::update);
        }
        None
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("crc") {
            return Some(aarch64::update);
        }
        None
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        None
    }
}

#[cfg(test)]
mod tests;
