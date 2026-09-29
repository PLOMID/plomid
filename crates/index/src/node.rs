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
//! ART node families and their child tables.
//!
//! # Representation
//!
//! PLOMID's ART is a *path-compressed radix tree* with explicitly adaptive
//! nodes. Every key byte is accounted for exactly once, either in a node's
//! `prefix` (the compressed run of bytes shared by everything below the node)
//! or in an *edge byte* on a child slot. A key that ends inside a node is
//! recorded in that node's `terminal` slot.
//!
//! ```text
//! node(prefix = "abc", terminal = Some(values for "abc"))
//!   ├── edge 'd' → node(prefix = "", terminal = Some(values for "abcd"))
//!   ├── edge 'e' → node(prefix = "fg", terminal = Some(values for "abcefg"))
//!   └── edge 'h' → node(prefix = "i")
//!                    └── edge 'j' → node(prefix = "", terminal = Some(...))
//! ```
//!
//! Consequences of this representation:
//!
//! * Lookup never compares whole keys and never allocates; it walks bytes and
//!   prefix slices only.
//! * `key ends here` (terminal slot) and `key continues through a child`
//!   (edge byte) are distinct, explicit states, so `a`, `ab`, `abc` and
//!   `abc`, `ab`, `a` are both unambiguous. No terminal-key information is
//!   lost.
//! * There are no separate leaf nodes, so there is no leaf/leaf merging case:
//!   deletion simply clears a terminal slot or a child slot.
//! * Invariant: for every child reached by edge byte `b` at depth `d`, every
//!   key `k` below that child satisfies `k.len() > d` and `k[d] == b`.
//!
//! # Node families
//!
//! Node4, Node16, Node48 and Node256 follow the standard adaptive radix tree.
//! Children are stored as `Option<Box<Node>>` (an 8-byte pointer when
//! occupied), never as trait objects, so the hot lookup/insert paths are
//! statically dispatched and the compiler can inline them. Nodes are plain
//! safe Rust; the crate forbids `unsafe_code` and this module needs none.

use crate::constants::{
    ART_NODE16_CAP, ART_NODE256_CAP, ART_NODE48_CAP, ART_NODE4_CAP, NODE_CAPACITY, NODE_SHRINK,
};
use crate::error::ArtError;
use crate::leaf::LeafValues;

/// ART node families, matching the standard adaptive radix tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    /// Up to 4 children; the smallest node, ideal for sparse fanout.
    Node4,
    /// Up to 16 children; keeps a sorted key array plus packed child pointers.
    Node16,
    /// Up to 48 children; a 256-entry slot index into a 48-slot child table.
    Node48,
    /// Up to 256 children; a direct byte-to-child table.
    Node256,
}

impl NodeKind {
    /// Position of this family in the constant tables, in growth order.
    ///
    /// Keeps [`NODE_CAPACITY`] and [`NODE_SHRINK`] as the single source of
    /// truth for the transition numbers.
    const fn index(self) -> usize {
        match self {
            Self::Node4 => 0,
            Self::Node16 => 1,
            Self::Node48 => 2,
            Self::Node256 => 3,
        }
    }

    /// Maximum number of children this node family can hold.
    #[must_use]
    pub const fn capacity(self) -> usize {
        NODE_CAPACITY[self.index()]
    }

    /// Child count at or below which a node of this family shrinks one level.
    ///
    /// Node4 is the floor and never shrinks. Thresholds sit below the growth
    /// points of the smaller families so growth and shrink cannot oscillate on
    /// a single insert/delete pair.
    #[must_use]
    pub const fn shrink_threshold(self) -> usize {
        match self {
            // Inert table entry: Node4 never shrinks.
            Self::Node4 => 0,
            Self::Node16 | Self::Node48 | Self::Node256 => NODE_SHRINK[self.index()],
        }
    }

    /// The next larger family, or `None` for Node256.
    #[must_use]
    pub const fn grown(self) -> Option<Self> {
        match self {
            Self::Node4 => Some(Self::Node16),
            Self::Node16 => Some(Self::Node48),
            Self::Node48 => Some(Self::Node256),
            Self::Node256 => None,
        }
    }

    /// The next smaller family, or `None` for Node4.
    #[must_use]
    pub const fn shrunk(self) -> Option<Self> {
        match self {
            Self::Node4 => None,
            Self::Node16 => Some(Self::Node4),
            Self::Node48 => Some(Self::Node16),
            Self::Node256 => Some(Self::Node48),
        }
    }
}

