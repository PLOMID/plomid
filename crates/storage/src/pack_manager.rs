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
//! PackManager: pack file with header, directory, blocks, footer.

use crate::physical::{BLOCK_SIZE as BLOCK_SIZE_U64, PAGE_SIZE as PAGE_SIZE_U64};
use crate::platform::FileExt;
use crate::{compute_checksum, verify_checksum, Page, PAGES_PER_BLOCK, PAGE_SIZE};
use plomid_core::{BlockId, ErrorKind, PlomidError, Result};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::Path;

/// Pack-manager constants are defined once in `plomid_core::constants` and
/// re-exported here; HDR_*/FTR_*/BLOCK_LEN keep their module-local names.
pub use plomid_core::{
    BLOCK_SIZE_USIZE as BLOCK_LEN, PACK_DIRECTORY_ENTRY_LEN as DIRECTORY_ENTRY_LEN,
    PACK_FOOTER_LEN, PACK_FOOTER_MAGIC, PACK_MANAGER_DIRECTORY_FREE as DIRECTORY_FREE,
    PACK_MANAGER_FTR_CHECKSUM as FTR_CHECKSUM, PACK_MANAGER_FTR_COUNT as FTR_COUNT,
    PACK_MANAGER_HDR_CHECKSUM as HDR_CHECKSUM, PACK_MANAGER_HDR_COUNT as HDR_COUNT,
    PACK_MANAGER_MAGIC, PACK_MANAGER_VERSION, PACK_TARGET_BLOCKS,
};

/// Header region length (one page) and footer region length (one page) are
/// re-exported from `plomid_core` above as [`PACK_FOOTER_LEN`] with the pack
/// header length aliased to [`PAGE_SIZE`]-derived [`PACK_HEADER_LEN`].
pub use plomid_core::PACK_HEADER_LEN;

/// File-backed pack: header + directory + block slots + footer.
pub struct PackManager {
    file: File,
    max_blocks: u64,
    next_block_id: u64,
    directory: Vec<u64>,
    id_to_slot: HashMap<BlockId, u64>,
    /// Cached physical file length; updated on extending writes. Avoids a
    /// `fstat` syscall on every read/write bounds check.
    file_len: u64,
    /// Scratch buffer reused across batched reads (grown to the largest batch).
    read_buf: Vec<u8>,
}

impl PackManager {
    /// Creates a pack with `max_blocks` slots (header+directory+blocks+footer).
    pub fn create(path: &Path, max_blocks: u64) -> Result<Self> {
        if max_blocks == 0 || max_blocks > PACK_TARGET_BLOCKS * 4 {
            return Err(invalid("invalid pack capacity"));
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(PlomidError::from)?;
        let total = Self::file_len(max_blocks)?;
        file.set_len(total).map_err(PlomidError::from)?;
        let mut manager = Self {
            file,
            max_blocks,
            next_block_id: 1,
            directory: vec![DIRECTORY_FREE; max_blocks as usize],
            id_to_slot: HashMap::new(),
            file_len: total,
            read_buf: Vec::new(),
        };
        manager.persist_all()?;
        Ok(manager)
    }

    /// Opens and validates header, directory, and footer.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(PlomidError::from)?;
        let (max_blocks, next_block_id) = Self::read_header(&file)?;
        let total = Self::file_len(max_blocks)?;
        let len = file
            .metadata()
            .map(|m| m.len())
            .map_err(PlomidError::from)?;
        if len < total {
            return Err(corrupt("truncated pack file"));
        }
        if (len - PACK_FOOTER_LEN - Self::blocks_offset(max_blocks)?) % BLOCK_SIZE_U64 != 0 {
            return Err(corrupt("pack file has invalid block alignment"));
        }
        let mut manager = Self {
            file,
            max_blocks,
            next_block_id,
            directory: vec![DIRECTORY_FREE; max_blocks as usize],
            id_to_slot: HashMap::new(),
            file_len: len,
            read_buf: Vec::new(),
        };
        manager.load_directory()?;
        manager.validate_footer()?;
        Ok(manager)
    }

