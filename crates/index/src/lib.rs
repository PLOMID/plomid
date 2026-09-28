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
//! Adaptive Radix Tree (ART) and persistent B+Tree indexes for PLOMID.
//!
//! This crate holds the index-layer abstractions that sit above the storage,
//! WAL, transaction, and MVCC subsystems (see section 2 of the architecture
//! brief).
//!
//! * `art` — an in-memory adaptive radix tree mapping byte-string keys to
//!   logical [`RowId`] references. It is intentionally independent of the
//!   storage engine, WAL, MVCC, and SQL layers (see [`ArtIndex`]).
//! * `btree` — a persistent, ordered B+Tree SQL index. It reuses PLOMID's
//!   existing page allocator ([`BufferPool`]), page format ([`Page`]), CRC32C
//!   page integrity, and WAL recovery path. It owns only the ordered key
//!   structure, node organization, and index lookup semantics (see
//!   [`btree::BTreeIndex`]).
//!
//! # Concurrency
//!
//! Mirroring the single-writer B+Tree in `plomid-storage`, all operations take
//! `&mut self` / `&self` with no internal locking. Callers must serialize
//! access (as the engine already does with its engine-level lock). No lock-free
//! or concurrent behavior is claimed.

#![forbid(unsafe_code)]

mod constants;
mod error;
mod invariant;
mod key;
mod leaf;
mod node;
mod tree;

pub mod btree;
pub mod generation;

pub use error::ArtError;
pub use generation::{
    AutomaticOutcome, IndexBuildFailPoint, IndexGcOutcome, IndexGenerationStore,
    PublishedIndexGeneration,
};
pub use invariant::{validate_tree, InvariantViolation};
pub use key::{common_prefix_len, encode_i64_ordered, encode_u64_be, ArtKey, ByteKey};
pub use leaf::{LeafInsert, LeafRemove, LeafValues};
pub use node::{Node, NodeKind};
pub use tree::{ArtIndex, ArtStats, IndexEntry, IndexKind};
