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
//! BlockManager: logical blocks over a block file.

use crate::physical::{
    BLOCK_SIZE as BLOCK_SIZE_U64, PAGES_PER_BLOCK, PAGE_SIZE as PHYS_PAGE_SIZE_U64,
};
use crate::platform::FileExt;
use crate::{compute_checksum, verify_checksum, Page, PAGE_SIZE};
use plomid_core::{BlockId, ErrorKind, PlomidError, Result};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::Path;

/// Block-manager constants are defined once in `plomid_core::constants` and
/// re-exported here; HDR_COUNT/HDR_CHECKSUM/BLOCK_SIZE keep module-local names.
pub use plomid_core::{
    BLOCK_MANAGER_HDR_CHECKSUM as HDR_CHECKSUM, BLOCK_MANAGER_HDR_COUNT as HDR_COUNT,
    BLOCK_MANAGER_HEADER_LEN, BLOCK_MANAGER_MAGIC, BLOCK_MANAGER_VERSION,
    BLOCK_SIZE_USIZE as BLOCK_SIZE,
};

/// File-backed manager mapping logical [`BlockId`]s to block slots.
pub struct BlockManager {
    file: File,
    next_block_id: u64,
    block_count: u64,
    blocks: HashMap<BlockId, u64>,
    /// Cached physical file length; updated on extending writes. Avoids a
    /// `fstat` syscall on every read bounds check.
    file_len: u64,
    /// Scratch buffer reused across batched reads (grown to the largest batch).
    read_buf: Vec<u8>,
}

