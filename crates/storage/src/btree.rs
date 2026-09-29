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
//! Single-writer on-disk B+Tree over [`crate::BufferPool`].
//!
//! Page zero is reserved for root metadata (`PLRT` plus a root page ID), and
//! all subsequent pages contain checksum-protected B+Tree nodes. Leaf values
//! are opaque bytes; callers may store values produced by the row codec
//! directly. Keys are compared lexicographically as byte slices.
//!
//! This v1 implementation is intentionally single-threaded: all mutation and
//! reads require `&mut self` because page handles are explicitly pinned and
//! unpinned. Range results are collected into an owned vector. The tree uses
//! recursive leaf/internal splits and an LRU buffer-pool policy underneath.

use crate::{BufferPool, PAGE_DATA_SIZE};
use plomid_core::{ErrorKind, PageId, PlomidError, Result};
use std::path::Path;

// B+Tree constants are defined once in `plomid_core::constants`; the
// module-local names below keep the body of this file unchanged.
use plomid_core::{
    BTREE_INTERNAL as INTERNAL, BTREE_LEAF as LEAF, BTREE_NODE_MAGIC as NODE_MAGIC,
    BTREE_NO_PAGE as NO_PAGE, BTREE_ROOT_MAGIC as ROOT_MAGIC,
};

#[derive(Clone, Debug)]
enum Node {
    Leaf {
        entries: Vec<(Vec<u8>, Vec<u8>)>,
        next: Option<PageId>,
    },
    Internal {
        first: PageId,
        separators: Vec<(Vec<u8>, PageId)>,
    },
}

#[derive(Debug)]
struct Split {
    separator: Vec<u8>,
    right: PageId,
}

/// A durable, single-writer B+Tree backed by real page files.
pub struct BTree {
    pool: BufferPool,
    root: PageId,
}

impl BTree {
    /// Creates a new tree in a truncated real page file.
    pub fn create(path: &Path, pool_capacity: usize) -> Result<Self> {
        tracing::info!(target: "storage", "create path={} capacity={}", path.display(), pool_capacity);
        Self::create_in(BufferPool::create(path, pool_capacity)?)
    }

    /// Initializes a tree in an empty page view without truncating its container.
    pub(crate) fn create_in(mut pool: BufferPool) -> Result<Self> {
        let metadata_id = pool.allocate_page_id()?;
        if metadata_id != PageId::new(0) {
            return Err(corruption("root metadata page is not page zero"));
        }
        let root_id = pool.allocate_page_id()?;
        write_node(
            &mut pool,
            root_id,
            &Node::Leaf {
                entries: Vec::new(),
                next: None,
            },
        )?;
        write_root(&mut pool, metadata_id, root_id)?;
        pool.sync()?;
        Ok(Self {
            pool,
            root: root_id,
        })
    }

    /// Opens an existing real page file and loads its persisted root ID.
    pub fn open(path: &Path, pool_capacity: usize) -> Result<Self> {
        tracing::info!(target: "storage", "open path={} capacity={}", path.display(), pool_capacity);
        Self::open_in(BufferPool::open(path, pool_capacity)?)
    }

    /// Loads a tree from a page view supplied by physical storage.
    pub(crate) fn open_in(mut pool: BufferPool) -> Result<Self> {
        let root = read_root(pool.data(PageId::new(0))?)?;
        Ok(Self { pool, root })
    }