    /// Allocates a logical block in the first free directory slot.
    pub fn allocate_block(&mut self) -> Result<BlockId> {
        let slot = self
            .directory
            .iter()
            .position(|entry| *entry == DIRECTORY_FREE)
            .ok_or_else(|| invalid("pack is full"))?;
        if self.next_block_id == u64::MAX {
            return Err(invalid("block ID space exhausted"));
        }
        let id = BlockId::new(self.next_block_id);
        self.next_block_id += 1;
        self.directory[slot] = id.get();
        self.id_to_slot.insert(id, slot as u64);
        self.write_slots(slot as u64, &vec![0_u8; BLOCK_LEN])?;
        self.persist_all()?;
        Ok(id)
    }

    /// Releases a block back to the free set (directory entry freed).
    pub fn release_block(&mut self, id: BlockId) -> Result<()> {
        let slot = self.slot_of(id)?;
        self.directory[slot as usize] = DIRECTORY_FREE;
        self.id_to_slot.remove(&id);
        self.write_slots(slot, &vec![0_u8; BLOCK_LEN])?;
        self.persist_all()?;
        Ok(())
    }

    /// Reads and verifies a full block.
    pub fn read_block(&mut self, id: BlockId) -> Result<Vec<Page>> {
        let slot = self.slot_of(id)?;
        decode_block(&self.read_slots(slot, 1)?)
    }

    /// Writes a full block (16 pages).
    pub fn write_block(&mut self, id: BlockId, pages: &[Page]) -> Result<()> {
        if pages.len() != PAGES_PER_BLOCK as usize {
            return Err(invalid("block write requires exactly 16 pages"));
        }
        let slot = self.slot_of(id)?;
        let mut buffer = Vec::with_capacity(BLOCK_LEN);
        for page in pages {
            // `to_bytes` re-encodes both checksums; a prior full-page
            // validation (an extra CRC pass) is redundant here.
            buffer.extend_from_slice(&page.to_bytes());
        }
        self.write_slots(slot, &buffer)?;
        Ok(())
    }

