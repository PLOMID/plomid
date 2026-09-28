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
//! [`PageManager`]: logical pages over a page-slot file.
//!
//! File layout: one [`PAGE_SIZE`]-byte header region at offset 0, then page
//! slots of [`PAGE_SIZE`] bytes. Slot `i` lives at `HEADER_LEN + i *
//! PAGE_SIZE`, so every page is page-aligned. [`PageId`] values are logical
//! keys into a slot map rebuilt by [`PageManager::open`] from on-disk page
//! headers; a `PageId` never implies a filesystem offset.
//!
//! Sequential I/O: [`PageManager::read_pages`] and
//! [`PageManager::write_pages`] merge contiguous slots into single reads and
//! writes instead of issuing one syscall per page.

use crate::physical::{PageType, PAGE_SIZE as PHYSICAL_PAGE_SIZE};
use crate::platform::FileExt;
use crate::{compute_checksum, verify_checksum, Page, PAGE_SIZE};
use plomid_core::{ErrorKind, GenerationId, Lsn, PageId, PlomidError, Result};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::Path;

/// Page-manager constants are defined once in `plomid_core::constants` and
/// re-exported here; the HDR_* offsets keep their module-local names.
pub use plomid_core::{
    PAGE_MANAGER_HDR_CHECKSUM_END as HDR_CHECKSUM_END,
    PAGE_MANAGER_HDR_GENERATION_END as HDR_GENERATION_END,
    PAGE_MANAGER_HDR_NEXT_ID_END as HDR_NEXT_ID_END,
    PAGE_MANAGER_HDR_PAGE_SIZE_END as HDR_PAGE_SIZE_END,
    PAGE_MANAGER_HDR_SLOT_COUNT_END as HDR_SLOT_COUNT_END,
    PAGE_MANAGER_HDR_VERSION_END as HDR_VERSION_END, PAGE_MANAGER_HEADER_LEN, PAGE_MANAGER_MAGIC,
    PAGE_MANAGER_VERSION,
};

const PAGE_SIZE_U64: u64 = PHYSICAL_PAGE_SIZE;

/// Metadata returned by [`PageManager::validate_page`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageMetadata {
    /// Logical page identity (not a file offset).
    pub page_id: PageId,
    /// Page type from the on-disk header.
    pub page_type: PageType,
    /// Generation counter from the on-disk header.
    pub generation: GenerationId,
    /// Last-write LSN from the on-disk header.
    pub lsn: Lsn,
}

/// File-backed manager mapping logical [`PageId`]s to page slots.
pub struct PageManager {
    file: File,
    next_page_id: u64,
    slot_count: u64,
    slots: HashMap<PageId, u64>,
    free_slots: Vec<u64>,
    generation: GenerationId,
    /// Cached physical file length; updated on extending writes. Avoids a
    /// `fstat` syscall on every read/write bounds check.
    file_len: u64,
    /// Scratch buffer reused across batched reads (grown to the largest batch).
    read_buf: Vec<u8>,
}