    /// Inserts or replaces the value for `key`.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        tracing::trace!(target: "storage", "insert key_len={} value_len={}", key.len(), value.len());
        if let Some(split) = self.insert_node(self.root, key, value)? {
            let old_root = self.root;
            let new_root = self.pool.allocate_page_id()?;
            let node = Node::Internal {
                first: old_root,
                separators: vec![(split.separator, split.right)],
            };
            write_node(&mut self.pool, new_root, &node)?;
            self.root = new_root;
            self.update_root_metadata()?;
        }
        Ok(())
    }

    /// Replaces an existing value without doing a separate lookup.
    ///
    /// The returned flag is determined by the leaf traversal that performs the
    /// replacement, so callers can probe a chain of immutable generations
    /// without paying for `get` followed by `insert` on the matching tree.
    pub fn replace_if_present(&mut self, key: &[u8], value: &[u8]) -> Result<bool> {
        tracing::trace!(target: "storage", "replace_if_present key_len={} value_len={}", key.len(), value.len());
        self.replace_node(self.root, key, value)
    }

    /// Deletes `key`, returning whether an entry was removed.
    ///
    /// V1 does not rebalance underfull nodes. Stale separators remain valid
    /// routing boundaries because they are lower bounds for their right child.
    pub fn delete(&mut self, key: &[u8]) -> Result<bool> {
        tracing::trace!(target: "storage", "delete key_len={}", key.len());
        self.delete_node(self.root, key)
    }

    /// Returns the value associated with `key`, if present.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        tracing::trace!(target: "storage", "get key_len={}", key.len());
        self.get_node(self.root, key)
    }

    /// Returns entries with `start <= key < end`, in key order.
    pub fn range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        tracing::trace!(target: "storage", "range start={:?} end={:?}", start.map(|s| s.len()), end.map(|e| e.len()));
        let mut entries = Vec::new();
        self.collect_range(self.root, start, end, &mut entries)?;
        Ok(entries)
    }

    /// Flushes dirty tree pages and the root metadata durably.
    pub fn sync(&mut self) -> Result<()> {
        tracing::trace!(target: "storage", "sync");
        self.pool.sync()
    }

    /// Bytes buffered dirty above the file image (see
    /// [`BufferPool::dirty_bytes`]).
    pub(crate) fn dirty_bytes(&self) -> u64 {
        self.pool.dirty_bytes()
    }

    fn update_root_metadata(&mut self) -> Result<()> {
        write_root(&mut self.pool, PageId::new(0), self.root)
    }

    fn insert_node(&mut self, page_id: PageId, key: &[u8], value: &[u8]) -> Result<Option<Split>> {
        // Fast path: an allocation-free in-place leaf update. Falls back to
        // the general path (returned as `None`) for splits and non-leaf
        // pages; behavior is otherwise identical.
        if try_upsert_leaf_in_place(&mut self.pool, page_id, key, value, false)?.is_some() {
            return Ok(None);
        }
        // Decode the node from the resident page without cloning it. Only the
        // branch that actually writes re-borrows the page for mutation.
        //
        // Internal levels never decode on the way down: the child is routed
        // borrowed (`route_internal_borrowed`) and the owned decode runs only
        // when a split below propagates upward (rare).
        let is_leaf = {
            let data = self.pool.data(page_id)?;
            if data.len() < 16 || data[..4] != NODE_MAGIC {
                return Err(corruption("invalid B+Tree node header"));
            }
            data[4] == LEAF
        };
        if !is_leaf {
            let child = {
                let data = self.pool.data(page_id)?;
                route_internal_borrowed(data, key)?
            };
            let split = self.insert_node(child, key, value)?;
            // No split below: this node is unchanged and was never copied.
            let Some(split) = split else {
                return Ok(None);
            };
            let node = decode_node(self.pool.data(page_id)?)?;
            let Node::Internal {
                first,
                mut separators,
            } = node
            else {
                return Err(corruption("B+Tree ancestor must be internal"));
            };
            let position = separators
                .binary_search_by(|(stored, _)| stored.as_slice().cmp(&split.separator))
                .unwrap_or_else(|position| position);
            separators.insert(position, (split.separator, split.right));
            if internal_encoded_len(&separators) <= PAGE_DATA_SIZE {
                write_node(
                    &mut self.pool,
                    page_id,
                    &Node::Internal { first, separators },
                )?;
                return Ok(None);
            }
            if separators.len() < 2 {
                return Err(corruption("internal node cannot be split"));
            }
            let middle = separators.len() / 2;
            let promoted = separators[middle].0.clone();
            let left = Node::Internal {
                first,
                separators: separators[..middle].to_vec(),
            };
            let right_first = separators[middle].1;
            let right = Node::Internal {
                first: right_first,
                separators: separators[middle + 1..].to_vec(),
            };
            let right_id = self.pool.allocate_page_id()?;
            write_node(&mut self.pool, page_id, &left)?;
            write_node(&mut self.pool, right_id, &right)?;
            return Ok(Some(Split {
                separator: promoted,
                right: right_id,
            }));
        }
        let node = decode_node(self.pool.data(page_id)?)?;
        match node {
            Node::Leaf { mut entries, next } => {
                match entries.binary_search_by(|(stored, _)| stored.as_slice().cmp(key)) {
                    Ok(index) => entries[index].1 = value.to_vec(),
                    Err(index) => entries.insert(index, (key.to_vec(), value.to_vec())),
                }
                if leaf_encoded_len(&entries) <= PAGE_DATA_SIZE {
                    write_node(&mut self.pool, page_id, &Node::Leaf { entries, next })?;
                    return Ok(None);
                }
                if entries.len() < 2 {
                    return Err(PlomidError::new(
                        ErrorKind::InvalidArgument,
                        "key/value pair is too large for a page",
                    ));
                }
                // Split by encoded size, not by entry count: entries differ
                // widely in size (small row updates next to multi-kilobyte
                // payloads), and a count-based midpoint can produce a half that
                // itself exceeds the page payload and cannot be written. The
                // boundary closest to the byte midpoint minimizes the larger
                // half; if even that half cannot fit, the tree genuinely cannot
                // hold the node and the write is rejected.
                let entry_bytes = |entry: &(Vec<u8>, Vec<u8>)| {
                    // 16-byte node header is amortized per node; per entry the
                    // encoder writes a u16 key length, a u32 value length, the
                    // key, and the value.
                    6 + entry.0.len() + entry.1.len()
                };
                let total: usize = entries.iter().map(entry_bytes).sum();
                let mut cumulative = 0_usize;
                let mut split_at = 1_usize;
                let mut best = usize::MAX;
                for (index, entry) in entries.iter().enumerate() {
                    if index > 0 && index < entries.len() {
                        let larger = usize::max(cumulative, total - cumulative);
                        if larger < best {
                            best = larger;
                            split_at = index;
                        }
                    }
                    cumulative += entry_bytes(entry);
                }
                let right_entries = entries[split_at..].to_vec();
                let left_entries = entries[..split_at].to_vec();
                let separator = right_entries[0].0.clone();
                let right_id = self.pool.allocate_page_id()?;
                let left = Node::Leaf {
                    entries: left_entries,
                    next: Some(right_id),
                };
                let right = Node::Leaf {
                    entries: right_entries,
                    next,
                };
                write_node(&mut self.pool, page_id, &left)?;
                write_node(&mut self.pool, right_id, &right)?;
                Ok(Some(Split {
                    separator,
                    right: right_id,
                }))
            }
            Node::Internal { .. } => {
                // Unreachable: internal pages return above through borrowed
                // routing. If the page kind changed between the two borrows
                // the tree is corrupt.
                Err(corruption("B+Tree ancestor must be internal"))
            }
        }
    }

    fn get_node(&mut self, page_id: PageId, key: &[u8]) -> Result<Option<Vec<u8>>> {
        // Borrowed descent: route through internal nodes and search the leaf
        // without decoding entries into owned vectors. The general path
        // allocates two `Vec<u8>` per entry per level (~13us for a 150-entry
        // leaf); this walk borrows keys in place and clones only a found
        // value. Corruption errors mirror `decode_node`.
        enum Step {
            /// Leaf answer: the cloned value or a proven miss.
            Found(Option<Vec<u8>>),
            /// Internal routing decision: descend into this child.
            Descend(PageId),
        }
        let step = {
            let data = self.pool.data(page_id)?;
            if data.len() < 16 || data[..4] != NODE_MAGIC {
                return Err(corruption("invalid B+Tree node header"));
            }
            match data[4] {
                LEAF => {
                    let _ = u64::from_le_bytes(
                        data[8..16]
                            .try_into()
                            .map_err(|_| corruption("invalid leaf link"))?,
                    );
                    let count = usize::from(u16::from_le_bytes([data[6], data[7]]));
                    let mut cursor = 16usize;
                    let mut found = None;
                    for _ in 0..count {
                        let (stored, vlen_off, vstart) = leaf_entry_at(data, cursor)?;
                        match stored.cmp(key) {
                            // Entries sort ascending: a greater stored key
                            // proves absence without scanning the rest.
                            std::cmp::Ordering::Equal => {
                                let vlen = leaf_value_len(data, vlen_off, vstart)?;
                                let vend = vstart
                                    .checked_add(vlen)
                                    .ok_or_else(|| corruption("node length overflow"))?;
                                found = Some(
                                    data.get(vstart..vend)
                                        .ok_or_else(|| corruption("B+Tree node is truncated"))?
                                        .to_vec(),
                                );
                                break;
                            }
                            std::cmp::Ordering::Greater => break,
                            std::cmp::Ordering::Less => {
                                let vlen = leaf_value_len(data, vlen_off, vstart)?;
                                cursor = vstart
                                    .checked_add(vlen)
                                    .ok_or_else(|| corruption("node length overflow"))?;
                                if cursor > data.len() {
                                    return Err(corruption("B+Tree node is truncated"));
                                }
                            }
                        }
                    }
                    Step::Found(found)
                }
                INTERNAL => {
                    let mut child = PageId::new(u64::from_le_bytes(
                        data[8..16]
                            .try_into()
                            .map_err(|_| corruption("invalid internal child"))?,
                    ));
                    let count = usize::from(u16::from_le_bytes([data[6], data[7]]));
                    let mut cursor = 16usize;
                    for _ in 0..count {
                        let (stored, kend) = internal_key_at(data, cursor)?;
                        // `child_for` semantics: the last separator not
                        // greater than the key wins; a greater separator ends
                        // the walk with the current child.
                        if key >= stored {
                            let coff = kend;
                            child = PageId::new(u64::from_le_bytes(
                                data.get(coff..coff + 8)
                                    .ok_or_else(|| corruption("B+Tree node is truncated"))?
                                    .try_into()
                                    .map_err(|_| corruption("invalid child page"))?,
                            ));
                        } else {
                            break;
                        }
                        cursor = kend
                            .checked_add(8)
                            .ok_or_else(|| corruption("node length overflow"))?;
                        if cursor > data.len() {
                            return Err(corruption("B+Tree node is truncated"));
                        }
                    }
                    Step::Descend(child)
                }
                _ => return Err(corruption("unknown B+Tree node kind")),
            }
        };
        match step {
            Step::Found(value) => Ok(value),
            Step::Descend(child) => self.get_node(child, key),
        }
    }

    fn replace_node(&mut self, page_id: PageId, key: &[u8], value: &[u8]) -> Result<bool> {
        // Fast path with `require_present`: present keys update in place,
        // absent keys report `false` without decoding the page.
        if let Some(replaced) = try_upsert_leaf_in_place(&mut self.pool, page_id, key, value, true)?
        {
            return Ok(replaced);
        }
        let mut node = decode_node(self.pool.data(page_id)?)?;
        match &mut node {
            Node::Leaf { entries, .. } => {
                let Ok(index) = entries.binary_search_by(|(stored, _)| stored.as_slice().cmp(key))
                else {
                    return Ok(false);
                };
                entries[index].1.clear();
                entries[index].1.extend_from_slice(value);
                // A larger replacement can push a full page past the payload:
                // the fast path above already declined exactly this shape, so
                // without a check this write is the "node exceeds page
                // payload" failure. Route through delete+insert instead, which
                // splits as needed. Removal only shrinks (the page held more
                // before), so the intermediate write always fits.
                if leaf_encoded_len(entries) <= PAGE_DATA_SIZE {
                    write_node(&mut self.pool, page_id, &node)?;
                    return Ok(true);
                }
                entries.remove(index);
                write_node(&mut self.pool, page_id, &node)?;
                drop(node);
                self.insert(key, value)?;
                Ok(true)
            }
            Node::Internal { first, separators } => {
                let child = child_for(separators, *first, key);
                self.replace_node(child, key, value)
            }
        }
    }

    fn delete_node(&mut self, page_id: PageId, key: &[u8]) -> Result<bool> {
        let mut node = decode_node(self.pool.data(page_id)?)?;
        match &mut node {
            Node::Leaf { entries, .. } => {
                let Ok(index) = entries.binary_search_by(|(stored, _)| stored.as_slice().cmp(key))
                else {
                    return Ok(false);
                };
                entries.remove(index);
                write_node(&mut self.pool, page_id, &node)?;
                Ok(true)
            }
            Node::Internal { first, separators } => {
                let child = child_for(separators, *first, key);
                self.delete_node(child, key)
            }
        }
    }

    fn collect_range(
        &mut self,
        page_id: PageId,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        output: &mut Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Result<()> {
        let node = decode_node(self.pool.data(page_id)?)?;
        match node {
            Node::Leaf { entries, .. } => {
                for (key, value) in entries {
                    if start.is_some_and(|bound| key.as_slice() < bound) {
                        continue;
                    }
                    if end.is_some_and(|bound| key.as_slice() >= bound) {
                        break;
                    }
                    output.push((key, value));
                }
                Ok(())
            }
            Node::Internal { first, separators } => {
                // Prune subtrees that cannot intersect `[start, end)`. Each
                // separator is a lower bound for the child to its right, so
                // child `index` covers keys in `[lower, upper)` where `lower`
                // is the previous separator (or unbounded for the first child)
                // and `upper` is this child's separator (or unbounded for the
                // last child). Visiting every child made an index-prefix scan
                // touch the whole tree, turning per-row unique-index checks
                // into O(N) work and bulk INSERT into O(N^2).
                for index in 0..=separators.len() {
                    let lower = index
                        .checked_sub(1)
                        .map(|previous| separators[previous].0.as_slice());
                    let upper = separators
                        .get(index)
                        .map(|(separator, _)| separator.as_slice());
                    // Entire child range is below `start`.
                    if let (Some(upper), Some(start)) = (upper, start) {
                        if upper <= start {
                            continue;
                        }
                    }
                    // Entire child range is at or above `end`.
                    if let (Some(lower), Some(end)) = (lower, end) {
                        if lower >= end {
                            continue;
                        }
                    }
                    let child = if index == 0 {
                        first
                    } else {
                        separators[index - 1].1
                    };
                    self.collect_range(child, start, end, output)?;
                }
                Ok(())
            }
        }
    }
}

