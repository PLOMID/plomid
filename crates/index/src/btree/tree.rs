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
//! The persistent B+Tree index surface.
//!
//! [`BTreeIndex`] owns the ordered key structure only. Durable bytes live in
//! 16 KiB pages of one file managed by [`BufferPool`]; integrity is the
//! page-level CRC32C; durability ordering is the existing WAL; structural
//! replay after a crash is the existing [`ReplayTarget`] recovery path driven
//! by `plomid_wal::recover_into`.
//!
//! Page 0 always holds the [`RootMeta`] block; tree nodes occupy pages 1..n.
//! The tree is single-writer: every operation takes `&mut self`, matching the
//! `plomid-storage` B+Tree convention.

use std::path::{Path, PathBuf};

use plomid_core::{ErrorKind, IndexId, ObjectId, PageId, PlomidError, Result, RowId};
use plomid_storage::BufferPool;

use super::constants::{
    INDEX_FLAG_UNIQUE, INDEX_FORMAT_VERSION, INTERNAL, LEAF, LEN_U16, NODE_HEADER_SIZE, NODE_MAGIC,
    NODE_MIN_ENTRIES, NO_PAGE, PAGE_PAYLOAD_SIZE, ROW_ID_SIZE,
};
use super::entry::{Bound, IndexEntry};
use super::node::{
    decode_node, encoded_len, internal_entry_size, leaf_entry_size, write_node, InternalEntry,
    LeafEntry, Node,
};
use super::root_meta::RootMeta;

/// Result of an insert into the index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertOutcome {
    /// The key was new; a fresh entry was created.
    Inserted,
    /// The key existed and the RowId was appended to its set.
    RowAppended,
    /// The key and RowId were both already present; nothing changed.
    Unchanged,
}

/// A persistent ordered B+Tree SQL index over one page file.
pub struct BTreeIndex {
    pool: BufferPool,
    meta: RootMeta,
    path: PathBuf,
}

/// Structural summary returned by [`BTreeIndex::stats`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BTreeStats {
    /// Total indexed keys.
    pub entry_count: u64,
    /// Pages holding tree nodes.
    pub node_pages: u64,
    /// Depth of the tree (single leaf root has depth 1).
    pub depth: u64,
}

impl BTreeIndex {
    /// Creates an empty index file at `path`.
    ///
    /// `index_id` and `object_id` are stable catalog identities; `generation`
    /// binds this index build to a data generation for later publication.
    /// `unique` selects unique-key enforcement.
    pub fn create(
        path: &Path,
        pool_capacity: usize,
        index_id: IndexId,
        object_id: ObjectId,
        generation: plomid_core::GenerationId,
        unique: bool,
    ) -> Result<Self> {
        let mut pool = BufferPool::create(path, pool_capacity)?;
        let _meta_page = pool.allocate_page()?.id();
        let flags = if unique { INDEX_FLAG_UNIQUE } else { 0 };
        let meta = RootMeta {
            version: INDEX_FORMAT_VERSION,
            flags,
            index_id,
            object_id,
            generation,
            // NO_PAGE means "no root yet": the tree is empty.
            root_page: PageId::new(NO_PAGE),
            entry_count: 0,
        };
        let mut index = Self {
            pool,
            meta,
            path: path.to_path_buf(),
        };
        index.persist_meta()?;
        tracing::debug!(target: "index::btree", "created path={} index_id={} unique={}", path.display(), index_id.get(), unique);
        Ok(index)
    }

