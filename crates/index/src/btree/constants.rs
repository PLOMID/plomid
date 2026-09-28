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
//! Format constants for the persistent SQL B+Tree index.
//!
//! The on-disk magic numbers (`PLBT` for nodes, `PLRT` for root metadata) and
//! type codes are shared with the storage-layer B+Tree via
//! [`plomid_core::constants`] so the whole workspace presents a single source of
//! truth for PLOMID structure magics. No index-specific magic is introduced.
//!
//! The root-metadata block itself lives in [`super::root_meta`]; the
//! `INDEX_*` / `ROOT_META_SIZE` constants here describe its fixed layout.

use plomid_core::PAGE_DATA_SIZE;

// Shared magics and type codes, re-exported for sibling modules. The aliased
// forms keep the body of this file short; the original names are available too.
pub(crate) use plomid_core::{
    BTREE_INTERNAL as INTERNAL, BTREE_LEAF as LEAF, BTREE_NODE_MAGIC as NODE_MAGIC,
    BTREE_NO_PAGE as NO_PAGE, BTREE_ROOT_MAGIC as ROOT_MAGIC,
};

/// Fixed-width node header: `magic[4] | type[u8] | reserved[1] | count[u16] |
/// link_a[u64] | link_b[u64]`.
pub(crate) const NODE_HEADER_SIZE: usize = 24;

/// On-disk format version of the index root-metadata block.
pub(crate) const INDEX_FORMAT_VERSION: u32 = 1;

/// Root-metadata flag: the index enforces key uniqueness.
pub(crate) const INDEX_FLAG_UNIQUE: u32 = 1;

/// Root-metadata flag mask (all currently known bits).
pub(crate) const INDEX_FLAG_MASK: u32 = INDEX_FLAG_UNIQUE;

/// Encoded size of the root-metadata block stored in page 0.
pub(crate) const ROOT_META_SIZE: usize = 52;

/// Usable payload of a 16 KiB page (minus the page header and trailer).
pub(crate) const PAGE_PAYLOAD_SIZE: usize = PAGE_DATA_SIZE;

/// Minimum number of entries a non-root node must hold.
///
/// Occupancy rule: a non-root node is *underfull* (a merge candidate) when it
/// is empty. This keeps the rebalance logic simple — merge with a sibling
/// whenever a node empties, redistribute only across a merge boundary — while
/// the root is exempt (an empty root leaf is a valid empty index; an internal
/// root with one child contracts to that child). Correctness-first deletion
/// per the architecture brief; eager fill-factor tuning is a future
/// refinement.
pub(crate) const NODE_MIN_ENTRIES: usize = 1;

/// RowId encoding width (little-endian `u64`).
pub(crate) const ROW_ID_SIZE: usize = 8;

/// `u16` length-prefix width used for keys and RowId counts.
pub(crate) const LEN_U16: usize = 2;