fn write_root(pool: &mut BufferPool, page_id: PageId, root: PageId) -> Result<()> {
    let data = pool.data_mut(page_id)?;
    data.fill(0);
    data[..4].copy_from_slice(&ROOT_MAGIC);
    data[4..12].copy_from_slice(&root.get().to_le_bytes());
    // The root pointer must never reach the file ahead of the tree it names.
    pool.pin_structural(page_id)?;
    Ok(())
}

fn read_root(data: &[u8]) -> Result<PageId> {
    if data.len() < 12 || data[..4] != ROOT_MAGIC {
        return Err(corruption("invalid root metadata"));
    }
    Ok(PageId::new(u64::from_le_bytes(
        data[4..12]
            .try_into()
            .map_err(|_| corruption("invalid root page ID"))?,
    )))
}

/// Allocation-free in-place upsert into a leaf page's raw bytes.
///
/// The general path decodes every entry of the page into owned `Vec`s,
/// modifies one, and re-encodes the whole node — O(page) allocations and
/// copies per put. Since B+Tree puts touch exactly one leaf (plus rare
/// splits), this dominates bulk-insert cost. This fast path instead walks the
/// entries borrowed, then shifts page bytes with a single `memmove` and writes
/// only the new entry in place. No allocation, no full-page re-encode.
///
/// Returns `Ok(true)` when the put was applied, `Ok(false)` when the caller
/// must use the general path: non-leaf page, page would overflow (split),
/// entry counts that would overflow `u16`, or any unexpected page shape. The
/// fallback keeps behavior (including corruption errors) identical; this path
/// only accelerates shapes it fully understands.
fn try_upsert_leaf_in_place(
    pool: &mut BufferPool,
    page_id: PageId,
    key: &[u8],
    value: &[u8],
    require_present: bool,
) -> Result<Option<bool>> {
    let key_len = u16::try_from(key.len())
        .map_err(|_| PlomidError::new(ErrorKind::InvalidArgument, "B+Tree key is too large"))?;
    let value_len = u32::try_from(value.len())
        .map_err(|_| PlomidError::new(ErrorKind::InvalidArgument, "B+Tree value is too large"))?;
    // Borrowed walk: locate the key and record byte offsets without cloning
    // a single entry.
    enum Found {
        Present {
            entry_off: usize,
            val_off: usize,
            val_len: usize,
            end: usize,
            bound: usize,
        },
        InsertAt {
            entry_off: usize,
            end: usize,
            bound: usize,
            count: usize,
        },
    }
    let found = {
        let data = pool.data(page_id)?;
        if data.len() < 16 || data[..4] != NODE_MAGIC || data[4] != LEAF {
            return Ok(None);
        }
        let count = usize::from(u16::from_le_bytes([data[6], data[7]]));
        let bound = PAGE_DATA_SIZE.min(data.len());
        let mut cursor = 16usize;
        let mut located: Option<Found> = None;
        for _ in 0..count {
            let klen = usize::from(u16::from_le_bytes(
                data.get(cursor..cursor + 2)
                    .ok_or_else(|| corruption("B+Tree node is truncated"))?
                    .try_into()
                    .map_err(|_| corruption("B+Tree node is truncated"))?,
            ));
            let kstart = cursor + 2;
            let kend = kstart
                .checked_add(klen)
                .ok_or_else(|| corruption("node length overflow"))?;
            let vlen_off = kend;
            let vstart = vlen_off
                .checked_add(4)
                .ok_or_else(|| corruption("node length overflow"))?;
            let stored = data
                .get(kstart..kend)
                .ok_or_else(|| corruption("B+Tree node is truncated"))?;
            match stored.cmp(key) {
                std::cmp::Ordering::Greater => {
                    located = Some(Found::InsertAt {
                        entry_off: cursor,
                        end: 0,
                        bound: 0,
                        count: 0,
                    });
                    break;
                }
                std::cmp::Ordering::Equal => {
                    let vlen = usize::try_from(u32::from_le_bytes(
                        data.get(vlen_off..vstart)
                            .ok_or_else(|| corruption("B+Tree node is truncated"))?
                            .try_into()
                            .map_err(|_| corruption("B+Tree node is truncated"))?,
                    ))
                    .map_err(|_| corruption("B+Tree node is truncated"))?;
                    let vend = vstart
                        .checked_add(vlen)
                        .ok_or_else(|| corruption("node length overflow"))?;
                    data.get(vstart..vend)
                        .ok_or_else(|| corruption("B+Tree node is truncated"))?;
                    located = Some(Found::Present {
                        entry_off: cursor,
                        val_off: vstart,
                        val_len: vlen,
                        end: 0,
                        bound: 0,
                    });
                    break;
                }
                std::cmp::Ordering::Less => {
                    let vlen = usize::try_from(u32::from_le_bytes(
                        data.get(vlen_off..vstart)
                            .ok_or_else(|| corruption("B+Tree node is truncated"))?
                            .try_into()
                            .map_err(|_| corruption("B+Tree node is truncated"))?,
                    ))
                    .map_err(|_| corruption("B+Tree node is truncated"))?;
                    cursor = vstart
                        .checked_add(vlen)
                        .ok_or_else(|| corruption("node length overflow"))?;
                    if cursor > bound {
                        return Err(corruption("B+Tree node is truncated"));
                    }
                }
            }
        }
        // The payload end is recomputed arithmetically (no allocation): the
        // walk above stops early on match/greater, so its cursor is not the
        // end in those cases. This second walk also validates every entry the
        // first walk did not reach, so a corrupt tail cannot be silently
        // preserved by an in-place write.
        let end = leaf_payload_end(data).ok_or_else(|| corruption("B+Tree node is truncated"))?;
        if end > bound {
            return Err(corruption("B+Tree node is truncated"));
        }
        match located {
            Some(Found::Present {
                entry_off,
                val_off,
                val_len,
                ..
            }) => Found::Present {
                entry_off,
                val_off,
                val_len,
                end,
                bound,
            },
            Some(Found::InsertAt { entry_off, .. }) => Found::InsertAt {
                entry_off,
                end,
                bound,
                count,
            },
            None => Found::InsertAt {
                entry_off: end,
                end,
                bound,
                count,
            },
        }
    };
    match found {
        Found::Present {
            entry_off,
            val_off,
            val_len,
            end,
            bound,
        } => {
            if val_len == value.len() {
                let data = pool.data_mut(page_id)?;
                if val_off + val_len > data.len() {
                    return Ok(None);
                }
                data[val_off..val_off + val_len].copy_from_slice(value);
                return Ok(Some(true));
            }
            // Different-length replace: shift the tail in place, then write
            // the new entry over the old one. Still allocation-free.
            let old_len = (val_off + val_len) - entry_off;
            let new_len = 2 + key.len() + 4 + value.len();
            let new_end = end - old_len + new_len;
            if new_end > bound {
                return Ok(None);
            }
            let data = pool.data_mut(page_id)?;
            let tail_start = entry_off + old_len;
            if tail_start > end || end > data.len() {
                return Ok(None);
            }
            if new_len > old_len {
                data.copy_within(tail_start..end, tail_start + (new_len - old_len));
            } else if new_len < old_len {
                data.copy_within(tail_start..end, tail_start - (old_len - new_len));
            }
            write_entry_in_place(data, entry_off, key, value, key_len, value_len)?;
            Ok(Some(true))
        }
        Found::InsertAt {
            entry_off,
            end,
            bound,
            count,
        } => {
            if require_present {
                return Ok(Some(false));
            }
            let new_len = 2 + key.len() + 4 + value.len();
            if end + new_len > bound {
                return Ok(None);
            }
            let new_count = u16::try_from(count + 1).map_err(|_| {
                PlomidError::new(ErrorKind::InvalidArgument, "too many B+Tree entries")
            })?;
            let data = pool.data_mut(page_id)?;
            if end > data.len() || entry_off > end {
                return Ok(None);
            }
            data.copy_within(entry_off..end, entry_off + new_len);
            write_entry_in_place(data, entry_off, key, value, key_len, value_len)?;
            data[6..8].copy_from_slice(&new_count.to_le_bytes());
            Ok(Some(true))
        }
    }
}