    /// Opens an existing index file, validating its root metadata.
    pub fn open(path: &Path, pool_capacity: usize) -> Result<Self> {
        let mut pool = BufferPool::open(path, pool_capacity)?;
        let handle = pool.get_page(PageId::new(0))?;
        let meta = RootMeta::decode(handle.data())?;
        if meta.root_page.get() != NO_PAGE && meta.root_page.get() == 0 {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "root metadata points at the metadata page",
            ));
        }
        Ok(Self {
            pool,
            meta,
            path: path.to_path_buf(),
        })
    }

    /// Returns the index's stable catalog identity.
    #[must_use]
    pub fn index_id(&self) -> IndexId {
        self.meta.index_id
    }

    /// Returns the owning table's object identity.
    #[must_use]
    pub fn object_id(&self) -> ObjectId {
        self.meta.object_id
    }

    /// Returns the generation this index build is bound to.
    #[must_use]
    pub fn generation(&self) -> plomid_core::GenerationId {
        self.meta.generation
    }

    /// Returns true when this index enforces key uniqueness.
    #[must_use]
    pub fn is_unique(&self) -> bool {
        self.meta.is_unique()
    }

    /// Returns the number of indexed keys.
    #[must_use]
    pub fn entry_count(&self) -> u64 {
        self.meta.entry_count
    }

    /// Returns the current root page, if the tree holds any entries.
    #[must_use]
    pub fn root_page(&self) -> Option<PageId> {
        self.root_id()
    }

    fn root_id(&self) -> Option<PageId> {
        (self.meta.root_page.get() != NO_PAGE).then_some(self.meta.root_page)
    }

    // -- metadata and node persistence ------------------------------------

    fn persist_meta(&mut self) -> Result<()> {
        let encoded = self.meta.encode()?;
        let mut handle = self.pool.get_page(PageId::new(0))?;
        let data = handle.data_mut();
        data.fill(0);
        data[..encoded.len()].copy_from_slice(&encoded);
        self.pool.unpin_page(handle, true)
    }

    fn set_root(&mut self, root: Option<PageId>) -> Result<()> {
        self.meta.root_page = root.unwrap_or(PageId::new(NO_PAGE));
        self.persist_meta()
    }

    fn read_node(&mut self, page_id: PageId) -> Result<Node> {
        let handle = self.pool.get_page(page_id)?;
        let node = decode_node(handle.data())?;
        self.pool.unpin_page(handle, false)?;
        Ok(node)
    }

    fn write_node_at(&mut self, page_id: PageId, node: &Node) -> Result<()> {
        let mut handle = self.pool.get_page(page_id)?;
        write_node(&mut handle, node)?;
        self.pool.unpin_page(handle, true)
    }

    fn alloc_node(&mut self, node: &Node) -> Result<PageId> {
        let mut handle = self.pool.allocate_page()?;
        let page_id = handle.id();
        write_node(&mut handle, node)?;
        self.pool.unpin_page(handle, true)?;
        Ok(page_id)
    }

    /// True when the encoded node still fits in one page payload.
    fn fits(node: &Node) -> Result<bool> {
        Ok(encoded_len(node)? <= PAGE_PAYLOAD_SIZE)
    }

    // -- search -----------------------------------------------------------

    /// Walks backward from `landing` across every page that may hold `key`,
    /// returning the pages oldest-first.
    ///
    /// A predecessor may hold `key` while its last entry sorts at-or-above
    /// it; empty predecessors are skipped over conservatively (they arise
    /// only when a partial-key deletion could not unlink, which reads
    /// tolerate). The walk provably terminates at the chain start, and every
    /// skipped page sorts strictly below `key`, so no page holding `key` is
    /// ever skipped: node order is physical key order, which deletions
    /// preserve.
    fn run_pages_before(&mut self, landing: PageId, key: &[u8]) -> Result<Vec<PageId>> {
        let mut pages = vec![landing];
        loop {
            let node = self.read_node(*pages.last().expect("non-empty"))?;
            let Node::Leaf { prev, .. } = node else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree key run must stay within leaves",
                ));
            };
            let Some(prev_page) = prev else {
                break;
            };
            let prev_node = self.read_node(prev_page)?;
            let Node::Leaf { entries, .. } = prev_node else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree key run must stay within leaves",
                ));
            };
            // Empty predecessors carry nothing to compare: skip over them.
            // A non-empty predecessor whose maximum sorts below `key` ends
            // the run; anything at-or-above may still hold it.
            if let Some(last) = entries.last() {
                if last.key.as_slice() < key {
                    break;
                }
            }
            pages.push(prev_page);
        }
        pages.reverse();
        Ok(pages)
    }

    /// Merges every `key` posting in `pages` (ascending) into one ascending
    /// RowId list. Pages hold disjoint ordered slices of the run, so plain
    /// concatenation in page order preserves global order.
    fn collect_run_ids(&mut self, pages: &[PageId], key: &[u8]) -> Result<Vec<RowId>> {
        let mut merged = Vec::new();
        for page in pages {
            let node = self.read_node(*page)?;
            let Node::Leaf { entries, .. } = node else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree key run must stay within leaves",
                ));
            };
            for entry in &entries {
                if entry.key.as_slice() == key {
                    merged.extend_from_slice(&entry.row_ids);
                }
            }
        }
        Ok(merged)
    }

    /// Rewinds `(page, index)` to the first entry at-or-after `key`,
    /// returning the run-start position for inclusive scans and lookups.
    /// Combines the backward page walk with an in-node lower bound.
    fn rewind_to_key(&mut self, page: PageId, key: &[u8]) -> Result<(PageId, usize)> {
        let pages = self.run_pages_before(page, key)?;
        let Some(&first) = pages.first() else {
            return Err(PlomidError::new(
                ErrorKind::Internal,
                "B+Tree run walk returned no pages",
            ));
        };
        let node = self.read_node(first)?;
        let Node::Leaf { entries, .. } = node else {
            return Err(PlomidError::new(
                ErrorKind::Internal,
                "B+Tree key run must stay within leaves",
            ));
        };
        let index = match Self::leaf_position(&entries, key) {
            Ok(found) => {
                // Binary search may land mid-run: retreat to its start.
                let mut start = found;
                while start > 0 && entries[start - 1].key.as_slice() == key {
                    start -= 1;
                }
                start
            }
            Err(insertion) => insertion,
        };
        Ok((first, index))
    }

    /// Binary-searches one leaf's entries for `key`.
    ///
    /// Returns the index of the matching entry, or the insertion point that
    /// keeps ordering (the `Err` arm of `binary_search_by`).
    fn leaf_position(entries: &[LeafEntry], key: &[u8]) -> std::result::Result<usize, usize> {
        entries.binary_search_by(|entry| entry.key.as_slice().cmp(key))
    }

    /// Maximum RowIds one entry with a `key_len`-byte key can carry while the
    /// encoded leaf still fits a page, or `None` when the key alone overflows
    /// (unfixable: no chunking can help, and the historical error stands).
    fn max_ids_per_entry(key_len: usize) -> Option<usize> {
        let overhead = NODE_HEADER_SIZE + LEN_U16 + LEN_U16 + key_len;
        let room = PAGE_PAYLOAD_SIZE.checked_sub(overhead)?;
        // At least one id per chunk, or chunking could never terminate.
        Some((room / ROW_ID_SIZE).max(1))
    }

    /// Splits an oversized posting list into page-fitting same-key chunks.
    ///
    /// Chunks keep ascending RowId order and reuse the exact entry encoding,
    /// so no format change, page type, or version bump is involved: a chunk
    /// is an ordinary entry that happens to share its key. Callers must merge
    /// same-key chunks on reads (see `collect_key_run`). Only an oversized
    /// *key* still errors, exactly as before.
    fn chunk_postings(key: &[u8], mut row_ids: Vec<RowId>) -> Result<Vec<LeafEntry>> {
        row_ids.sort_unstable();
        Self::chunk_postings_sorted(key, row_ids)
    }

    /// Splits an already-sorted posting list into page-fitting same-key
    /// chunks, without re-sorting. Same layout and even distribution as
    /// [`Self::chunk_postings`]; the caller guarantees ascending input
    /// (fresh-build appends, tail re-cuts). Only an oversized *key* errors.
    fn chunk_postings_sorted(key: &[u8], row_ids: Vec<RowId>) -> Result<Vec<LeafEntry>> {
        let Some(max_ids) = Self::max_ids_per_entry(key.len()) else {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "single entry exceeds the index page payload",
            ));
        };
        // A single fitting entry keeps the historical single-entry layout.
        if leaf_entry_size(key.len(), row_ids.len()) <= PAGE_PAYLOAD_SIZE - NODE_HEADER_SIZE {
            return Ok(vec![LeafEntry {
                key: key.to_vec(),
                row_ids,
            }]);
        }
        // Even distribution (sizes differ by at most one) instead of
        // full-plus-runt: no degenerate single-row tail chunk exists to
        // empty out later, which keeps middle-node deletions rare.
        let total = row_ids.len();
        let chunks = total.div_ceil(max_ids);
        let base = total / chunks;
        let extra = total % chunks;
        let mut out = Vec::with_capacity(chunks);
        let mut rest = row_ids;
        for index in 0..chunks {
            let take = (base + usize::from(index < extra)).min(rest.len());
            let tail = rest.split_off(take);
            out.push(LeafEntry {
                key: key.to_vec(),
                row_ids: std::mem::replace(&mut rest, tail),
            });
        }
        debug_assert!(rest.is_empty());
        Ok(out)
    }

    /// Splits every oversized entry of an in-memory leaf into fitting chunks.
    ///
    /// Used wherever a node is assembled before the fit check (inserts that
    /// grew a posting list, splits), so the "single entry exceeds the page"
    /// error survives only for genuinely oversized keys.
    fn chunk_oversized_entries(entries: Vec<LeafEntry>) -> Result<Vec<LeafEntry>> {
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            if leaf_entry_size(entry.key.len(), entry.row_ids.len())
                <= PAGE_PAYLOAD_SIZE - NODE_HEADER_SIZE
            {
                out.push(entry);
                continue;
            }
            out.extend(Self::chunk_postings(&entry.key, entry.row_ids)?);
        }
        Ok(out)
    }

    /// Routes `key` through an encoded internal node without allocating.
    ///
    /// Returns the child the owned descent ([`Self::path_to`]) would select:
    /// the last separator at or below `key`, or `first`. The whole node is
    /// still walked (not just to the routing decision) so truncated or
    /// overflowing pages fail with the same corruption errors decoding
    /// would produce. This is the bulk-insert hot path: the owned decode
    /// allocated one key `Vec` per separator on every insert at every level.
    fn route_internal_borrowed(data: &[u8], key: &[u8]) -> Result<PageId> {
        if data.len() < NODE_HEADER_SIZE || data[..4] != NODE_MAGIC || data[4] != INTERNAL {
            return Err(PlomidError::new(
                ErrorKind::Internal,
                "B+Tree internal routing on a non-internal node",
            ));
        }
        let count = usize::from(u16::from_le_bytes([data[6], data[7]]));
        let mut child =
            PageId::new(u64::from_le_bytes(data[8..16].try_into().map_err(
                |_| PlomidError::new(ErrorKind::Corruption, "invalid internal child"),
            )?));
        let mut cursor = NODE_HEADER_SIZE;
        for _ in 0..count {
            // Internal entry layout (see `encode_node`): key_len[u16] |
            // child[u64] | key[key_len].
            let key_len = usize::from(u16::from_le_bytes(
                data.get(cursor..cursor + 2)
                    .ok_or_else(|| PlomidError::new(ErrorKind::Corruption, "separator truncated"))?
                    .try_into()
                    .map_err(|_| PlomidError::new(ErrorKind::Corruption, "separator truncated"))?,
            ));
            let child_start = cursor + 2;
            let child_end = child_start.checked_add(8).ok_or_else(|| {
                PlomidError::new(ErrorKind::Corruption, "separator length overflow")
            })?;
            let key_end = child_end.checked_add(key_len).ok_or_else(|| {
                PlomidError::new(ErrorKind::Corruption, "separator length overflow")
            })?;
            let stored = data
                .get(child_end..key_end)
                .ok_or_else(|| PlomidError::new(ErrorKind::Corruption, "separator truncated"))?;
            // Lower-bound routing, mirroring `path_to`: the last separator
            // at or below `key` wins. Later separators are still validated
            // below so a corrupt tail cannot hide behind an early decision.
            if key >= stored {
                child = PageId::new(u64::from_le_bytes(
                    data.get(child_start..child_end)
                        .ok_or_else(|| {
                            PlomidError::new(ErrorKind::Corruption, "separator truncated")
                        })?
                        .try_into()
                        .map_err(|_| {
                            PlomidError::new(ErrorKind::Corruption, "invalid child page")
                        })?,
                ));
            }
            cursor = key_end;
            if cursor > data.len() {
                return Err(PlomidError::new(
                    ErrorKind::Corruption,
                    "B+Tree node is truncated",
                ));
            }
        }
        Ok(child)
    }

    /// Descends from the root to the leaf that owns `key`, returning
    /// ancestor pages (root first) and the leaf page, decoding only the
    /// leaf. An empty tree returns `None`. Internal levels route borrowed
    /// (see [`Self::route_internal_borrowed`]); callers needing an ancestor
    /// for mutation read it then (split path only).
    fn descend_to_leaf(&mut self, key: &[u8]) -> Result<Option<(Vec<PageId>, PageId)>> {
        let Some(mut page_id) = self.root_id() else {
            return Ok(None);
        };
        let mut ancestors = Vec::new();
        loop {
            let handle = self.pool.get_page(page_id)?;
            let data = handle.data();
            let is_leaf =
                data.len() >= NODE_HEADER_SIZE && data[..4] == NODE_MAGIC && data[4] == LEAF;
            if is_leaf {
                self.pool.unpin_page(handle, false)?;
                return Ok(Some((ancestors, page_id)));
            }
            let child = Self::route_internal_borrowed(data, key)?;
            self.pool.unpin_page(handle, false)?;
            ancestors.push(page_id);
            page_id = child;
        }
    }

    /// Bulk-builds the tree from sorted `(key, row_id)` pairs in one pass.
    ///
    /// Used by generation builds, where the whole key set is known up front
    /// and pairs are unique: per-row insertion would re-descend, re-chunk,
    /// and re-split on every row (quadratic on low-cardinality runs),
    /// while bottom-up packing touches each entry once. The on-disk result
    /// is indistinguishable from incremental insertion — same chunking
    /// ([`Self::chunk_postings_sorted`]), same leaf chain, same separator
    /// convention (each separator is its child's first key, exactly as
    /// [`Self::split_leaf`] publishes), same counts — so every read,
    /// delete, split, and reopen path behaves identically.
    ///
    /// The input is sorted here (by key, then RowId); callers must not rely
    /// on input order beyond the uniqueness contract shared with
    /// [`Self::insert_append`].
    pub fn build_bulk(&mut self, mut rows: Vec<(Vec<u8>, RowId)>) -> Result<()> {
        rows.sort_unstable();
        rows.dedup();
        // One chunked entry list per distinct key, in ascending key order.
        let mut entries: Vec<LeafEntry> = Vec::new();
        let mut cursor = 0_usize;
        while cursor < rows.len() {
            let end = rows[cursor..]
                .iter()
                .position(|(key, _)| *key != rows[cursor].0)
                .map_or(rows.len(), |offset| cursor + offset);
            let ids: Vec<RowId> = rows[cursor..end].iter().map(|(_, id)| *id).collect();
            // Rows arrive grouped by key with ascending RowIds after the
            // sort above, so the no-sort chunker applies directly.
            entries.extend(Self::chunk_postings_sorted(&rows[cursor].0, ids)?);
            cursor = end;
        }
        let entry_count = entries.len() as u64;
        if entries.is_empty() {
            self.meta.entry_count = 0;
            self.set_root(None)?;
            return Ok(());
        }
        // Pack leaves greedily by encoded size; every leaf but the last is
        // guaranteed non-empty, and no leaf exceeds the page payload.
        // Successor links are fixed up as each leaf lands (allocation order
        // is sequential, but the code reads back and rewrites instead of
        // assuming id arithmetic).
        let mut leaves: Vec<(Vec<u8>, PageId)> = Vec::new();
        let mut start = 0_usize;
        let mut used = NODE_HEADER_SIZE;
        let mut prev: Option<PageId> = None;
        for index in 0..=entries.len() {
            let boundary = index == entries.len()
                || used + leaf_entry_size(entries[index].key.len(), entries[index].row_ids.len())
                    > PAGE_PAYLOAD_SIZE;
            if boundary && index > start {
                let page = self.alloc_leaf(&entries[start..index], prev)?;
                if let Some(prev_page) = prev {
                    let mut prev_node = self.read_node(prev_page)?;
                    if let Node::Leaf { next, .. } = &mut prev_node {
                        *next = Some(page);
                        self.write_node_at(prev_page, &prev_node)?;
                    }
                }
                prev = Some(page);
                leaves.push((entries[start].key.clone(), page));
                start = index;
                used = NODE_HEADER_SIZE;
            }
            if index < entries.len() {
                used += leaf_entry_size(entries[index].key.len(), entries[index].row_ids.len());
            }
        }
        // The loop above always emits the trailing leaf: when the final
        // entry overflows the current page it is emitted alone (a lone
        // oversized entry errors inside `alloc_leaf` exactly like the
        // incremental path's single-entry error).
        let mut children = leaves;
        // Raise internal levels until one root remains. Each parent takes a
        // maximal run of children that fits; separators are the exact
        // first-keys the split path would publish, so routing (including
        // the upper-bound duplicate rule) is unchanged.
        while children.len() > 1 {
            let mut parents: Vec<(Vec<u8>, PageId)> = Vec::new();
            let mut start = 0_usize;
            let mut used = NODE_HEADER_SIZE;
            for index in 0..=children.len() {
                let boundary = index == children.len()
                    || (index > start
                        && used + internal_entry_size(children[index].0.len()) > PAGE_PAYLOAD_SIZE);
                if boundary && index > start {
                    let page = self.alloc_internal(&children[start..index])?;
                    parents.push((children[start].0.clone(), page));
                    start = index;
                    used = NODE_HEADER_SIZE;
                }
                if index < children.len() {
                    used += internal_entry_size(children[index].0.len());
                }
            }
            children = parents;
        }
        self.meta.entry_count = entry_count;
        let (_, root) = children.pop().expect("at least one child");
        self.set_root(Some(root))?;
        // `set_root` persists; keep the explicit persist for symmetry with
        // the incremental path (harmless duplicate write of page 0).
        self.persist_meta()?;
        Ok(())
    }

    /// Allocates one leaf page over `entries` with the given predecessor
    /// link, returning its page id. The successor link is fixed up by the
    /// caller (or left `None` for the final leaf).
    fn alloc_leaf(&mut self, entries: &[LeafEntry], prev: Option<PageId>) -> Result<PageId> {
        let node = Node::Leaf {
            prev,
            next: None,
            entries: entries.to_vec(),
        };
        if Self::fits(&node)? {
            return self.alloc_node(&node);
        }
        Err(PlomidError::new(
            ErrorKind::InvalidArgument,
            "single entry exceeds the index page payload",
        ))
    }

    /// Allocates one internal page over `(first_key, child)` pairs; the
    /// first pair supplies `first`, the rest become separators.
    fn alloc_internal(&mut self, children: &[(Vec<u8>, PageId)]) -> Result<PageId> {
        let first = children.first().expect("non-empty level").1;
        let separators: Vec<InternalEntry> = children[1..]
            .iter()
            .map(|(key, child)| InternalEntry {
                key: key.clone(),
                child: *child,
            })
            .collect();
        self.alloc_node(&Node::Internal { first, separators })
    }

    /// Descends from the root to the leaf that owns `key`, returning the full
    /// path (root first, leaf last). An empty tree returns an empty path.
    fn path_to(&mut self, key: &[u8]) -> Result<Vec<(PageId, Node)>> {
        let mut path = Vec::new();
        let Some(mut page_id) = self.root_id() else {
            return Ok(path);
        };
        loop {
            let node = self.read_node(page_id)?;
            let is_leaf = node.is_leaf();
            path.push((page_id, node));
            if is_leaf {
                return Ok(path);
            }
            let Node::Internal { first, separators } = &path.last().expect("pushed").1 else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree path type mismatch",
                ));
            };
            // Lower-bound routing: the child is the last separator whose key
            // is <= `key`, or `first` below all separator keys.
            page_id = separators
                .iter()
                .rev()
                .find(|entry| key >= entry.key.as_slice())
                .map(|entry| entry.child)
                .unwrap_or(*first);
        }
    }

    /// Returns the leftmost leaf page, if the tree is non-empty.
    fn leftmost_leaf(&mut self) -> Result<Option<PageId>> {
        let Some(mut page_id) = self.root_id() else {
            return Ok(None);
        };
        loop {
            let node = self.read_node(page_id)?;
            match node {
                Node::Leaf { .. } => return Ok(Some(page_id)),
                Node::Internal { first, .. } => page_id = first,
            }
        }
    }

    /// Returns the rightmost leaf page, if the tree is non-empty.
    fn rightmost_leaf(&mut self) -> Result<Option<PageId>> {
        let Some(mut page_id) = self.root_id() else {
            return Ok(None);
        };
        loop {
            let node = self.read_node(page_id)?;
            match node {
                Node::Leaf { .. } => return Ok(Some(page_id)),
                Node::Internal { separators, .. } => {
                    page_id = separators.last().map(|entry| entry.child).ok_or_else(|| {
                        PlomidError::new(ErrorKind::Corruption, "internal node has no children")
                    })?;
                }
            }
        }
    }

    /// Point lookup: all RowIds registered under `key`.
    ///
    /// Same-key chunks (see `chunk_postings`) merge here: the leaf is located
    /// by routing, then the run is collected backward to its start and
    /// forward to its end, so a key split across nodes by earlier compactions
    /// still resolves completely. Single-entry keys take the historical
    /// single-leaf path plus one predecessor read.
    pub fn lookup(&mut self, key: &[u8]) -> Result<Option<Vec<RowId>>> {
        let Some(landing) = self.leaf_page_for(key)? else {
            return Ok(None);
        };
        let pages = self.key_run_pages(landing, key)?;
        let merged = self.collect_run_ids(&pages, key)?;
        if merged.is_empty() {
            return Ok(None);
        }
        Ok(Some(merged))
    }

    /// All pages that may hold `key`, oldest-first: backward to the run start
    /// (skipping empty nodes conservatively), then forward past the landing
    /// to the run end. Empty nodes contribute nothing at collection time.
    fn key_run_pages(&mut self, landing: PageId, key: &[u8]) -> Result<Vec<PageId>> {
        let mut pages = self.run_pages_before(landing, key)?;
        // Forward past the landing while the next node may still hold `key`
        // (first entry at-or-below it, or an empty node we conservatively
        // cross). Stops at the first node sorting strictly above `key`.
        loop {
            let last = *pages.last().expect("non-empty run pages");
            let node = self.read_node(last)?;
            let Node::Leaf { next, .. } = node else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree key run must stay within leaves",
                ));
            };
            let Some(next_page) = next else {
                break;
            };
            let next_node = self.read_node(next_page)?;
            let Node::Leaf { entries, .. } = next_node else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree key run must stay within leaves",
                ));
            };
            match entries.first() {
                None => pages.push(next_page),
                Some(first) if first.key.as_slice() <= key => pages.push(next_page),
                _ => break,
            }
        }
        Ok(pages)
    }

    /// Returns true when `key` is present.
    ///
    /// Run-aware like [`Self::lookup`]: chunked same-key entries may span
    /// nodes, so presence checks the whole run, stopping at the first hit.
    pub fn contains(&mut self, key: &[u8]) -> Result<bool> {
        let Some(landing) = self.leaf_page_for(key)? else {
            return Ok(false);
        };
        for page in self.key_run_pages(landing, key)? {
            let node = self.read_node(page)?;
            let Node::Leaf { entries, .. } = node else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree key run must stay within leaves",
                ));
            };
            if entries.iter().any(|entry| entry.key.as_slice() == key) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Descends from the root to the leaf that owns `key`, decoding only the
    /// internal routing nodes. An empty tree returns `None`.
    fn leaf_page_for(&mut self, key: &[u8]) -> Result<Option<PageId>> {
        let Some(mut page_id) = self.root_id() else {
            return Ok(None);
        };
        loop {
            let node = self.read_node(page_id)?;
            match &node {
                Node::Leaf { .. } => return Ok(Some(page_id)),
                Node::Internal { first, separators } => {
                    page_id = separators
                        .iter()
                        .rev()
                        .find(|entry| key >= entry.key.as_slice())
                        .map(|entry| entry.child)
                        .unwrap_or(*first);
                }
            }
        }
    }

    // -- insert ------------------------------------------------------------

    /// Inserts `key -> row_id`.
    ///
    /// Unique indexes reject a second distinct RowId under an existing key
    /// with the PLOMID `Conflict` error. Non-unique indexes append the RowId
    /// to the key's ordered set. Structural splits happen whenever the encoded
    /// node no longer fits a page; the parent chain is updated bottom-up.
    pub fn insert(&mut self, key: &[u8], row_id: RowId) -> Result<InsertOutcome> {
        self.insert_inner(key, row_id)
    }

    fn insert_inner(&mut self, key: &[u8], row_id: RowId) -> Result<InsertOutcome> {
        // Borrowed descent: only the landing leaf is decoded. Ancestor pages
        // travel as ids; the split path below reads them back if (and only
        // if) the leaf overflows, which is rare next to the per-insert
        // routing that used to decode every level.
        let Some((path, leaf_page)) = self.descend_to_leaf(key)? else {
            // First key in an empty tree: create a root leaf.
            let node = Node::Leaf {
                prev: None,
                next: None,
                entries: vec![LeafEntry {
                    key: key.to_vec(),
                    row_ids: vec![row_id],
                }],
            };
            let page_id = self.alloc_node(&node)?;
            self.meta.entry_count = 1;
            self.set_root(Some(page_id))?;
            return Ok(InsertOutcome::Inserted);
        };

        let node = self.read_node(leaf_page)?;
        let (prev, next, mut entries) = match node {
            Node::Leaf {
                prev,
                next,
                entries,
            } => (prev, next, entries),
            Node::Internal { .. } => {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree path must end at a leaf",
                ))
            }
        };
        let mut inserted = false;
        // Full-run duplicate suppression: the row may already sit in any
        // chunk of this key (here or in earlier nodes after splits). The
        // walks are read-only; mutation below touches only the landing node,
        // whose ancestor path is in hand. Append mode (`insert_append`)
        // skips this O(run length) scan by caller contract (unique pairs);
        // the landing node's own portion is still checked below.
        let run_pages = self.run_pages_before(leaf_page, key)?;
        let mut key_present = false;
        let mut duplicate = false;
        'scan: for page in &run_pages {
            let node = self.read_node(*page)?;
            let Node::Leaf { entries, .. } = node else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree key run must stay within leaves",
                ));
            };
            for entry in &entries {
                if entry.key.as_slice() == key {
                    key_present = true;
                    if entry.row_ids.contains(&row_id) {
                        duplicate = true;
                        break 'scan;
                    }
                }
            }
        }
        if duplicate {
            // Nothing changed: no leaf rewrite and no metadata write.
            return Ok(InsertOutcome::Unchanged);
        }
        if self.is_unique() && key_present {
            return Err(PlomidError::new(
                ErrorKind::Conflict,
                "duplicate key violates unique index",
            ));
        }
        // Entry-count delta for this node (chunk splits add entries).
        let delta: i64;
        match Self::leaf_position(&entries, key) {
            Ok(index) => {
                // Append into this node's run portion and re-chunk it: the
                // portion stays ordered and page-fitting as one unit.
                let mut start = index;
                while start > 0 && entries[start - 1].key.as_slice() == key {
                    start -= 1;
                }
                let mut end = index + 1;
                while end < entries.len() && entries[end].key.as_slice() == key {
                    end += 1;
                }
                let before = (end - start) as i64;
                let mut ids: Vec<RowId> = entries[start..end]
                    .iter()
                    .flat_map(|entry| entry.row_ids.iter().copied())
                    .collect();
                ids.push(row_id);
                let chunks = Self::chunk_postings(key, ids)?;
                delta = chunks.len() as i64 - before;
                entries.splice(start..end, chunks);
            }
            Err(position) => {
                entries.insert(
                    position,
                    LeafEntry {
                        key: key.to_vec(),
                        row_ids: vec![row_id],
                    },
                );
                inserted = !key_present;
                delta = 1;
            }
        }
        if delta != 0 {
            self.meta.entry_count = (self.meta.entry_count as i64)
                .checked_add(delta)
                .and_then(|count| u64::try_from(count).ok())
                .ok_or_else(|| {
                    PlomidError::new(ErrorKind::Internal, "index entry count overflow")
                })?;
        }

        let leaf_node = Node::Leaf {
            prev,
            next,
            entries,
        };
        if Self::fits(&leaf_node)? {
            self.write_node_at(leaf_page, &leaf_node)?;
            // Persist whenever the entry count changed: new keys and chunk
            // splits both alter it, while pure appends leave page 0 alone.
            if inserted || delta != 0 {
                self.persist_meta()?;
            }
            return Ok(outcome_of(inserted));
        }
        self.insert_with_split(leaf_page, leaf_node, path)
    }
}

