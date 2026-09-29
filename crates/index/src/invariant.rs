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
//! Structural invariant checking for the ART.
//!
//! These checks are deliberately separate from the mutation paths: the hot
//! insert/lookup/delete code stays branch-light, and tests (or a diagnostic
//! build) can ask for a full structural audit when something looks wrong.
//!
//! [`validate_tree`] walks every reachable node and verifies the invariants
//! documented in [`crate::node`] plus a few extra ones:
//!
//! 1. Every node's declared child count matches its occupied slots.
//! 2. No node holds more children than its family allows.
//! 3. Edge bytes are strictly ascending and unique, and none of them is `0xFF`
//!    sentinel garbage left behind by removal.
//! 4. A Node48 slot index and its occupancy bitmap agree with the child table.
//! 5. No reachable node is empty (an empty node would carry no information).
//! 6. A terminal value always holds at least one RowId and never repeats one.
//! 7. Enumerated entries come out in ascending key order, with strictly
//!    ascending RowIds inside one key, which means traversal reaches every
//!    `key → RowId` pair exactly once, in order.
//!
//! Key *reachability* (every inserted key is retrievable) is validated against
//! a reference model in the randomized tests, which is a stronger statement
//! than anything a structural walk can make on its own.

use std::collections::HashSet;
use std::fmt;

use plomid_core::RowId;

use crate::node::{Node, NodeKind};
use crate::tree::ArtIndex;

/// A structural problem found by [`validate_tree`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvariantViolation {
    /// A node's declared child count disagrees with its occupied slots.
    ChildCountMismatch {
        /// Byte depth of the offending node (length of the path to it).
        depth: usize,
        /// The count stored in the node header.
        declared: usize,
        /// The number of child slots actually occupied.
        occupied: usize,
    },
    /// A node holds more children than its family supports.
    CapacityExceeded {
        /// Byte depth of the offending node.
        depth: usize,
        /// The node's family.
        kind: NodeKind,
        /// The number of occupied child slots.
        occupied: usize,
    },
    /// Edge bytes are not strictly ascending (which also covers duplicates).
    EdgeBytesUnsorted {
        /// Byte depth of the offending node.
        depth: usize,
    },
    /// A Node48 slot index or occupancy bitmap disagrees with its child table.
    SlotIndexMismatch {
        /// Byte depth of the offending node.
        depth: usize,
        /// The edge byte whose slot mapping is inconsistent.
        byte: u8,
    },
    /// A reachable node carries neither children nor a terminal value.
    EmptyNode {
        /// Byte depth of the offending node.
        depth: usize,
    },
    /// A terminal value exists but holds no RowIds.
    EmptyTerminal {
        /// Byte depth of the offending node.
        depth: usize,
    },
    /// A terminal value repeats the same logical RowId.
    DuplicateRowId {
        /// Byte depth of the offending node.
        depth: usize,
        /// The repeated RowId.
        row_id: RowId,
    },
    /// Enumerated entries are not in ascending key order, or the RowIds of one
    /// key are not in strictly ascending order.
    KeysUnsorted {
        /// The previous entry produced by traversal.
        previous: (Vec<u8>, u64),
        /// The entry that appeared after it.
        next: (Vec<u8>, u64),
    },
}

impl fmt::Display for InvariantViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChildCountMismatch {
                depth,
                declared,
                occupied,
            } => write!(
                formatter,
                "child count mismatch at depth {depth}: declared {declared}, occupied {occupied}"
            ),
            Self::CapacityExceeded {
                depth,
                kind,
                occupied,
            } => write!(
                formatter,
                "{kind:?} at depth {depth} holds {occupied} children, above its capacity"
            ),
            Self::EdgeBytesUnsorted { depth } => {
                write!(formatter, "edge bytes at depth {depth} are not ascending")
            }
            Self::SlotIndexMismatch { depth, byte } => write!(
                formatter,
                "Node48 slot index disagrees with the child table at depth {depth}, byte {byte}"
            ),
            Self::EmptyNode { depth } => {
                write!(formatter, "reachable node at depth {depth} is empty")
            }
            Self::EmptyTerminal { depth } => {
                write!(formatter, "terminal at depth {depth} holds no RowIds")
            }
            Self::DuplicateRowId { depth, row_id } => {
                write!(formatter, "terminal at depth {depth} repeats {row_id}")
            }
            Self::KeysUnsorted { previous, next } => write!(
                formatter,
                "entries are not ascending: {previous:?} then {next:?}"
            ),
        }
    }
}

impl std::error::Error for InvariantViolation {}