/// Absolute end offset of a leaf page's entry payload (`16 + used`), derived
/// without allocation. Returns `None` on any unexpected shape (caller falls
/// back to the general path).
fn leaf_payload_end(data: &[u8]) -> Option<usize> {
    if data.len() < 16 || data[..4] != NODE_MAGIC || data[4] != LEAF {
        return None;
    }
    let count = usize::from(u16::from_le_bytes([data[6], data[7]]));
    let mut cursor = 16usize;
    for _ in 0..count {
        let klen = usize::from(u16::from_le_bytes([
            *data.get(cursor)?,
            *data.get(cursor + 1)?,
        ]));
        cursor = cursor.checked_add(2 + klen)?;
        let vlen = usize::try_from(u32::from_le_bytes(
            data.get(cursor..cursor + 4)?.try_into().ok()?,
        ))
        .ok()?;
        cursor = cursor.checked_add(4 + vlen)?;
        if cursor > data.len() {
            return None;
        }
    }
    Some(cursor)
}

fn write_entry_in_place(
    data: &mut [u8],
    entry_off: usize,
    key: &[u8],
    value: &[u8],
    key_len: u16,
    value_len: u32,
) -> Result<()> {
    let new_len = 2 + key.len() + 4 + value.len();
    if entry_off + new_len > data.len() {
        return Err(PlomidError::new(
            ErrorKind::Internal,
            "B+Tree in-place write out of bounds",
        ));
    }
    data[entry_off..entry_off + 2].copy_from_slice(&key_len.to_le_bytes());
    data[entry_off + 2..entry_off + 2 + key.len()].copy_from_slice(key);
    data[entry_off + 2 + key.len()..entry_off + 2 + key.len() + 4]
        .copy_from_slice(&value_len.to_le_bytes());
    data[entry_off + 2 + key.len() + 4..entry_off + new_len].copy_from_slice(value);
    Ok(())
}

