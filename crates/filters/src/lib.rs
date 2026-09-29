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
//! PLOMID 1G: SQL filtering data structures.
//!
//! This crate implements two filtering structures used in the SQL predicate
//! pruning pipeline:
//!
//! ```text
//! SQL predicate
//!     ↓
//! Zone Map        (plomid-columnar pruning)
//!     ↓
//! BRIN            (plomid-columnar pruning)
//!     ↓
//! XOR filter      (this crate — approximate membership)
//!     ↓
//! Roaring bitmap  (this crate — exact candidate row-position sets)
//!     ↓
//! exact predicate evaluation
//! ```
//!
//! # Roaring bitmap ([`roaring`])
//!
//! A Roaring-style compressed bitmap over a 32-bit value space. The space is
//! divided into `2^16` buckets of 65 536 values each. Each non-empty bucket
//! is stored in the most compact of three container types:
//!
//! - **`Array`** — sorted `u16` values; optimal when the bucket cardinality is
//!   small (≤ 4 095).
//! - **`Bitmap`** — 1 024 64-bit words (8 192 bytes); optimal when the bucket
//!   is dense (≥ 4 096 set bits).
//! - **`Run`** — run-length encoded `(start, length)` pairs; optimal when the
//!   bucket contains long consecutive stretches.
//!
//! The container type is chosen automatically and may change as values are
//! inserted or removed. Serialization is deterministic: two bitmaps with the
//! same contents always encode to identical bytes.
//!
//! # XOR filter ([`xor`])
//!
//! A probabilistic membership filter with the following guarantees:
//!
//! - **No false negatives**: if `contains(x)` returns `false`, then `x` is
//!   definitely not in the set.
//! - **Possible false positives**: `contains(x)` may return `true` for an
//!   element that was never inserted; the false-positive rate is ~0.83% with
//!   8-bit fingerprints.
//! - **Controlled construction failure**: construction either succeeds with a
//!   valid filter or returns a controlled error; an invalid filter is never
//!   silently constructed.
//!
//! XOR filters are never authoritative query-result structures. They exist
//! only to cheaply eliminate candidates — a `false` answer from the XOR
//! filter allows a caller to skip expensive exact evaluation, but a `true`
//! answer always falls through to exact processing.
//!
//! # Persistence
//!
//! Both structures serialize to a self-describing little-endian binary format
//! with:
//!
//! - a 4-byte magic (`"PLRB"` / `"PLXF"`),
//! - a format version (rejected if unknown),
//! - explicit counts, lengths, and reserved fields,
//! - a CRC32C checksum over the payload,
//! - cross-validated length fields.
//!
//! CRC32C comes directly from the isolated [`crc32c`] component; no second
//! checksum implementation is introduced.
//!
//! # Physical placement
//!
//! Filter metadata belongs to the logical SQL object / generation
//! architecture. Physical bytes are ultimately allocated through the existing
//! storage layer — the directory `PLOMID_DATA/shared/filters/` is managed by
//! the generation / storage manager, and filter objects carry no hardcoded
//! device paths.
//!
//! Do not create a filter-specific WAL, MVCC, checkpoint, or recovery
//! pipeline. Filters are rebuilt from the materialized columns at flush time
//! and are therefore recoverable from the existing generation machinery.
//!
//! The crate must not depend on [`plomid_storage`]: storage segments use these
//! filters for sealed-segment negative pruning, so the dependency runs
//! storage → filters, never the reverse.

#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery, missing_docs)]

mod constants;
mod layout;

pub mod roaring;
pub mod xor;

pub use constants::{
    ROARING_FORMAT_VERSION, ROARING_MAGIC, ROARING_MAX_CONTAINERS, XOR_FILTER_FORMAT_VERSION,
    XOR_FILTER_MAGIC, XOR_MAX_ELEMENTS,
};
pub use roaring::{
    Container, ContainerIter, Iter, RelationalOp, RoaringBitmap, RoaringError, RunWord,
};
pub use xor::{XorError, XorFilter};

/// CRC32C convenience for callers that verify filter images externally.
#[must_use]
pub fn compute_checksum(bytes: &[u8]) -> u32 {
    crc32c::compute(bytes)
}

/// Verifies `bytes` against an expected CRC32C checksum.
///
/// # Errors
///
/// Returns [`XorError::Corrupt`] when the computed checksum differs from
/// `expected`.
pub fn verify_checksum(bytes: &[u8], expected: u32) -> Result<(), XorError> {
    crc32c::verify(bytes, expected).map_err(|_| XorError::Corrupt("checksum mismatch"))
}