/// Maps the inserted flag onto the public outcome.
const fn outcome_of(inserted: bool) -> InsertOutcome {
    if inserted {
        InsertOutcome::Inserted
    } else {
        InsertOutcome::RowAppended
    }
}

impl BTreeIndex {
    /// Writes a leaf that no longer fits, splitting upward as needed.
    fn insert_with_split(
        &mut self,
        leaf_page: PageId,
        leaf_node: Node,
        path: Vec<PageId>,
    ) -> Result<InsertOutcome> {
        // Split the oversized leaf: the first key of the right half becomes
        // the separator that routes to the sibling page.
        let (right_page, separator) = self.split_leaf(leaf_page, leaf_node)?;
        // `path` already excludes the leaf (the caller popped it to apply the
        // change); ascend through the ancestors only.
        self.ascend_with_split(path, separator, right_page)?;
        self.persist_meta()?;
        Ok(InsertOutcome::Inserted)
    }

    /// Pushes `(separator, new_child)` into the ancestor chain, splitting
    /// internal nodes upward and creating a new root when the old root splits.
    /// Ancestors arrive as page ids (borrowed descent) and are decoded here
    /// only because a split below forces their mutation — the rare path.
    fn ascend_with_split(
        &mut self,
        mut path: Vec<PageId>,
        mut separator: Vec<u8>,
        mut new_child: PageId,
    ) -> Result<()> {
        loop {
            let Some(parent_page) = path.pop() else {
                // The old root split. Its left half still lives at the old
                // root page (leaf or internal); build the new root above it.
                let old_root = self.meta.root_page;
                let new_root = Node::Internal {
                    first: old_root,
                    separators: vec![InternalEntry {
                        key: separator,
                        child: new_child,
                    }],
                };
                let page = self.alloc_node(&new_root)?;
                self.set_root(Some(page))?;
                return Ok(());
            };
            let parent_node = self.read_node(parent_page)?;
            let Node::Internal {
                first,
                mut separators,
            } = parent_node
            else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree ancestor must be internal",
                ));
            };
            // Upper-bound insertion: the new child sorts AFTER all existing
            // equal separators. With duplicate keys (chunked posting runs share
            // one key) lower-bound insertion would file the newest sibling
            // mid-array while routing ("last separator <= key") sends queries
            // to the last equal — landing mid-run and breaking global RowId
            // order on subsequent appends. Appending after equals keeps
            // creation order == chain order.
            let position =
                separators.partition_point(|entry| entry.key.as_slice() <= separator.as_slice());
            separators.insert(
                position,
                InternalEntry {
                    key: separator,
                    child: new_child,
                },
            );
            let updated = Node::Internal { first, separators };
            if Self::fits(&updated)? {
                self.write_node_at(parent_page, &updated)?;
                return Ok(());
            }
            let (sibling_page, up_separator) = self.split_internal(parent_page, updated)?;
            separator = up_separator;
            new_child = sibling_page;
        }
    }

    /// Byte-aware leaf split: chooses the boundary closest to the encoded
    /// byte midpoint so neither half exceeds the page payload. Maintains the
    /// previous/next leaf chain in both directions.
    fn split_leaf(&mut self, leaf_page: PageId, leaf: Node) -> Result<(PageId, Vec<u8>)> {
        let Node::Leaf {
            prev,
            next,
            entries,
        } = leaf
        else {
            return Err(PlomidError::new(
                ErrorKind::Internal,
                "leaf split applied to an internal node",
            ));
        };
        // Chunk oversized postings first: a lone oversized entry becomes
        // ordinary same-key chunks (which split freely below), so the error
        // below survives only for genuinely oversized keys.
        let mut entries = Self::chunk_oversized_entries(entries)?;
        if entries.len() < 2 {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "single entry exceeds the index page payload",
            ));
        }
        let entry_bytes = |entry: &LeafEntry| leaf_entry_size(entry.key.len(), entry.row_ids.len());
        let split_at = byte_midpoint(entries.iter().map(entry_bytes));
        let right_entries = entries.split_off(split_at);
        let separator = right_entries[0].key.clone();
        let right = Node::Leaf {
            prev: Some(leaf_page),
            next,
            entries: right_entries,
        };
        let right_page = self.alloc_node(&right)?;
        // Re-point the old successor's `prev` at the new sibling.
        if let Some(next_page) = next {
            let mut next_node = self.read_node(next_page)?;
            if let Node::Leaf { prev, .. } = &mut next_node {
                *prev = Some(right_page);
            }
            self.write_node_at(next_page, &next_node)?;
        }
        let left = Node::Leaf {
            prev,
            next: Some(right_page),
            entries,
        };
        self.write_node_at(leaf_page, &left)?;
        tracing::trace!(target: "index::btree", "leaf split page={} right={}", leaf_page.get(), right_page.get());
        Ok((right_page, separator))
    }

    /// Byte-aware internal split mirroring [`Self::split_leaf`]. The first
    /// separator of the right half is promoted upward and its child moves
    /// into the sibling's `first` slot.
    fn split_internal(&mut self, internal_page: PageId, node: Node) -> Result<(PageId, Vec<u8>)> {
        let Node::Internal {
            first,
            mut separators,
        } = node
        else {
            return Err(PlomidError::new(
                ErrorKind::Internal,
                "internal split applied to a leaf",
            ));
        };
        let entry_bytes = |entry: &InternalEntry| internal_entry_size(entry.key.len());
        let split_at = byte_midpoint(separators.iter().map(entry_bytes));
        let right_separators = separators.split_off(split_at);
        let up_separator = right_separators[0].key.clone();
        let right_first = right_separators[0].child;
        let right = Node::Internal {
            first: right_first,
            separators: right_separators.into_iter().skip(1).collect(),
        };
        let right_page = self.alloc_node(&right)?;
        let left = Node::Internal { first, separators };
        self.write_node_at(internal_page, &left)?;
        tracing::trace!(target: "index::btree", "internal split page={} right={}", internal_page.get(), right_page.get());
        Ok((right_page, up_separator))
    }
}

