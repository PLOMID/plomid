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
//! Immutable physical identifiers and binary format foundations.
//!
//! This module establishes the PLOMID physical storage contract:
//! - Named constants for page/block/extent/segment/pack sizes.
//! - Strongly typed page header, block metadata, pack header/footer.
//! - Deterministic little-endian binary serialization.
//! - Corruption detection hooks for integrity validation.
//!
//! All structures are designed for deterministic encoding; tests cover
//! round-trip, malformed input, bounds, and magic/version validation.

use plomid_core::{BlockId, DatabaseId, ErrorKind, GenerationId, Lsn, PackId, PageId};

// ---------------------------------------------------------------------------
// Central physical constants
// ---------------------------------------------------------------------------

pub use plomid_core::{
    BLOCKS_PER_EXTENT, BLOCK_FORMAT_VERSION, BLOCK_MAGIC, BLOCK_SIZE,
    DEFAULT_STORAGE_SEGMENT_SIZE_BYTES, EXTENT_SIZE, PACK_FOOTER_MAGIC, PACK_FORMAT_VERSION,
    PACK_MAGIC, PACK_TARGET_SIZE, PAGES_PER_BLOCK, PAGES_PER_EXTENT, PAGE_FORMAT_VERSION,
    PAGE_MAGIC, PAGE_SIZE,
};

// ---------------------------------------------------------------------------
// Page types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum PageType {
    Free = 0,
    Root = 1,
    Internal = 2,
    Leaf = 3,
    Transaction = 4,
    Wal = 5,
    Reserved = 255,
}

impl PageType {
    pub fn from_byte(value: u8) -> Result<Self, plomid_core::PlomidError> {
        match value {
            0 => Ok(Self::Free),
            1 => Ok(Self::Root),
            2 => Ok(Self::Internal),
            3 => Ok(Self::Leaf),
            4 => Ok(Self::Transaction),
            5 => Ok(Self::Wal),
            255 => Ok(Self::Reserved),
            _ => Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "unknown page type byte",
            )),
        }
    }
    pub fn to_byte(self) -> u8 {
        self as u8
    }
}
// ---------------------------------------------------------------------------
// Page header
// ---------------------------------------------------------------------------

/// Page header stored in the first bytes of every base page.
///
/// Layout (little-endian):
/// magic[4] | format_version[u32] | page_type[u8] | reserved[3] |
/// page_id[u64] | generation_id[u64] | lsn[u64] |
/// payload_length[u32] | flags[u32] | checksum[u32]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageHeader {
    pub magic: [u8; 4],
    pub format_version: u32,
    pub page_type: PageType,
    pub page_id: PageId,
    pub generation_id: GenerationId,
    pub lsn: Lsn,
    pub payload_length: u32,
    pub flags: u32,
    pub checksum: u32,
}

impl PageHeader {
    pub const SIZE: usize = plomid_core::PAGE_HEADER_SIZE;
    /// Byte offset of the checksum field within the encoded header.
    pub const CHECKSUM_OFFSET: usize = plomid_core::PAGE_HEADER_CHECKSUM_OFFSET;

    pub fn new(page_id: PageId, page_type: PageType) -> Self {
        Self {
            magic: PAGE_MAGIC,
            format_version: PAGE_FORMAT_VERSION,
            page_type,
            page_id,
            generation_id: GenerationId::new(0),
            lsn: Lsn::new(0),
            payload_length: 0,
            flags: 0,
            checksum: 0,
        }
    }

