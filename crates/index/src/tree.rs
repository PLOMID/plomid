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
//! The in-memory adaptive radix tree index.
//!
//! # What this is
//!
//! `ArtIndex` maps a variable-length **byte** key to one or more **logical**
//! [`RowId`] values. It is a pure in-memory structure: it owns no pages, no
//! file handles, and no durability protocol. Everything it stores is a logical
//! reference plus the key bytes needed to reach it.
//!
//! ```text
//! SQL predicate
//!       │
//!       ▼
//! ArtIndex::lookup(key)      ← candidate RowIds (no visibility decisions)
//!       │
//!       ▼
//! existing MVCC visibility rules
//!       │
//!       ▼
//! visible rows
//! ```
//!
//! ART deliberately stops at the first arrow's right-hand side. It never
//! consults a transaction id, a commit sequence number, or a snapshot: a single
//! `ArtIndex` is either private to one writer or shared read-only, and the
//! caller decides which of the returned RowIds are visible.
//!
//! # Concurrency
//!
//! Mirroring the single-writer B+Tree in `plomid-storage`
//! (`BTreeIndex::insert(&mut self, …)`), mutation takes `&mut self` and lookup
//! takes `&self`, with **no internal locking**. There is no latch, no version
//! counter, and no lock-free claim: callers must serialize mutation through
//! whatever lock the engine already holds (today the engine-level lock, or the
//! single writer that owns the index). This is the same contract the existing
//! storage index offers, so no new concurrency framework is introduced.
//!
//! # Uniqueness
//!
//! The same structure serves both index kinds. [`IndexKind::Unique`] rejects a
//! second, different RowId for a key with [`ArtError::DuplicateKey`] (mapped by
//! the engine onto its existing `PL-CONFLICT` kind); [`IndexKind::NonUnique`]
//! keeps a sorted set of RowIds per key. Constraint *policy* (when a violation
//! is reported, deferrable constraints, and so on) stays with the existing
//! planner/executor/constraint code — ART only reports the conflict.
//!
//! # Persistence
//!
//! Not implemented here, on purpose. `crates/index` depends only on
//! `plomid-core`, has no buffer-pool/page/WAL dependency, and therefore cannot
//! and does not claim crash safety or recovery. See the crate docs in `lib.rs`
//! for the exact persistence boundary and what a later phase must provide.

use plomid_core::RowId;

use crate::error::ArtError;
use crate::invariant::{validate_tree, InvariantViolation};
use crate::key::{common_prefix_len, ArtKey};
use crate::leaf::{LeafInsert, LeafRemove, LeafValues};
use crate::node::{Node, NodeKind};

/// How an index treats multiple RowIds for a single key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexKind {
    /// At most one RowId per key (primary key or unique index).
    Unique,
    /// Many RowIds per key (secondary index).
    NonUnique,
}

/// One `key → RowId` pair produced by enumeration.
///
/// Keys are owned here because enumeration reports whole keys, not slices into
/// internal prefix storage (which is split across nodes).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexEntry {
    key: Vec<u8>,
    row_id: RowId,
}

impl IndexEntry {
    /// The full key bytes for this entry.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        self.key.as_slice()
    }

    /// The logical RowId stored under the key.
    #[must_use]
    pub fn row_id(&self) -> RowId {
        self.row_id
    }
}

/// Point-in-time structural statistics.
///
/// `bytes` is an approximation based on `size_of` plus heap allocations that
/// ART itself owns (node tables, prefixes, leaf vectors). It is intended for
/// reporting memory per entry, not for allocator accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArtStats {
    /// Number of `key → RowId` pairs.
    pub entries: usize,
    /// Number of distinct keys.
    pub keys: usize,
    /// Number of reachable nodes.
    pub nodes: usize,
    /// Nodes per family, indexed by [`NodeKind`] declaration order.
    pub node_counts: [usize; 4],
    /// Deepest byte depth at which a node was found.
    pub max_depth: usize,
    /// Approximate bytes owned by the ART.
    pub bytes: usize,
}

/// An adaptive radix tree over byte keys, mapping to logical RowIds.
#[derive(Clone, Debug)]
pub struct ArtIndex {
    /// Root node. Its prefix may become non-empty after a split, so traversal
    /// always consumes the root's prefix before descending.
    root: Node,
    /// Uniqueness policy for this index.
    kind: IndexKind,
    /// Number of `key → RowId` pairs currently stored.
    entries: usize,
}

impl ArtIndex {
    /// Creates an empty unique index (at most one RowId per key).
    #[must_use]
    pub fn unique() -> Self {
        Self::with_kind(IndexKind::Unique)
    }