/// One ART node.
///
/// `keys` holds the edge byte of each child slot. For Node4/Node16 the arrays
/// are kept sorted ascending by edge byte, which makes [`Node::children_slots`]
/// deterministic and lets a branch search exit early. Node48 keeps a 256-entry
/// slot index (`slot_index[byte] == slot + 1`, zero meaning "no child", so the
/// table needs no separate initialization pass) plus a 48-bit `used` bitmap
/// that makes free-slot selection a single `trailing_zeros`. Node256 indexes the
/// child table by the byte value directly.
///
/// The Node256 child table is boxed so that the enum stays small on the stack;
/// each node is heap-allocated anyway.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Node {
    /// Small node for sparse fanout.
    Node4 {
        count: u8,
        prefix: Vec<u8>,
        terminal: Option<LeafValues>,
        keys: [u8; ART_NODE4_CAP],
        children: [Option<Box<Node>>; ART_NODE4_CAP],
    },
    /// Medium node with a sorted key array.
    Node16 {
        count: u8,
        prefix: Vec<u8>,
        terminal: Option<LeafValues>,
        keys: [u8; ART_NODE16_CAP],
        children: [Option<Box<Node>>; ART_NODE16_CAP],
    },
    /// Dense node using a slot index.
    Node48 {
        count: u8,
        prefix: Vec<u8>,
        terminal: Option<LeafValues>,
        used: u64,
        slot_index: [u8; ART_NODE256_CAP],
        children: [Option<Box<Node>>; ART_NODE48_CAP],
    },
    /// Full-width node indexed by byte value.
    Node256 {
        count: u16,
        prefix: Vec<u8>,
        terminal: Option<LeafValues>,
        children: Box<[Option<Box<Node>>; ART_NODE256_CAP]>,
    },
}

impl Node {
    /// Creates an empty Node4 with the given prefix.
    #[must_use]
    pub fn node4(prefix: Vec<u8>) -> Self {
        Self::Node4 {
            count: 0,
            prefix,
            terminal: None,
            keys: [0u8; ART_NODE4_CAP],
            children: [None, None, None, None],
        }
    }

    /// Creates an empty root node (empty prefix, no children, no terminal).
    #[must_use]
    pub fn empty_root() -> Self {
        Self::node4(Vec::new())
    }

    /// Returns this node's family.
    #[must_use]
    pub fn kind(&self) -> NodeKind {
        match self {
            Self::Node4 { .. } => NodeKind::Node4,
            Self::Node16 { .. } => NodeKind::Node16,
            Self::Node48 { .. } => NodeKind::Node48,
            Self::Node256 { .. } => NodeKind::Node256,
        }
    }

    /// Number of child slots currently occupied.
    #[must_use]
    pub fn count(&self) -> usize {
        match self {
            Self::Node4 { count, .. } | Self::Node16 { count, .. } => usize::from(*count),
            Self::Node48 { count, .. } => usize::from(*count),
            Self::Node256 { count, .. } => usize::from(*count),
        }
    }