    pub fn checksummed_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.magic);
        b[4..8].copy_from_slice(&self.format_version.to_le_bytes());
        b[8] = self.page_type.to_byte();
        b[9..12].copy_from_slice(&[0, 0, 0]);
        b[12..20].copy_from_slice(&self.page_id.to_le_bytes());
        b[20..28].copy_from_slice(&self.generation_id.to_le_bytes());
        b[28..36].copy_from_slice(&self.lsn.to_le_bytes());
        b[36..40].copy_from_slice(&self.payload_length.to_le_bytes());
        b[40..44].copy_from_slice(&self.flags.to_le_bytes());
        b[44..48].copy_from_slice(&self.checksum.to_le_bytes());
        b
    }

    pub fn compute_checksum(&self) -> u32 {
        let mut b = self.checksummed_bytes();
        b[44..48].fill(0);
        crate::compute_checksum(&b)
    }

    pub fn with_checksum(self) -> Self {
        let cs = self.compute_checksum();
        Self {
            checksum: cs,
            ..self
        }
    }

    pub fn encode(&self) -> [u8; Self::SIZE] {
        self.checksummed_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, plomid_core::PlomidError> {
        if bytes.len() < Self::SIZE {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "page header truncated",
            ));
        }
        if bytes[0..4] != PAGE_MAGIC {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "invalid page magic",
            ));
        }
        let fv = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if fv != PAGE_FORMAT_VERSION {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                format!("unsupported page format version {fv}"),
            ));
        }
        let pt = PageType::from_byte(bytes[8])?;
        if bytes[9..12] != [0, 0, 0] {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "page header non-zero reserved bytes",
            ));
        }
        let pid = PageId::from_le_bytes(bytes[12..20].try_into().unwrap());
        let gid = GenerationId::from_le_bytes(bytes[20..28].try_into().unwrap());
        let lsn = Lsn::from_le_bytes(bytes[28..36].try_into().unwrap());
        let pl = u32::from_le_bytes(bytes[36..40].try_into().unwrap());
        let fl = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        let cs = u32::from_le_bytes(bytes[44..48].try_into().unwrap());

        let mut cbs = bytes[..Self::SIZE].to_vec();
        cbs[44..48].fill(0);
        crate::verify_checksum(&cbs, cs)?;

        Ok(Self {
            magic: PAGE_MAGIC,
            format_version: fv,
            page_type: pt,
            page_id: pid,
            generation_id: gid,
            lsn,
            payload_length: pl,
            flags: fl,
            checksum: cs,
        })
    }
}
// ---------------------------------------------------------------------------
// Block metadata
// ---------------------------------------------------------------------------

/// Block metadata: 256 KiB = 16 pages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockMetadata {
    pub block_id: BlockId,
    pub generation_id: GenerationId,
    pub lsn: Lsn,
    pub pages: [PageId; PAGES_PER_BLOCK as usize],
}

impl BlockMetadata {
    pub const SIZE: usize = plomid_core::BLOCK_METADATA_SIZE;

    pub fn new(block_id: BlockId) -> Self {
        Self {
            block_id,
            generation_id: GenerationId::new(0),
            lsn: Lsn::new(0),
            pages: [PageId::new(0); PAGES_PER_BLOCK as usize],
        }
    }

    pub fn validate(&self) -> Result<(), plomid_core::PlomidError> {
        if self.pages.iter().any(|p| p.is_zero()) {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "block metadata contains zero page ID",
            ));
        }
        Ok(())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::SIZE);
        v.extend_from_slice(&self.block_id.to_le_bytes());
        v.extend_from_slice(&self.generation_id.to_le_bytes());
        v.extend_from_slice(&self.lsn.to_le_bytes());
        for p in &self.pages {
            v.extend_from_slice(&p.to_le_bytes());
        }
        v
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, plomid_core::PlomidError> {
        if bytes.len() != Self::SIZE {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                format!(
                    "block metadata size mismatch: expected {}, got {}",
                    Self::SIZE,
                    bytes.len()
                ),
            ));
        }
        let mut cur = 0;
        let bid = BlockId::from_le_bytes(bytes[cur..cur + 8].try_into().unwrap());
        cur += 8;
        let gid = GenerationId::from_le_bytes(bytes[cur..cur + 8].try_into().unwrap());
        cur += 8;
        let lsn = Lsn::from_le_bytes(bytes[cur..cur + 8].try_into().unwrap());
        cur += 8;
        let mut pages = [PageId::new(0); PAGES_PER_BLOCK as usize];
        for p in pages.iter_mut() {
            *p = PageId::from_le_bytes(bytes[cur..cur + 8].try_into().unwrap());
            cur += 8;
        }
        Ok(Self {
            block_id: bid,
            generation_id: gid,
            lsn,
            pages,
        })
    }
}
// ---------------------------------------------------------------------------
// Pack header
// ---------------------------------------------------------------------------

/// Pack header at the beginning of a .dat file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackHeader {
    pub magic: [u8; 4],
    pub format_version: u32,
    pub pack_id: PackId,
    pub database_id: DatabaseId,
    pub generation_id: GenerationId,
    pub capacity_blocks: u64,
    pub allocated_blocks: u64,
    pub flags: u32,
    pub checksum: u32,
}

