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
//! Filter format constants.
//!
//! The byte-level constants — magics, format versions, container limits, XOR
//! parameters, footer size — live in [`plomid_core`] as the single source of
//! truth for the whole engine and are re-exported here, so a filter module
//! imports exactly one constants module (the same convention the columnar
//! `format` module follows).
//!
//! Only constants that are meaningful *inside* this crate's algorithms are
//! defined below; they are derived from the shared ones instead of being
//! written out again.

pub use plomid_core::{
    ROARING_ARRAY_LAZY_MAX, ROARING_BITMAP_BYTES, ROARING_BITMAP_WORDS, ROARING_CONTAINER_ARRAY,
    ROARING_CONTAINER_BITMAP, ROARING_CONTAINER_RUN, ROARING_FOOTER_LEN, ROARING_FORMAT_VERSION,
    ROARING_MAGIC, ROARING_MAX_CONTAINERS, XOR_ALPHA_DEN, XOR_ALPHA_NUM, XOR_FILTER_FORMAT_VERSION,
    XOR_FILTER_MAGIC, XOR_FINGERPRINT_BITS, XOR_FINGERPRINT_MODULUS, XOR_FOOTER_LEN,
    XOR_MAX_BUILD_ATTEMPTS, XOR_MAX_ELEMENTS, XOR_MAX_SLOTS, XOR_SLOTS_SLACK,
};

/// Values addressed by one container: the 16-bit offset space.
pub(crate) const CONTAINER_VALUES: u32 = 1 << 16;

/// Cardinality at (and below) which an array container is the most compact
/// representation. Above it a container is stored as a bitmap or as runs.
pub(crate) const ARRAY_MAX_CARDINALITY: u32 = ROARING_ARRAY_LAZY_MAX as u32;

/// Bits addressed by one bitmap word.
pub(crate) const BITMAP_WORD_BITS: u32 = 64;

/// Bytes occupied by one array-container entry (`u16` value).
pub(crate) const ARRAY_ENTRY_BYTES: usize = 2;

/// Bytes occupied by one run-container pair (`start[u16]` + `length[u16]`).
pub(crate) const RUN_PAIR_BYTES: usize = 4;