fn write_node(pool: &mut BufferPool, page_id: PageId, node: &Node) -> Result<()> {
    let encoded = encode_node(node)?;
    if encoded.len() > PAGE_DATA_SIZE {
        return Err(PlomidError::new(
            ErrorKind::InvalidArgument,
            "B+Tree node exceeds page payload",
        ));
    }
    let data = pool.data_mut(page_id)?;
    data.fill(0);
    data[..encoded.len()].copy_from_slice(&encoded);
    // Internal nodes pin against eviction: a flushed parent referencing
    // never-flushed children is a dangling reference that neither WAL replay
    // nor a fresh mount can traverse. Leaves evict freely (self-consistent
    // content wherever it lands); internals leave the file only via `sync`.
    if matches!(node, Node::Internal { .. }) {
        pool.pin_structural(page_id)?;
    }
    Ok(())
}

/// Exact encoded length of a leaf's entry list plus the 16-byte node header.
///
/// Computed arithmetically so a fit check before a write does not have to
/// materialize the encoded node (which was a full allocation of every key and
/// value, thrown away immediately after measuring its length).
fn leaf_encoded_len(entries: &[(Vec<u8>, Vec<u8>)]) -> usize {
    16 + entries
        .iter()
        .map(|(key, value)| 2 + key.len() + 4 + value.len())
        .sum::<usize>()
}

/// Exact encoded length of an internal separator list plus the node header.
fn internal_encoded_len(separators: &[(Vec<u8>, PageId)]) -> usize {
    16 + separators
        .iter()
        .map(|(key, _)| 2 + key.len() + 8)
        .sum::<usize>()
}

fn encode_node(node: &Node) -> Result<Vec<u8>> {
    // One exact-size allocation: the previous build grew a `Vec` from 16 bytes
    // through repeated `extend_from_slice` calls, reallocating and copying the
    // whole node several times per insert.
    let capacity = match node {
        Node::Leaf { entries, .. } => leaf_encoded_len(entries),
        Node::Internal { separators, .. } => internal_encoded_len(separators),
    };
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(&[0_u8; 16]);
    output[..4].copy_from_slice(&NODE_MAGIC);
    match node {
        Node::Leaf { entries, next } => {
            output[4] = LEAF;
            output[8..16].copy_from_slice(&next.map_or(NO_PAGE, PageId::get).to_le_bytes());
            put_count(&mut output, entries.len())?;
            for (key, value) in entries {
                put_bytes(&mut output, key, true)?;
                put_bytes(&mut output, value, false)?;
            }
        }
        Node::Internal { first, separators } => {
            output[4] = INTERNAL;
            output[8..16].copy_from_slice(&first.get().to_le_bytes());
            put_count(&mut output, separators.len())?;
            for (key, child) in separators {
                put_bytes(&mut output, key, true)?;
                output.extend_from_slice(&child.get().to_le_bytes());
            }
        }
    }
    Ok(output)
}