impl PackHeader {
    pub const SIZE: usize = plomid_core::PACK_HEADER_SIZE;

    pub fn new(pack_id: PackId, database_id: DatabaseId, capacity_blocks: u64) -> Self {
        Self {
            magic: PACK_MAGIC,
            format_version: PACK_FORMAT_VERSION,
            pack_id,
            database_id,
            generation_id: GenerationId::new(0),
            capacity_blocks,
            allocated_blocks: 0,
            flags: 0,
            checksum: 0,
        }
    }

    pub fn checksummed_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.magic);
        b[4..8].copy_from_slice(&self.format_version.to_le_bytes());
        b[8..16].copy_from_slice(&self.pack_id.to_le_bytes());
        b[16..24].copy_from_slice(&self.database_id.to_le_bytes());
        b[24..32].copy_from_slice(&self.generation_id.to_le_bytes());
        b[32..40].copy_from_slice(&self.capacity_blocks.to_le_bytes());
        b[40..48].copy_from_slice(&self.allocated_blocks.to_le_bytes());
        b[48..52].copy_from_slice(&self.flags.to_le_bytes());
        b[52..56].copy_from_slice(&self.checksum.to_le_bytes());
        b
    }

    pub fn compute_checksum(&self) -> u32 {
        let mut b = self.checksummed_bytes();
        b[52..56].fill(0);
        crate::compute_checksum(&b)
    }

    pub fn with_checksum(self) -> Self {
        let cs = self.compute_checksum();
        Self {
            checksum: cs,
            ..self
        }
    }

    pub fn encode(&self) -> [u8; Self::SIZE] {
        self.checksummed_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, plomid_core::PlomidError> {
        if bytes.len() < Self::SIZE {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "pack header truncated",
            ));
        }
        if bytes[0..4] != PACK_MAGIC {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "invalid pack magic",
            ));
        }
        let fv = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if fv != PACK_FORMAT_VERSION {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                format!("unsupported pack format version {fv}"),
            ));
        }
        let pid = PackId::from_le_bytes(bytes[8..16].try_into().unwrap());
        let did = DatabaseId::from_le_bytes(bytes[16..24].try_into().unwrap());
        let gid = GenerationId::from_le_bytes(bytes[24..32].try_into().unwrap());
        let cap = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        let alloc = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
        let fl = u32::from_le_bytes(bytes[48..52].try_into().unwrap());
        let cs = u32::from_le_bytes(bytes[52..56].try_into().unwrap());

        let mut cbs = bytes[..Self::SIZE].to_vec();
        cbs[52..56].fill(0);
        crate::verify_checksum(&cbs, cs)?;

        Ok(Self {
            magic: PACK_MAGIC,
            format_version: fv,
            pack_id: pid,
            database_id: did,
            generation_id: gid,
            capacity_blocks: cap,
            allocated_blocks: alloc,
            flags: fl,
            checksum: cs,
        })
    }
}
// ---------------------------------------------------------------------------
// Pack footer
// ---------------------------------------------------------------------------

/// Pack footer at the end of a .dat file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFooter {
    pub magic: [u8; 4],
    pub format_version: u32,
    pub pack_id: PackId,
    pub database_id: DatabaseId,
    pub generation_id: GenerationId,
    pub header_offset: u64,
    pub total_bytes: u64,
    pub checksum: u32,
}

impl PackFooter {
    pub const SIZE: usize = plomid_core::PACK_FOOTER_SIZE;

    pub fn new(pack_id: PackId, database_id: DatabaseId) -> Self {
        Self {
            magic: PACK_FOOTER_MAGIC,
            format_version: PACK_FORMAT_VERSION,
            pack_id,
            database_id,
            generation_id: GenerationId::new(0),
            header_offset: 0,
            total_bytes: 0,
            checksum: 0,
        }
    }