/// Splits an entry-size sequence at the boundary closest to the byte midpoint.
///
/// Mirrors the storage-layer B+Tree rule: minimize the larger half's encoded
/// size so no half can exceed the page payload when a valid boundary exists.
fn byte_midpoint(sizes: impl Iterator<Item = usize>) -> usize {
    let sizes: Vec<usize> = sizes.collect();
    let total: usize = sizes.iter().sum();
    let mut cumulative = 0_usize;
    let mut split_at = 1_usize;
    let mut best = usize::MAX;
    for (index, size) in sizes.iter().enumerate() {
        if index > 0 {
            let larger = usize::max(cumulative, total - cumulative);
            if larger < best {
                best = larger;
                split_at = index;
            }
        }
        cumulative += size;
    }
    split_at
}

impl BTreeIndex {
    // -- delete -------------------------------------------------------------

    /// Removes one RowId from `key`, or the whole entry when `all` is set.
    ///
    /// Returns true when the index changed. A leaf that empties during
    /// deletion is unlinked from the leaf chain and detached from its parent;
    /// the detachment propagates upward, contracting the root when only one
    /// child remains. Freed pages are not reclaimed in place; generation-level
    /// collection owns reclamation.
    pub fn delete(&mut self, key: &[u8], row_id: Option<RowId>, all: bool) -> Result<bool> {
        if !all && row_id.is_none() {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "delete requires a RowId unless deleting the whole key",
            ));
        }
        // Tail-loop: every iteration re-descends, so the mutated node always
        // carries a valid ancestor path (required for separator maintenance
        // and unlinking). Under exact routing the descent lands on the run
        // tail; each iteration removes at least one chunk there, so the loop
        // terminates. A landing that lacks the key ends the loop with prior
        // miss behavior.
        let mut removed_any = false;
        let mut delta: i64 = 0;
        loop {
            let mut path = self.path_to(key)?;
            let Some((leaf_page, node)) = path.pop() else {
                // Empty tree.
                break;
            };
            let (prev, next, mut entries) = match node {
                Node::Leaf {
                    prev,
                    next,
                    entries,
                } => (prev, next, entries),
                Node::Internal { .. } => {
                    return Err(PlomidError::new(
                        ErrorKind::Internal,
                        "B+Tree path must end at a leaf",
                    ))
                }
            };
            // This node's run portion (possibly empty: stale landings miss).
            let start = match Self::leaf_position(&entries, key) {
                Ok(index) => {
                    let mut start = index;
                    while start > 0 && entries[start - 1].key.as_slice() == key {
                        start -= 1;
                    }
                    start
                }
                Err(_) => break,
            };
            let mut end = start;
            while end < entries.len() && entries[end].key.as_slice() == key {
                end += 1;
            }
            if all {
                let removed = (end - start) as i64;
                entries.drain(start..end);
                delta -= removed;
                removed_any = true;
            } else {
                let target = row_id.expect("checked above");
                // Single-row deletes normally land on the run tail holding
                // the row; when the row sits in an earlier chunk (only
                // possible across chunked nodes), locate it with a backward
                // walk and rewrite that page by id. Structural work below
                // still applies only to freshly-descended nodes.
                let mut found: Option<(PageId, usize, usize)> = None;
                for (index, entry) in entries[start..end].iter().enumerate() {
                    if let Some(position) = entry.row_ids.iter().position(|id| *id == target) {
                        found = Some((leaf_page, start + index, position));
                        break;
                    }
                }
                let (target_page, entry_index, row_position) = match found {
                    Some(located) => located,
                    None => {
                        // Search earlier run nodes (read-only walk).
                        let mut located = None;
                        for page in self.run_pages_before(leaf_page, key)? {
                            if page == leaf_page {
                                continue;
                            }
                            let node = self.read_node(page)?;
                            let Node::Leaf { entries, .. } = node else {
                                return Err(PlomidError::new(
                                    ErrorKind::Internal,
                                    "B+Tree key run must stay within leaves",
                                ));
                            };
                            for (index, entry) in entries.iter().enumerate() {
                                if entry.key.as_slice() != key {
                                    continue;
                                }
                                if let Some(position) =
                                    entry.row_ids.iter().position(|id| *id == target)
                                {
                                    located = Some((page, index, position));
                                    break;
                                }
                            }
                            if located.is_some() {
                                break;
                            }
                        }
                        match located {
                            Some(located) => located,
                            // Absent everywhere reachable: miss, like before.
                            None => break,
                        }
                    }
                };
                // Rewrite by page id when the row lives outside the descended
                // node; structural maintenance below only runs for the
                // descended node itself (valid path in hand).
                if target_page != leaf_page {
                    let mut node = self.read_node(target_page)?;
                    let Node::Leaf { entries, .. } = &mut node else {
                        return Err(PlomidError::new(
                            ErrorKind::Internal,
                            "B+Tree key run must stay within leaves",
                        ));
                    };
                    let Some(entry) = entries.get_mut(entry_index) else {
                        break;
                    };
                    entry.row_ids.remove(row_position);
                    if entry.row_ids.is_empty() {
                        entries.remove(entry_index);
                        delta -= 1;
                    }
                    self.write_node_at(target_page, &node)?;
                    removed_any = true;
                    if delta != 0 {
                        self.meta.entry_count = (self.meta.entry_count as i64)
                            .checked_add(delta)
                            .and_then(|count| u64::try_from(count).ok())
                            .ok_or_else(|| {
                                PlomidError::new(ErrorKind::Internal, "index entry count underflow")
                            })?;
                        self.persist_meta()?;
                    }
                    return Ok(removed_any);
                }
                let entry = &mut entries[entry_index];
                entry.row_ids.remove(row_position);
                if entry.row_ids.is_empty() {
                    entries.remove(entry_index);
                    delta -= 1;
                }
                removed_any = true;
            }
            // Separator maintenance for the descended node: its first key may
            // have changed (entry removal), and the parent separator must
            // track it — otherwise later descents misroute. Order is safe:
            // removal only raises a first key, which stays below the next
            // sibling's minimum.
            let leaf_node = Node::Leaf {
                prev,
                next,
                entries,
            };
            // Borrow the first key before moving entries into the node.
            let first_key: Option<Vec<u8>> = match &leaf_node {
                Node::Leaf { entries, .. } => entries.first().map(|entry| entry.key.clone()),
                _ => None,
            };
            let underfull = leaf_node.len() < NODE_MIN_ENTRIES;
            self.write_node_at(leaf_page, &leaf_node)?;
            if let (Some(first), Some((parent_page, parent_node))) = (first_key, path.last_mut()) {
                if let Node::Internal {
                    first: parent_first,
                    separators,
                } = parent_node
                {
                    let stale = separators
                        .iter()
                        .any(|sep| sep.child == leaf_page && sep.key != first);
                    if stale {
                        for sep in separators.iter_mut() {
                            if sep.child == leaf_page {
                                sep.key = first.clone();
                            }
                        }
                        let rebuilt = Node::Internal {
                            first: *parent_first,
                            separators: std::mem::take(separators),
                        };
                        self.write_node_at(*parent_page, &rebuilt)?;
                    }
                }
            }
            if underfull {
                self.unlink_leaf(prev, next, leaf_page)?;
                self.detach_empty(path, leaf_page)?;
            }
            if !all {
                break;
            }
        }
        // Entry-count and metadata persistence for net removals.
        if delta != 0 {
            self.meta.entry_count = (self.meta.entry_count as i64)
                .checked_add(delta)
                .and_then(|count| u64::try_from(count).ok())
                .ok_or_else(|| {
                    PlomidError::new(ErrorKind::Internal, "index entry count underflow")
                })?;
            self.persist_meta()?;
        }
        Ok(removed_any)
    }

    /// Removes an emptied leaf from the prev/next chain: the neighbors link
    /// to each other across the removed page.
    fn unlink_leaf(
        &mut self,
        prev: Option<PageId>,
        next: Option<PageId>,
        removed: PageId,
    ) -> Result<()> {
        if let Some(prev_page) = prev {
            let mut node = self.read_node(prev_page)?;
            if let Node::Leaf { next: link, .. } = &mut node {
                if *link == Some(removed) {
                    *link = next;
                }
            }
            self.write_node_at(prev_page, &node)?;
        }
        if let Some(next_page) = next {
            let mut node = self.read_node(next_page)?;
            if let Node::Leaf { prev: link, .. } = &mut node {
                if *link == Some(removed) {
                    *link = prev;
                }
            }
            self.write_node_at(next_page, &node)?;
        }
        Ok(())
    }

    /// Detaches an emptied node from its parent, collapsing the ancestor chain
    /// upward. `path` holds the ancestors from the root down, exclusive of the
    /// emptied node itself.
    ///
    /// An internal node "empties" only by losing its last routing entry; the
    /// collapse then repeats at the grandparent. When the root collapses to a
    /// single child the root contracts to that child; when it collapses to
    /// nothing the tree becomes empty.
    fn detach_empty(&mut self, mut path: Vec<(PageId, Node)>, mut removed: PageId) -> Result<()> {
        loop {
            let Some((page, node)) = path.pop() else {
                // The removed node was the root: the tree is empty.
                self.set_root(None)?;
                return Ok(());
            };
            let Node::Internal {
                first,
                mut separators,
            } = node
            else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "B+Tree ancestor must be internal",
                ));
            };
            let original_len = separators.len();
            let mut new_first = first;
            if first == removed {
                match separators.first() {
                    // Promote the first separator's child into `first`.
                    Some(entry) => {
                        new_first = entry.child;
                        separators.remove(0);
                    }
                    None => {
                        // The parent had one child, which just emptied.
                        if path.is_empty() {
                            self.set_root(None)?;
                        } else {
                            removed = page;
                            continue;
                        }
                        return Ok(());
                    }
                }
            } else {
                separators.retain(|entry| entry.child != removed);
                if separators.len() == original_len {
                    // A deeper level already unlinked this child.
                    return Ok(());
                }
            }
            if separators.is_empty() {
                // The root contracted to a single child: root becomes that child.
                self.set_root(Some(new_first))?;
                return Ok(());
            }
            self.write_node_at(
                page,
                &Node::Internal {
                    first: new_first,
                    separators,
                },
            )?;
            return Ok(());
        }
    }
}