    /// Returns true when adding one more child requires a growth step.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.count() >= self.kind().capacity()
    }

    /// Returns true when this node holds neither a terminal value nor children.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count() == 0 && self.terminal().is_none()
    }

    /// The compressed prefix stored at this node.
    #[must_use]
    pub fn prefix(&self) -> &[u8] {
        match self {
            Self::Node4 { prefix, .. }
            | Self::Node16 { prefix, .. }
            | Self::Node48 { prefix, .. }
            | Self::Node256 { prefix, .. } => prefix.as_slice(),
        }
    }

    /// Replaces the compressed prefix in place.
    pub fn set_prefix(&mut self, new_prefix: Vec<u8>) {
        match self {
            Self::Node4 { prefix, .. }
            | Self::Node16 { prefix, .. }
            | Self::Node48 { prefix, .. }
            | Self::Node256 { prefix, .. } => *prefix = new_prefix,
        }
    }

    /// Truncates the compressed prefix to `len` bytes.
    ///
    /// Used when splitting a node: the shared part moves up into the new
    /// parent node and the remainder stays here.
    pub fn truncate_prefix(&mut self, len: usize) {
        match self {
            Self::Node4 { prefix, .. }
            | Self::Node16 { prefix, .. }
            | Self::Node48 { prefix, .. }
            | Self::Node256 { prefix, .. } => prefix.truncate(len),
        }
    }

    /// Values for the key that ends exactly at this node, if that key exists.
    #[must_use]
    pub fn terminal(&self) -> Option<&LeafValues> {
        match self {
            Self::Node4 { terminal, .. }
            | Self::Node16 { terminal, .. }
            | Self::Node48 { terminal, .. }
            | Self::Node256 { terminal, .. } => terminal.as_ref(),
        }
    }

    /// Mutable access to the terminal slot, if the key exists.
    pub fn terminal_mut(&mut self) -> Option<&mut LeafValues> {
        match self {
            Self::Node4 { terminal, .. }
            | Self::Node16 { terminal, .. }
            | Self::Node48 { terminal, .. }
            | Self::Node256 { terminal, .. } => terminal.as_mut(),
        }
    }

    /// Returns the terminal slot, creating it when the key is new.
    pub fn terminal_entry(&mut self) -> &mut LeafValues {
        let slot = match self {
            Self::Node4 { terminal, .. }
            | Self::Node16 { terminal, .. }
            | Self::Node48 { terminal, .. }
            | Self::Node256 { terminal, .. } => terminal,
        };
        slot.get_or_insert_with(LeafValues::default)
    }

    /// Removes and returns the terminal values, marking the key absent.
    pub fn take_terminal(&mut self) -> Option<LeafValues> {
        match self {
            Self::Node4 { terminal, .. }
            | Self::Node16 { terminal, .. }
            | Self::Node48 { terminal, .. }
            | Self::Node256 { terminal, .. } => terminal.take(),
        }
    }

    /// Inserts a child under edge byte `byte`.
    ///
    /// The caller must guarantee there is room (see
    /// [`Node::insert_child_growing`]); a full node or an already occupied edge
    /// byte is reported as [`ArtError::InvalidOperation`] rather than silently
    /// overwriting an existing subtree. Node4/Node16 keep their key array
    /// sorted by edge byte.
    pub fn insert_child(&mut self, byte: u8, child: Box<Node>) -> Result<(), ArtError> {
        if self.is_full() {
            return Err(ArtError::InvalidOperation);
        }
        match self {
            Self::Node4 {
                count,
                keys,
                children,
                ..
            } => insert_sorted(count, keys, children, byte, child),
            Self::Node16 {
                count,
                keys,
                children,
                ..
            } => insert_sorted(count, keys, children, byte, child),
            Self::Node48 {
                count,
                used,
                slot_index,
                children,
                ..
            } => {
                if slot_index[usize::from(byte)] != 0 {
                    return Err(ArtError::InvalidOperation);
                }
                // The low 48 bits of `used` are the occupancy bitmap; the
                // first zero bit is the free slot. A free slot must exist
                // because the caller checked `is_full`.
                let slot = (!*used).trailing_zeros() as usize;
                if slot >= ART_NODE48_CAP {
                    return Err(ArtError::InvalidOperation);
                }
                children[slot] = Some(child);
                slot_index[usize::from(byte)] = (slot as u8) + 1;
                *used |= 1u64 << slot;
                *count += 1;
                Ok(())
            }
            Self::Node256 {
                count, children, ..
            } => {
                let slot = &mut children[usize::from(byte)];
                if slot.is_some() {
                    return Err(ArtError::InvalidOperation);
                }
                *slot = Some(child);
                *count += 1;
                Ok(())
            }
        }
    }

    /// Grows this node to the next family when needed, then inserts a child.
    ///
    /// Growth happens before the insertion, so every previously inserted key
    /// stays reachable from the (possibly reallocated) node.
    pub fn insert_child_growing(&mut self, byte: u8, child: Box<Node>) -> Result<(), ArtError> {
        if self.is_full() {
            let replacement = std::mem::replace(self, Node::empty_root()).grow();
            *self = replacement;
        }
        self.insert_child(byte, child)
    }

    /// Removes and returns the child on edge byte `byte`.
    ///
    /// Node4/Node16 close the gap in their sorted key array. The child count
    /// and family stay valid; shrinking is a separate, explicit step
    /// ([`Node::shrink_if_needed`]).
    pub fn remove_child(&mut self, byte: u8) -> Option<Box<Node>> {
        match self {
            Self::Node4 {
                count,
                keys,
                children,
                ..
            } => remove_sorted(count, keys, children, byte),
            Self::Node16 {
                count,
                keys,
                children,
                ..
            } => remove_sorted(count, keys, children, byte),
            Self::Node48 {
                count,
                used,
                slot_index,
                children,
                ..
            } => {
                let slot = slot_index[usize::from(byte)];
                if slot == 0 {
                    return None;
                }
                slot_index[usize::from(byte)] = 0;
                let index = usize::from(slot) - 1;
                *used &= !(1u64 << index);
                *count -= 1;
                children[index].take()
            }
            Self::Node256 {
                count, children, ..
            } => {
                let removed = children[usize::from(byte)].take();
                if removed.is_some() {
                    *count -= 1;
                }
                removed
            }
        }
    }

    /// Returns the child on edge byte `byte`, without allocating.
    #[must_use]
    pub fn child(&self, byte: u8) -> Option<&Node> {
        match self {
            Self::Node4 {
                count,
                keys,
                children,
                ..
            } => sorted_child(*count, keys, children, byte),
            Self::Node16 {
                count,
                keys,
                children,
                ..
            } => sorted_child(*count, keys, children, byte),
            Self::Node48 {
                slot_index,
                children,
                ..
            } => {
                let slot = slot_index[usize::from(byte)];
                if slot == 0 {
                    None
                } else {
                    children[usize::from(slot) - 1].as_deref()
                }
            }
            Self::Node256 { children, .. } => children[usize::from(byte)].as_deref(),
        }
    }

    /// Returns the child on edge byte `byte` for mutation, without allocating.
    pub fn child_mut(&mut self, byte: u8) -> Option<&mut Node> {
        match self {
            Self::Node4 {
                count,
                keys,
                children,
                ..
            } => sorted_child_mut(*count, keys, children, byte),
            Self::Node16 {
                count,
                keys,
                children,
                ..
            } => sorted_child_mut(*count, keys, children, byte),
            Self::Node48 {
                slot_index,
                children,
                ..
            } => {
                let slot = slot_index[usize::from(byte)];
                if slot == 0 {
                    None
                } else {
                    children[usize::from(slot) - 1].as_deref_mut()
                }
            }
            Self::Node256 { children, .. } => children[usize::from(byte)].as_deref_mut(),
        }
    }

    /// Calls `visit` for every occupied child slot in ascending edge-byte order.
    ///
    /// A callback is used instead of an iterator so traversal stays
    /// allocation-free and statically dispatched: the four node families would
    /// otherwise have to return four different iterator types. Ascending
    /// edge-byte order is guaranteed for all families, which keeps key
    /// enumeration deterministic.
    pub fn for_each_child<F>(&self, mut visit: F)
    where
        F: FnMut(u8, &Node),
    {
        match self {
            Self::Node4 {
                count,
                keys,
                children,
                ..
            } => {
                visit_sorted(*count, keys, children, &mut visit);
            }
            Self::Node16 {
                count,
                keys,
                children,
                ..
            } => {
                visit_sorted(*count, keys, children, &mut visit);
            }
            Self::Node48 {
                slot_index,
                children,
                ..
            } => {
                for byte in 0..=u8::MAX {
                    let slot = slot_index[usize::from(byte)];
                    if slot == 0 {
                        continue;
                    }
                    if let Some(child) = children[usize::from(slot) - 1].as_deref() {
                        visit(byte, child);
                    }
                }
            }
            Self::Node256 { children, .. } => {
                for (byte, slot) in children.iter().enumerate() {
                    if let Some(child) = slot.as_deref() {
                        visit(byte as u8, child);
                    }
                }
            }
        }
    }

    /// Grows this node into the next larger family.
    ///
    /// All keys, RowIds, prefixes, child references and terminal values are
    /// preserved: contents move through a flat representation and are rebuilt
    /// in the target family. Node256 is already the widest family and is
    /// returned unchanged.
    #[must_use]
    pub fn grow(self) -> Node {
        match self.kind().grown() {
            Some(target) => {
                let (prefix, terminal, children) = self.into_parts();
                Self::from_parts(target, prefix, terminal, children)
            }
            None => self,
        }
    }

    /// Shrinks this node one family when its child count fell to the family's
    /// shrink threshold.
    ///
    /// Node4 is the floor and never shrinks. Contents are preserved exactly as
    /// in [`Node::grow`], so a shrink can never lose or alter an entry.
    #[must_use]
    pub fn shrink_if_needed(self) -> Node {
        let current = self.kind();
        let Some(target) = current.shrunk() else {
            return self;
        };
        if self.count() > current.shrink_threshold() {
            return self;
        }
        let (prefix, terminal, children) = self.into_parts();
        Self::from_parts(target, prefix, terminal, children)
    }

    /// Builds a node of `kind` from flat contents.
    ///
    /// `entries` are sorted ascending by edge byte and must fit `kind`'s
    /// capacity. An overflowing entry is impossible by construction (a node
    /// only grows after the previous family was full); it is dropped
    /// defensively rather than panicked on, and the invariant checker will
    /// report the resulting count mismatch in tests.
    fn from_parts(
        kind: NodeKind,
        prefix: Vec<u8>,
        terminal: Option<LeafValues>,
        entries: Vec<(u8, Box<Node>)>,
    ) -> Node {
        match kind {
            NodeKind::Node4 => Self::from_sorted_parts(kind, prefix, terminal, entries),
            NodeKind::Node16 => Self::from_sorted_parts(kind, prefix, terminal, entries),
            NodeKind::Node48 => {
                let mut used = 0u64;
                let mut slot_index = [0u8; ART_NODE256_CAP];
                let mut children: [Option<Box<Node>>; ART_NODE48_CAP] =
                    std::array::from_fn(|_| None);
                let mut count = 0u8;
                for (byte, child) in entries {
                    let slot = usize::from(count);
                    if slot >= ART_NODE48_CAP {
                        break;
                    }
                    children[slot] = Some(child);
                    slot_index[usize::from(byte)] = (slot as u8) + 1;
                    used |= 1u64 << slot;
                    count += 1;
                }
                Self::Node48 {
                    count,
                    prefix,
                    terminal,
                    used,
                    slot_index,
                    children,
                }
            }
            NodeKind::Node256 => {
                let mut children: Box<[Option<Box<Node>>; ART_NODE256_CAP]> =
                    Box::new(std::array::from_fn(|_| None));
                let mut count = 0u16;
                for (byte, child) in entries {
                    children[usize::from(byte)] = Some(child);
                    count += 1;
                }
                Self::Node256 {
                    count,
                    prefix,
                    terminal,
                    children,
                }
            }
        }
    }

    /// Builds a sorted-table node (Node4 or Node16) from flat contents.
    fn from_sorted_parts(
        kind: NodeKind,
        prefix: Vec<u8>,
        terminal: Option<LeafValues>,
        entries: Vec<(u8, Box<Node>)>,
    ) -> Node {
        match kind {
            NodeKind::Node16 => {
                let mut keys = [0u8; ART_NODE16_CAP];
                let mut children: [Option<Box<Node>>; ART_NODE16_CAP] =
                    std::array::from_fn(|_| None);
                let mut count = 0u8;
                for (byte, child) in entries {
                    let slot = usize::from(count);
                    if slot >= ART_NODE16_CAP {
                        break;
                    }
                    keys[slot] = byte;
                    children[slot] = Some(child);
                    count += 1;
                }
                Self::Node16 {
                    count,
                    prefix,
                    terminal,
                    keys,
                    children,
                }
            }
            _ => {
                let mut keys = [0u8; ART_NODE4_CAP];
                let mut children: [Option<Box<Node>>; ART_NODE4_CAP] =
                    std::array::from_fn(|_| None);
                let mut count = 0u8;
                for (byte, child) in entries {
                    let slot = usize::from(count);
                    if slot >= ART_NODE4_CAP {
                        break;
                    }
                    keys[slot] = byte;
                    children[slot] = Some(child);
                    count += 1;
                }
                Self::Node4 {
                    count,
                    prefix,
                    terminal,
                    keys,
                    children,
                }
            }
        }
    }

    /// Destructures this node into `(prefix, terminal, sorted children)`.
    ///
    /// Entries are produced in ascending edge-byte order for every family, so
    /// the rebuilt node keeps a sorted table and no entry can be duplicated or
    /// dropped during a transition.
    fn into_parts(self) -> NodeParts {
        match self {
            Self::Node4 {
                count,
                prefix,
                terminal,
                keys,
                mut children,
            } => (
                prefix,
                terminal,
                take_sorted(usize::from(count), &keys, &mut children),
            ),
            Self::Node16 {
                count,
                prefix,
                terminal,
                keys,
                mut children,
            } => (
                prefix,
                terminal,
                take_sorted(usize::from(count), &keys, &mut children),
            ),
            Self::Node48 {
                prefix,
                terminal,
                slot_index,
                mut children,
                ..
            } => {
                let mut entries = Vec::new();
                for byte in 0..=u8::MAX {
                    let slot = slot_index[usize::from(byte)];
                    if slot == 0 {
                        continue;
                    }
                    if let Some(child) = children[usize::from(slot) - 1].take() {
                        entries.push((byte, child));
                    }
                }
                (prefix, terminal, entries)
            }
            Self::Node256 {
                prefix,
                terminal,
                mut children,
                ..
            } => {
                let mut entries = Vec::new();
                for (byte, slot) in children.iter_mut().enumerate() {
                    if let Some(child) = slot.take() {
                        entries.push((byte as u8, child));
                    }
                }
                (prefix, terminal, entries)
            }
        }
    }

    /// Approximate memory footprint of this node, its prefix, and its leaf.
    ///
    /// The estimate uses `size_of::<Node>()` (the largest family variant) for
    /// the node itself plus the heap allocation of a Node256 child table. It is
    /// intended for reporting memory per entry, not for allocator accounting.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        // The prefix is a `Vec<u8>`, but only its logical length is knowable
        // here; report that rather than guessing at allocator overhead.
        let mut bytes = std::mem::size_of::<Node>() + self.prefix().len();
        if matches!(self, Self::Node256 { .. }) {
            bytes += std::mem::size_of::<[Option<Box<Node>>; ART_NODE256_CAP]>();
        }
        if let Some(values) = self.terminal() {
            bytes += values.memory_bytes();
        }
        bytes
    }
}