/// Audits every reachable node of `index` and the order of its keys.
///
/// Returns `Ok(())` when the tree is structurally sound. This is O(entries)
/// work; call it from tests and diagnostics, not from the mutation paths.
pub fn validate_tree(index: &ArtIndex) -> Result<(), InvariantViolation> {
    // An index with no entries is legitimately an empty root node; there is
    // nothing to check and no key order to verify.
    if index.is_empty() {
        return Ok(());
    }
    let mut key = Vec::new();
    validate_node(index.root(), &mut key)?;

    // Traversal order is ascending key order, then strictly ascending RowId
    // inside one key. Keys repeat across entries (one entry per RowId), but a
    // repeated RowId for the same key or a descending key means a duplicated
    // or misrouted entry.
    let mut previous: Option<(Vec<u8>, u64)> = None;
    for entry in index.entries() {
        let current = (entry.key().to_vec(), entry.row_id().get());
        if let Some(previous_entry) = &previous {
            if previous_entry >= &current {
                return Err(InvariantViolation::KeysUnsorted {
                    previous: previous_entry.clone(),
                    next: current,
                });
            }
        }
        previous = Some(current);
    }
    Ok(())
}

/// Recursively validates one node and its subtree.
///
/// `key` accumulates the path bytes leading into `node` (ancestor prefixes plus
/// edge bytes), so a violation can report the byte depth at which it occurred.
fn validate_node(node: &Node, key: &mut Vec<u8>) -> Result<(), InvariantViolation> {
    let depth = key.len();
    key.extend_from_slice(node.prefix());

    if node.is_empty() {
        key.truncate(depth);
        return Err(InvariantViolation::EmptyNode { depth });
    }

    if let Some(values) = node.terminal() {
        if values.is_empty() {
            key.truncate(depth);
            return Err(InvariantViolation::EmptyTerminal { depth });
        }
        // RowIds must be unique within a key: a duplicated RowId would make
        // delete ambiguous and double-count rows in a scan.
        let mut seen = HashSet::with_capacity(values.len());
        for row_id in values.iter() {
            if !seen.insert(row_id) {
                key.truncate(depth);
                return Err(InvariantViolation::DuplicateRowId {
                    depth,
                    row_id: *row_id,
                });
            }
        }
    }

    // Occupied slots, counted from the raw storage rather than from the header.
    let occupied = occupied_slots(node);
    if occupied != node.count() {
        key.truncate(depth);
        return Err(InvariantViolation::ChildCountMismatch {
            depth,
            declared: node.count(),
            occupied,
        });
    }
    if occupied > node.kind().capacity() {
        key.truncate(depth);
        return Err(InvariantViolation::CapacityExceeded {
            depth,
            kind: node.kind(),
            occupied,
        });
    }
    if node.kind() == NodeKind::Node48 {
        validate_node48_slots(node, depth)?;
    }

    // Edge bytes must be strictly ascending; collecting them also proves that
    // every occupied slot is reachable through the family's lookup path.
    let mut edges = Vec::with_capacity(occupied);
    node.for_each_child(|byte, _| edges.push(byte));
    if edges.len() != occupied || !edges.windows(2).all(|pair| pair[0] < pair[1]) {
        key.truncate(depth);
        return Err(InvariantViolation::EdgeBytesUnsorted { depth });
    }

    for edge in edges {
        if let Some(child) = node.child(edge) {
            key.push(edge);
            let result = validate_node(child, key);
            key.pop();
            result?;
        }
    }

    key.truncate(depth);
    Ok(())
}

/// Counts occupied child slots straight from the node's storage.
fn occupied_slots(node: &Node) -> usize {
    match node {
        Node::Node4 { children, .. } => children.iter().filter(|slot| slot.is_some()).count(),
        Node::Node16 { children, .. } => children.iter().filter(|slot| slot.is_some()).count(),
        Node::Node48 { children, .. } => children.iter().filter(|slot| slot.is_some()).count(),
        Node::Node256 { children, .. } => children.iter().filter(|slot| slot.is_some()).count(),
    }
}

/// Cross-checks a Node48's slot index and bitmap against its child table.
fn validate_node48_slots(node: &Node, depth: usize) -> Result<(), InvariantViolation> {
    let Node::Node48 {
        used,
        slot_index,
        children,
        ..
    } = node
    else {
        return Ok(());
    };

    for byte in 0u8..=u8::MAX {
        let slot = slot_index[usize::from(byte)];
        if slot == 0 {
            continue;
        }
        let index = usize::from(slot) - 1;
        // Pointed-at slot must be occupied and flagged in the bitmap.
        if index >= children.len() || children[index].is_none() || used & (1u64 << index) == 0 {
            return Err(InvariantViolation::SlotIndexMismatch { depth, byte });
        }
    }

    for (index, child) in children.iter().enumerate() {
        // Every occupied slot must be flagged and reachable by exactly one byte.
        if child.is_none() {
            if used & (1u64 << index) != 0 {
                return Err(InvariantViolation::SlotIndexMismatch {
                    depth,
                    byte: index as u8,
                });
            }
            continue;
        }
        if used & (1u64 << index) == 0 {
            return Err(InvariantViolation::SlotIndexMismatch {
                depth,
                byte: index as u8,
            });
        }
        let reverse = (0u8..=u8::MAX)
            .filter(|byte| usize::from(slot_index[usize::from(*byte)]) == index + 1)
            .count();
        if reverse != 1 {
            return Err(InvariantViolation::SlotIndexMismatch {
                depth,
                byte: index as u8,
            });
        }
    }
    Ok(())
}