impl BTreeIndex {
    // -- ordered scans ------------------------------------------------------

    /// Resolves the starting leaf position for a scan: either the leftmost
    /// leaf, or the run-normalized position for `key`.
    ///
    /// Inclusive bounds rewind to the run start (a binary search may land
    /// mid-run after chunking); exclusive bounds advance past the whole run,
    /// including chunks spilling into later nodes. Both normalizations are
    /// read-only page walks bounded by the run length.
    fn scan_start(
        &mut self,
        from: Option<&[u8]>,
        inclusive: bool,
    ) -> Result<Option<(PageId, usize)>> {
        let leaf_page = match from {
            Some(key) => match self.path_to(key)?.last() {
                Some((page, _)) => *page,
                None => return Ok(None),
            },
            None => match self.leftmost_leaf()? {
                Some(page) => page,
                None => return Ok(None),
            },
        };
        let Some(key) = from else {
            return Ok(Some((leaf_page, 0)));
        };
        let (mut page, mut index) = self.rewind_to_key(leaf_page, key)?;
        if !inclusive {
            // Skip the whole matching run, even across nodes.
            loop {
                let node = self.read_node(page)?;
                let Node::Leaf { entries, next, .. } = node else {
                    return Err(PlomidError::new(
                        ErrorKind::Internal,
                        "scan descent must end at a leaf",
                    ));
                };
                while index < entries.len() && entries[index].key.as_slice() == key {
                    index += 1;
                }
                if index < entries.len() {
                    return Ok(Some((page, index)));
                }
                match next {
                    Some(next_page) => {
                        page = next_page;
                        index = 0;
                    }
                    None => return Ok(Some((page, index))),
                }
            }
        }
        Ok(Some((page, index)))
    }

    /// Collects every entry in the requested window, walking the leaf chain
    /// from the starting leaf without re-descending from the root.
    ///
    /// Same-key chunks merge into single entries with concatenated ascending
    /// RowIds, preserving the historical one-entry-per-key output contract.
    fn collect_range(
        &mut self,
        start: Option<&[u8]>,
        start_inclusive: bool,
        end: Option<&[u8]>,
        end_inclusive: bool,
    ) -> Result<Vec<IndexEntry>> {
        let mut output: Vec<IndexEntry> = Vec::new();
        let mut pending: Option<IndexEntry> = None;
        // Merges each visited entry into the pending output row when keys
        // match, so chunked postings surface as one entry per key.
        let mut emit = |key: Vec<u8>, ids: Vec<RowId>| match pending.as_mut() {
            Some(current) if current.key == key => {
                current.row_ids.extend_from_slice(&ids);
            }
            _ => {
                if let Some(finished) = pending.take() {
                    output.push(finished);
                }
                pending = Some(IndexEntry { key, row_ids: ids });
            }
        };
        let Some((mut leaf_page, mut index)) = self.scan_start(start, start_inclusive)? else {
            return Ok(output);
        };
        loop {
            let node = self.read_node(leaf_page)?;
            let Node::Leaf { entries, next, .. } = node else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "leaf chain must contain leaves only",
                ));
            };
            while index < entries.len() {
                let entry = &entries[index];
                if let Some(bound) = end {
                    let ordering = entry.key.as_slice().cmp(bound);
                    let beyond = if end_inclusive {
                        ordering == std::cmp::Ordering::Greater
                    } else {
                        ordering != std::cmp::Ordering::Less
                    };
                    if beyond {
                        if let Some(finished) = pending.take() {
                            output.push(finished);
                        }
                        return Ok(output);
                    }
                }
                emit(entry.key.clone(), entry.row_ids.clone());
                index += 1;
            }
            match next {
                Some(next_page) => {
                    leaf_page = next_page;
                    index = 0;
                }
                None => {
                    if let Some(finished) = pending.take() {
                        output.push(finished);
                    }
                    return Ok(output);
                }
            }
        }
    }

    /// Full ordered scan of the index.
    pub fn scan_all(&mut self) -> Result<Vec<IndexEntry>> {
        self.collect_range(None, true, None, false)
    }

    /// Ordered scan between bounds; see [`Bound`] for the boundary algebra.
    pub fn range_scan(&mut self, start: Bound, end: Bound) -> Result<Vec<IndexEntry>> {
        let (from, from_inclusive) = match &start {
            Bound::Included(key) => (Some(key.as_slice()), true),
            Bound::Excluded(key) => (Some(key.as_slice()), false),
            Bound::Unbounded => (None, true),
        };
        let (to, to_inclusive) = match &end {
            Bound::Included(key) => (Some(key.as_slice()), true),
            Bound::Excluded(key) => (Some(key.as_slice()), false),
            Bound::Unbounded => (None, false),
        };
        self.collect_range(from, from_inclusive, to, to_inclusive)
    }

    /// First entry whose key is greater than or equal to `key`.
    pub fn lower_bound(&mut self, key: &[u8]) -> Result<Option<IndexEntry>> {
        Ok(self
            .collect_range(Some(key), true, None, false)?
            .into_iter()
            .next())
    }

    /// First entry whose key is strictly greater than `key`.
    pub fn upper_bound(&mut self, key: &[u8]) -> Result<Option<IndexEntry>> {
        Ok(self
            .collect_range(Some(key), false, None, false)?
            .into_iter()
            .next())
    }

    /// Smallest key in the index.
    pub fn first(&mut self) -> Result<Option<IndexEntry>> {
        Ok(self.scan_all()?.into_iter().next())
    }

    /// Largest key in the index, read from the rightmost leaf.
    ///
    /// Merges a trailing same-key run backward so chunked postings report
    /// the complete RowId set, mirroring the forward merge.
    pub fn last(&mut self) -> Result<Option<IndexEntry>> {
        let Some(mut page) = self.rightmost_leaf()? else {
            return Ok(None);
        };
        let mut merged: Option<IndexEntry> = None;
        loop {
            let node = self.read_node(page)?;
            let Node::Leaf { entries, prev, .. } = node else {
                return Ok(None);
            };
            // Walk entries back-to-front, merging one trailing run.
            let mut index = entries.len();
            let mut key: Option<Vec<u8>> = merged.as_ref().map(|entry| entry.key.clone());
            while index > 0 {
                index -= 1;
                let entry = &entries[index];
                match &key {
                    Some(current) if *current == entry.key => {
                        let ids = std::mem::replace(
                            &mut merged.as_mut().expect("merged present").row_ids,
                            Vec::new(),
                        );
                        let mut combined = entry.row_ids.clone();
                        combined.extend(ids);
                        merged.as_mut().expect("merged present").row_ids = combined;
                    }
                    Some(_) => return Ok(merged),
                    None => {
                        key = Some(entry.key.clone());
                        merged = Some(IndexEntry {
                            key: entry.key.clone(),
                            row_ids: entry.row_ids.clone(),
                        });
                    }
                }
            }
            match prev {
                // An empty predecessor ends the walk only when a run is
                // already open... it never opens one: keep walking past
                // empties (they carry no keys to extend the run, but the
                // run may continue beyond them).
                Some(prev_page) => {
                    let prev_node = self.read_node(prev_page)?;
                    let Node::Leaf { entries, .. } = prev_node else {
                        return Ok(merged);
                    };
                    if entries.is_empty() {
                        page = prev_page;
                        continue;
                    }
                    let Some(last) = entries.last() else {
                        page = prev_page;
                        continue;
                    };
                    match &key {
                        // Run continues backward only on equal keys; a lower
                        // key ends it (chain order guarantees nothing earlier
                        // can rejoin it).
                        Some(current) if last.key == *current => {
                            page = prev_page;
                        }
                        Some(_) => return Ok(merged),
                        // No run open yet (rightmost node was empty): adopt
                        // the predecessor's trailing run instead of stopping.
                        None => {
                            page = prev_page;
                        }
                    }
                }
                None => return Ok(merged),
            }
        }
    }

    /// Reverse ordered scan using the `prev` leaf links. The forward scan
    /// remains the default executor path; this serves descending requests.
    /// Same-key chunks merge exactly like the forward scan (in reverse).
    pub fn scan_all_reverse(&mut self) -> Result<Vec<IndexEntry>> {
        let mut reversed: Vec<IndexEntry> = Vec::new();
        let mut pending: Option<IndexEntry> = None;
        let mut emit = |key: Vec<u8>, ids: Vec<RowId>| {
            match pending.as_mut() {
                Some(current) if current.key == key => {
                    // Descending collection: prepend to preserve order.
                    let mut combined = ids;
                    combined.extend(std::mem::take(&mut current.row_ids));
                    current.row_ids = combined;
                }
                _ => {
                    if let Some(finished) = pending.take() {
                        reversed.push(finished);
                    }
                    pending = Some(IndexEntry { key, row_ids: ids });
                }
            }
        };
        let Some(mut leaf_page) = self.rightmost_leaf()? else {
            return Ok(reversed);
        };
        loop {
            let node = self.read_node(leaf_page)?;
            let Node::Leaf { entries, prev, .. } = node else {
                return Err(PlomidError::new(
                    ErrorKind::Internal,
                    "leaf chain must contain leaves only",
                ));
            };
            for entry in entries.iter().rev() {
                emit(entry.key.clone(), entry.row_ids.clone());
            }
            match prev {
                Some(prev_page) => leaf_page = prev_page,
                None => {
                    if let Some(finished) = pending.take() {
                        reversed.push(finished);
                    }
                    return Ok(reversed);
                }
            }
        }
    }

    // -- persistence --------------------------------------------------------

    /// Flushes all dirty pages and fsyncs the index file.
    pub fn sync(&mut self) -> Result<()> {
        self.pool.sync()
    }

    /// Flushes and closes the index, returning the underlying file path.
    pub fn close(mut self) -> Result<PathBuf> {
        self.pool.sync()?;
        Ok(self.path)
    }

    /// Returns structural statistics: entry count, node pages, and depth.
    ///
    /// `node_pages` counts internal nodes plus all leaves reachable through
    /// the leaf chain (the chain is authoritative for leaf coverage).
    pub fn stats(&mut self) -> Result<BTreeStats> {
        let Some(mut page_id) = self.root_id() else {
            return Ok(BTreeStats {
                entry_count: 0,
                node_pages: 0,
                depth: 0,
            });
        };
        let mut depth = 0_u64;
        let mut internal_pages = 0_u64;
        loop {
            depth += 1;
            let node = self.read_node(page_id)?;
            match node {
                Node::Leaf { .. } => break,
                Node::Internal { first, .. } => {
                    internal_pages += 1;
                    page_id = first;
                }
            }
        }
        let mut leaf_pages = 0_u64;
        if let Some(leaf) = self.leftmost_leaf()? {
            let mut cursor = leaf;
            loop {
                leaf_pages += 1;
                let node = self.read_node(cursor)?;
                match node {
                    Node::Leaf { next, .. } => match next {
                        Some(next_page) => cursor = next_page,
                        None => break,
                    },
                    Node::Internal { .. } => {
                        return Err(PlomidError::new(
                            ErrorKind::Corruption,
                            "leaf chain reached an internal node",
                        ))
                    }
                }
            }
        }
        Ok(BTreeStats {
            entry_count: self.meta.entry_count,
            node_pages: internal_pages + leaf_pages,
            depth,
        })
    }
}

// -- WAL recovery integration ----------------------------------------------

/// The index is a WAL replay target: committed `Put` records carry an 8-byte
/// little-endian RowId as their value, and `Delete` records remove the whole
/// key. Replay goes through the existing `plomid_wal` recovery machinery —
/// `plomid_wal::recover_into(path, &mut index)` — with no index-specific WAL
/// format, reader, or recovery loop.
impl plomid_wal::ReplayHandler for BTreeIndex {
    fn on_record(&mut self, _record: &plomid_wal::Record) -> Result<bool> {
        Ok(true)
    }
}