impl PageManager {
    /// Creates a truncated page file with `PLPM` header.
    pub fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(PlomidError::from)?;
        file.set_len(PAGE_MANAGER_HEADER_LEN)
            .map_err(PlomidError::from)?;
        let mut manager = Self {
            file,
            next_page_id: 1,
            slot_count: 0,
            slots: HashMap::new(),
            free_slots: Vec::new(),
            generation: GenerationId::new(1),
            file_len: PAGE_MANAGER_HEADER_LEN,
            read_buf: Vec::new(),
        };
        manager.persist_header()?;
        Ok(manager)
    }

    /// Opens an existing page file; validates header + rebuilds slot map.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(PlomidError::from)?;
        let (next_page_id, slot_count, generation, file_len) = Self::read_header(&file)?;
        let mut manager = Self {
            file,
            next_page_id,
            slot_count,
            slots: HashMap::new(),
            free_slots: Vec::new(),
            generation,
            file_len,
            read_buf: Vec::new(),
        };
        manager.rebuild_slot_map()?;
        Ok(manager)
    }

    /// Allocates a logical page; reuses a freed slot when available.
    pub fn allocate_page(&mut self, page_type: PageType) -> Result<Page> {
        if self.next_page_id == u64::MAX {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "page ID space exhausted",
            ));
        }
        let id = PageId::new(self.next_page_id);
        self.next_page_id += 1;
        let slot = self.free_slots.pop().unwrap_or_else(|| {
            let slot = self.slot_count;
            self.slot_count += 1;
            slot
        });
        let mut page = Page::with_type(id, page_type);
        page.set_generation(self.generation);
        self.slots.insert(id, slot);
        let bytes = page.to_bytes();
        self.write_slot(slot, &bytes)?;
        self.persist_header()?;
        Ok(page)
    }

    /// Reads and verifies the page with logical `id`.
    pub fn read_page(&mut self, id: PageId) -> Result<Page> {
        let slot = self.slot_of(id)?;
        let bytes = self.read_slot(slot)?;
        let page = Page::from_bytes(&bytes)?;
        if page.id() != id {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "page slot holds a different page ID",
            ));
        }
        Ok(page)
    }

    /// Encodes, checksums, and writes `page` back to its slot.
    ///
    /// The page may be dirty (payload mutated via [`crate::Page::data_mut`]);
    /// the on-disk copy gets a freshly computed checksum.
    pub fn write_page(&mut self, page: &Page) -> Result<()> {
        let id = page.id();
        let slot = self.slot_of(id)?;
        let bytes = page.to_bytes();
        self.write_slot(slot, &bytes)?;
        Ok(())
    }

    /// Batched read: contiguous slots merge into single reads.
    pub fn read_pages(&mut self, ids: &[PageId]) -> Result<Vec<Page>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut slots: Vec<(usize, PageId, u64)> = Vec::with_capacity(ids.len());
        for (index, id) in ids.iter().enumerate() {
            slots.push((index, *id, self.slot_of(*id)?));
        }
        slots.sort_by_key(|(_, _, slot)| *slot);
        let mut output: Vec<Option<Page>> = (0..ids.len()).map(|_| None).collect();
        let mut run_start = 0_usize;
        while run_start < slots.len() {
            let mut run_end = run_start + 1;
            while run_end < slots.len()
                && slots[run_end].2 == slots[run_start].2 + (run_end - run_start) as u64
            {
                run_end += 1;
            }
            let bytes = self.read_slots(slots[run_start].2, (run_end - run_start) as u64)?;
            for (pos, (index, id, _)) in slots[run_start..run_end].iter().enumerate() {
                let page = Page::from_bytes(&bytes[pos * PAGE_SIZE..(pos + 1) * PAGE_SIZE])?;
                if &page.id() != id {
                    return Err(PlomidError::new(
                        ErrorKind::Corruption,
                        "page slot holds a different page ID",
                    ));
                }
                output[*index] = Some(page);
            }
            run_start = run_end;
        }
        Ok(output.into_iter().map(|p| p.expect("filled")).collect())
    }

    /// Batched write: contiguous slots merge into single writes.
    pub fn write_pages(&mut self, pages: &[Page]) -> Result<()> {
        if pages.is_empty() {
            return Ok(());
        }
        let mut encoded: Vec<(PageId, u64, [u8; PAGE_SIZE])> = Vec::with_capacity(pages.len());
        for page in pages {
            let slot = self.slot_of(page.id())?;
            encoded.push((page.id(), slot, page.to_bytes()));
        }
        encoded.sort_by_key(|(_, slot, _)| *slot);
        let mut run_start = 0_usize;
        while run_start < encoded.len() {
            let mut run_end = run_start + 1;
            while run_end < encoded.len()
                && encoded[run_end].1 == encoded[run_start].1 + (run_end - run_start) as u64
            {
                run_end += 1;
            }
            let mut buffer = Vec::with_capacity((run_end - run_start) * PAGE_SIZE);
            for (_, _, bytes) in &encoded[run_start..run_end] {
                buffer.extend_from_slice(bytes);
            }
            self.write_slots(encoded[run_start].1, &buffer)?;
            run_start = run_end;
        }
        Ok(())
    }

    /// Validates header + checksums; returns tracked LSN/generation/type.
    pub fn validate_page(&mut self, id: PageId) -> Result<PageMetadata> {
        let page = self.read_page(id)?;
        page.validate()?;
        Ok(PageMetadata {
            page_id: page.id(),
            page_type: page.page_type(),
            generation: page.generation(),
            lsn: page.lsn(),
        })
    }

    /// Returns the number of allocated logical pages.
    #[must_use]
    pub fn page_count(&self) -> u64 {
        self.slots.len() as u64
    }

    /// Flushes data + header durably (`sync_all` hook).
    pub fn flush(&mut self) -> Result<()> {
        self.persist_header()?;
        self.file.sync_all().map_err(PlomidError::from)?;
        Ok(())
    }

    fn slot_of(&self, id: PageId) -> Result<u64> {
        self.slots
            .get(&id)
            .copied()
            .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "page ID is not allocated"))
    }

    fn slot_offset(slot: u64) -> Result<u64> {
        slot.checked_mul(PAGE_SIZE_U64)
            .and_then(|off| off.checked_add(PAGE_MANAGER_HEADER_LEN))
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "page slot offset overflow"))
    }

    fn write_slot(&mut self, slot: u64, bytes: &[u8; PAGE_SIZE]) -> Result<()> {
        self.write_slots(slot, bytes)
    }

    fn write_slots(&mut self, start: u64, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() || bytes.len() % PAGE_SIZE != 0 {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "page write length is not page aligned",
            ));
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

    fn read_slot(&mut self, slot: u64) -> Result<[u8; PAGE_SIZE]> {
        // Positional single-syscall read straight into the stack buffer:
        // no seek, no heap allocation, no intermediate copy.
        let offset = Self::slot_offset(slot)?;
        let end = offset.checked_add(PAGE_SIZE as u64).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "page batch exceeds file bounds")
        })?;
        if end > self.file_len {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "page batch exceeds file bounds",
            ));
        }
        let mut raw = [0_u8; PAGE_SIZE];
        self.file.read_exact_at(&mut raw, offset).map_err(|error| {
            PlomidError::new(
                ErrorKind::Corruption,
                format!("truncated page read ({error:?})"),
            )
        })?;
        Ok(raw)
    }

    fn read_slots(&mut self, start: u64, count: u64) -> Result<&[u8]> {
        if count == 0 {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "page batch is empty",
            ));
        }
        let total = (count as usize).checked_mul(PAGE_SIZE).ok_or_else(|| {
            PlomidError::new(ErrorKind::InvalidArgument, "page batch is too large")
        })?;
        let offset = Self::slot_offset(start)?;
        let end = offset.checked_add(total as u64).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "page batch exceeds file bounds")
        })?;
        if end > self.file_len {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "page batch exceeds file bounds",
            ));
        }
        // Reusable read buffer: grown once, never re-zeroed. `read_exact_at`
        // overwrites every byte before it is observed, so stale contents from
        // a previous batch are unreachable.
        if self.read_buf.len() < total {
            self.read_buf.resize(total, 0);
        }
        // One positional read syscall (no seek, no fstat).
        self.file
            .read_exact_at(&mut self.read_buf[..total], offset)
            .map_err(|error| {
                PlomidError::new(
                    ErrorKind::Corruption,
                    format!("truncated page read ({error:?})"),
                )
            })?;
        Ok(&self.read_buf[..total])
    }
    fn persist_header(&mut self) -> Result<()> {
        let mut header = [0_u8; PAGE_MANAGER_HEADER_LEN as usize];
        header[..4].copy_from_slice(&PAGE_MANAGER_MAGIC);
        header[4..HDR_VERSION_END].copy_from_slice(&PAGE_MANAGER_VERSION.to_le_bytes());
        header[HDR_VERSION_END..HDR_PAGE_SIZE_END].copy_from_slice(&PAGE_SIZE_U64.to_le_bytes());
        header[HDR_PAGE_SIZE_END..HDR_NEXT_ID_END]
            .copy_from_slice(&self.next_page_id.to_le_bytes());
        header[HDR_NEXT_ID_END..HDR_SLOT_COUNT_END].copy_from_slice(&self.slot_count.to_le_bytes());
        header[HDR_SLOT_COUNT_END..HDR_GENERATION_END]
            .copy_from_slice(&self.generation.get().to_le_bytes());
        let checksum = compute_checksum(&header[..HDR_GENERATION_END]);
        header[HDR_GENERATION_END..HDR_CHECKSUM_END].copy_from_slice(&checksum.to_le_bytes());
        self.file
            .write_all_at(&header, 0)
            .map_err(PlomidError::from)?;
        Ok(())
    }

    fn read_header(file: &File) -> Result<(u64, u64, GenerationId, u64)> {
        let len = file
            .metadata()
            .map(|m| m.len())
            .map_err(PlomidError::from)?;
        if len < PAGE_MANAGER_HEADER_LEN {
            return Err(corrupt("truncated page-manager file"));
        }
        let mut header = [0_u8; PAGE_MANAGER_HEADER_LEN as usize];
        file.read_exact_at(&mut header, 0)
            .map_err(|_| corrupt("truncated page-manager file"))?;
        if header[..4] != PAGE_MANAGER_MAGIC {
            return Err(corrupt("invalid page-manager magic"));
        }
        let version = le_u32(&header[4..HDR_VERSION_END])?;
        if version != PAGE_MANAGER_VERSION {
            return Err(corrupt(&format!(
                "unsupported page-manager version {version}"
            )));
        }
        let page_size = le_u64(&header[HDR_VERSION_END..HDR_PAGE_SIZE_END])?;
        if page_size != PAGE_SIZE_U64 {
            return Err(corrupt("invalid page size in page-manager header"));
        }
        let next_page_id = le_u64(&header[HDR_PAGE_SIZE_END..HDR_NEXT_ID_END])?;
        let slot_count = le_u64(&header[HDR_NEXT_ID_END..HDR_SLOT_COUNT_END])?;
        let generation = le_u64(&header[HDR_SLOT_COUNT_END..HDR_GENERATION_END])?;
        let stored = le_u32(&header[HDR_GENERATION_END..HDR_CHECKSUM_END])?;
        verify_checksum(&header[..HDR_GENERATION_END], stored)?;
        if next_page_id == 0 {
            return Err(corrupt("page-manager next page ID is zero"));
        }
        let data_len = slot_count.checked_mul(PAGE_SIZE_U64).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "page-manager slot count overflow")
        })?;
        let expected = PAGE_MANAGER_HEADER_LEN
            .checked_add(data_len)
            .ok_or_else(|| PlomidError::new(ErrorKind::Corruption, "page-manager size overflow"))?;
        if len < expected {
            return Err(corrupt("truncated page-manager file"));
        }
        if (len - PAGE_MANAGER_HEADER_LEN) % PAGE_SIZE_U64 != 0 {
            return Err(corrupt("page-manager file has invalid page alignment"));
        }
        Ok((next_page_id, slot_count, GenerationId::new(generation), len))
    }

    fn rebuild_slot_map(&mut self) -> Result<()> {
        self.slots.clear();
        self.free_slots.clear();
        let mut max_id = 0_u64;
        for slot in 0..self.slot_count {
            let raw = self.read_slot(slot)?;
            if raw[..4] == [0, 0, 0, 0] {
                self.free_slots.push(slot);
                continue;
            }
            let page = Page::from_bytes(&raw)?;
            let id = page.id();
            if self.slots.insert(id, slot).is_some() {
                return Err(corrupt("duplicate page ID in page-manager file"));
            }
            max_id = max_id.max(id.get());
        }
        if self.next_page_id <= max_id {
            return Err(corrupt(
                "page-manager next page ID is behind allocated pages",
            ));
        }
        Ok(())
    }
}