impl BlockManager {
    /// Creates a truncated block file.
    pub fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(PlomidError::from)?;
        file.set_len(BLOCK_MANAGER_HEADER_LEN)
            .map_err(PlomidError::from)?;
        let mut manager = Self {
            file,
            next_block_id: 1,
            block_count: 0,
            blocks: HashMap::new(),
            file_len: BLOCK_MANAGER_HEADER_LEN,
            read_buf: Vec::new(),
        };
        manager.persist_header()?;
        Ok(manager)
    }

    /// Opens an existing block file and validates header + alignment.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(PlomidError::from)?;
        let (next_block_id, block_count, file_len) = Self::read_header(&file)?;
        let mut manager = Self {
            file,
            next_block_id,
            block_count,
            blocks: HashMap::new(),
            file_len,
            read_buf: Vec::new(),
        };
        manager.rebuild_map()?;
        Ok(manager)
    }

    /// Allocates a logical block and zeroes its slot.
    pub fn allocate_block(&mut self) -> Result<BlockId> {
        if self.next_block_id == u64::MAX {
            return Err(invalid("block ID space exhausted"));
        }
        let id = BlockId::new(self.next_block_id);
        self.next_block_id += 1;
        let slot = self.block_count;
        self.block_count += 1;
        self.blocks.insert(id, slot);
        self.write_slots(slot, &vec![0_u8; BLOCK_SIZE])?;
        self.persist_header()?;
        Ok(id)
    }

    /// Page addressing: byte offset of a block slot, checked for overflow.
    pub fn block_offset(&self, id: BlockId) -> Result<u64> {
        let slot = self.slot_of(id)?;
        Self::slot_offset(slot)
    }

    /// Page addressing: byte offset of `page_index` inside `id`.
    pub fn page_offset(&self, id: BlockId, page_index: usize) -> Result<u64> {
        if page_index >= usize::try_from(PAGES_PER_BLOCK).unwrap_or(usize::MAX) {
            return Err(invalid("page index exceeds block capacity"));
        }
        let base = self.block_offset(id)?;
        let delta = (page_index as u64)
            .checked_mul(PHYS_PAGE_SIZE_U64)
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "page offset overflow"))?;
        base.checked_add(delta)
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "page offset overflow"))
    }

    /// Reads a full verified block (16 pages).
    pub fn read_block(&mut self, id: BlockId) -> Result<Vec<Page>> {
        let slot = self.slot_of(id)?;
        let bytes = self.read_slots(slot, 1)?;
        decode_block(&bytes)
    }

    /// Writes a full block (16 pages) to its slot.
    pub fn write_block(&mut self, id: BlockId, pages: &[Page]) -> Result<()> {
        if pages.len() != PAGES_PER_BLOCK as usize {
            return Err(invalid("block write requires exactly 16 pages"));
        }
        let slot = self.slot_of(id)?;
        let mut buffer = Vec::with_capacity(BLOCK_SIZE_U64 as usize);
        for page in pages {
            // `to_bytes` re-encodes both checksums; a prior full-page
            // validation (an extra CRC pass) is redundant here.
            buffer.extend_from_slice(&page.to_bytes());
        }
        self.write_slots(slot, &buffer)?;
        Ok(())
    }

    /// Contiguous block reads merge into single file reads.
    pub fn read_blocks(&mut self, ids: &[BlockId]) -> Result<Vec<Vec<Page>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut slots: Vec<(usize, u64)> = Vec::with_capacity(ids.len());
        for (index, id) in ids.iter().enumerate() {
            slots.push((index, self.slot_of(*id)?));
        }
        let order: Vec<usize> = {
            let mut o: Vec<usize> = (0..slots.len()).collect();
            o.sort_by_key(|i| slots[*i].1);
            o
        };
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
                let base = k * BLOCK_SIZE_U64 as usize;
                let chunk = &bytes[base..base + BLOCK_SIZE_U64 as usize];
                output[slots[*oi].0] = Some(decode_block(chunk)?);
            }
            pos = end;
        }
        Ok(output.into_iter().map(|p| p.expect("filled")).collect())
    }

    /// Contiguous block writes merge into single file writes.
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
            let mut buffer = Vec::with_capacity(BLOCK_SIZE_U64 as usize);
            for page in pages {
                // `to_bytes` re-encodes both checksums; a prior full-page
                // validation (an extra CRC pass) is redundant here.
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

    /// Validates block alignment, size, and page checksums.
    pub fn validate_block(&mut self, id: BlockId) -> Result<()> {
        let slot = self.slot_of(id)?;
        let offset = Self::slot_offset(slot)?;
        if offset % BLOCK_SIZE_U64 != 0 {
            return Err(corrupt("block offset is not block aligned"));
        }
        let pages = self.read_block(id)?;
        for page in &pages {
            page.validate()?;
        }
        Ok(())
    }

    /// Returns the number of allocated logical blocks.
    #[must_use]
    pub fn block_count(&self) -> u64 {
        self.blocks.len() as u64
    }

    /// Durable flush hook (`sync_all`).
    pub fn flush(&mut self) -> Result<()> {
        self.persist_header()?;
        self.file.sync_all().map_err(PlomidError::from)?;
        Ok(())
    }

    fn slot_of(&self, id: BlockId) -> Result<u64> {
        self.blocks
            .get(&id)
            .copied()
            .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "block ID is not allocated"))
    }

    fn slot_offset(slot: u64) -> Result<u64> {
        slot.checked_mul(BLOCK_SIZE_U64)
            .and_then(|off| off.checked_add(BLOCK_MANAGER_HEADER_LEN))
            .ok_or_else(|| invalid("block slot offset overflow"))
    }

    fn write_slots(&mut self, start: u64, bytes: &[u8]) -> Result<()> {
        let unit = BLOCK_SIZE_U64 as usize;
        if bytes.is_empty() || bytes.len() % unit != 0 {
            return Err(invalid("block write length is not block aligned"));
        }
        let offset = Self::slot_offset(start)?;
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
            return Err(invalid("block batch is empty"));
        }
        let unit = BLOCK_SIZE_U64 as usize;
        let total = (count as usize).checked_mul(unit).ok_or_else(|| {
            PlomidError::new(ErrorKind::InvalidArgument, "block batch is too large")
        })?;
        let offset = Self::slot_offset(start)?;
        let end = offset.checked_add(total as u64).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "block batch exceeds file bounds")
        })?;
        if end > self.file_len {
            return Err(corrupt("block batch exceeds file bounds"));
        }
        if offset % BLOCK_SIZE_U64 != 0 {
            return Err(corrupt("block offset is not block aligned"));
        }
        // Reusable read buffer: grown once, never re-zeroed. `read_exact_at`
        // overwrites every byte before it is observed.
        if self.read_buf.len() < total {
            self.read_buf.resize(total, 0);
        }
        // One positional read syscall (no seek, no fstat).
        self.file
            .read_exact_at(&mut self.read_buf[..total], offset)
            .map_err(|_| corrupt("truncated block read"))?;
        Ok(&self.read_buf[..total])
    }

    fn persist_header(&mut self) -> Result<()> {
        let mut header = vec![0_u8; BLOCK_MANAGER_HEADER_LEN as usize];
        header[..4].copy_from_slice(&BLOCK_MANAGER_MAGIC);
        header[4..8].copy_from_slice(&BLOCK_MANAGER_VERSION.to_le_bytes());
        header[8..16].copy_from_slice(&BLOCK_SIZE_U64.to_le_bytes());
        header[16..24].copy_from_slice(&PHYS_PAGE_SIZE_U64.to_le_bytes());
        header[24..32].copy_from_slice(&self.next_block_id.to_le_bytes());
        header[32..HDR_COUNT].copy_from_slice(&self.block_count.to_le_bytes());
        let checksum = compute_checksum(&header[..HDR_COUNT]);
        header[HDR_COUNT..HDR_CHECKSUM].copy_from_slice(&checksum.to_le_bytes());
        self.file
            .write_all_at(&header, 0)
            .map_err(PlomidError::from)?;
        Ok(())
    }

    fn read_header(file: &File) -> Result<(u64, u64, u64)> {
        let len = file
            .metadata()
            .map(|m| m.len())
            .map_err(PlomidError::from)?;
        if len < BLOCK_MANAGER_HEADER_LEN {
            return Err(corrupt("truncated block-manager file"));
        }
        let mut header = vec![0_u8; BLOCK_MANAGER_HEADER_LEN as usize];
        file.read_exact_at(&mut header, 0)
            .map_err(|_| corrupt("truncated block-manager file"))?;
        if header[..4] != BLOCK_MANAGER_MAGIC {
            return Err(corrupt("invalid block-manager magic"));
        }
        let version = le_u32(&header[4..8])?;
        if version != BLOCK_MANAGER_VERSION {
            return Err(corrupt("unsupported block-manager version"));
        }
        if le_u64(&header[8..16])? != BLOCK_SIZE_U64 {
            return Err(corrupt("invalid block size in block-manager header"));
        }
        if le_u64(&header[16..24])? != PHYS_PAGE_SIZE_U64 {
            return Err(corrupt("invalid page size in block-manager header"));
        }
        let next_block_id = le_u64(&header[24..32])?;
        let block_count = le_u64(&header[32..HDR_COUNT])?;
        let stored = le_u32(&header[HDR_COUNT..HDR_CHECKSUM])?;
        verify_checksum(&header[..HDR_COUNT], stored)?;
        if next_block_id == 0 {
            return Err(corrupt("block-manager next block ID is zero"));
        }
        let data = block_count.checked_mul(BLOCK_SIZE_U64).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "block-manager count overflow")
        })?;
        let expected = BLOCK_MANAGER_HEADER_LEN.checked_add(data).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "block-manager size overflow")
        })?;
        if len < expected {
            return Err(corrupt("truncated block-manager file"));
        }
        if (len - BLOCK_MANAGER_HEADER_LEN) % BLOCK_SIZE_U64 != 0 {
            return Err(corrupt("block-manager file has invalid block alignment"));
        }
        Ok((next_block_id, block_count, len))
    }

    fn rebuild_map(&mut self) -> Result<()> {
        self.blocks.clear();
        for slot in 0..self.block_count {
            let bytes = self.read_slots(slot, 1)?;
            if !bytes.iter().all(|b| *b == 0) {
                let _ = decode_block(&bytes)?;
            }
        }
        for slot in 0..self.block_count {
            let id = BlockId::new(slot + 1);
            if id.get() >= self.next_block_id {
                return Err(corrupt("block-manager next ID is behind blocks"));
            }
            self.blocks.insert(id, slot);
        }
        Ok(())
    }
}