    pub fn checksummed_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.magic);
        b[4..8].copy_from_slice(&self.format_version.to_le_bytes());
        b[8..16].copy_from_slice(&self.pack_id.to_le_bytes());
        b[16..24].copy_from_slice(&self.database_id.to_le_bytes());
        b[24..32].copy_from_slice(&self.generation_id.to_le_bytes());
        b[32..40].copy_from_slice(&self.header_offset.to_le_bytes());
        b[40..48].copy_from_slice(&self.total_bytes.to_le_bytes());
        b[48..52].copy_from_slice(&self.checksum.to_le_bytes());
        b
    }

    pub fn compute_checksum(&self) -> u32 {
        let mut b = self.checksummed_bytes();
        b[48..52].fill(0);
        crate::compute_checksum(&b)
    }

    pub fn with_checksum(self) -> Self {
        let cs = self.compute_checksum();
        Self {
            checksum: cs,
            ..self
        }
    }

    pub fn encode(&self) -> [u8; Self::SIZE] {
        self.checksummed_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, plomid_core::PlomidError> {
        if bytes.len() < Self::SIZE {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "pack footer truncated",
            ));
        }
        if bytes[0..4] != PACK_FOOTER_MAGIC {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "invalid pack footer magic",
            ));
        }
        let fv = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if fv != PACK_FORMAT_VERSION {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                format!("unsupported pack footer format version {fv}"),
            ));
        }
        let pid = PackId::from_le_bytes(bytes[8..16].try_into().unwrap());
        let did = DatabaseId::from_le_bytes(bytes[16..24].try_into().unwrap());
        let gid = GenerationId::from_le_bytes(bytes[24..32].try_into().unwrap());
        let hoff = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        let total = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
        let cs = u32::from_le_bytes(bytes[48..52].try_into().unwrap());

        let mut cbs = bytes[..Self::SIZE].to_vec();
        cbs[48..52].fill(0);
        crate::verify_checksum(&cbs, cs)?;

        Ok(Self {
            magic: PACK_FOOTER_MAGIC,
            format_version: fv,
            pack_id: pid,
            database_id: did,
            generation_id: gid,
            header_offset: hoff,
            total_bytes: total,
            checksum: cs,
        })
    }
}

// ---------------------------------------------------------------------------
// Physical location
// ---------------------------------------------------------------------------

/// Explicit physical location of a page inside storage.
///
/// Logical IDs (RowID, ObjectID, PageID, ...) never encode physical position.
/// Use this type to resolve `Device -> Pack -> Block -> Page -> offset`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PhysicalLocation {
    pub device_index: u32,
    pub pack_id: PackId,
    pub block_id: BlockId,
    pub page_id: PageId,
    pub byte_offset: u64,
}

impl PhysicalLocation {
    pub fn new(
        device_index: u32,
        pack_id: PackId,
        block_id: BlockId,
        page_id: PageId,
        byte_offset: u64,
    ) -> Self {
        Self {
            device_index,
            pack_id,
            block_id,
            page_id,
            byte_offset,
        }
    }

    /// Validates alignment invariants against the central constants.
    pub fn validate(&self) -> Result<(), plomid_core::PlomidError> {
        if self.byte_offset % PAGE_SIZE != 0 {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                format!(
                    "physical location byte offset {} is not page aligned",
                    self.byte_offset
                ),
            ));
        }
        if self.page_id.is_zero() {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "physical location has zero page ID",
            ));
        }
        if self.pack_id.is_zero() {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "physical location has zero pack ID",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Block directory entry
// ---------------------------------------------------------------------------

/// One entry of the pack block directory.
///
/// The directory sits between BLOCKS and PACK FOOTER and maps each logical
/// [`BlockId`] to its byte offset inside the pack file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct BlockDirectoryEntry {
    pub block_id: BlockId,
    pub byte_offset: u64,
    pub generation_id: GenerationId,
    pub lsn: Lsn,
}

impl BlockDirectoryEntry {
    pub const SIZE: usize = plomid_core::BLOCK_DIRECTORY_ENTRY_SIZE;

    pub fn new(block_id: BlockId, byte_offset: u64) -> Self {
        Self {
            block_id,
            byte_offset,
            generation_id: GenerationId::new(0),
            lsn: Lsn::new(0),
        }
    }

    pub fn validate(&self) -> Result<(), plomid_core::PlomidError> {
        if self.block_id.is_zero() {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "block directory entry has zero block ID",
            ));
        }
        if self.byte_offset % BLOCK_SIZE != 0 {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                format!(
                    "block directory entry offset {} is not block aligned",
                    self.byte_offset
                ),
            ));
        }
        Ok(())
    }

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..8].copy_from_slice(&self.block_id.to_le_bytes());
        b[8..16].copy_from_slice(&self.byte_offset.to_le_bytes());
        b[16..24].copy_from_slice(&self.generation_id.to_le_bytes());
        b[24..32].copy_from_slice(&self.lsn.to_le_bytes());
        b
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, plomid_core::PlomidError> {
        if bytes.len() < Self::SIZE {
            return Err(plomid_core::PlomidError::new(
                ErrorKind::Corruption,
                "block directory entry truncated",
            ));
        }
        let block_id = BlockId::from_le_bytes(bytes[0..8].try_into().unwrap());
        let byte_offset = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let generation_id = GenerationId::from_le_bytes(bytes[16..24].try_into().unwrap());
        let lsn = Lsn::from_le_bytes(bytes[24..32].try_into().unwrap());
        let entry = Self {
            block_id,
            byte_offset,
            generation_id,
            lsn,
        };
        entry.validate()?;
        Ok(entry)
    }
}