fn put_count(output: &mut [u8], count: usize) -> Result<()> {
    let count = u16::try_from(count)
        .map_err(|_| PlomidError::new(ErrorKind::InvalidArgument, "too many B+Tree entries"))?;
    output[6..8].copy_from_slice(&count.to_le_bytes());
    Ok(())
}

fn put_bytes(output: &mut Vec<u8>, value: &[u8], key: bool) -> Result<()> {
    if key {
        let length = u16::try_from(value.len())
            .map_err(|_| PlomidError::new(ErrorKind::InvalidArgument, "B+Tree key is too large"))?;
        output.extend_from_slice(&length.to_le_bytes());
    } else {
        let length = u32::try_from(value.len()).map_err(|_| {
            PlomidError::new(ErrorKind::InvalidArgument, "B+Tree value is too large")
        })?;
        output.extend_from_slice(&length.to_le_bytes());
    }
    output.extend_from_slice(value);
    Ok(())
}

fn decode_node(data: &[u8]) -> Result<Node> {
    if data.len() < 16 || data[..4] != NODE_MAGIC {
        return Err(corruption("invalid B+Tree node header"));
    }
    let kind = data[4];
    let count = usize::from(u16::from_le_bytes([data[6], data[7]]));
    let mut cursor = 16;
    match kind {
        LEAF => {
            let next = u64::from_le_bytes(
                data[8..16]
                    .try_into()
                    .map_err(|_| corruption("invalid leaf link"))?,
            );
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let key = take(data, &mut cursor, true)?;
                let value = take(data, &mut cursor, false)?;
                entries.push((key, value));
            }
            if cursor > data.len() {
                return Err(corruption("leaf node is truncated"));
            }
            Ok(Node::Leaf {
                entries,
                next: (next != NO_PAGE).then(|| PageId::new(next)),
            })
        }
        INTERNAL => {
            let first = PageId::new(u64::from_le_bytes(
                data[8..16]
                    .try_into()
                    .map_err(|_| corruption("invalid internal child"))?,
            ));
            let mut separators = Vec::with_capacity(count);
            for _ in 0..count {
                let key = take(data, &mut cursor, true)?;
                let child = PageId::new(u64::from_le_bytes(
                    take_exact(data, &mut cursor, 8)?
                        .try_into()
                        .map_err(|_| corruption("invalid child page"))?,
                ));
                separators.push((key, child));
            }
            Ok(Node::Internal { first, separators })
        }
        _ => Err(corruption("unknown B+Tree node kind")),
    }
}

fn take(data: &[u8], cursor: &mut usize, key: bool) -> Result<Vec<u8>> {
    let length = if key {
        usize::from(u16::from_le_bytes(
            take_exact(data, cursor, 2)?
                .try_into()
                .map_err(|_| corruption("invalid key length"))?,
        ))
    } else {
        usize::try_from(u32::from_le_bytes(
            take_exact(data, cursor, 4)?
                .try_into()
                .map_err(|_| corruption("invalid value length"))?,
        ))
        .map_err(|_| corruption("invalid value length"))?
    };
    Ok(take_exact(data, cursor, length)?.to_vec())
}

fn take_exact<'a>(data: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8]> {
    let end = cursor
        .checked_add(length)
        .ok_or_else(|| corruption("node length overflow"))?;
    let value = data
        .get(*cursor..end)
        .ok_or_else(|| corruption("B+Tree node is truncated"))?;
    *cursor = end;
    Ok(value)
}

/// Borrows one leaf entry's key without cloning it.
///
/// Returns the key bytes plus the value-length field offset and value start,
/// so callers read the value length lazily (hits stop at the key compare;
/// misses skip the value entirely).
fn leaf_entry_at(data: &[u8], cursor: usize) -> Result<(&[u8], usize, usize)> {
    let klen = usize::from(u16::from_le_bytes(
        data.get(cursor..cursor + 2)
            .ok_or_else(|| corruption("B+Tree node is truncated"))?
            .try_into()
            .map_err(|_| corruption("B+Tree node is truncated"))?,
    ));
    let kstart = cursor
        .checked_add(2)
        .ok_or_else(|| corruption("node length overflow"))?;
    let kend = kstart
        .checked_add(klen)
        .ok_or_else(|| corruption("node length overflow"))?;
    let stored = data
        .get(kstart..kend)
        .ok_or_else(|| corruption("B+Tree node is truncated"))?;
    let vlen_off = kend;
    let vstart = vlen_off
        .checked_add(4)
        .ok_or_else(|| corruption("node length overflow"))?;
    Ok((stored, vlen_off, vstart))
}

/// Reads a leaf value length without touching the value bytes.
fn leaf_value_len(data: &[u8], vlen_off: usize, vstart: usize) -> Result<usize> {
    usize::try_from(u32::from_le_bytes(
        data.get(vlen_off..vstart)
            .ok_or_else(|| corruption("B+Tree node is truncated"))?
            .try_into()
            .map_err(|_| corruption("B+Tree node is truncated"))?,
    ))
    .map_err(|_| corruption("invalid value length"))
}

/// Borrows one internal separator key without cloning it.
///
/// Returns the separator bytes plus the offset just past the key (where the
/// 8-byte child page ID starts).
fn internal_key_at(data: &[u8], cursor: usize) -> Result<(&[u8], usize)> {
    let klen = usize::from(u16::from_le_bytes(
        data.get(cursor..cursor + 2)
            .ok_or_else(|| corruption("B+Tree node is truncated"))?
            .try_into()
            .map_err(|_| corruption("B+Tree node is truncated"))?,
    ));
    let kstart = cursor
        .checked_add(2)
        .ok_or_else(|| corruption("node length overflow"))?;
    let kend = kstart
        .checked_add(klen)
        .ok_or_else(|| corruption("node length overflow"))?;
    let stored = data
        .get(kstart..kend)
        .ok_or_else(|| corruption("B+Tree node is truncated"))?;
    Ok((stored, kend))
}

fn child_for(separators: &[(Vec<u8>, PageId)], first: PageId, key: &[u8]) -> PageId {
    separators
        .iter()
        .take_while(|(separator, _)| key >= separator.as_slice())
        .last()
        .map_or(first, |(_, child)| *child)
}