fn decode_block(bytes: &[u8]) -> Result<Vec<Page>> {
    let unit = BLOCK_SIZE_U64 as usize;
    if bytes.len() != unit || bytes.len() % PAGE_SIZE != 0 {
        return Err(corrupt("block has invalid page alignment"));
    }
    let mut pages = Vec::with_capacity(PAGES_PER_BLOCK as usize);
    for chunk in bytes.chunks_exact(PAGE_SIZE) {
        pages.push(Page::from_bytes(chunk)?);
    }
    Ok(pages)
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
        .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid block-manager field"))
}

fn le_u64(bytes: &[u8]) -> Result<u64> {
    bytes
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid block-manager field"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical::PageType;
    use plomid_core::PageId;

    fn block_pages(base: u64) -> Vec<Page> {
        (0..PAGES_PER_BLOCK)
            .map(|i| Page::with_type(PageId::new(base + i), PageType::Leaf))
            .collect()
    }

    #[test]
    fn block_round_trip_and_batch() {
        let dir = std::env::temp_dir().join(format!(
            "plomid-block-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("blocks.plbm");
        let mut manager = BlockManager::create(&path).expect("create");
        let a = manager.allocate_block().expect("alloc a");
        let b = manager.allocate_block().expect("alloc b");
        manager.write_block(a, &block_pages(100)).expect("write a");
        manager
            .write_blocks(&[(b, block_pages(200))])
            .expect("write b");
        manager.flush().expect("flush");
        let pages = manager.read_block(a).expect("read a");
        assert_eq!(pages.len(), PAGES_PER_BLOCK as usize);
        assert_eq!(pages[0].id(), PageId::new(100));
        let batch = manager.read_blocks(&[b, a]).expect("batch");
        assert_eq!(batch[0][0].id(), PageId::new(200));
        assert_eq!(batch[1][0].id(), PageId::new(100));
        manager.validate_block(a).expect("validate");
        let off_a = manager.block_offset(a).expect("off a");
        let off_b = manager.block_offset(b).expect("off b");
        assert_eq!(off_b, off_a + BLOCK_SIZE_U64);
        assert_eq!(
            manager.page_offset(a, 1).expect("page off"),
            off_a + PHYS_PAGE_SIZE_U64
        );
        drop(manager);
        let mut reopened = BlockManager::open(&path).expect("open");
        reopened.validate_block(b).expect("revalidate");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_misaligned_trailing_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "plomid-block-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("blocks.plbm");
        let mut manager = BlockManager::create(&path).expect("create");
        let a = manager.allocate_block().expect("alloc");
        manager.write_block(a, &block_pages(1)).expect("write");
        drop(manager);
        // Append one stray byte so the file is no longer block aligned.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        use std::io::Write as _;
        file.write_all(&[9]).expect("write stray");
        drop(file);
        assert!(BlockManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