/// The flat form used to move a node between families.
///
/// `(prefix, terminal, children)` where `children` is sorted ascending by edge
/// byte. Growth and shrink go through this representation so the transition
/// logic exists once instead of eight times.
type NodeParts = (Vec<u8>, Option<LeafValues>, Vec<(u8, Box<Node>)>);

/// Linear scan over a sorted Node4/Node16 child table.
///
/// The tables hold at most 16 slots, so a scan is cheaper than any index
/// structure; the arrays are kept sorted so the scan can stop early.
fn sorted_child<'tree, const N: usize>(
    count: u8,
    keys: &[u8; N],
    children: &'tree [Option<Box<Node>>; N],
    byte: u8,
) -> Option<&'tree Node> {
    let limit = usize::from(count).min(N);
    keys[..limit]
        .iter()
        .position(|key| *key == byte)
        .and_then(|slot| children[slot].as_deref())
}

/// Mutable variant of [`sorted_child`].
fn sorted_child_mut<'tree, const N: usize>(
    count: u8,
    keys: &[u8; N],
    children: &'tree mut [Option<Box<Node>>; N],
    byte: u8,
) -> Option<&'tree mut Node> {
    let limit = usize::from(count).min(N);
    keys[..limit]
        .iter()
        .position(|key| *key == byte)
        .and_then(|slot| children[slot].as_deref_mut())
}

