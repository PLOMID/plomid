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
//! Single-threaded buffer pool for checksum-protected real-file pages.
//!
//! The v1 pool uses an LRU eviction policy. All public operations require
//! `&mut self`, so callers must serialize access to a pool. Dirty unpinned
//! pages are written through [`RealFs`](crate::RealFs) before eviction; the
//! pool's `sync` method writes all dirty pages and fsyncs the data file.

use crate::{FileSystem, Page, RealFs, PAGE_SIZE};
use plomid_core::{ErrorKind, PageId, PlomidError, Result};
use std::{collections::HashMap, path::Path};

#[derive(Debug)]
struct Frame {
    page: Page,
    pin_count: usize,
    dirty: bool,
    last_used: u64,
    /// Permanently reserved: structural pages (internal B-tree nodes and the
    /// root-metadata page) are never evicted. A flushed parent referencing an
    /// unflushed child is a dangling reference that neither WAL replay nor a
    /// fresh mount can traverse, so internal structure only reaches the file
    /// via `sync`, when the whole image is consistent. Leaves evict freely:
    /// leaf content is self-consistent wherever it lands. The reservation is
    /// bounded by internal-node count (logarithmic in tree size) and is
    /// released only when the page is reallocated for another purpose.
    pinned_structural: bool,
}

/// An owned pinned page returned by [`BufferPool::get_page`] or
/// [`BufferPool::allocate_page`].
#[derive(Clone, Debug)]
pub struct PageHandle {
    page: Page,
}

impl PageHandle {
    /// Returns the ID of the pinned page.
    #[must_use]
    pub fn id(&self) -> PageId {
        self.page.id()
    }

    /// Returns the page payload for reading.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        self.page.data()
    }

    /// Returns the page payload for modification.
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.page.data_mut()
    }
}

/// A fixed-capacity page cache over one real filesystem file.
///
/// The default type parameter makes the product path ergonomic while retaining
/// a generic SPI for future production filesystem implementations.
pub struct BufferPool<F: FileSystem = RealFs> {
    fs: F,
    file: F::File,
    capacity: usize,
    frames: HashMap<PageId, Frame>,
    next_page_id: u64,
    clock: u64,
}

impl BufferPool<RealFs> {
    /// Creates a truncated real page file and an empty buffer pool.
    pub fn create(path: &Path, capacity: usize) -> Result<Self> {
        Self::create_with(RealFs, path, capacity)
    }

    /// Opens an existing real page file and its buffer pool.
    pub fn open(path: &Path, capacity: usize) -> Result<Self> {
        Self::open_with(RealFs, path, capacity)
    }
}

impl<F: FileSystem> BufferPool<F> {
    /// Creates a page file using the supplied filesystem implementation.
    pub fn create_with(fs: F, path: &Path, capacity: usize) -> Result<Self> {
        let file = fs.create(path)?;
        Self::from_file(fs, file, capacity)
    }

    /// Opens a page file using the supplied filesystem implementation.
    pub fn open_with(fs: F, path: &Path, capacity: usize) -> Result<Self> {
        let file = fs.open(path)?;
        Self::from_file(fs, file, capacity)
    }