    /// Creates an empty non-unique index (many RowIds per key).
    #[must_use]
    pub fn non_unique() -> Self {
        Self::with_kind(IndexKind::NonUnique)
    }

    /// Creates an empty index with the given uniqueness policy.
    #[must_use]
    pub fn with_kind(kind: IndexKind) -> Self {
        Self {
            root: Node::empty_root(),
            kind,
            entries: 0,
        }
    }

    /// Rebuilds an in-memory index from the ordered entries of a durable index.
    ///
    /// The persistent index is the authority; an `ArtIndex` is always a derived
    /// runtime structure, so rebuilding is a replay of the same [`Self::insert`]
    /// the writer uses rather than a second persistence format:
    ///
    /// ```text
    /// persistent index generation  ← authority
    ///             │  ordered entries
    ///             ▼
    /// ArtIndex::rebuild_from_entries
    ///             │  in-memory structure
    ///             ▼
    ///         runtime lookup
    /// ```
    ///
    /// Insertion follows the caller's iteration order. A persistent B+Tree
    /// yields keys in order with each key's RowIds ascending, so the resulting
    /// shape is deterministic for a given durable state.
    ///
    /// The rebuilt structure is audited against the ART invariants before it is
    /// returned, so a reconstruction that produced an invalid tree is reported
    /// instead of being handed out as a usable index.
    pub fn rebuild_from_entries<I>(kind: IndexKind, entries: I) -> Result<Self, ArtError>
    where
        I: IntoIterator<Item = (Vec<u8>, RowId)>,
    {
        let mut index = Self::with_kind(kind);
        for (key, row_id) in entries {
            index.insert(key.as_slice(), row_id)?;
        }
        index.validate().map_err(|_| ArtError::Corrupt)?;
        Ok(index)
    }

    /// Returns this index's uniqueness policy.
    #[must_use]
    pub fn kind(&self) -> IndexKind {
        self.kind
    }