/// Visits a sorted Node4/Node16 child table in ascending edge-byte order.
fn visit_sorted<const N: usize, F>(
    count: u8,
    keys: &[u8; N],
    children: &[Option<Box<Node>>; N],
    visit: &mut F,
) where
    F: FnMut(u8, &Node),
{
    let limit = usize::from(count).min(N);
    for slot in 0..limit {
        if let Some(child) = children[slot].as_deref() {
            visit(keys[slot], child);
        }
    }
}

/// Moves a sorted child table into `(edge byte, child)` pairs.
///
/// Slots beyond `limit` are never read, and a `None` slot (which would mean a
/// count/slot mismatch) is skipped rather than trusted.
fn take_sorted<const N: usize>(
    limit: usize,
    keys: &[u8; N],
    children: &mut [Option<Box<Node>>; N],
) -> Vec<(u8, Box<Node>)> {
    let limit = limit.min(N);
    let mut entries = Vec::with_capacity(limit);
    for slot in 0..limit {
        if let Some(child) = children[slot].take() {
            entries.push((keys[slot], child));
        }
    }
    entries
}

/// Inserts a child into a sorted Node4/Node16 table.
///
/// Rejects an already occupied edge byte so an existing subtree can never be
/// silently replaced.
fn insert_sorted<const N: usize>(
    count: &mut u8,
    keys: &mut [u8; N],
    children: &mut [Option<Box<Node>>; N],
    byte: u8,
    child: Box<Node>,
) -> Result<(), ArtError> {
    let limit = usize::from(*count).min(N);
    let position = keys[..limit]
        .iter()
        .position(|key| *key >= byte)
        .unwrap_or(limit);

    if position < limit && keys[position] == byte {
        return Err(ArtError::InvalidOperation);
    }
    if limit >= N {
        return Err(ArtError::InvalidOperation);
    }

    // Shift the tail one slot to the right, keeping the key array sorted.
    for slot in (position..limit).rev() {
        keys[slot + 1] = keys[slot];
        let moved = children[slot].take();
        children[slot + 1] = moved;
    }
    keys[position] = byte;
    children[position] = Some(child);
    *count += 1;
    Ok(())
}

