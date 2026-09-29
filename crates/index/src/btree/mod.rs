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
//! Persistent B+Tree SQL index.
//!
//! This module is the durable, ordered index structure of PLOMID. It reuses
//! the existing storage hierarchy (16 KiB [`Page`] frames through
//! [`BufferPool`]), the canonical CRC32C page integrity boundaries, and the
//! existing WAL recovery path. It deliberately does not own a WAL, a
//! transaction manager, an allocator, or a checksum implementation.
//!
//! # Module map
//!
//! * [`constants`] — fixed-width format constants (magics shared with
//!   `plomid_core`).
//! * [`root_meta`] — page-0 root-metadata block (index identity, root page,
//!   format version, generation binding).
//! * [`node`] — leaf/internal node encoding and decoding.
//! * [`entry`] — public entry types and range bounds.
//! * [`tree`] — [`BTreeIndex`]: search, insert, split, delete, merge,
//!   rebalance, persistence, WAL replay target, transaction adapter.

mod constants;
mod entry;
mod node;
mod root_meta;
mod tree;

pub use entry::{Bound, IndexEntry};
pub use tree::{BTreeIndex, BTreeStats};