/// Routes `key` through an encoded internal node without allocating.
///
/// Returns the child the owned descent would select: the last separator at
/// or below `key`, or `first`. The whole node is still walked (not just to
/// the routing decision) so truncated or overflowing pages fail with the
/// same corruption errors `decode_node` would produce — a corrupt tail must
/// never be silently preserved. This is the insert hot path: the owned
/// decode allocated one `Vec<u8>` per separator on every insert at every
/// level, which dominated bulk-insert CPU (measured 11.9µs/put, 53% of a
/// 5K-row batch commit).
fn route_internal_borrowed(data: &[u8], key: &[u8]) -> Result<PageId> {
    if data.len() < 16 || data[..4] != NODE_MAGIC || data[4] != INTERNAL {
        return Err(corruption("invalid B+Tree node header"));
    }
    let count = usize::from(u16::from_le_bytes([data[6], data[7]]));
    let first = PageId::new(u64::from_le_bytes(
        data[8..16]
            .try_into()
            .map_err(|_| corruption("invalid internal child"))?,
    ));
    let mut cursor = 16_usize;
    let mut child = first;
    for _ in 0..count {
        let klen = usize::from(u16::from_le_bytes(
            data.get(cursor..cursor + 2)
                .ok_or_else(|| corruption("B+Tree node is truncated"))?
                .try_into()
                .map_err(|_| corruption("B+Tree node is truncated"))?,
        ));
        let kstart = cursor + 2;
        let kend = kstart
            .checked_add(klen)
            .ok_or_else(|| corruption("node length overflow"))?;
        let vstart = kend
            .checked_add(8)
            .ok_or_else(|| corruption("node length overflow"))?;
        let stored = data
            .get(kstart..kend)
            .ok_or_else(|| corruption("B+Tree node is truncated"))?;
        if key >= stored {
            let page = u64::from_le_bytes(
                data.get(kend..vstart)
                    .ok_or_else(|| corruption("B+Tree node is truncated"))?
                    .try_into()
                    .map_err(|_| corruption("B+Tree node is truncated"))?,
            );
            child = PageId::new(page);
        } else {
            // Routing is decided, but the tail is still validated below.
            data.get(kend..vstart)
                .ok_or_else(|| corruption("B+Tree node is truncated"))?;
        }
        cursor = vstart;
        if cursor > data.len() {
            return Err(corruption("B+Tree node is truncated"));
        }
    }
    Ok(child)
}