    /// Contiguous slot runs merge into single reads.
    pub fn read_blocks(&mut self, ids: &[BlockId]) -> Result<Vec<Vec<Page>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut slots: Vec<(usize, u64)> = Vec::with_capacity(ids.len());
        for (index, id) in ids.iter().enumerate() {
            slots.push((index, self.slot_of(*id)?));
        }
        let mut order: Vec<usize> = (0..slots.len()).collect();
        order.sort_by_key(|i| slots[*i].1);
        let mut output: Vec<Option<Vec<Page>>> = (0..ids.len()).map(|_| None).collect();
        let mut pos = 0_usize;
        while pos < order.len() {
            let mut end = pos + 1;
            while end < order.len()
                && slots[order[end]].1 == slots[order[pos]].1 + (end - pos) as u64
            {
                end += 1;
            }
            let bytes = self.read_slots(slots[order[pos]].1, (end - pos) as u64)?;
            for (k, oi) in order[pos..end].iter().enumerate() {
                let chunk = &bytes[k * BLOCK_LEN..(k + 1) * BLOCK_LEN];
                output[slots[*oi].0] = Some(decode_block(chunk)?);
            }
            pos = end;
        }
        Ok(output.into_iter().map(|p| p.expect("filled")).collect())
    }

    /// Contiguous slot runs merge into single writes.
    pub fn write_blocks(&mut self, blocks: &[(BlockId, Vec<Page>)]) -> Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        let mut encoded: Vec<(u64, Vec<u8>)> = Vec::with_capacity(blocks.len());
        for (id, pages) in blocks {
            if pages.len() != PAGES_PER_BLOCK as usize {
                return Err(invalid("block write requires exactly 16 pages"));
            }
            let slot = self.slot_of(*id)?;
            let mut buffer = Vec::with_capacity(BLOCK_LEN);
            for page in pages {
                page.validate()?;
                buffer.extend_from_slice(&page.to_bytes());
            }
            encoded.push((slot, buffer));
        }
        encoded.sort_by_key(|(slot, _)| *slot);
        let mut start = 0_usize;
        while start < encoded.len() {
            let mut end = start + 1;
            while end < encoded.len() && encoded[end].0 == encoded[start].0 + (end - start) as u64 {
                end += 1;
            }
            let mut buffer = Vec::new();
            for (_, bytes) in &encoded[start..end] {
                buffer.extend_from_slice(bytes);
            }
            self.write_slots(encoded[start].0, &buffer)?;
            start = end;
        }
        Ok(())
    }

    /// Returns the pack block directory (slot -> block ID or free).
    #[must_use]
    pub fn block_directory(&self) -> Vec<Option<BlockId>> {
        self.directory
            .iter()
            .map(|entry| {
                if *entry == DIRECTORY_FREE {
                    None
                } else {
                    Some(BlockId::new(*entry))
                }
            })
            .collect()
    }

    /// Validates header, directory, a sample of blocks, and footer.
    pub fn validate(&mut self) -> Result<()> {
        let (max_blocks, _) = Self::read_header(&self.file)?;
        if max_blocks != self.max_blocks {
            return Err(corrupt("pack header capacity changed"));
        }
        self.load_directory()?;
        self.validate_footer()?;
        let mut checked = 0;
        for slot in 0..self.max_blocks {
            if self.directory[slot as usize] == DIRECTORY_FREE {
                continue;
            }
            if checked < 2 {
                let bytes = self.read_slots(slot, 1)?;
                if !bytes.iter().all(|b| *b == 0) {
                    let _ = decode_block(&bytes)?;
                }
                checked += 1;
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Durable flush hook: persists metadata then `sync_all`.
    pub fn flush(&mut self) -> Result<()> {
        self.persist_all()?;
        self.file.sync_all().map_err(PlomidError::from)?;
        Ok(())
    }

    fn slot_of(&self, id: BlockId) -> Result<u64> {
        self.id_to_slot
            .get(&id)
            .copied()
            .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "block ID is not allocated"))
    }

    fn directory_len(max_blocks: u64) -> Result<u64> {
        max_blocks
            .checked_mul(DIRECTORY_ENTRY_LEN)
            .ok_or_else(|| PlomidError::new(ErrorKind::Corruption, "pack directory overflow"))
    }

    fn directory_offset() -> u64 {
        PACK_HEADER_LEN
    }

    /// First block slot offset, rounded up to the next [`BLOCK_SIZE_U64`]
    /// boundary so every block is block aligned.
    fn blocks_offset(max_blocks: u64) -> Result<u64> {
        let base = Self::directory_offset()
            .checked_add(Self::directory_len(max_blocks)?)
            .ok_or_else(|| {
                PlomidError::new(ErrorKind::Corruption, "pack blocks offset overflow")
            })?;
        let rem = base % BLOCK_SIZE_U64;
        if rem == 0 {
            return Ok(base);
        }
        base.checked_add(BLOCK_SIZE_U64 - rem).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "pack blocks alignment overflow")
        })
    }

    fn footer_offset(max_blocks: u64) -> Result<u64> {
        Self::blocks_offset(max_blocks)?
            .checked_add(
                max_blocks.checked_mul(BLOCK_SIZE_U64).ok_or_else(|| {
                    PlomidError::new(ErrorKind::Corruption, "pack blocks overflow")
                })?,
            )
            .ok_or_else(|| PlomidError::new(ErrorKind::Corruption, "pack footer overflow"))
    }

    fn file_len(max_blocks: u64) -> Result<u64> {
        Self::footer_offset(max_blocks)?
            .checked_add(PACK_FOOTER_LEN)
            .ok_or_else(|| PlomidError::new(ErrorKind::Corruption, "pack size overflow"))
    }

    fn slot_offset(&self, slot: u64) -> Result<u64> {
        if slot >= self.max_blocks {
            return Err(invalid("block slot exceeds pack capacity"));
        }
        Self::blocks_offset(self.max_blocks)?
            .checked_add(slot.checked_mul(BLOCK_SIZE_U64).ok_or_else(|| {
                PlomidError::new(ErrorKind::Internal, "pack slot offset overflow")
            })?)
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "pack slot overflow"))
    }

    fn write_slots(&mut self, start: u64, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() || bytes.len() % BLOCK_LEN != 0 {
            return Err(invalid("pack write length is not block aligned"));
        }
        let offset = self.slot_offset(start)?;
        let count = bytes.len() as u64 / BLOCK_SIZE_U64;
        if start + count > self.max_blocks {
            return Err(invalid("pack write exceeds pack capacity"));
        }
        if offset % BLOCK_SIZE_U64 != 0 {
            return Err(corrupt("pack block offset is not block aligned"));
        }
        // One positional write syscall (no seek); extends the file if needed.
        self.file
            .write_all_at(bytes, offset)
            .map_err(PlomidError::from)?;
        let end = offset + bytes.len() as u64;
        if end > self.file_len {
            self.file_len = end;
        }
        Ok(())
    }

    fn read_slots(&mut self, start: u64, count: u64) -> Result<&[u8]> {
        if count == 0 {
            return Err(invalid("pack batch is empty"));
        }
        let total = (count as usize).checked_mul(BLOCK_LEN).ok_or_else(|| {
            PlomidError::new(ErrorKind::InvalidArgument, "pack batch is too large")
        })?;
        if start + count > self.max_blocks {
            return Err(corrupt("pack batch exceeds pack bounds"));
        }
        let offset = self.slot_offset(start)?;
        let end = offset.checked_add(total as u64).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "pack batch exceeds pack bounds")
        })?;
        if end > self.file_len {
            return Err(corrupt("truncated pack read"));
        }
        // Reusable read buffer: grown once, never re-zeroed. `read_exact_at`
        // overwrites every byte before it is observed.
        if self.read_buf.len() < total {
            self.read_buf.resize(total, 0);
        }
        // One positional read syscall (no seek, no fstat).
        self.file
            .read_exact_at(&mut self.read_buf[..total], offset)
            .map_err(|_| corrupt("truncated pack read"))?;
        Ok(&self.read_buf[..total])
    }

    fn persist_all(&mut self) -> Result<()> {
        let max_blocks = self.max_blocks;
        let next_block_id = self.next_block_id;
        let directory = self.directory.clone();
        self.persist_header(max_blocks, next_block_id)?;
        self.persist_directory(&directory)?;
        self.persist_footer(max_blocks, next_block_id, &directory)?;
        Ok(())
    }

    fn persist_header(&mut self, max_blocks: u64, next_block_id: u64) -> Result<()> {
        let mut header = vec![0_u8; PACK_HEADER_LEN as usize];
        header[..4].copy_from_slice(&PACK_MANAGER_MAGIC);
        header[4..8].copy_from_slice(&PACK_MANAGER_VERSION.to_le_bytes());
        header[8..16].copy_from_slice(&BLOCK_SIZE_U64.to_le_bytes());
        header[16..24].copy_from_slice(&PAGE_SIZE_U64.to_le_bytes());
        header[24..32].copy_from_slice(&max_blocks.to_le_bytes());
        header[32..40].copy_from_slice(&next_block_id.to_le_bytes());
        header[40..48].copy_from_slice(&Self::directory_len(max_blocks)?.to_le_bytes());
        header[48..HDR_COUNT].copy_from_slice(&Self::footer_offset(max_blocks)?.to_le_bytes());
        let checksum = compute_checksum(&header[..HDR_COUNT]);
        header[HDR_COUNT..HDR_CHECKSUM].copy_from_slice(&checksum.to_le_bytes());
        self.file
            .write_all_at(&header, 0)
            .map_err(PlomidError::from)?;
        Ok(())
    }

    fn persist_directory(&mut self, directory: &[u64]) -> Result<()> {
        let mut buffer = Vec::with_capacity(directory.len() * 16);
        for entry in directory {
            buffer.extend_from_slice(&entry.to_le_bytes());
            let checksum = compute_checksum(&entry.to_le_bytes());
            buffer.extend_from_slice(&checksum.to_le_bytes());
            buffer.extend_from_slice(&[0_u8; 4]);
        }
        self.file
            .write_all_at(&buffer, Self::directory_offset())
            .map_err(PlomidError::from)?;
        Ok(())
    }

    fn persist_footer(
        &mut self,
        max_blocks: u64,
        next_block_id: u64,
        directory: &[u64],
    ) -> Result<()> {
        let mut footer = vec![0_u8; PACK_FOOTER_LEN as usize];
        footer[..4].copy_from_slice(&PACK_FOOTER_MAGIC);
        footer[4..8].copy_from_slice(&PACK_MANAGER_VERSION.to_le_bytes());
        footer[8..16].copy_from_slice(&max_blocks.to_le_bytes());
        footer[16..24].copy_from_slice(&next_block_id.to_le_bytes());
        footer[24..32].copy_from_slice(&directory_checksum(directory).to_le_bytes());
        footer[32..FTR_COUNT].copy_from_slice(&Self::footer_offset(max_blocks)?.to_le_bytes());
        let checksum = compute_checksum(&footer[..FTR_COUNT]);
        footer[FTR_COUNT..FTR_CHECKSUM].copy_from_slice(&checksum.to_le_bytes());
        let offset = Self::footer_offset(max_blocks)?;
        self.file
            .write_all_at(&footer, offset)
            .map_err(PlomidError::from)?;
        Ok(())
    }

    fn read_header(file: &File) -> Result<(u64, u64)> {
        let len = file
            .metadata()
            .map(|m| m.len())
            .map_err(PlomidError::from)?;
        if len < PACK_HEADER_LEN + PACK_FOOTER_LEN {
            return Err(corrupt("truncated pack file"));
        }
        let mut header = vec![0_u8; PACK_HEADER_LEN as usize];
        file.read_exact_at(&mut header, 0)
            .map_err(|_| corrupt("truncated pack file"))?;
        if header[..4] != PACK_MANAGER_MAGIC {
            return Err(corrupt("invalid pack magic"));
        }
        if le_u32(&header[4..8])? != PACK_MANAGER_VERSION {
            return Err(corrupt("unsupported pack version"));
        }
        if le_u64(&header[8..16])? != BLOCK_SIZE_U64 {
            return Err(corrupt("invalid block size in pack header"));
        }
        if le_u64(&header[16..24])? != PAGE_SIZE_U64 {
            return Err(corrupt("invalid page size in pack header"));
        }
        let max_blocks = le_u64(&header[24..32])?;
        let next_block_id = le_u64(&header[32..40])?;
        if le_u64(&header[40..48])? != Self::directory_len(max_blocks)? {
            return Err(corrupt("invalid block directory in pack header"));
        }
        if le_u64(&header[48..HDR_COUNT])? != Self::footer_offset(max_blocks)? {
            return Err(corrupt("invalid pack footer offset"));
        }
        let stored = le_u32(&header[HDR_COUNT..HDR_CHECKSUM])?;
        verify_checksum(&header[..HDR_COUNT], stored)?;
        if max_blocks == 0 || next_block_id == 0 {
            return Err(corrupt("invalid pack header IDs"));
        }
        if len < Self::file_len(max_blocks)? {
            return Err(corrupt("truncated pack file"));
        }
        Ok((max_blocks, next_block_id))
    }

    fn load_directory(&mut self) -> Result<()> {
        let len = Self::directory_len(self.max_blocks)? as usize;
        let mut buffer = vec![0_u8; len];
        self.file
            .read_exact_at(&mut buffer, Self::directory_offset())
            .map_err(|_| corrupt("truncated pack file"))?;
        self.directory.clear();
        self.id_to_slot.clear();
        let mut max_id = 0_u64;
        for (slot, chunk) in buffer.chunks_exact(16).enumerate() {
            let entry = le_u64(&chunk[..8]).map_err(|_| corrupt("invalid block directory"))?;
            let stored = le_u32(&chunk[8..12]).map_err(|_| corrupt("invalid block directory"))?;
            if chunk[12..16] != [0, 0, 0, 0] {
                return Err(corrupt("invalid block directory"));
            }
            verify_checksum(&chunk[..8], stored).map_err(|_| corrupt("invalid block directory"))?;
            if entry != DIRECTORY_FREE {
                if entry == 0 || entry >= self.next_block_id {
                    return Err(corrupt("invalid block directory"));
                }
                let id = BlockId::new(entry);
                if self.id_to_slot.insert(id, slot as u64).is_some() {
                    return Err(corrupt("invalid block directory"));
                }
                max_id = max_id.max(entry);
            }
            self.directory.push(entry);
        }
        if self.next_block_id <= max_id {
            return Err(corrupt("pack next block ID is behind directory"));
        }
        Ok(())
    }

    fn validate_footer(&mut self) -> Result<()> {
        let offset = Self::footer_offset(self.max_blocks)?;
        let mut footer = vec![0_u8; PACK_FOOTER_LEN as usize];
        self.file
            .read_exact_at(&mut footer, offset)
            .map_err(|_| corrupt("truncated pack file"))?;
        if footer[..4] != PACK_FOOTER_MAGIC {
            return Err(corrupt("invalid pack footer magic"));
        }
        if le_u32(&footer[4..8])? != PACK_MANAGER_VERSION {
            return Err(corrupt("unsupported pack footer version"));
        }
        if le_u64(&footer[8..16])? != self.max_blocks {
            return Err(corrupt("pack footer capacity mismatch"));
        }
        if le_u64(&footer[16..24])? != self.next_block_id {
            return Err(corrupt("pack footer next ID mismatch"));
        }
        if le_u64(&footer[24..32])? != directory_checksum(&self.directory) {
            return Err(corrupt("invalid block directory"));
        }
        if le_u64(&footer[32..FTR_COUNT])? != offset {
            return Err(corrupt("invalid pack footer offset"));
        }
        let stored = le_u32(&footer[FTR_COUNT..FTR_CHECKSUM])?;
        verify_checksum(&footer[..FTR_COUNT], stored)?;
        Ok(())
    }
}

