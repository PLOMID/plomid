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
//! Persistent B+Tree node format for the SQL index.
//!
//! Nodes are persisted inside 16 KiB [`Page`](plomid_storage::Page) frames
//! managed by [`BufferPool`](plomid_storage::BufferPool). Each node carries its
//! own magic + type byte so the structure is self-describing on disk; the
//! surrounding page supplies the canonical CRC32C header/trailer integrity
//! boundaries — no index-specific checksum is added, per the shared-crc32c
//! requirement.
//!
//! ```text
//! // Leaf node header (24 bytes)
//! magic[4] = PLBT | type[u8] = 1 | reserved[1] | count[u16]
//! prev_page[u64] | next_page[u64]
//! // entries follow, one per (key, RowId-set):
//!   row_count[u16] | key_len[u16] | row_ids[u64 × row_count] | key[key_len]
//!
//! // Internal node header (24 bytes, shared fixed fields)
//! magic[4] = PLBT | type[u8] = 2 | reserved[1] | count[u16]
//! first_child[u64] | reserved2[u64]
//! // separators follow, one per (key, child):
//!   key_len[u16] | child[u64] | key[key_len]
//! ```
//!
//! `count` is the number of entries (leaves) or separators (internals). Child
//! routing follows the same lower-bound convention as the storage-layer
//! B+Tree: `separators[i].key` is the minimum key in `child`, and
//! `first_child` holds keys smaller than the first separator key. All
//! integers are little-endian.

use plomid_core::{ErrorKind, PageId, PlomidError, Result, RowId};

use super::constants::{
    INTERNAL, LEAF, LEN_U16, NODE_HEADER_SIZE, NODE_MAGIC, NO_PAGE, PAGE_PAYLOAD_SIZE, ROW_ID_SIZE,
};

/// One distinct key and its ordered RowId set in a leaf node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LeafEntry {
    pub key: Vec<u8>,
    pub row_ids: Vec<RowId>,
}

/// One `(separator_key, child_page)` pair in an internal node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InternalEntry {
    pub key: Vec<u8>,
    pub child: PageId,
}

/// Decoded node ready for in-memory manipulation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    Leaf {
        prev: Option<PageId>,
        next: Option<PageId>,
        entries: Vec<LeafEntry>,
    },
    Internal {
        first: PageId,
        separators: Vec<InternalEntry>,
    },
}

impl Node {
    /// Returns true for a leaf node.
    pub(crate) fn is_leaf(&self) -> bool {
        matches!(self, Node::Leaf { .. })
    }

    /// Returns the number of routing entries.
    pub(crate) fn len(&self) -> usize {
        match self {
            Node::Leaf { entries, .. } => entries.len(),
            Node::Internal { separators, .. } => separators.len(),
        }
    }
}

/// Serializes a node into its canonical header + entries form.
pub(crate) fn encode_node(node: &Node) -> Result<Vec<u8>> {
    let mut output = vec![0u8; NODE_HEADER_SIZE];
    output[..4].copy_from_slice(&NODE_MAGIC);
    match node {
        Node::Leaf {
            prev,
            next,
            entries,
        } => {
            output[4] = LEAF;
            output[6..8].copy_from_slice(&(entries.len() as u16).to_le_bytes());
            output[8..16].copy_from_slice(&prev.map_or(NO_PAGE, PageId::get).to_le_bytes());
            output[16..24].copy_from_slice(&next.map_or(NO_PAGE, PageId::get).to_le_bytes());
            for entry in entries {
                put_u16(&mut output, entry.row_ids.len());
                put_u16(&mut output, entry.key.len());
                for row_id in &entry.row_ids {
                    output.extend_from_slice(&row_id.to_le_bytes());
                }
                output.extend_from_slice(&entry.key);
            }
        }
        Node::Internal { first, separators } => {
            output[4] = INTERNAL;
            output[6..8].copy_from_slice(&(separators.len() as u16).to_le_bytes());
            output[8..16].copy_from_slice(&first.get().to_le_bytes());
            for sep in separators {
                put_u16(&mut output, sep.key.len());
                output.extend_from_slice(&sep.child.to_le_bytes());
                output.extend_from_slice(&sep.key);
            }
        }
    }
    Ok(output)
}

/// Decodes a node from a page payload, validating magic/type/lengths.
pub(crate) fn decode_node(data: &[u8]) -> Result<Node> {
    if data.len() < NODE_HEADER_SIZE || data[..4] != NODE_MAGIC {
        return Err(corruption("invalid B+Tree node header"));
    }
    let kind = data[4];
    let count = usize::from(u16::from_le_bytes([data[6], data[7]]));
    let mut cursor = NODE_HEADER_SIZE;
    match kind {
        LEAF => {
            let prev = u64::from_le_bytes(data[8..16].try_into().unwrap_or([0u8; 8]));
            let next = u64::from_le_bytes(data[16..24].try_into().unwrap_or([0u8; 8]));
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let row_count = read_u16(data, &mut cursor)?;
                let key_len = read_u16(data, &mut cursor)?;
                let mut row_ids = Vec::with_capacity(row_count);
                for _ in 0..row_count {
                    let raw = take_exact(data, &mut cursor, ROW_ID_SIZE)?;
                    row_ids.push(RowId::new(u64::from_le_bytes(
                        raw.try_into()
                            .map_err(|_| corruption("invalid RowId field"))?,
                    )));
                }
                let key = take_exact(data, &mut cursor, key_len)?.to_vec();
                entries.push(LeafEntry { key, row_ids });
            }
            Ok(Node::Leaf {
                prev: page_maybe(prev),
                next: page_maybe(next),
                entries,
            })
        }
        INTERNAL => {
            let first = PageId::new(u64::from_le_bytes(
                data[8..16]
                    .try_into()
                    .map_err(|_| corruption("invalid internal first child"))?,
            ));
            let mut separators = Vec::with_capacity(count);
            for _ in 0..count {
                let key_len = read_u16(data, &mut cursor)?;
                let child = PageId::new(u64::from_le_bytes(
                    take_exact(data, &mut cursor, 8)?
                        .try_into()
                        .map_err(|_| corruption("invalid child page"))?,
                ));
                let key = take_exact(data, &mut cursor, key_len)?.to_vec();
                separators.push(InternalEntry { key, child });
            }
            Ok(Node::Internal { first, separators })
        }
        _ => Err(corruption("unknown B+Tree node kind")),
    }
}