    /// Number of `key → RowId` pairs stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
    }

    /// Returns true when no key is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries == 0
    }

    /// Returns the root node.
    ///
    /// Exposed for structural validation, diagnostics, and benchmarks that need
    /// node-family distributions. Mutation still goes through the public
    /// insert/delete API.
    #[must_use]
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// Audits the whole structure against the ART invariants.
    ///
    /// O(entries) work, intended for tests and diagnostics.
    pub fn validate(&self) -> Result<(), InvariantViolation> {
        validate_tree(self)
    }

    /// Number of distinct keys, obtained by walking the tree.
    ///
    /// The maintained `len` counts `key → RowId` pairs, not keys, so a
    /// non-unique index is where the two differ.
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.stats().keys
    }

    /// Returns the RowIds stored for `key`, or `None` when the key is absent.
    ///
    /// The slice is borrowed from the tree, so the common path allocates
    /// nothing and copies nothing. Callers that expect at most one RowId should
    /// use [`ArtIndex::lookup_unique`] to avoid holding a borrowed slice.
    #[must_use]
    pub fn lookup(&self, key: &ArtKey) -> Option<&[RowId]> {
        self.find_node(key)
            .and_then(Node::terminal)
            .map(LeafValues::as_slice)
    }

    /// Returns the single RowId for `key` in a unique index.
    ///
    /// Returns the lowest RowId when the index is non-unique (which cannot
    /// happen for `IndexKind::Unique`, where a leaf holds at most one value).
    #[must_use]
    pub fn lookup_unique(&self, key: &ArtKey) -> Option<RowId> {
        self.find_node(key)
            .and_then(Node::terminal)
            .and_then(LeafValues::first)
    }

    /// Returns true when `key` is present, regardless of how many RowIds it has.
    #[must_use]
    pub fn contains(&self, key: &ArtKey) -> bool {
        self.find_node(key)
            .and_then(Node::terminal)
            .is_some_and(|values| !values.is_empty())
    }

    /// Returns true when `key` maps to exactly this RowId.
    #[must_use]
    pub fn contains_row(&self, key: &ArtKey, row_id: RowId) -> bool {
        self.find_node(key)
            .and_then(Node::terminal)
            .is_some_and(|values| values.contains(row_id))
    }

    /// Inserts `row_id` under `key`.
    ///
    /// * New key → [`LeafInsert::Inserted`].
    /// * Non-unique index, RowId already present → [`LeafInsert::AlreadyPresent`]
    ///   (idempotent; nothing is overwritten).
    /// * Unique index, a *different* RowId already present →
    ///   [`ArtError::DuplicateKey`], with the tree left unchanged.
    ///
    /// Node growth and prefix splits happen inside the recursive walk, so no
    /// caller ever observes a half-updated key: either the whole insert is
    /// applied or an error is returned first.
    pub fn insert(&mut self, key: &ArtKey, row_id: RowId) -> Result<LeafInsert, ArtError> {
        let outcome = insert_at(&mut self.root, key, 0, row_id, self.kind)?;
        if outcome == LeafInsert::Inserted {
            self.entries += 1;
        }
        Ok(outcome)
    }

    /// Removes `row_id` from `key`.
    ///
    /// Returns [`LeafRemove::Removed`] when the RowId was present, and
    /// [`LeafRemove::NotFound`] when either the key or the RowId is absent.
    /// Removing the last RowId of a key removes the key itself; the now-empty
    /// node is pruned and its ancestors shrink or merge as needed.
    pub fn delete(&mut self, key: &ArtKey, row_id: RowId) -> Result<LeafRemove, ArtError> {
        let outcome = delete_at(&mut self.root, key, 0, row_id);
        if outcome == LeafRemove::Removed {
            self.entries -= 1;
            compact_root(&mut self.root);
        }
        Ok(outcome)
    }

    /// Every `key → RowId` pair in the index, in ascending key order.
    ///
    /// This is an O(entries) snapshot intended for tests, diagnostics, and
    /// benchmarks. Equality lookups and primary-key lookups must use
    /// [`ArtIndex::lookup`], which walks only the key's own path.
    #[must_use]
    pub fn entries(&self) -> Vec<IndexEntry> {
        let mut out = Vec::with_capacity(self.entries);
        collect_entries(&self.root, Vec::new(), &mut out);
        out
    }

    /// Point-in-time structural statistics.
    ///
    /// Produced by walking the tree, so the numbers always describe the
    /// structure that is actually there.
    #[must_use]
    pub fn stats(&self) -> ArtStats {
        let mut stats = ArtStats {
            entries: self.entries,
            ..ArtStats::default()
        };
        collect_stats(&self.root, 1, &mut stats);
        stats
    }

    /// Walks to the node that holds `key`'s terminal slot, without allocating.
    ///
    /// The walk compares only the compressed prefix of each visited node (not
    /// the whole key) and stops as soon as a byte diverges, so a miss costs
    /// one prefix comparison per level.
    fn find_node(&self, key: &ArtKey) -> Option<&Node> {
        let mut node = &self.root;
        let mut position = 0usize;
        loop {
            let prefix = node.prefix();
            // `position <= key.len()` always holds, so this subtraction cannot
            // underflow and the slice below cannot panic.
            if prefix.len() > key.len() - position {
                return None;
            }
            if &key[position..position + prefix.len()] != prefix {
                return None;
            }
            position += prefix.len();
            if position == key.len() {
                // The key ends inside this node: its value lives in the
                // terminal slot (which may be empty, meaning "absent").
                return Some(node);
            }
            node = node.child(key[position])?;
            position += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Insertion
// ---------------------------------------------------------------------------

/// Inserts into the subtree rooted at `node`, whose prefix begins at
/// `key[position..]`.
///
/// Recursion depth is bounded by the key length: at most one level per byte.
fn insert_at(
    node: &mut Node,
    key: &ArtKey,
    position: usize,
    row_id: RowId,
    kind: IndexKind,
) -> Result<LeafInsert, ArtError> {
    let prefix_length = node.prefix().len();
    let consumed = common_prefix_len(&key[position..], node.prefix());

    if consumed < prefix_length {
        // The key diverges inside this node's compressed prefix, or ends there.
        // Split the node so the shared part becomes a new parent.
        return split_and_insert(node, key, position, consumed, row_id, kind);
    }

    let position = position + prefix_length;
    if position == key.len() {
        // The key ends exactly at this node.
        return insert_terminal(node, row_id, kind);
    }

    let byte = key[position];
    match node.child_mut(byte) {
        Some(child) => insert_at(child, key, position + 1, row_id, kind),
        None => {
            // A fresh single-entry subtree for the remainder of the key. The
            // remaining bytes become that node's prefix, so one edge byte is
            // consumed per level and the key is fully accounted for.
            //
            // `position < key.len()` was checked above, so `position + 1` is a
            // valid slice bound. The empty key cannot reach here: it ends at
            // the node above.
            let mut fresh = Node::node4(key[position + 1..].to_vec());
            *fresh.terminal_entry() = LeafValues::single(row_id);
            node.insert_child_growing(byte, Box::new(fresh))?;
            Ok(LeafInsert::Inserted)
        }
    }
}

/// Splits `node` after `consumed` prefix bytes, then inserts the incoming key.
///
/// Two shapes are possible, and both are handled here:
///
/// ```text
/// incoming key ends inside the prefix  →  the new parent holds the terminal
///                                        value; the old node keeps its edge byte
/// prefixes diverge                     →  the new parent has two children
/// ```
///
/// `consumed < old_prefix.len()` is the caller's guarantee, so the byte at
/// `consumed` always exists: it is the edge byte that separates the two sides.
fn split_and_insert(
    node: &mut Node,
    key: &ArtKey,
    position: usize,
    consumed: usize,
    row_id: RowId,
    kind: IndexKind,
) -> Result<LeafInsert, ArtError> {
    let old_prefix = node.prefix().to_vec();
    let shared = old_prefix[..consumed].to_vec();
    let remainder = &key[position + consumed..];
    let existing_byte = old_prefix[consumed];

    // Take the old node out of the slot and give it the tail of its prefix:
    // the shared part moves up into the new parent.
    let mut old_node = std::mem::replace(node, Node::node4(Vec::new()));
    old_node.set_prefix(old_prefix[consumed + 1..].to_vec());

    let mut parent = Node::node4(shared);
    parent.insert_child(existing_byte, Box::new(old_node))?;

    if remainder.is_empty() {
        // The incoming key ends exactly at the new parent, so its value belongs
        // in the parent's terminal slot; the existing subtree hangs off the
        // parent under `existing_byte`.
        let outcome = insert_terminal(&mut parent, row_id, kind)?;
        *node = parent;
        return Ok(outcome);
    }

    // Both sides continue: the new key gets its own single-entry child, whose
    // prefix is everything after the edge byte that separates the two sides.
    let mut fresh = Node::node4(remainder[1..].to_vec());
    *fresh.terminal_entry() = LeafValues::single(row_id);
    parent.insert_child(remainder[0], Box::new(fresh))?;
    *node = parent;
    Ok(LeafInsert::Inserted)
}

/// Inserts `row_id` into a node's terminal slot.
///
/// A missing terminal slot is created first, so a failed unique insert leaves
/// the existing (non-empty) leaf untouched and never leaves an empty leaf
/// behind. A duplicate is reported before any structural change is visible.
fn insert_terminal(
    node: &mut Node,
    row_id: RowId,
    kind: IndexKind,
) -> Result<LeafInsert, ArtError> {
    let values = node.terminal_entry();
    match kind {
        IndexKind::Unique => values.insert_unique(row_id),
        IndexKind::NonUnique => Ok(values.insert(row_id)),
    }
}

// ---------------------------------------------------------------------------
// Deletion
// ---------------------------------------------------------------------------

/// Removes `row_id` from the subtree rooted at `node`.
///
/// The terminal slot disappears only when its **last** RowId goes away, so a
/// key that other rows still reference is never lost. Repair happens on the way
/// back up:
///
/// 1. a child that became empty (no terminal, no children) is pruned, so no
///    unreachable or stale entry is left behind;
/// 2. a node left with a single child and no terminal key is merged with that
///    child (path compression), keeping the tree compact;
/// 3. otherwise a node whose child count fell to its family threshold shrinks
///    one family.
fn delete_at(node: &mut Node, key: &ArtKey, position: usize, row_id: RowId) -> LeafRemove {
    let prefix_length = node.prefix().len();
    let consumed = common_prefix_len(&key[position..], node.prefix());
    if consumed < prefix_length {
        // The key diverges or ends inside this prefix: not in this subtree.
        return LeafRemove::NotFound;
    }

    let position = position + prefix_length;
    if position == key.len() {
        let last_row_removed = {
            let Some(values) = node.terminal_mut() else {
                return LeafRemove::NotFound;
            };
            let outcome = values.remove(row_id);
            if outcome != LeafRemove::Removed {
                return outcome;
            }
            values.is_empty()
        };
        if last_row_removed {
            // Last RowId for this key: remove the key itself.
            node.take_terminal();
        }
        // The node that owned the removed RowId repairs itself too: if it is
        // left as pure indirection (one child, no terminal key) it is merged
        // with that child here, so a deletion cannot leave a long chain of
        // single-child nodes behind. The root's own merge happens in
        // `ArtIndex::delete`, which is why the root is also repaired there.
        repair_after_removal(node);
        return LeafRemove::Removed;
    }

    let byte = key[position];
    let outcome = match node.child_mut(byte) {
        Some(child) => delete_at(child, key, position + 1, row_id),
        None => LeafRemove::NotFound,
    };
    if outcome == LeafRemove::Removed {
        prune_child_if_empty(node, byte);
        repair_after_removal(node);
    }
    outcome
}

/// Removes the child on `byte` when that child became empty.
///
/// An empty child is one with no terminal value and no children; it would be
/// reachable garbage, so it is detached. The child *slot* always remains valid:
/// this only ever removes a whole subtree.
fn prune_child_if_empty(node: &mut Node, byte: u8) {
    let empty = node.child(byte).is_some_and(Node::is_empty);
    if empty {
        node.remove_child(byte);
    }
}

/// Keeps a node's shape proportional to its contents after a removal.
fn repair_after_removal(node: &mut Node) {
    if node.count() == 1 && node.terminal().is_none() {
        merge_single_child(node);
        return;
    }
    shrink_node_if_needed(node);
}

// ---------------------------------------------------------------------------
// Structural repair: path compression and node shrinkage
// ---------------------------------------------------------------------------

/// Restores the root's canonical shape after a removal.
///
/// Two things can be left behind by a delete:
///
/// 1. a root that holds one child and no terminal key — pure indirection, so it
///    merges with that child (this is how the tree loses height);
/// 2. a root with nothing left at all — reset to a fresh empty root, so prefix
///    metadata can never outlive the keys it described.
fn compact_root(root: &mut Node) {
    merge_single_child(root);
    if root.count() == 0 && root.terminal().is_none() {
        *root = Node::empty_root();
    }
}

/// Merges a node that has exactly one child and no terminal key with that child.
///
/// Such a node holds no key of its own, so it is pure indirection. The merge
/// concatenates prefixes (`parent_prefix + edge_byte + child_prefix`) and moves
/// the child's contents into the parent slot. Merging can newly qualify an
/// ancestor, so [`ArtIndex::delete`] re-applies this at the root.
///
/// The root is never "detached" here; merging into the root slot is exactly how
/// the tree loses height.
pub fn merge_single_child(node: &mut Node) {
    if node.count() != 1 || node.terminal().is_some() {
        return;
    }
    let mut edge = 0u8;
    node.for_each_child(|byte, _| edge = byte);

    let Some(child) = node.remove_child(edge) else {
        return;
    };
    // Unbox the child so it can be moved straight into the parent slot.
    let mut child = *child;
    let mut merged_prefix = node.prefix().to_vec();
    merged_prefix.push(edge);
    merged_prefix.extend_from_slice(child.prefix());
    child.set_prefix(merged_prefix);

    // Also sheds a now-too-large node family for the merged node's contents.
    *node = child.shrink_if_needed();
}

/// Shrinks a node one family when its child count reached that family's
/// threshold. Contents (and therefore every key) are preserved.
fn shrink_node_if_needed(node: &mut Node) {
    let replacement = std::mem::replace(node, Node::empty_root()).shrink_if_needed();
    *node = replacement;
}

// ---------------------------------------------------------------------------
// Enumeration and statistics
// ---------------------------------------------------------------------------

/// Appends every `key → RowId` pair below `node`, in ascending key order.
///
/// `prefix` accumulates the path bytes (ancestor prefixes plus edge bytes);
/// `node.prefix()` is appended here, so the key handed back is the complete key
/// as it was inserted — traversal never re-derives a key from a leaf.
fn collect_entries(node: &Node, prefix: Vec<u8>, out: &mut Vec<IndexEntry>) {
    let mut full = prefix;
    full.extend_from_slice(node.prefix());
    if let Some(values) = node.terminal() {
        for row_id in values.iter() {
            out.push(IndexEntry {
                key: full.clone(),
                row_id: *row_id,
            });
        }
    }
    node.for_each_child(|byte, child| {
        let mut child_prefix = full.clone();
        child_prefix.push(byte);
        collect_entries(child, child_prefix, out);
    });
}

/// Accumulates structural statistics for a subtree.
///
/// `depth` is the node's byte depth, with the root at 1. Counts are derived
/// from the tree itself, never from a cached counter, so they cannot drift.
fn collect_stats(node: &Node, depth: usize, stats: &mut ArtStats) {
    stats.nodes += 1;
    match node.kind() {
        NodeKind::Node4 => stats.node_counts[0] += 1,
        NodeKind::Node16 => stats.node_counts[1] += 1,
        NodeKind::Node48 => stats.node_counts[2] += 1,
        NodeKind::Node256 => stats.node_counts[3] += 1,
    }
    stats.max_depth = stats.max_depth.max(depth);
    // `memory_bytes` already includes this node's prefix and terminal vector.
    stats.bytes += node.memory_bytes();
    if node.terminal().is_some() {
        stats.keys += 1;
    }
    node.for_each_child(|_, child| collect_stats(child, depth + 1, stats));
}