fn corrupt(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}

fn le_u32(bytes: &[u8]) -> Result<u32> {
    bytes
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid page-manager field"))
}

fn le_u64(bytes: &[u8]) -> Result<u64> {
    bytes
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid page-manager field"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical::PageType;
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
        let path = dir.join("pages.plpm");
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
    }

    #[test]
    fn round_trip_and_batched_io() {
        let (dir, path) = scratch("pm-rt");
        let mut manager = PageManager::create(&path).expect("create");
        let mut first = manager.allocate_page(PageType::Leaf).expect("alloc a");
        let mut second = manager.allocate_page(PageType::Internal).expect("alloc b");
        first.data_mut()[..4].copy_from_slice(b"aaaa");
        second.data_mut()[..4].copy_from_slice(b"bbbb");
        manager.write_page(&first).expect("write a");
        manager.write_page(&second).expect("write b");
        manager.flush().expect("flush");

        let batch = manager
            .read_pages(&[second.id(), first.id()])
            .expect("batched read");
        assert_eq!(batch.len(), 2);
        assert_eq!(&batch[0].data()[..4], b"bbbb");
        assert_eq!(&batch[1].data()[..4], b"aaaa");

        drop(manager);
        let mut reopened = PageManager::open(&path).expect("open");
        let meta = reopened.validate_page(first.id()).expect("validate");
        assert_eq!(meta.page_id, first.id());
        assert_eq!(meta.page_type, PageType::Leaf);
        reopened.flush().expect("flush");
        drop(reopened);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_invalid_magic() {
        let (dir, path) = scratch("pm-magic");
        let _ = PageManager::create(&path).expect("create");
        write_at(&path, 0, b"XXXX");
        assert!(PageManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_invalid_page_size() {
        let (dir, path) = scratch("pm-psize");
        let _ = PageManager::create(&path).expect("create");
        // Page-size field lives at bytes 8..16 of the header.
        let mut bad = [0_u8; 8];
        bad.copy_from_slice(&(4096_u64).to_le_bytes());
        write_at(&path, 8, &bad);
        assert!(PageManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_header_checksum_mismatch() {
        let (dir, path) = scratch("pm-cksum");
        let _ = PageManager::create(&path).expect("create");
        write_at(&path, 30, &[0xAB]);
        assert!(PageManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_truncated_file() {
        let (dir, path) = scratch("pm-trunc");
        let _ = PageManager::create(&path).expect("create");
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open")
            .set_len(100)
            .expect("truncate");
        assert!(PageManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_misaligned_trailing_bytes() {
        let (dir, path) = scratch("pm-align");
        let mut manager = PageManager::create(&path).expect("create");
        let page = manager.allocate_page(PageType::Leaf).expect("alloc");
        manager.write_page(&page).expect("write");
        drop(manager);
        // Append one stray byte so file is no longer page aligned.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        use std::io::Write as _;
        file.write_all(&[7]).expect("stray");
        drop(file);
        assert!(PageManager::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