impl plomid_wal::ReplayTarget for BTreeIndex {
    fn apply_put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let bytes: [u8; ROW_ID_SIZE] = value.try_into().map_err(|_| {
            PlomidError::new(
                ErrorKind::Corruption,
                "index WAL value must be an 8-byte little-endian RowId",
            )
        })?;
        self.insert(key, RowId::from_le_bytes(bytes))?;
        Ok(())
    }

    fn apply_delete(&mut self, key: &[u8]) -> Result<()> {
        self.delete(key, None, true)?;
        Ok(())
    }

    fn apply_sync(&mut self) -> Result<()> {
        self.sync()
    }
}

// -- Transaction adapter ----------------------------------------------------

/// Transaction adapter: interprets the KV-shaped [`DataStore`] surface over
/// key → RowId entries. A stored value is the 8-byte little-endian RowId of
/// the indexed row; the authoritative row remains in the row/storage layer,
/// and MVCC visibility continues to be applied there by the existing
/// transaction engine.
impl plomid_txn::DataStore for BTreeIndex {
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self
            .lookup(key)?
            .and_then(|row_ids| row_ids.first().map(|row| row.to_le_bytes().to_vec())))
    }

    fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let bytes: [u8; ROW_ID_SIZE] = value.try_into().map_err(|_| {
            PlomidError::new(
                ErrorKind::InvalidArgument,
                "index transaction value must be an 8-byte little-endian RowId",
            )
        })?;
        self.insert(key, RowId::from_le_bytes(bytes))?;
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<bool> {
        self.delete(key, None, true)
    }

    fn range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let from = start
            .map(|key| Bound::Included(key.to_vec()))
            .unwrap_or(Bound::Unbounded);
        let to = end
            .map(|key| Bound::Excluded(key.to_vec()))
            .unwrap_or(Bound::Unbounded);
        Ok(self
            .range_scan(from, to)?
            .into_iter()
            .map(|entry| {
                let value = entry
                    .row_ids
                    .first()
                    .map(|row| row.to_le_bytes().to_vec())
                    .unwrap_or_default();
                (entry.key, value)
            })
            .collect())
    }

    fn sync(&mut self) -> Result<()> {
        BTreeIndex::sync(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plomid_core::{GenerationId, IndexId, ObjectId};

    const POOL: usize = 64;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("plomid-btree-tests");
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir.join(name)
    }

    struct Scratch {
        path: std::path::PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = temp_path(name);
            let _ = std::fs::remove_file(&path);
            Self { path }
        }

        fn create(&self, unique: bool) -> BTreeIndex {
            BTreeIndex::create(
                &self.path,
                POOL,
                IndexId::new(1),
                ObjectId::new(1),
                GenerationId::new(1),
                unique,
            )
            .expect("create index")
        }

        fn open(&self) -> BTreeIndex {
            BTreeIndex::open(&self.path, POOL).expect("open index")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn rid(n: u64) -> RowId {
        RowId::new(n)
    }

    fn keys_of(entries: &[IndexEntry]) -> Vec<Vec<u8>> {
        entries.iter().map(|entry| entry.key.clone()).collect()
    }

    // -- basic ---------------------------------------------------------------

    #[test]
    fn empty_tree_lookups_and_scans() {
        let mut index = Scratch::new("empty").create(false);
        assert_eq!(index.entry_count(), 0);
        assert!(index.root_page().is_none());
        assert_eq!(index.lookup(b"missing").unwrap(), None);
        assert!(!index.contains(b"missing").unwrap());
        assert!(index.scan_all().unwrap().is_empty());
        assert!(index.first().unwrap().is_none());
        assert!(index.last().unwrap().is_none());
        assert!(index.scan_all_reverse().unwrap().is_empty());
        let stats = index.stats().unwrap();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.node_pages, 0);
    }

    /// Regression: the offset-based read path (used by `contains`/`lookup`)
    /// must agree with the full decode path, including across reopen, with
    /// duplicate-heavy keys, variable key sizes, and unchanged-outcome inserts
    /// that no longer rewrite pages.
    #[test]
    fn offset_lookup_matches_decode_across_reopen() {
        let path = temp_path("offset-regression");
        let _ = std::fs::remove_file(&path);
        let expected: Vec<(Vec<u8>, Vec<RowId>)> = (0..500_u64)
            .map(|n| {
                let key = {
                    let mut k = vec![b'x'; (n % 7) as usize];
                    k.extend_from_slice(&n.to_be_bytes());
                    k
                };
                let rows: Vec<RowId> = (0..(n % 4) + 1).map(|d| rid(n * 10 + d)).collect();
                (key, rows)
            })
            .collect();
        {
            let mut index = BTreeIndex::create(
                &path,
                64,
                IndexId::new(1),
                ObjectId::new(1),
                GenerationId::new(1),
                false,
            )
            .expect("create");
            for (key, rows) in &expected {
                for row in rows {
                    index.insert(key, *row).expect("insert");
                    // Re-inserting the same (key, row) is Unchanged and must
                    // leave the index readable and uncorrupted.
                    assert!(index.contains(key).expect("contains"));
                }
            }
            for (key, rows) in &expected {
                assert_eq!(index.lookup(key).expect("lookup"), Some(rows.clone()));
                assert!(index.contains(key).expect("contains"));
            }
            assert_eq!(index.entry_count(), expected.len() as u64);
            index.sync().expect("sync");
        }
        let mut reopened = BTreeIndex::open(&path, 64).expect("reopen");
        assert_eq!(reopened.entry_count(), expected.len() as u64);
        for (key, rows) in &expected {
            assert_eq!(reopened.lookup(key).expect("lookup"), Some(rows.clone()));
        }
        // The decode path must see exactly the same entries (key order).
        let mut expected_sorted = expected.clone();
        expected_sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let decoded: Vec<(Vec<u8>, Vec<RowId>)> = reopened
            .scan_all()
            .expect("scan")
            .into_iter()
            .map(|entry| (entry.key, entry.row_ids))
            .collect();
        assert_eq!(decoded, expected_sorted);
        reopened.close().expect("close");
        let _ = std::fs::remove_file(&path);
    }

    /// Regression: a sequential build must not degenerate into a linked list.
    /// A previous optimization double-popped the insert path on split,
    /// promoting separators one level too high and producing a depth-245 tree
    /// for 100K entries. The shape is asserted here via `stats()`.
    #[test]
    fn sequential_build_keeps_balanced_shape() {
        let path = temp_path("shape");
        let _ = std::fs::remove_file(&path);
        let mut index = BTreeIndex::create(
            &path,
            64,
            IndexId::new(1),
            ObjectId::new(1),
            GenerationId::new(1),
            false,
        )
        .expect("create");
        let entries = 20_000_u64;
        for n in 0..entries {
            index.insert(&n.to_be_bytes(), rid(n)).expect("insert");
        }
        let stats = index.stats().expect("stats");
        assert_eq!(stats.entry_count, entries);
        // ~819 8-byte entries per leaf: depth 1..=3 for this size. A degenerate
        // build reaches depth in the hundreds.
        assert!(stats.depth <= 4, "degenerate tree: {stats:?}");
        assert!(stats.node_pages < entries / 50, "sparse tree: {stats:?}");
        index.close().expect("close");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn single_key_round_trip() {
        let mut index = Scratch::new("single").create(false);
        assert_eq!(index.insert(b"k", rid(1)).unwrap(), InsertOutcome::Inserted);
        assert_eq!(index.lookup(b"k").unwrap(), Some(vec![rid(1)]));
        assert_eq!(
            index.insert(b"k", rid(1)).unwrap(),
            InsertOutcome::Unchanged
        );
        assert!(index.contains(b"k").unwrap());
        assert!(index.delete(b"k", None, true).unwrap());
        assert!(!index.contains(b"k").unwrap());
        assert_eq!(index.entry_count(), 0);
    }

    #[test]
    fn unique_index_rejects_duplicate_key() {
        let mut index = Scratch::new("unique-dup").create(true);
        index.insert(b"k", rid(1)).unwrap();
        let error = index.insert(b"k", rid(2)).unwrap_err();
        assert_eq!(error.kind(), plomid_core::ErrorKind::Conflict);
        assert_eq!(
            index.insert(b"k", rid(1)).unwrap(),
            InsertOutcome::Unchanged
        );
    }

    #[test]
    fn non_unique_index_appends_row_ids_in_order() {
        let mut index = Scratch::new("nonunique").create(false);
        for row in [rid(5), rid(1), rid(3)] {
            index.insert(b"k", row).unwrap();
        }
        assert_eq!(
            index.lookup(b"k").unwrap(),
            Some(vec![rid(1), rid(3), rid(5)])
        );
        assert!(index.delete(b"k", Some(rid(3)), false).unwrap());
        assert_eq!(index.lookup(b"k").unwrap(), Some(vec![rid(1), rid(5)]));
        assert!(index.delete(b"k", None, true).unwrap());
        assert_eq!(index.lookup(b"k").unwrap(), None);
    }

    #[test]
    fn delete_missing_key_is_a_no_op() {
        let mut index = Scratch::new("delete-missing").create(false);
        index.insert(b"present", rid(1)).unwrap();
        assert!(!index.delete(b"missing", None, true).unwrap());
        assert!(!index.delete(b"present", Some(rid(9)), false).unwrap());
        assert!(index.contains(b"present").unwrap());
    }

    // -- ordering ------------------------------------------------------------

    #[test]
    fn sequential_reverse_and_random_inserts_stay_ordered() {
        let mut index = Scratch::new("ordering").create(false);
        let mut expected = std::collections::BTreeSet::new();
        for n in 0..200_u64 {
            index.insert(&n.to_be_bytes(), rid(n)).unwrap();
            expected.insert(n.to_be_bytes().to_vec());
        }
        for n in (0..200_u64).rev() {
            index.insert(&(1_000 + n).to_be_bytes(), rid(n)).unwrap();
            expected.insert((1_000 + n).to_be_bytes().to_vec());
        }
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        for _ in 0..300 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = (state >> 33) % 2_000;
            index.insert(&n.to_be_bytes(), rid(n)).unwrap();
            expected.insert(n.to_be_bytes().to_vec());
        }
        let scanned = index.scan_all().unwrap();
        assert_eq!(scanned.len(), expected.len());
        let keys = keys_of(&scanned);
        let expected: Vec<Vec<u8>> = expected.into_iter().collect();
        assert_eq!(keys, expected);
        let mut reversed = index.scan_all_reverse().unwrap();
        reversed.reverse();
        assert_eq!(keys_of(&reversed), keys);
        assert_eq!(index.entry_count(), expected.len() as u64);
    }

    #[test]
    fn binary_keys_and_mixed_key_sizes_order_lexicographically() {
        let mut index = Scratch::new("binary").create(false);
        let cases: Vec<Vec<u8>> = vec![
            vec![0x00],
            vec![0x00, 0x00],
            vec![0x00, 0x01],
            vec![0x7F],
            vec![0x80],
            vec![0xFF],
            vec![0xFF, 0xFF, 0xFF],
            vec![],
        ];
        for key in &cases {
            index.insert(key, rid(1)).unwrap();
        }
        let scanned = index.scan_all().unwrap();
        let keys = keys_of(&scanned);
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert!(index.contains(&[]).unwrap());
    }

    // -- splits ---------------------------------------------------------------

    #[test]
    fn large_keys_force_leaf_and_cascading_splits() {
        let mut index = Scratch::new("splits").create(false);
        let value = 2_048_usize;
        for n in 0..200_u64 {
            let mut key = format!("key-{n:012}").into_bytes();
            key.resize(value, b'x');
            index.insert(&key, rid(n)).unwrap();
        }
        assert_eq!(index.entry_count(), 200);
        let stats = index.stats().unwrap();
        assert!(stats.depth > 1, "large keys must split the root");
        // Every key remains reachable.
        for n in 0..200_u64 {
            let mut key = format!("key-{n:012}").into_bytes();
            key.resize(value, b'x');
            assert!(index.contains(&key).unwrap(), "key {n} missing");
        }
    }

    #[test]
    fn root_split_creates_internal_root_with_two_leaves() {
        let mut index = Scratch::new("root-split").create(false);
        // Small keys still overflow one 16 KiB leaf after enough entries.
        for n in 0..1_000_u64 {
            index.insert(&n.to_be_bytes(), rid(n)).unwrap();
        }
        let stats = index.stats().unwrap();
        assert!(stats.depth >= 2);
        assert!(index.contains(&499_u64.to_be_bytes()).unwrap());
        let scanned = index.scan_all().unwrap();
        assert_eq!(scanned.len(), 1_000);
    }

    #[test]
    fn leaf_chain_is_fully_linked_after_splits() {
        let mut index = Scratch::new("chain").create(false);
        for n in 0..800_u64 {
            index.insert(&n.to_be_bytes(), rid(n)).unwrap();
        }
        // Forward chain walk must visit every key exactly once.
        let forward = index.scan_all().unwrap();
        assert_eq!(forward.len(), 800);
        assert_eq!(forward.first().unwrap().key, 0_u64.to_be_bytes().to_vec());
        assert_eq!(forward.last().unwrap().key, 799_u64.to_be_bytes().to_vec());
        let backward = index.scan_all_reverse().unwrap();
        assert_eq!(
            keys_of(&forward),
            keys_of(&backward).into_iter().rev().collect::<Vec<_>>()
        );
    }

    // -- deletes and rebalance ----------------------------------------------

    #[test]
    fn deleting_all_keys_leaves_a_valid_empty_tree() {
        let mut index = Scratch::new("drain").create(false);
        for n in 0..500_u64 {
            index.insert(&n.to_be_bytes(), rid(n)).unwrap();
        }
        for n in 0..500_u64 {
            assert!(index.delete(&n.to_be_bytes(), None, true).unwrap());
        }
        assert_eq!(index.entry_count(), 0);
        assert!(index.root_page().is_none());
        assert!(index.scan_all().unwrap().is_empty());
        // The drained tree accepts inserts again.
        index.insert(&7_u64.to_be_bytes(), rid(7)).unwrap();
        assert!(index.contains(&7_u64.to_be_bytes()).unwrap());
    }

    #[test]
    fn deletion_collapses_to_sibling_and_contracts_root() {
        let mut index = Scratch::new("contract").create(false);
        for n in 0..600_u64 {
            index.insert(&n.to_be_bytes(), rid(n)).unwrap();
        }
        // Delete the entire upper half: many leaves empty and merge upward.
        for n in 300..600_u64 {
            assert!(index.delete(&n.to_be_bytes(), None, true).unwrap());
        }
        assert_eq!(index.entry_count(), 300);
        let scanned = index.scan_all().unwrap();
        assert_eq!(scanned.len(), 300);
        assert_eq!(
            keys_of(&scanned),
            (0..300_u64)
                .map(|n| n.to_be_bytes().to_vec())
                .collect::<Vec<_>>()
        );
        // Continue draining: the root must eventually contract to nothing.
        for n in 0..300_u64 {
            assert!(index.delete(&n.to_be_bytes(), None, true).unwrap());
        }
        assert!(index.root_page().is_none());
    }

    // -- range scans ----------------------------------------------------------

    #[test]
    fn range_bound_algebra() {
        let mut index = Scratch::new("bounds").create(false);
        for n in 0..10_u64 {
            index.insert(&n.to_be_bytes(), rid(n)).unwrap();
        }
        let key = |n: u64| n.to_be_bytes().to_vec();
        let closed = index
            .range_scan(Bound::Included(key(2)), Bound::Included(key(5)))
            .unwrap();
        assert_eq!(keys_of(&closed), vec![key(2), key(3), key(4), key(5)]);
        let half_open = index
            .range_scan(Bound::Included(key(2)), Bound::Excluded(key(5)))
            .unwrap();
        assert_eq!(keys_of(&half_open), vec![key(2), key(3), key(4)]);
        let open_open = index
            .range_scan(Bound::Excluded(key(2)), Bound::Excluded(key(5)))
            .unwrap();
        assert_eq!(keys_of(&open_open), vec![key(3), key(4)]);
        let open_closed = index
            .range_scan(Bound::Excluded(key(2)), Bound::Included(key(5)))
            .unwrap();
        assert_eq!(keys_of(&open_closed), vec![key(3), key(4), key(5)]);
        let lower_only = index
            .range_scan(Bound::Included(key(8)), Bound::Unbounded)
            .unwrap();
        assert_eq!(keys_of(&lower_only), vec![key(8), key(9)]);
        let upper_only = index
            .range_scan(Bound::Unbounded, Bound::Excluded(key(2)))
            .unwrap();
        assert_eq!(keys_of(&upper_only), vec![key(0), key(1)]);
        let empty = index
            .range_scan(Bound::Included(key(7)), Bound::Excluded(key(7)))
            .unwrap();
        assert!(empty.is_empty());
        let inverted = index
            .range_scan(Bound::Included(key(5)), Bound::Included(key(2)))
            .unwrap();
        assert!(inverted.is_empty());
        let below = index
            .range_scan(Bound::Included(vec![0u8; 1]), Bound::Excluded(key(2)))
            .unwrap();
        assert_eq!(keys_of(&below), vec![key(0), key(1)]);
        let above = index
            .range_scan(Bound::Included(key(9)), Bound::Unbounded)
            .unwrap();
        assert_eq!(keys_of(&above), vec![key(9)]);
    }

    #[test]
    fn bounds_and_first_last() {
        let mut index = Scratch::new("bound-points").create(false);
        for n in [10_u64, 20, 30, 40] {
            index.insert(&n.to_be_bytes(), rid(n)).unwrap();
        }
        let entry = index.lower_bound(&25_u64.to_be_bytes()).unwrap().unwrap();
        assert_eq!(entry.key, 30_u64.to_be_bytes().to_vec());
        let entry = index.lower_bound(&20_u64.to_be_bytes()).unwrap().unwrap();
        assert_eq!(entry.key, 20_u64.to_be_bytes().to_vec());
        let entry = index.upper_bound(&20_u64.to_be_bytes()).unwrap().unwrap();
        assert_eq!(entry.key, 30_u64.to_be_bytes().to_vec());
        assert!(index.upper_bound(&40_u64.to_be_bytes()).unwrap().is_none());
        assert_eq!(
            index.first().unwrap().unwrap().key,
            10_u64.to_be_bytes().to_vec()
        );
        assert_eq!(
            index.last().unwrap().unwrap().key,
            40_u64.to_be_bytes().to_vec()
        );
    }

    // -- persistence -----------------------------------------------------------

    #[test]
    fn state_survives_close_and_reopen() {
        let scratch = Scratch::new("reopen");
        {
            let mut index = scratch.create(false);
            for n in 0..300_u64 {
                index.insert(&n.to_be_bytes(), rid(n)).unwrap();
            }
            for n in 0..50_u64 {
                index.delete(&n.to_be_bytes(), None, true).unwrap();
            }
            index.sync().unwrap();
        }
        {
            let mut index = scratch.open();
            assert_eq!(index.entry_count(), 250);
            assert!(!index.contains(&10_u64.to_be_bytes()).unwrap());
            assert!(index.contains(&100_u64.to_be_bytes()).unwrap());
            index.insert(&1_000_u64.to_be_bytes(), rid(1_000)).unwrap();
            assert!(index.contains(&1_000_u64.to_be_bytes()).unwrap());
            assert!(index.delete(&100_u64.to_be_bytes(), None, true).unwrap());
            assert!(!index.contains(&100_u64.to_be_bytes()).unwrap());
            index.sync().unwrap();
        }
        {
            let mut index = scratch.open();
            assert_eq!(index.entry_count(), 250);
            let scanned = index.scan_all().unwrap();
            assert_eq!(scanned.len(), 250);
            assert!(scanned
                .iter()
                .zip(scanned.iter().skip(1))
                .all(|(a, b)| a.key < b.key));
        }
    }

    #[test]
    fn reopening_a_foreign_file_reports_corruption() {
        let scratch = Scratch::new("bad-magic");
        std::fs::write(&scratch.path, vec![0_u8; 16_384]).unwrap();
        let error = match BTreeIndex::open(&scratch.path, POOL) {
            Err(error) => error,
            Ok(_) => panic!("opening a file without root metadata must fail"),
        };
        assert_eq!(error.kind(), plomid_core::ErrorKind::Corruption);
    }

    // -- WAL recovery ------------------------------------------------------------

    fn append_txn(
        wal: &mut plomid_wal::SharedWal,
        txn: plomid_core::TxnId,
        timestamp: u64,
        puts: &[(&[u8], u64)],
        deletes: &[&[u8]],
    ) {
        wal.append(
            plomid_wal::RecordType::Begin,
            &plomid_wal::encode_begin(txn),
        )
        .unwrap();
        for (key, row) in puts {
            wal.append(
                plomid_wal::RecordType::Data,
                &plomid_wal::encode_data(
                    txn,
                    &plomid_wal::DataOperation::Put {
                        key: key.to_vec(),
                        value: row.to_le_bytes().to_vec(),
                    },
                )
                .unwrap(),
            )
            .unwrap();
        }
        for key in deletes {
            wal.append(
                plomid_wal::RecordType::Data,
                &plomid_wal::encode_data(
                    txn,
                    &plomid_wal::DataOperation::Delete { key: key.to_vec() },
                )
                .unwrap(),
            )
            .unwrap();
        }
        let lsn = wal
            .append(
                plomid_wal::RecordType::Commit,
                &plomid_wal::encode_commit_with_timestamp(
                    txn,
                    plomid_core::CommitTimestamp::new(timestamp),
                ),
            )
            .unwrap();
        wal.commit(lsn).unwrap();
    }

    #[test]
    fn committed_wal_transactions_replay_into_the_index() {
        let scratch = Scratch::new("wal-recovery");
        let wal_path = temp_path("wal-recovery.log");
        let _ = std::fs::remove_file(&wal_path);
        {
            let mut index = scratch.create(false);
            let mut wal = plomid_wal::SharedWal::create(&wal_path).unwrap();
            let txn1 = plomid_core::TxnId::new(1);
            let txn2 = plomid_core::TxnId::new(2);
            let txn3 = plomid_core::TxnId::new(3);
            append_txn(&mut wal, txn1, 1, &[(b"alpha", 11)], &[]);
            // Aborted transaction: must never reach the index.
            wal.append(
                plomid_wal::RecordType::Begin,
                &plomid_wal::encode_begin(txn3),
            )
            .unwrap();
            wal.append(
                plomid_wal::RecordType::Data,
                &plomid_wal::encode_data(
                    txn3,
                    &plomid_wal::DataOperation::Put {
                        key: b"aborted".to_vec(),
                        value: 99_u64.to_le_bytes().to_vec(),
                    },
                )
                .unwrap(),
            )
            .unwrap();
            let lsn = wal
                .append(
                    plomid_wal::RecordType::Abort,
                    &plomid_wal::encode_abort(txn3),
                )
                .unwrap();
            wal.commit(lsn).unwrap();
            append_txn(&mut wal, txn2, 2, &[(b"beta", 22)], &[]);
            drop(wal);

            // Replay through the existing plomid_wal recovery path.
            let report = plomid_wal::recover_target(&wal_path, &mut index).unwrap();
            assert_eq!(report.committed_transactions, 2);
            assert_eq!(report.applied_operations, 2);
            assert!(index.contains(b"alpha").unwrap());
            assert!(index.contains(b"beta").unwrap());
            assert!(!index.contains(b"aborted").unwrap());
            assert_eq!(index.lookup(b"alpha").unwrap(), Some(vec![rid(11)]));
        }
        let _ = std::fs::remove_file(&wal_path);
    }

    #[test]
    fn recovery_replays_splits_deterministically() {
        let scratch = Scratch::new("wal-splits");
        let wal_path = temp_path("wal-splits.log");
        let _ = std::fs::remove_file(&wal_path);
        {
            let mut index = scratch.create(false);
            let mut wal = plomid_wal::SharedWal::create(&wal_path).unwrap();
            for round in 0..3_u64 {
                let txn = plomid_core::TxnId::new(round + 1);
                let puts: Vec<(&[u8], u64)> = (0..300_u64)
                    .map(|n| {
                        let key = (round * 300 + n).to_be_bytes().to_vec();
                        (Box::leak(key.into_boxed_slice()) as &[u8], round * 300 + n)
                    })
                    .collect();
                append_txn(&mut wal, txn, round + 1, &puts, &[]);
            }
            drop(wal);

            let report = plomid_wal::recover_target(&wal_path, &mut index).unwrap();
            assert_eq!(report.applied_operations, 900);
            assert_eq!(index.entry_count(), 900);
            let stats = index.stats().unwrap();
            assert!(stats.depth >= 2, "900 entries must split the root");
            let scanned = index.scan_all().unwrap();
            assert_eq!(scanned.len(), 900);
        }
        let _ = std::fs::remove_file(&wal_path);
    }

    // -- transaction adapter -----------------------------------------------------

    #[test]
    fn datastore_adapter_round_trips_row_ids() {
        use plomid_txn::DataStore;
        let mut index = Scratch::new("datastore").create(false);
        // Trait calls are qualified because the inherent insert/delete take
        // RowIds while the adapter takes encoded values.
        DataStore::insert(&mut index, b"k1", &1_u64.to_le_bytes()).unwrap();
        DataStore::insert(&mut index, b"k2", &2_u64.to_le_bytes()).unwrap();
        assert_eq!(
            DataStore::get(&mut index, b"k1").unwrap(),
            Some(1_u64.to_le_bytes().to_vec())
        );
        assert_eq!(DataStore::get(&mut index, b"missing").unwrap(), None);
        let rows = DataStore::range(&mut index, Some(b"k1"), Some(b"k2")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, b"k1".to_vec());
        assert!(DataStore::delete(&mut index, b"k1").unwrap());
        assert_eq!(DataStore::get(&mut index, b"k1").unwrap(), None);
        DataStore::sync(&mut index).unwrap();
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::super::tree::*;
    use super::*;
    use plomid_core::{GenerationId, IndexId, ObjectId};

    const POOL: usize = 32;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("plomid-chunk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn open_fresh(name: &str, unique: bool) -> (BTreeIndex, std::path::PathBuf) {
        let path = temp_path(name);
        let index = BTreeIndex::create(
            &path,
            POOL,
            IndexId::new(1),
            ObjectId::new(1),
            GenerationId::new(1),
            unique,
        )
        .expect("create");
        (index, path)
    }

    fn ascending(n: u64) -> Vec<RowId> {
        (0..n).map(RowId::new).collect()
    }

    #[test]
    fn oversized_posting_round_trips() {
        let (mut index, path) = open_fresh("roundtrip", false);
        for id in ascending(5000) {
            index.insert(b"big", id).unwrap();
        }
        let found = index.lookup(b"big").unwrap().expect("present");
        assert_eq!(found, ascending(5000));
        assert!(index.contains(b"big").unwrap());
        // Single delete keeps order and completeness.
        assert!(index.delete(b"big", Some(RowId::new(2500)), false).unwrap());
        let found = index.lookup(b"big").unwrap().expect("present");
        assert_eq!(found.len(), 4999);
        assert!(!found.contains(&RowId::new(2500)));
        assert!(found.windows(2).all(|w| w[0] < w[1]), "ids stay ordered");
        // Delete-all removes every chunk.
        assert!(index.delete(b"big", None, true).unwrap());
        assert_eq!(index.lookup(b"big").unwrap(), None);
        assert!(!index.contains(b"big").unwrap());
        assert_eq!(index.entry_count(), 0);
        index.close().expect("close");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn posting_chunks_span_nodes_and_merge() {
        let (mut index, path) = open_fresh("span", false);
        for id in ascending(20_000) {
            index.insert(b"hot", id).unwrap();
        }
        // Surrounding keys keep the run bracketed across splits.
        for suffix in [b"a".as_slice(), b"z".as_slice()] {
            index.insert(suffix, RowId::new(999_999)).unwrap();
        }
        let found = index.lookup(b"hot").unwrap().expect("present");
        assert_eq!(found, ascending(20_000));
        // Range scans merge chunks into one entry per key.
        let range = index
            .range_scan(
                Bound::Included(b"hot".to_vec()),
                Bound::Included(b"hot".to_vec()),
            )
            .unwrap();
        assert_eq!(range.len(), 1, "one merged entry per key");
        assert_eq!(range[0].row_ids, ascending(20_000));
        // Full scans (both directions) agree.
        let all = index.scan_all().unwrap();
        assert_eq!(all.len(), 3);
        let hot = all.iter().find(|entry| entry.key == b"hot").expect("hot");
        assert_eq!(hot.row_ids, ascending(20_000));
        let reversed = index.scan_all_reverse().unwrap();
        assert_eq!(reversed.len(), 3);
        let hot = reversed
            .iter()
            .find(|entry| entry.key == b"hot")
            .expect("hot");
        assert_eq!(hot.row_ids, ascending(20_000));
        // Bounds see the merged run.
        assert_eq!(
            index
                .lower_bound(b"hot")
                .unwrap()
                .expect("lower")
                .row_ids
                .len(),
            20_000
        );
        assert!(index.upper_bound(b"hot").unwrap().expect("upper").key != b"hot");
        index.close().expect("close");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn chunked_delete_keeps_order_and_counts() {
        let (mut index, path) = open_fresh("delorder", false);
        for id in ascending(6000) {
            index.insert(b"k", id).unwrap();
        }
        // Delete every third id.
        for id in (0..6000).step_by(3) {
            assert!(index.delete(b"k", Some(RowId::new(id)), false).unwrap());
        }
        let found = index.lookup(b"k").unwrap().expect("present");
        assert_eq!(found.len(), 4000);
        assert!(found.windows(2).all(|w| w[0] < w[1]));
        assert!(found.iter().all(|id| id.get() % 3 != 0));
        // Reopen: counts and content survive (entry_count verified).
        index.sync().expect("sync");
        index.close().expect("close");
        let mut reopened = BTreeIndex::open(&path, POOL).expect("reopen");
        let found = reopened.lookup(b"k").unwrap().expect("present");
        assert_eq!(found.len(), 4000);
        let stats = reopened.stats().expect("stats");
        assert!(
            stats.entry_count >= 1,
            "chunks counted, got {}",
            stats.entry_count
        );
        reopened.close().expect("close");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn oversized_key_still_errors() {
        let (mut index, path) = open_fresh("bigkey", false);
        let big_key = vec![7u8; 20_000];
        let error = index.insert(&big_key, RowId::new(1)).unwrap_err();
        assert_eq!(error.kind(), plomid_core::ErrorKind::InvalidArgument);
        let _ = index.close();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unique_indexes_never_chunk() {
        let (mut index, path) = open_fresh("unique", true);
        index.insert(b"k", RowId::new(1)).unwrap();
        assert_eq!(
            index.insert(b"k", RowId::new(1)).unwrap(),
            InsertOutcome::Unchanged
        );
        let error = index.insert(b"k", RowId::new(2)).unwrap_err();
        assert_eq!(error.kind(), plomid_core::ErrorKind::Conflict);
        assert_eq!(index.lookup(b"k").unwrap(), Some(vec![RowId::new(1)]));
        let _ = index.close();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn duplicate_insert_stays_idempotent_across_chunks() {
        let (mut index, path) = open_fresh("dupid", false);
        for id in ascending(3000) {
            index.insert(b"k", id).unwrap();
        }
        // Re-inserting every id must be a no-op, including ids in head chunks.
        for id in ascending(3000) {
            assert_eq!(index.insert(b"k", id).unwrap(), InsertOutcome::Unchanged);
        }
        assert_eq!(index.lookup(b"k").unwrap().expect("present").len(), 3000);
        let _ = index.close();
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod bulk_tests {
    use super::super::tree::*;
    use super::*;
    use plomid_core::{GenerationId, IndexId, ObjectId};

    const POOL: usize = 32;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("plomid-bulk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn open_fresh(name: &str) -> (BTreeIndex, std::path::PathBuf) {
        let path = temp_path(name);
        let index = BTreeIndex::create(
            &path,
            POOL,
            IndexId::new(1),
            ObjectId::new(1),
            GenerationId::new(1),
            false,
        )
        .expect("create");
        (index, path)
    }

    /// 25-key, 800-rows-each build: bulk construction must resolve
    /// identically to per-row insertion across every read path.
    fn bulk_rows() -> Vec<(Vec<u8>, RowId)> {
        let mut rows = Vec::with_capacity(20_000);
        for id in 0..20_000_u64 {
            rows.push((format!("k-{:02}", id % 25).into_bytes(), RowId::new(id)));
        }
        rows
    }

    #[test]
    fn bulk_build_matches_incremental_insert() {
        let (mut incremental, incremental_path) = open_fresh("inc");
        let (mut bulk, bulk_path) = open_fresh("blk");
        let rows = bulk_rows();
        for (key, id) in &rows {
            incremental.insert(key, *id).unwrap();
        }
        bulk.build_bulk(rows).unwrap();
        for suffix in 0..25_u32 {
            let key = format!("k-{suffix:02}");
            assert_eq!(
                incremental.lookup(key.as_bytes()).unwrap(),
                bulk.lookup(key.as_bytes()).unwrap(),
                "lookup must match for {key}"
            );
        }
        assert_eq!(
            incremental.scan_all().unwrap(),
            bulk.scan_all().unwrap(),
            "forward scans must match"
        );
        assert_eq!(
            incremental.scan_all_reverse().unwrap(),
            bulk.scan_all_reverse().unwrap(),
            "reverse scans must match"
        );
        assert_eq!(incremental.entry_count(), bulk.entry_count());
        let _ = incremental.close();
        let _ = bulk.close();
        let _ = std::fs::remove_file(&incremental_path);
        let _ = std::fs::remove_file(&bulk_path);
    }

    /// Bulk-built trees survive close/reopen with identical content, and
    /// support deletes (including middle-node partial deletes) afterwards.
    #[test]
    fn bulk_build_reopens_and_deletes() {
        let (mut bulk, path) = open_fresh("reopen");
        bulk.build_bulk(bulk_rows()).unwrap();
        bulk.sync().unwrap();
        bulk.close().unwrap();
        let mut reopened = BTreeIndex::open(&path, POOL).unwrap();
        let found = reopened.lookup(b"k-07").unwrap().expect("present");
        assert_eq!(found.len(), 800);
        // Delete every third id of one key across chunks and nodes.
        for id in (7..20_000_u64).step_by(25 * 3) {
            assert!(reopened
                .delete(b"k-07", Some(RowId::new(id)), false)
                .unwrap());
        }
        let found = reopened.lookup(b"k-07").unwrap().expect("present");
        assert!(found.windows(2).all(|w| w[0] < w[1]), "order kept");
        assert!(found.iter().all(|id| (id.get() - 7) % 75 != 0));
        reopened.close().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    /// Unique keys and single-key trees pack correctly, including the
    /// single-leaf root case and out-of-order input.
    #[test]
    fn bulk_build_unique_and_single_key() {
        let (mut bulk, path) = open_fresh("uniq");
        let mut rows: Vec<(Vec<u8>, RowId)> = (0..5000_u64)
            .map(|id| (format!("u-{id:05}").into_bytes(), RowId::new(5000 - id)))
            .collect();
        rows.reverse();
        bulk.build_bulk(rows).unwrap();
        assert_eq!(bulk.entry_count(), 5000);
        assert_eq!(
            bulk.lookup(b"u-01234").unwrap(),
            Some(vec![RowId::new(5000 - 1234)])
        );
        assert_eq!(
            bulk.lower_bound(b"u-01234").unwrap().expect("lower").key,
            b"u-01234".to_vec()
        );
        bulk.close().unwrap();
        let _ = std::fs::remove_file(&path);

        let (mut single, single_path) = open_fresh("single");
        single
            .build_bulk(vec![(b"only".to_vec(), RowId::new(9))])
            .unwrap();
        assert_eq!(single.lookup(b"only").unwrap(), Some(vec![RowId::new(9)]));
        single.close().unwrap();
        let _ = std::fs::remove_file(&single_path);
    }

    /// Empty input yields an empty tree, not an error.
    #[test]
    fn bulk_build_empty_is_empty() {
        let (mut bulk, path) = open_fresh("empty");
        bulk.build_bulk(Vec::new()).unwrap();
        assert_eq!(bulk.entry_count(), 0);
        assert_eq!(bulk.lookup(b"anything").unwrap(), None);
        bulk.close().unwrap();
        let _ = std::fs::remove_file(&path);
    }
}