fn decode_block(bytes: &[u8]) -> Result<Vec<Page>> {
    let unit = BLOCK_SIZE_U64 as usize;
    if bytes.len() != unit || bytes.len() % PAGE_SIZE != 0 {
        return Err(corrupt("pack block has invalid page alignment"));
    }
    let mut pages = Vec::with_capacity(PAGES_PER_BLOCK as usize);
    for chunk in bytes.chunks_exact(PAGE_SIZE) {
        pages.push(Page::from_bytes(chunk)?);
    }
    Ok(pages)
}

fn directory_checksum(directory: &[u64]) -> u64 {
    let mut buffer = Vec::with_capacity(directory.len() * 8);
    for entry in directory {
        buffer.extend_from_slice(&entry.to_le_bytes());
    }
    u64::from(compute_checksum(&buffer))
}

fn invalid(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::InvalidArgument, message)
}

fn corrupt(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}

fn le_u32(bytes: &[u8]) -> Result<u32> {
    bytes
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid pack field"))
}

fn le_u64(bytes: &[u8]) -> Result<u64> {
    bytes
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid pack field"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plomid_core::PageId;
    use std::io::Seek;
    use std::path::PathBuf;

    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("pack.plpk");
        (dir, path)
    }

    fn write_at(path: &Path, offset: u64, bytes: &[u8]) {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open");
        file.seek(std::io::SeekFrom::Start(offset)).expect("seek");
        use std::io::Write as _;
        file.write_all(bytes).expect("write");
        drop(file);
    }

    fn block_pages(base: u64) -> Vec<Page> {
        use crate::physical::PageType;
        use plomid_core::PageId;
        (0..PAGES_PER_BLOCK as usize)
            .map(|i| Page::with_type(PageId::new(base + i as u64), PageType::Leaf))
            .collect()
    }

    #[test]
    fn round_trip_and_release() {
        let (dir, path) = scratch("pk-rt");
        let mut pack = PackManager::create(&path, 64).expect("create");
        let a = pack.allocate_block().expect("alloc a");
        let b = pack.allocate_block().expect("alloc b");
        pack.write_block(a, &block_pages(100)).expect("write a");
        pack.write_blocks(&[(b, block_pages(200))])
            .expect("write b");
        pack.flush().expect("flush");

        assert_eq!(
            pack.read_block(a).expect("read a")[0].id(),
            PageId::new(100)
        );
        let batch = pack.read_blocks(&[b, a]).expect("batch");
        assert_eq!(batch[0][0].id(), PageId::new(200));
        assert_eq!(batch[1][0].id(), PageId::new(100));
        let listing = pack.block_directory();
        assert_eq!(listing.iter().filter(|e| e.is_some()).count(), 2);

        pack.release_block(a).expect("release a");
        assert!(pack.read_block(a).is_err());
        pack.validate().expect("validate after release");

        drop(pack);
        let mut reopened = PackManager::open(&path).expect("open");
        reopened.validate().expect("revalidate");
        let listing2 = reopened.block_directory();
        assert_eq!(listing2.iter().filter(|e| e.is_some()).count(), 1);
        drop(reopened);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_invalid_magic() {
        let (dir, path) = scratch("pk-magic");
        let _ = PackManager::create(&path, 16).expect("create");
        write_at(&path, 0, b"XXXX");
        assert!(PackManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_invalid_block_size() {
        let (dir, path) = scratch("pk-bsize");
        let _ = PackManager::create(&path, 16).expect("create");
        // Block-size field at header bytes 8..16.
        write_at(&path, 8, &(4096_u64).to_le_bytes());
        assert!(PackManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_invalid_page_size() {
        let (dir, path) = scratch("pk-psize");
        let _ = PackManager::create(&path, 16).expect("create");
        // Page-size field at header bytes 16..24.
        write_at(&path, 16, &(4096_u64).to_le_bytes());
        assert!(PackManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_invalid_block_directory() {
        let (dir, path) = scratch("pk-directory");
        let mut pack = PackManager::create(&path, 16).expect("create");
        let block = pack.allocate_block().expect("alloc");
        pack.write_block(block, &block_pages(1)).expect("write");
        drop(pack);
        // Corrupt the first directory entry's checksum field (entry starts at PACK_HEADER_LEN).
        write_at(&path, PACK_HEADER_LEN + 8, &[0x55, 0x55, 0x55, 0x55]);
        assert!(PackManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_truncated_pack() {
        let (dir, path) = scratch("pk-trunc");
        let _ = PackManager::create(&path, 64).expect("create");
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open")
            .set_len(100)
            .expect("truncate");
        assert!(PackManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_footer_checksum_mismatch() {
        let (dir, path) = scratch("pk-ftrcksum");
        let pack = PackManager::create(&path, 16).expect("create");
        drop(pack);
        let footer_offset = PackManager::footer_offset(16).expect("footer offset");
        write_at(&path, footer_offset + 42, &[0x01]);
        assert!(PackManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