/// Removes a child from a sorted Node4/Node16 table, closing the gap.
fn remove_sorted<const N: usize>(
    count: &mut u8,
    keys: &mut [u8; N],
    children: &mut [Option<Box<Node>>; N],
    byte: u8,
) -> Option<Box<Node>> {
    let limit = usize::from(*count).min(N);
    let position = keys[..limit].iter().position(|key| *key == byte)?;
    let removed = children[position].take();

    // Shift the tail one slot to the left and clear the vacated final slot so
    // no stale child reference stays reachable.
    for slot in position..limit - 1 {
        keys[slot] = keys[slot + 1];
        let moved = children[slot + 1].take();
        children[slot] = moved;
    }
    keys[limit - 1] = 0;
    children[limit - 1] = None;
    *count -= 1;
    removed
}

#[cfg(test)]
mod tests {
    use super::{Node, NodeKind};
    use crate::constants::{ART_NODE16_CAP, ART_NODE256_CAP, ART_NODE48_CAP, ART_NODE4_CAP};
    use crate::error::ArtError;
    use crate::leaf::LeafValues;
    use plomid_core::RowId;

    /// A stand-in child node; its contents are irrelevant to these tests.
    fn child() -> Box<Node> {
        Box::new(Node::node4(Vec::new()))
    }