    pub(crate) fn from_file(fs: F, file: F::File, capacity: usize) -> Result<Self> {
        if capacity == 0 {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "buffer pool capacity must be greater than zero",
            ));
        }
        let length = fs.len(&file)?;
        if length % PAGE_SIZE as u64 != 0 {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "page file length is not aligned to page size",
            ));
        }
        Ok(Self {
            fs,
            file,
            capacity,
            frames: HashMap::new(),
            next_page_id: length / PAGE_SIZE as u64,
            clock: 0,
        })
    }

    /// Allocates a new page ID and returns its pinned, dirty page handle.
    pub fn allocate_page(&mut self) -> Result<PageHandle> {
        self.ensure_space()?;
        let page_id = PageId::new(self.next_page_id);
        self.next_page_id = self
            .next_page_id
            .checked_add(1)
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "page ID counter exhausted"))?;
        self.clock = self.clock.wrapping_add(1);
        let page = Page::new(page_id);
        // One owned page is constructed; the frame keeps the clone and the
        // handle takes the original, so allocation pays a single 16 KiB
        // initialization instead of two.
        self.frames.insert(
            page_id,
            Frame {
                page: page.clone(),
                pin_count: 1,
                pinned_structural: false,
                dirty: true,
                last_used: self.clock,
            },
        );
        Ok(PageHandle { page })
    }

    /// Pins a page, loading and checksum-verifying it from the real file on a miss.
    pub fn get_page(&mut self, page_id: PageId) -> Result<PageHandle> {
        // Single lookup: the previous `contains_key` + `get_mut` pair hashed
        // the same key twice on every warm-cache hit.
        if let Some(frame) = self.frames.get_mut(&page_id) {
            self.clock = self.clock.wrapping_add(1);
            frame.pin_count = frame.pin_count.saturating_add(1);
            frame.last_used = self.clock;
            return Ok(PageHandle {
                page: frame.page.clone(),
            });
        }
        self.ensure_space()?;
        let page = self.read_page(page_id)?;
        self.clock = self.clock.wrapping_add(1);
        self.frames.insert(
            page_id,
            Frame {
                page: page.clone(),
                pin_count: 1,
                pinned_structural: false,
                dirty: false,
                last_used: self.clock,
            },
        );
        Ok(PageHandle { page })
    }

    /// Makes `page_id` resident without copying it, refreshing its LRU stamp.
    ///
    /// [`Self::get_page`] returns an owned 16 KiB [`Page`] copy, and
    /// [`Self::unpin_page`] moves it back. A B+Tree descent touches several
    /// pages per key, so read-only levels paid a full 16 KiB clone plus an
    /// equal-sized move for nothing. Callers that decode a node and then
    /// finish with the page use this instead and address the page by ID.
    pub fn ensure_resident(&mut self, page_id: PageId) -> Result<()> {
        if self.frames.contains_key(&page_id) {
            self.clock = self.clock.wrapping_add(1);
            let clock = self.clock;
            if let Some(frame) = self.frames.get_mut(&page_id) {
                frame.last_used = clock;
            }
            return Ok(());
        }
        self.ensure_space()?;
        let page = self.read_page(page_id)?;
        self.clock = self.clock.wrapping_add(1);
        self.frames.insert(
            page_id,
            Frame {
                page,
                pin_count: 0,
                pinned_structural: false,
                dirty: false,
                last_used: self.clock,
            },
        );
        Ok(())
    }

    /// Borrows a resident page's payload without copying the page.
    ///
    /// The returned slice borrows the pool, so the page cannot be evicted
    /// while it is held.
    pub fn data(&mut self, page_id: PageId) -> Result<&[u8]> {
        self.ensure_resident(page_id)?;
        Ok(self
            .frames
            .get(&page_id)
            .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "page is not resident"))?
            .page
            .data())
    }

    /// Borrows a resident page's payload mutably and marks the page dirty.
    ///
    /// This is the write-side counterpart of [`Self::data`]: a node write
    /// encodes straight into the resident frame instead of cloning the page
    /// into a handle and moving it back on unpin.
    pub fn data_mut(&mut self, page_id: PageId) -> Result<&mut [u8]> {
        self.ensure_resident(page_id)?;
        let frame = self
            .frames
            .get_mut(&page_id)
            .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "page is not resident"))?;
        frame.dirty = true;
        Ok(frame.page.data_mut())
    }

    /// Allocates a page and returns only its ID, avoiding the owned handle.
    ///
    /// The frame is installed with a zero pin count because no [`PageHandle`]
    /// is outstanding; callers address the page through [`Self::data_mut`].
    /// Returning a handle that is immediately dropped would otherwise leave
    /// the frame permanently pinned and eventually exhaust the pool.
    pub fn allocate_page_id(&mut self) -> Result<PageId> {
        self.ensure_space()?;
        let page_id = PageId::new(self.next_page_id);
        self.next_page_id = self
            .next_page_id
            .checked_add(1)
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "page ID counter exhausted"))?;
        self.clock = self.clock.wrapping_add(1);
        self.frames.insert(
            page_id,
            Frame {
                page: Page::new(page_id),
                pin_count: 0,
                pinned_structural: false,
                dirty: true,
                last_used: self.clock,
            },
        );
        Ok(page_id)
    }

    /// Unpins a page handle, optionally replacing the cached page with changes.
    pub fn unpin_page(&mut self, handle: PageHandle, dirty: bool) -> Result<()> {
        let page_id = handle.id();
        let frame = self
            .frames
            .get_mut(&page_id)
            .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "page is not resident"))?;
        if frame.pin_count == 0 {
            return Err(PlomidError::new(
                ErrorKind::Internal,
                "page pin count underflow",
            ));
        }
        frame.pin_count -= 1;
        if dirty {
            frame.page = handle.page;
            frame.dirty = true;
        }
        Ok(())
    }

    /// Marks a resident page dirty without changing its pin count.
    pub fn mark_dirty(&mut self, page_id: PageId) -> Result<()> {
        let frame = self
            .frames
            .get_mut(&page_id)
            .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "page is not resident"))?;
        frame.dirty = true;
        Ok(())
    }

    /// Permanently reserves a resident structural page against eviction.
    ///
    /// Internal B-tree nodes and the root-metadata page must never reach the
    /// file ahead of the children they reference: a flushed parent pointing
    /// at never-written pages is a dangling reference that WAL replay cannot
    /// traverse. Structural pages therefore leave the file only via `sync`,
    /// which writes a consistent image. Reservation is idempotent and bounded
    /// by internal-node count.
    pub fn pin_structural(&mut self, page_id: PageId) -> Result<()> {
        let frame = self
            .frames
            .get_mut(&page_id)
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "page is not resident"))?;
        frame.pinned_structural = true;
        Ok(())
    }

    /// Flushes one resident dirty page to the real file.
    pub fn flush_page(&mut self, page_id: PageId) -> Result<()> {
        let offset = page_id
            .get()
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| PlomidError::new(ErrorKind::InvalidArgument, "page offset overflow"))?;
        // Disjoint field borrows: refreshing the resident frame in place and
        // writing it needs `frames` mutably while `fs`/`file` are borrowed
        // mutably/immutably; destructuring keeps the borrow checker satisfied
        // without an intermediate 16 KiB copy.
        let Self {
            fs, file, frames, ..
        } = self;
        let frame = frames
            .get_mut(&page_id)
            .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "page is not resident"))?;
        if frame.dirty {
            // Refresh checksums in place: the previous path cloned the page
            // (`to_bytes` = one 16 KiB copy), refreshed the clone, then copied
            // the result into the write call. Here the resident frame itself is
            // refreshed, so the write borrows it directly.
            let bytes = frame.page.refresh_for_write();
            let mut written = 0;
            while written < bytes.len() {
                let count = fs.write_at(file, offset + written as u64, &bytes[written..])?;
                if count == 0 {
                    return Err(PlomidError::new(
                        ErrorKind::Io,
                        "filesystem accepted no page bytes",
                    ));
                }
                written += count;
            }
            frame.dirty = false;
        }
        Ok(())
    }

    /// Flushes every dirty page and fsyncs the underlying real file.
    pub fn sync(&mut self) -> Result<()> {
        let dirty_ids: Vec<PageId> = self
            .frames
            .iter()
            .filter_map(|(id, frame)| frame.dirty.then_some(*id))
            .collect();
        for page_id in dirty_ids {
            self.flush_page(page_id)?;
        }
        self.fs.fsync(&mut self.file)
    }

    /// Bytes held dirty in memory and not yet reflected in the file size.
    ///
    /// Rotation decisions read the filesystem size, which lags buffered
    /// writes by up to the pool's dirty set; without this term a batched fill
    /// can grow a segment many poolfuls past its limit before any `stat`
    /// observes it. The estimate over-counts rewritten pages (their old image
    /// is already in the file), which errs toward slightly early rotation — a
    /// safe direction for a soft threshold.
    pub fn dirty_bytes(&self) -> u64 {
        self.frames.values().filter(|frame| frame.dirty).count() as u64 * PAGE_SIZE as u64
    }

    fn ensure_space(&mut self) -> Result<()> {
        if self.frames.len() < self.capacity {
            return Ok(());
        }
        let candidate = self
            .frames
            .iter()
            // Structural pages (internal nodes, root metadata) are never
            // eviction victims: flushing one ahead of its children would
            // leave a dangling reference in the file image. Leaves evict
            // freely; their content is self-consistent wherever it lands.
            .filter(|(_, frame)| frame.pin_count == 0 && !frame.pinned_structural)
            .min_by_key(|(_, frame)| frame.last_used)
            .map(|(id, _)| *id)
            .ok_or_else(|| {
                PlomidError::new(ErrorKind::Conflict, "all buffer pool pages are pinned")
            })?;
        self.flush_page(candidate)?;
        self.frames.remove(&candidate);
        Ok(())
    }

    fn read_page(&mut self, page_id: PageId) -> Result<Page> {
        let offset = page_id
            .get()
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| PlomidError::new(ErrorKind::InvalidArgument, "page offset overflow"))?;
        let mut bytes = [0_u8; PAGE_SIZE];
        let mut read = 0;
        while read < PAGE_SIZE {
            let count =
                self.fs
                    .read_at(&mut self.file, offset + read as u64, &mut bytes[read..])?;
            if count == 0 {
                return Err(PlomidError::new(ErrorKind::Corruption, "page is truncated"));
            }
            read += count;
        }
        let page = Page::from_bytes(&bytes)?;
        if page.id() != page_id {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "page ID does not match offset",
            ));
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::BufferPool;
    use crate::PAGE_SIZE;
    use plomid_core::{ErrorKind, PageId};
    use std::{
        fs::{self, OpenOptions},
        io::{Seek, SeekFrom, Write},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn temp_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "plomid-buffer-pool-{label}-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn dirty_page_flushes_and_is_read_by_a_new_pool() {
        let path = temp_path("round-trip");
        let result = (|| {
            let mut pool = BufferPool::create(&path, 2)?;
            let mut handle = pool.allocate_page()?;
            handle.data_mut()[..5].copy_from_slice(b"hello");
            pool.unpin_page(handle, true)?;
            pool.sync()?;
            drop(pool);

            let mut reopened = BufferPool::open(&path, 2)?;
            let handle = reopened.get_page(PageId::new(0))?;
            assert_eq!(&handle.data()[..5], b"hello");
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "page round trip failed: {result:?}");
    }

    #[test]
    fn pinned_page_prevents_eviction() {
        let path = temp_path("pinned");
        let result = (|| {
            let mut pool = BufferPool::create(&path, 1)?;
            let handle = pool.allocate_page()?;
            let error = pool
                .allocate_page()
                .expect_err("pinned frame must block allocation");
            assert_eq!(error.kind(), ErrorKind::Conflict);
            pool.unpin_page(handle, false)?;
            let second = pool.allocate_page()?;
            assert_eq!(second.id(), PageId::new(1));
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "pin/eviction test failed: {result:?}");
    }

    #[test]
    fn corrupted_page_is_rejected_on_read() {
        let path = temp_path("corruption");
        let result = (|| {
            let mut pool = BufferPool::create(&path, 1)?;
            let handle = pool.allocate_page()?;
            pool.unpin_page(handle, false)?;
            pool.sync()?;
            drop(pool);

            let mut file = OpenOptions::new().write(true).open(&path)?;
            file.seek(SeekFrom::Start((PAGE_SIZE - 1) as u64))?;
            file.write_all(&[1])?;
            file.sync_all()?;
            drop(file);

            let mut reopened = BufferPool::open(&path, 1)?;
            let error = reopened
                .get_page(PageId::new(0))
                .expect_err("corrupted page must fail verification");
            assert_eq!(error.kind(), ErrorKind::Corruption);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "corruption test failed: {result:?}");
    }
}