fn corruption(message: &'static str) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}

#[cfg(test)]
mod tests {
    use super::leaf_payload_end;
    use super::BTree;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    #[test]
    fn leaf_payload_end_matches_hand_bytes() {
        let mut data = vec![0u8; 16332];
        data[0..4].copy_from_slice(b"PLBT");
        data[4] = 1;
        data[6..8].copy_from_slice(&1u16.to_le_bytes());
        data[8..16].copy_from_slice(&[0xff; 8]);
        data[16..18].copy_from_slice(&4u16.to_le_bytes());
        data[18..22].copy_from_slice(b"beta");
        data[22..26].copy_from_slice(&3u32.to_le_bytes());
        data[26..29].copy_from_slice(b"two");
        // Bisect: guard components first.
        assert!(data.len() >= 16, "len guard");
        assert_eq!(&data[..4], b"PLBT", "magic guard");
        assert_eq!(data[4], 1, "kind guard");
        assert_eq!(leaf_payload_end(&data), Some(29));
    }

    fn temp_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("plomid-btree-{label}-{}-{id}", std::process::id()))
    }

    #[test]
    fn replace_with_larger_value_splits_instead_of_overflowing() {
        // `replace_if_present` on a full page with a larger value used to hit
        // "B+Tree node exceeds page payload": the general path rewrote the
        // value without a fit check. It must delete+re-insert through the
        // splitting insert path instead, preserving every key.
        let path = temp_path("replace-overflow");
        let result = (|| {
            let mut tree = BTree::create(&path, 64)?;
            // ~212-byte entries pack a 16KiB page nearly full.
            let value = vec![0x11_u8; 200];
            for index in 0..200_u32 {
                tree.insert(&format!("key-{index:05}").into_bytes(), &value)?;
            }
            // Grow one entry 10x on a full page: must split, not error.
            let big = vec![0x22_u8; 2000];
            assert!(tree.replace_if_present(b"key-00042", &big)?);
            assert_eq!(tree.get(b"key-00042")?, Some(big.clone()));
            // Grow every entry: sustained overflow pressure, no loss.
            for index in 0..200_u32 {
                let key = format!("key-{index:05}").into_bytes();
                assert!(tree.replace_if_present(&key, &big)?);
            }
            for index in 0..200_u32 {
                let key = format!("key-{index:05}").into_bytes();
                assert_eq!(tree.get(&key)?, Some(big.clone()));
            }
            // Absent keys still report false, and fresh inserts still work.
            assert!(!tree.replace_if_present(b"key-missing", &big)?);
            tree.insert(b"key-new", &value)?;
            assert_eq!(tree.get(b"key-new")?, Some(value));
            tree.sync()
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "replace-overflow test failed: {result:?}");
    }

    #[test]
    fn splits_leaves_with_mixed_entry_sizes() {
        // Small entries fill leaves, then interleaved multi-kilobyte values
        // force splits. A count-based midpoint would produce halves that exceed
        // the page payload; the byte-aware split must keep every half writable.
        let path = temp_path("mixed-sizes");
        let result = (|| {
            let mut tree = BTree::create(&path, 64)?;
            let small = vec![0x11_u8; 64];
            let large = vec![0x22_u8; 4 * 1024];
            for round in 0..40_u32 {
                for step in 0..32_u32 {
                    let mut key = format!("row-{round:04}-{step:04}").into_bytes();
                    tree.insert(&key, &small)?;
                    key[0] = b'z';
                    tree.insert(&key, &large)?;
                }
            }
            let mut key = b"row-0000-0000".to_vec();
            assert_eq!(tree.get(&key)?, Some(small.clone()));
            key[0] = b'z';
            assert_eq!(tree.get(&key)?, Some(large.clone()));
            tree.sync()
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "mixed-size split test failed: {result:?}");
    }

    #[test]
    fn inserts_and_reads_on_disk() {
        let path = temp_path("get");
        let result = (|| {
            let mut tree = BTree::create(&path, 32)?;
            tree.insert(b"beta", b"two")?;
            tree.insert(b"alpha", b"one")?;
            assert_eq!(tree.get(b"alpha")?, Some(b"one".to_vec()));
            assert_eq!(tree.get(b"missing")?, None);
            tree.sync()
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "B+Tree get test failed: {result:?}");
    }

    /// Model-equivalence fuzz over the in-place fast path: deterministic
    /// pseudorandom inserts (forward, reverse, interleaved), same-length and
    /// different-length replacements, and deletes, verified against a
    /// `BTreeMap` after every batch — plus a reopen that proves the bytes on
    /// disk decode identically. This exercises fast-path inserts, in-place
    /// replaces, fallback splits, and delete interplay together.
    #[test]
    fn randomized_ops_match_btreemap_model() {
        let path = temp_path("model");
        let result = (|| {
            let mut tree = BTree::create(&path, 64)?;
            let mut model = std::collections::BTreeMap::<Vec<u8>, Vec<u8>>::new();
            // Deterministic xorshift: no dev-dependency needed.
            let mut state = 0x9E3779B97F4A7C15u64;
            let mut next = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            let key = |n: u64| format!("key-{n:05}").into_bytes();
            for round in 0..30 {
                for _ in 0..200 {
                    let n = next() % 400;
                    let op = next() % 10;
                    if op < 7 {
                        // Varying lengths so replaces hit same-length,
                        // grow, and shrink cases.
                        let len = (next() % 60) as usize;
                        let value = vec![(n & 0xff) as u8; len];
                        tree.insert(&key(n), &value)?;
                        model.insert(key(n), value);
                    } else if op < 8 {
                        let k = key(n);
                        let deleted_tree = tree.delete(&k)?;
                        let deleted_model = model.remove(&k).is_some();
                        assert_eq!(deleted_tree, deleted_model, "delete mismatch for {k:?}");
                    } else {
                        let k = key(n);
                        assert_eq!(
                            tree.get(&k)?,
                            model.get(&k).cloned(),
                            "get mismatch for {k:?}"
                        );
                    }
                }
                // Full ordered comparison every round.
                let scanned = tree.range(None, None)?;
                let expected: Vec<(Vec<u8>, Vec<u8>)> =
                    model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                assert_eq!(scanned, expected, "scan mismatch at round {round}");
                // Same-length and different-length replaces on live keys.
                if let Some(k) = model.keys().next().cloned() {
                    tree.insert(&k, b"same")?;
                    model.insert(k.clone(), b"same".to_vec());
                    let long = vec![9u8; 300];
                    tree.insert(&k, &long)?;
                    model.insert(k.clone(), long);
                    assert_eq!(tree.get(&k)?, model.get(&k).cloned());
                }
            }
            tree.sync()?;
            drop(tree);
            let mut reopened = BTree::open(&path, 64)?;
            let scanned = reopened.range(None, None)?;
            let expected: Vec<(Vec<u8>, Vec<u8>)> =
                model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            assert_eq!(scanned, expected, "post-reopen mismatch");
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "model test failed: {result:?}");
    }

    #[test]
    fn range_is_ordered() {
        let path = temp_path("range");
        let result = (|| {
            let mut tree = BTree::create(&path, 32)?;
            for key in [b"delta".as_slice(), b"alpha", b"charlie", b"bravo"] {
                tree.insert(key, key)?;
            }
            let values = tree.range(Some(b"bravo"), Some(b"delta"))?;
            let keys: Vec<Vec<u8>> = values.into_iter().map(|(key, _)| key).collect();
            assert_eq!(keys, vec![b"bravo".to_vec(), b"charlie".to_vec()]);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "B+Tree range test failed: {result:?}");
    }

    #[test]
    fn bounded_range_prunes_to_the_requested_window() {
        // Regression: `collect_range` used to descend into every internal
        // child, so a bounded range touched the whole tree. Insert enough keys
        // to force several levels of internal nodes, then check that bounded
        // ranges return exactly the keys in `[start, end)` for windows placed
        // at the beginning, middle, and end of the tree.
        let path = temp_path("bounded-range");
        let result = (|| {
            let mut tree = BTree::create(&path, 64)?;
            for number in 0..600_u32 {
                let key = number.to_be_bytes();
                tree.insert(&key, &number.to_le_bytes())?;
            }
            let window =
                |tree: &mut BTree, start: u32, end: u32| -> plomid_core::Result<Vec<u32>> {
                    let entries =
                        tree.range(Some(&start.to_be_bytes()), Some(&end.to_be_bytes()))?;
                    Ok(entries
                        .into_iter()
                        .map(|(key, _)| u32::from_be_bytes(key.try_into().expect("key width")))
                        .collect())
                };
            assert_eq!(window(&mut tree, 0, 5)?, vec![0, 1, 2, 3, 4]);
            assert_eq!(window(&mut tree, 250, 260)?, (250..260).collect::<Vec<_>>());
            assert_eq!(window(&mut tree, 595, 700)?, (595..600).collect::<Vec<_>>());
            assert_eq!(window(&mut tree, 100_000, 100_010)?, Vec::<u32>::new());
            assert_eq!(window(&mut tree, 0, 600)?.len(), 600);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(
            result.is_ok(),
            "bounded range prune test failed: {result:?}"
        );
    }

    #[test]
    fn values_survive_reopen() {
        let path = temp_path("reopen");
        let result = (|| {
            let mut tree = BTree::create(&path, 32)?;
            for number in 0..100_u32 {
                let key = number.to_be_bytes();
                tree.insert(&key, &number.to_le_bytes())?;
            }
            tree.sync()?;
            drop(tree);
            let mut reopened = BTree::open(&path, 32)?;
            assert_eq!(
                reopened.get(&42_u32.to_be_bytes())?,
                Some(42_u32.to_le_bytes().to_vec())
            );
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "B+Tree reopen test failed: {result:?}");
    }

    #[test]
    fn larger_values_force_leaf_splits_and_ordered_scan() {
        let path = temp_path("splits");
        let result = (|| {
            let mut tree = BTree::create(&path, 64)?;
            for number in 0..40_u16 {
                let key = number.to_be_bytes();
                let value = vec![number as u8; 400];
                tree.insert(&key, &value)?;
            }
            let entries = tree.range(None, None)?;
            assert_eq!(entries.len(), 40);
            for (number, (key, value)) in entries.into_iter().enumerate() {
                assert_eq!(key, (number as u16).to_be_bytes());
                assert_eq!(value, vec![number as u8; 400]);
            }
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "B+Tree split test failed: {result:?}");
    }
}