// ---------------------------------------------------------------------------
// Corruption detection hooks (architecture interfaces only)
// ---------------------------------------------------------------------------

/// Outcome of a corruption check performed without recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrityCheck {
    Ok,
    Corrupt { reason: &'static str },
}

/// Verifies a page header buffer; returns [`IntegrityCheck::Corrupt`] on any
/// malformed or failed validation without attempting repair.
pub fn check_page_header(bytes: &[u8]) -> IntegrityCheck {
    match PageHeader::decode(bytes) {
        Ok(_) => IntegrityCheck::Ok,
        Err(_) => IntegrityCheck::Corrupt {
            reason: "page header failed validation",
        },
    }
}

/// Verifies a pack header buffer; returns [`IntegrityCheck::Corrupt`] on any
/// malformed or failed validation without attempting repair.
pub fn check_pack_header(bytes: &[u8]) -> IntegrityCheck {
    match PackHeader::decode(bytes) {
        Ok(_) => IntegrityCheck::Ok,
        Err(_) => IntegrityCheck::Corrupt {
            reason: "pack header failed validation",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        check_pack_header, check_page_header, BlockDirectoryEntry, BlockId, BlockMetadata,
        DatabaseId, GenerationId, IntegrityCheck, Lsn, PackFooter, PackHeader, PackId, PageHeader,
        PageId, PageType, PhysicalLocation, BLOCK_FORMAT_VERSION, BLOCK_SIZE,
        DEFAULT_STORAGE_SEGMENT_SIZE_BYTES, EXTENT_SIZE, PACK_FORMAT_VERSION, PACK_TARGET_SIZE,
        PAGES_PER_BLOCK, PAGE_FORMAT_VERSION, PAGE_SIZE,
    };

    #[test]
    fn constants_match_architecture() {
        assert_eq!(PAGE_SIZE, 16 * 1024);
        assert_eq!(BLOCK_SIZE, 256 * 1024);
        assert_eq!(EXTENT_SIZE, 64 * 1024 * 1024);
        assert_eq!(DEFAULT_STORAGE_SEGMENT_SIZE_BYTES, 8 * 1024 * 1024);
        assert_eq!(PACK_TARGET_SIZE, 16 * 1024 * 1024 * 1024);
        assert_eq!(PAGES_PER_BLOCK, 16);
        assert_eq!(BLOCK_SIZE, PAGES_PER_BLOCK * PAGE_SIZE);
        assert_eq!(PAGE_FORMAT_VERSION, 1);
        assert_eq!(BLOCK_FORMAT_VERSION, 1);
        assert_eq!(PACK_FORMAT_VERSION, 1);
    }

    #[test]
    fn page_header_round_trip() {
        let header = PageHeader::new(PageId::new(7), PageType::Leaf).with_checksum();
        let bytes = header.encode();
        let decoded = PageHeader::decode(&bytes).expect("decode must succeed");
        assert_eq!(decoded, header);
    }

    #[test]
    fn page_header_rejects_truncated_bad_magic_and_bad_version() {
        let header = PageHeader::new(PageId::new(7), PageType::Leaf).with_checksum();
        let bytes = header.encode();
        assert!(PageHeader::decode(&bytes[..10]).is_err());
        let mut bad_magic = bytes;
        bad_magic[0] = b'X';
        assert!(PageHeader::decode(&bad_magic).is_err());
        let mut bad_version = bytes;
        bad_version[4] = 0xFF;
        assert!(PageHeader::decode(&bad_version).is_err());
    }

    #[test]
    fn page_header_rejects_corrupt_checksum_and_bad_type() {
        let header = PageHeader::new(PageId::new(7), PageType::Leaf).with_checksum();
        let mut bytes = header.encode();
        bytes[20] ^= 0xFF;
        assert!(PageHeader::decode(&bytes).is_err());
        let mut bad_type = header.encode();
        bad_type[8] = 99;
        assert!(PageHeader::decode(&bad_type).is_err());
    }

    #[test]
    fn block_metadata_round_trip_and_alignment() {
        let mut meta = BlockMetadata::new(BlockId::new(3));
        for (i, page) in meta.pages.iter_mut().enumerate() {
            *page = PageId::new((i as u64) + 1);
        }
        meta.validate().expect("valid block");
        let bytes = meta.encode();
        let decoded = BlockMetadata::decode(&bytes).expect("ok");
        assert_eq!(decoded, meta);
        assert_eq!(BLOCK_SIZE % PAGE_SIZE, 0);
        assert_eq!(EXTENT_SIZE % BLOCK_SIZE, 0);
    }

    #[test]
    fn block_metadata_rejects_zero_page_and_bad_size() {
        let meta = BlockMetadata::new(BlockId::new(3));
        assert!(meta.validate().is_err());
        assert!(BlockMetadata::decode(&[0u8; 8]).is_err());
    }

    #[test]
    fn pack_header_round_trip() {
        let h = PackHeader::new(PackId::new(1), DatabaseId::new(2), 1024).with_checksum();
        let b = h.encode();
        assert_eq!(PackHeader::decode(&b).expect("ok"), h);
    }

    #[test]
    fn pack_header_rejects_bad_inputs() {
        let h = PackHeader::new(PackId::new(1), DatabaseId::new(2), 1024).with_checksum();
        let b = h.encode();
        assert!(PackHeader::decode(&b[..10]).is_err());
        let mut m = b;
        m[0] = b'X';
        assert!(PackHeader::decode(&m).is_err());
        let mut v = b;
        v[4] = 0xFF;
        assert!(PackHeader::decode(&v).is_err());
    }

    #[test]
    fn pack_footer_round_trip_and_rejects_corruption() {
        let f = PackFooter::new(PackId::new(1), DatabaseId::new(2)).with_checksum();
        let b = f.encode();
        assert_eq!(PackFooter::decode(&b).expect("ok"), f);
        let mut bad = b;
        bad[10] ^= 0xFF;
        assert!(PackFooter::decode(&bad).is_err());
        assert!(PackFooter::decode(&b[..10]).is_err());
    }

    #[test]
    fn block_directory_entry_round_trip_and_alignment() {
        let e = BlockDirectoryEntry {
            block_id: BlockId::new(9),
            byte_offset: BLOCK_SIZE,
            generation_id: GenerationId::new(1),
            lsn: Lsn::new(2),
        };
        e.validate().expect("valid");
        let b = e.encode();
        assert_eq!(BlockDirectoryEntry::decode(&b).expect("ok"), e);
        assert!(BlockDirectoryEntry::new(BlockId::new(1), 1)
            .validate()
            .is_err());
        assert!(BlockDirectoryEntry::decode(&[0u8; 4]).is_err());
    }

    #[test]
    fn physical_location_validation_and_alignment() {
        let ok = PhysicalLocation::new(
            0,
            PackId::new(1),
            BlockId::new(2),
            PageId::new(3),
            PAGE_SIZE,
        );
        ok.validate().expect("valid");
        let bad = PhysicalLocation::new(0, PackId::new(1), BlockId::new(2), PageId::new(3), 1);
        assert!(bad.validate().is_err());
        let zp = PhysicalLocation::new(
            0,
            PackId::new(1),
            BlockId::new(2),
            PageId::new(0),
            PAGE_SIZE,
        );
        assert!(zp.validate().is_err());
    }

    #[test]
    fn corruption_hooks_detect_bad_buffers() {
        let h = PageHeader::new(PageId::new(7), PageType::Leaf).with_checksum();
        assert_eq!(check_page_header(&h.encode()), IntegrityCheck::Ok);
        assert!(matches!(
            check_page_header(&[0u8; 4]),
            IntegrityCheck::Corrupt { .. }
        ));
        let p = PackHeader::new(PackId::new(1), DatabaseId::new(2), 8).with_checksum();
        assert_eq!(check_pack_header(&p.encode()), IntegrityCheck::Ok);
        assert!(matches!(
            check_pack_header(&[0u8; 4]),
            IntegrityCheck::Corrupt { .. }
        ));
    }
}