    /// Edge bytes present in a node, in traversal order.
    fn edges(node: &Node) -> Vec<u8> {
        let mut found = Vec::new();
        node.for_each_child(|byte, _| found.push(byte));
        found
    }

    #[test]
    fn node4_keeps_edge_bytes_sorted() {
        let mut node = Node::node4(b"pfx".to_vec());
        for byte in [9u8, 3, 7] {
            node.insert_child(byte, child()).expect("space available");
        }
        assert_eq!(node.kind(), NodeKind::Node4);
        assert_eq!(node.count(), 3);
        assert_eq!(node.prefix(), b"pfx");
        // Ascending order regardless of insertion order.
        assert_eq!(edges(&node), vec![3, 7, 9]);
        assert!(node.child(7).is_some());
        assert!(node.child(8).is_none());
    }

    #[test]
    fn occupied_edge_byte_and_full_node_are_rejected() {
        let mut node = Node::node4(Vec::new());
        node.insert_child(5, child()).expect("first insert");
        assert_eq!(
            node.insert_child(5, child()),
            Err(ArtError::InvalidOperation)
        );

        for byte in 6u8..=7 {
            node.insert_child(byte, child()).expect("within capacity");
        }
        // A fourth child fills Node4 exactly.
        node.insert_child(8, child()).expect("capacity is four");
        assert!(node.is_full());
        assert_eq!(
            node.insert_child(9, child()),
            Err(ArtError::InvalidOperation)
        );
        assert_eq!(node.count(), ART_NODE4_CAP);
    }

    #[test]
    fn growth_preserves_every_entry() {
        let mut node = Node::node4(Vec::new());
        // Inserting the fifth child forces Node4 → Node16.
        for byte in 0u8..ART_NODE4_CAP as u8 {
            node.insert_child_growing(byte, child()).expect("insert");
        }
        node.insert_child_growing(4, child())
            .expect("grow to Node16");
        assert_eq!(node.kind(), NodeKind::Node16);
        assert_eq!(node.count(), 5);
        for byte in 0u8..5 {
            assert!(node.child(byte).is_some(), "byte {byte} survived growth");
        }

        // Fill Node16 and force Node16 → Node48.
        for byte in 5u8..ART_NODE16_CAP as u8 {
            node.insert_child_growing(byte, child()).expect("insert");
        }
        node.insert_child_growing(16, child())
            .expect("grow to Node48");
        assert_eq!(node.kind(), NodeKind::Node48);
        assert_eq!(node.count(), 17);
        let expected: Vec<u8> = (0u8..=16).collect();
        assert_eq!(edges(&node), expected);

        // Fill Node48 and force Node48 → Node256.
        for byte in 17u8..ART_NODE48_CAP as u8 {
            node.insert_child_growing(byte, child()).expect("insert");
        }
        node.insert_child_growing(48, child())
            .expect("grow to Node256");
        assert_eq!(node.kind(), NodeKind::Node256);
        assert_eq!(node.count(), 49);
        let expected: Vec<u8> = (0u8..=48).collect();
        assert_eq!(edges(&node), expected);
        for byte in 0u8..=48 {
            assert!(node.child(byte).is_some());
        }
        assert!(node.child(49).is_none());
    }