/// Writes an encoded node into `handle`'s payload, zero-filling the remainder.
pub(crate) fn write_node(handle: &mut plomid_storage::PageHandle, node: &Node) -> Result<()> {
    let encoded = encode_node(node)?;
    if encoded.len() > PAGE_PAYLOAD_SIZE {
        return Err(PlomidError::new(
            ErrorKind::InvalidArgument,
            "B+Tree node exceeds page payload",
        ));
    }
    let data = handle.data_mut();
    data.fill(0);
    data[..encoded.len()].copy_from_slice(&encoded);
    Ok(())
}

/// Encoded byte length of a node (header + entries).
///
/// Computed arithmetically from entry sizes; encoding to measure would copy
/// every key twice per insert.
pub(crate) fn encoded_len(node: &Node) -> Result<usize> {
    Ok(NODE_HEADER_SIZE
        + match node {
            Node::Leaf { entries, .. } => entries
                .iter()
                .map(|entry| leaf_entry_size(entry.key.len(), entry.row_ids.len()))
                .sum::<usize>(),
            Node::Internal { separators, .. } => separators
                .iter()
                .map(|entry| internal_entry_size(entry.key.len()))
                .sum::<usize>(),
        })
}

/// Encoded byte size of one leaf entry.
pub(crate) fn leaf_entry_size(key_len: usize, row_count: usize) -> usize {
    LEN_U16 + LEN_U16 + row_count * ROW_ID_SIZE + key_len
}

/// Encoded byte size of one internal entry.
pub(crate) fn internal_entry_size(key_len: usize) -> usize {
    LEN_U16 + ROW_ID_SIZE + key_len
}

fn page_maybe(value: u64) -> Option<PageId> {
    if value == NO_PAGE {
        None
    } else {
        Some(PageId::new(value))
    }
}

fn put_u16(buffer: &mut Vec<u8>, value: usize) {
    let value = u16::try_from(value).unwrap_or(u16::MAX);
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn read_u16(data: &[u8], cursor: &mut usize) -> Result<usize> {
    let raw = take_exact(data, cursor, LEN_U16)?;
    Ok(usize::from(u16::from_le_bytes(
        raw.try_into()
            .map_err(|_| corruption("invalid u16 length field"))?,
    )))
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

fn corruption(message: &str) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(v: u64) -> RowId {
        RowId::new(v)
    }

    fn leaf_entry(key: &[u8], rows: &[RowId]) -> LeafEntry {
        LeafEntry {
            key: key.to_vec(),
            row_ids: rows.to_vec(),
        }
    }

    #[test]
    fn leaf_round_trips_with_links() {
        let node = Node::Leaf {
            prev: Some(PageId::new(3)),
            next: Some(PageId::new(7)),
            entries: vec![
                leaf_entry(b"alpha", &[rid(1)]),
                leaf_entry(b"beta", &[rid(2), rid(5)]),
            ],
        };
        let encoded = encode_node(&node).unwrap();
        assert_eq!(encoded[..4], NODE_MAGIC);
        assert_eq!(encoded[4], LEAF);
        let decoded = decode_node(&encoded).unwrap();
        assert_eq!(decoded, node);
    }

    #[test]
    fn internal_round_trips_with_separators() {
        let node = Node::Internal {
            first: PageId::new(2),
            separators: vec![
                InternalEntry {
                    key: b"m".to_vec(),
                    child: PageId::new(10),
                },
                InternalEntry {
                    key: b"z".to_vec(),
                    child: PageId::new(20),
                },
            ],
        };
        let encoded = encode_node(&node).unwrap();
        let decoded = decode_node(&encoded).unwrap();
        assert_eq!(decoded, node);
    }

    #[test]
    fn no_page_link_decodes_to_none() {
        let node = Node::Leaf {
            prev: None,
            next: None,
            entries: vec![leaf_entry(b"k", &[rid(1)])],
        };
        let encoded = encode_node(&node).unwrap();
        let decoded = decode_node(&encoded).unwrap();
        match decoded {
            Node::Leaf { prev, next, .. } => {
                assert!(prev.is_none());
                assert!(next.is_none());
            }
            _ => panic!("expected leaf"),
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut data = vec![0u8; NODE_HEADER_SIZE];
        assert!(decode_node(&data).is_err());
        data[..4].copy_from_slice(b"XXXX");
        assert!(decode_node(&data).is_err());
    }

    #[test]
    fn rejects_truncated_node() {
        assert!(decode_node(&[0u8; 8]).is_err());
    }

    #[test]
    fn rejects_unknown_node_kind() {
        let mut data = vec![0u8; NODE_HEADER_SIZE];
        data[..4].copy_from_slice(&NODE_MAGIC);
        data[4] = 99;
        assert!(decode_node(&data).is_err());
    }
}
