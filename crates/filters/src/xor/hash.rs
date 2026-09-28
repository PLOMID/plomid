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
//! Deterministic key hashing for the XOR filter.
//!
//! The filter needs three independent slot indices per key and one
//! fingerprint. Rather than pull in a hashing crate, this module implements the
//! one mixer the structure requires — documented, tested, and independent of
//! the rest of the engine, so the persisted image is reproducible across builds
//! and platforms.
//!
//! ```text
//! key bytes ──fnv1a─▶ 64-bit seed ──splitmix64─▶ h0 ‖ h1 ‖ h2 ‖ fingerprint
//! ```
//!
//! `h0`, `h1`, `h2` are derived from one 64-bit value by [splitmix64]
//! iteration, so the filter's correctness never depends on the quality of an
//! external hash: the constructor verifies the built filter against every
//! inserted key before returning it.
//!
//! [splitmix64]: https://prng.di.unimi.it/splitmix64.c

use crate::constants::XOR_FINGERPRINT_BITS;

/// The 64-bit FNV-1a offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// The 64-bit FNV-1a prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// One splitmix64 step.
///
/// Advances `state` and returns the mixed value; the caller keeps the state.
/// This is the single mixing primitive for both seed generation and hash
/// derivation.
#[must_use]
pub(crate) fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Hashes a key to a 64-bit value with FNV-1a.
#[must_use]
pub(crate) fn hash_key(key: &[u8]) -> u64 {
    let mut digest = FNV_OFFSET_BASIS;
    for byte in key {
        digest ^= u64::from(*byte);
        digest = digest.wrapping_mul(FNV_PRIME);
    }
    digest
}

/// The three slot indices and the fingerprint of one key.
///
/// `a`, `b`, and `c` always address distinct slots of a filter with
/// [`slots`](XorFilter::slots) slots; `fingerprint` is in
/// `1..XOR_FINGERPRINT_MODULUS` (never zero, so an all-zero slot array cannot
/// claim membership for any key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyHashes {
    /// First slot index.
    pub(crate) a: usize,
    /// Second slot index.
    pub(crate) b: usize,
    /// Third slot index.
    pub(crate) c: usize,
    /// Non-zero fingerprint in `1..2^XOR_FINGERPRINT_BITS`.
    pub(crate) fingerprint: u64,
}

/// Derives the slot indices and fingerprint of one key hash.
///
/// The three indices are drawn from four consecutive splitmix64 outputs and
/// mapped into the three consecutive thirds of the slot array:
///
/// ```text
/// slots  = 3 * block (+ tail)      block = slots / 3
/// a ∈ [0, block)   b ∈ [block, 2·block)   c ∈ [2·block, 3·block)
/// ```
///
/// Because the thirds do not overlap, `a`, `b`, and `c` are always three
/// *distinct* slots — the property the peel step relies on. Callers guarantee
/// `slots >= 3` so each third is non-empty. The fingerprint is never zero: an
/// all-zero slot array must not claim membership for any key.
#[must_use]
pub(crate) fn derive(key_hash: u64, slots: usize, seed: u64) -> KeyHashes {
    let mut state = key_hash ^ seed.wrapping_mul(FNV_PRIME);
    let first = splitmix64(&mut state);
    let second = splitmix64(&mut state);
    let third = splitmix64(&mut state);
    let fourth = splitmix64(&mut state);

    let block = (slots / 3).max(1);
    let mask = (1u64 << XOR_FINGERPRINT_BITS) - 1;
    KeyHashes {
        a: (first % block as u64) as usize,
        b: block + (second % block as u64) as usize,
        c: (2 * block + (third % block as u64) as usize).min(slots.saturating_sub(1)),
        fingerprint: (fourth & mask).max(1),
    }
}