    #[test]
    fn shrink_preserves_remaining_entries() {
        let mut node = Node::node4(Vec::new());
        for byte in 0u8..=ART_NODE48_CAP as u8 {
            node.insert_child_growing(byte, child()).expect("insert");
        }
        assert_eq!(node.kind(), NodeKind::Node256);

        // 49 children → one removal brings the count to the Node256 shrink
        // threshold (48), which is where Node256 → Node48 happens.
        assert!(node.remove_child(48).is_some());
        let mut node = node.shrink_if_needed();
        assert_eq!(node.kind(), NodeKind::Node48);
        assert_eq!(node.count(), ART_NODE48_CAP);
        assert_eq!(
            edges(&node),
            (0u8..ART_NODE48_CAP as u8).collect::<Vec<_>>()
        );

        // Node48 → Node16 at the 16-child threshold.
        for byte in 16u8..ART_NODE48_CAP as u8 {
            assert!(node.remove_child(byte).is_some());
        }
        let mut node = node.shrink_if_needed();
        assert_eq!(node.kind(), NodeKind::Node16);
        assert_eq!(node.count(), ART_NODE16_CAP);

        // Node16 → Node4 at the 4-child threshold.
        for byte in ART_NODE4_CAP as u8..ART_NODE16_CAP as u8 {
            assert!(node.remove_child(byte).is_some());
        }
        let node = node.shrink_if_needed();
        assert_eq!(node.kind(), NodeKind::Node4);
        assert_eq!(node.count(), ART_NODE4_CAP);
        for byte in 0u8..ART_NODE4_CAP as u8 {
            assert!(node.child(byte).is_some(), "byte {byte} survived shrink");
        }
    }

    #[test]
    fn shrink_is_deferred_above_the_threshold() {
        let mut node = Node::node4(Vec::new());
        for byte in 0u8..=ART_NODE48_CAP as u8 {
            node.insert_child_growing(byte, child()).expect("insert");
        }
        // 49 children is above the Node256 shrink threshold, so the family stays.
        let node = node.shrink_if_needed();
        assert_eq!(node.kind(), NodeKind::Node256);
    }

    #[test]
    fn removal_leaves_no_stale_reference() {
        let mut node = Node::node4(Vec::new());
        for byte in [4u8, 5, 6] {
            node.insert_child(byte, child()).expect("insert");
        }
        assert!(node.remove_child(5).is_some());
        assert_eq!(node.count(), 2);
        assert_eq!(edges(&node), vec![4, 6]);
        assert!(node.child(5).is_none());
        // Removing a byte that is not present changes nothing.
        assert!(node.remove_child(5).is_none());
        assert_eq!(node.count(), 2);
    }

    #[test]
    fn node48_reuses_freed_slots() {
        let mut node = Node::node4(Vec::new());
        for byte in 0u8..=ART_NODE48_CAP as u8 {
            node.insert_child_growing(byte, child()).expect("insert");
        }
        assert_eq!(node.kind(), NodeKind::Node256, "grew past Node48");

        // 49 children → one removal returns to the Node48 threshold, where the
        // node family drops back to Node48.
        assert!(node.remove_child(48).is_some());
        let mut node = node.shrink_if_needed();
        assert_eq!(node.kind(), NodeKind::Node48);
        assert_eq!(node.count(), ART_NODE48_CAP);

        // Free a slot, then reuse it: count stays right and lookups still work.
        assert!(node.remove_child(7).is_some());
        assert_eq!(node.count(), ART_NODE48_CAP - 1);
        node.insert_child_growing(200, child()).expect("freed slot");
        assert_eq!(node.count(), ART_NODE48_CAP);
        assert!(node.child(200).is_some());
        assert!(node.child(7).is_none());
    }

    #[test]
    fn prefix_and_terminal_slots_are_independent_of_children() {
        let mut node = Node::node4(Vec::new());
        node.insert_child(1, child()).expect("insert");
        assert!(node.terminal().is_none());

        node.terminal_entry().insert(RowId::new(11));
        assert_eq!(node.terminal().map(LeafValues::len), Some(1));

        node.set_prefix(b"ab".to_vec());
        node.truncate_prefix(1);
        assert_eq!(node.prefix(), b"a");
        assert_eq!(node.count(), 1);
        assert!(node.take_terminal().is_some());
        assert!(node.terminal().is_none());
        // The child is untouched by terminal and prefix edits.
        assert!(node.child(1).is_some());
        assert!(!node.is_empty());
    }

    #[test]
    fn family_capacities_and_thresholds_are_monotonic() {
        assert_eq!(NodeKind::Node4.capacity(), ART_NODE4_CAP);
        assert_eq!(NodeKind::Node256.capacity(), ART_NODE256_CAP);
        assert!(NodeKind::Node16.shrink_threshold() < NodeKind::Node16.capacity());
        assert!(NodeKind::Node48.shrink_threshold() < NodeKind::Node48.capacity());
        assert!(NodeKind::Node256.shrink_threshold() < NodeKind::Node256.capacity());
        assert_eq!(NodeKind::Node4.shrunk(), None);
        assert_eq!(NodeKind::Node256.grown(), None);
        assert_eq!(NodeKind::Node4.grown(), Some(NodeKind::Node16));
    }
}
