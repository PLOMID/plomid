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
//! Physical storage-device management with persistent extent allocation.
//!
//! A device is a single container file following the repository's `.dat`
//! conventions. File layout:
//!
//! ```text
//! offset 0          : device header (one 256 KiB block, `PLDV` magic)
//! offset 256 KiB    : allocator snapshot (N blocks, `PLEX` magic)
//! offset data_start : extent data area (M * 64 MiB, extent aligned)
//! ```
//!
//! The snapshot length scales with capacity so every allocated extent can be
//! persisted; `data_start` is stored in the header. Extent `start`/`length`
//! are byte offsets relative to the data area. Each region carries a single
//! CRC32C checksum over its defined fields; no nested checksums are used.
//!
//! # Logical capacity vs physical length
//!
//! `capacity` is a *logical* property of the device: it bounds the address
//! space the extent allocator may hand out and is recorded in the header. It
//! is deliberately independent of the container's physical length, because a
//! database that declares 512 GiB of address space must not require 512 GiB of
//! allocated storage, must not exceed filesystem file-size limits (FAT32 caps
//! a file at 4 GiB), and must not defeat sparse-unaware backup/container/rsync
//! tooling that would otherwise materialize every hole.
//!
//! Demand-driven growth follows from that split:
//!
//! * Creation sizes the container to its **metadata footprint**
//!   (`data_start` = header + snapshot), which is all the durable metadata a
//!   device needs.
//! * Metadata writes always fit inside that footprint, so they never grow the
//!   file: the snapshot is sized for the *maximum* extent count of the logical
//!   capacity, not for the extents currently allocated.
//! * The data area grows only when bytes are actually placed in it, through
//!   [`StorageDevice::ensure_data_capacity`], which extends the file up to the
//!   requested data-area offset and never truncates it.
//! * [`StorageDevice::open`] validates the metadata footprint and the logical
//!   bound, not the physical length: a demand-grown container and a container
//!   whose data area is still unwritten are both valid.
//!
//! Nothing about addressing changes: extent offsets, page/block placement,
//! checksums and the allocator snapshot format are identical to the pre-sized
//! layout, so containers written by the pre-sized layout open unchanged.
//!
//! [`StorageDevice`] serializes all operations through an internal mutex so a
//! single handle can be shared across threads. Allocation and release persist
//! the allocator snapshot and sync the file before reporting success, so a
//! persistence failure rolls back the in-memory mutation instead of leaving
//! the allocator ahead of durable state.

use crate::physical::{EXTENT_SIZE, PAGE_SIZE};
use crate::platform::FileExt;
use crate::{compute_checksum, verify_checksum};
use plomid_core::{DeviceId, ErrorKind, PlomidError, Result};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// Device constants are defined once in `plomid_core::constants` and
/// re-exported here; HDR_CHECKSUMMED_END/HDR_CHECKSUM_END keep module names.
pub use plomid_core::{
    ALLOCATOR_FORMAT_VERSION, ALLOCATOR_MAGIC, DEVICE_FORMAT_VERSION,
    DEVICE_HDR_CHECKSUMMED_END as HDR_CHECKSUMMED_END, DEVICE_HDR_CHECKSUM_END as HDR_CHECKSUM_END,
    DEVICE_HEADER_LEN, DEVICE_MAGIC, MAX_DEVICE_CAPACITY, MIN_DEVICE_CAPACITY, SNAPSHOT_ENTRY_LEN,
    SNAPSHOT_PAGE_LEN, SNAPSHOT_PREFIX_LEN,
};

/// Lifecycle state of a storage device.
///
/// The header records the state at the time it was persisted; transitions
/// are checked with [`Lifecycle::can_enter`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(u32)]
pub enum Lifecycle {
    Discovered = 0,
    Initializing = 1,
    Online = 2,
    Quiescing = 3,
    Offline = 4,
    Failed = 5,
    Recovering = 6,
}

impl Lifecycle {
    pub(crate) fn from_u32(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::Discovered),
            1 => Ok(Self::Initializing),
            2 => Ok(Self::Online),
            3 => Ok(Self::Quiescing),
            4 => Ok(Self::Offline),
            5 => Ok(Self::Failed),
            6 => Ok(Self::Recovering),
            other => Err(PlomidError::new(
                ErrorKind::Corruption,
                format!("invalid device lifecycle state {other}"),
            )),
        }
    }

    /// Returns true when a transition from `self` to `next` is permitted.
    #[must_use]
    pub fn can_enter(self, next: Self) -> bool {
        match (self, next) {
            (current, target) if current == target => true,
            (Self::Discovered, Self::Initializing) => true,
            (Self::Initializing, Self::Online | Self::Failed) => true,
            (Self::Online, Self::Quiescing | Self::Offline | Self::Failed) => true,
            (Self::Quiescing, Self::Offline | Self::Failed | Self::Online) => true,
            (Self::Offline, Self::Online | Self::Failed) => true,
            (Self::Failed, Self::Recovering | Self::Offline) => true,
            (Self::Recovering, Self::Online | Self::Offline | Self::Failed) => true,
            _ => false,
        }
    }
}

/// Durable initialization marker stored in the device header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
enum InitState {
    Incomplete = 0,
    Complete = 1,
}

impl InitState {
    fn from_u32(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::Incomplete),
            1 => Ok(Self::Complete),
            other => Err(PlomidError::new(
                ErrorKind::Corruption,
                format!("invalid device initialization state {other}"),
            )),
        }
    }
}

/// A contiguous physical extent.
///
/// `start` and `length` are byte offsets relative to the device data area
/// (file offset `data_start + start`). Neither field encodes device identity
/// or filesystem location; `extent_id` is a logical allocation sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Extent {
    /// Byte offset relative to the data area; multiple of 64 MiB.
    pub start: u64,
    /// Length in bytes; positive multiple of 64 MiB.
    pub length: u64,
    /// Logical allocation sequence number.
    pub extent_id: u64,
}

impl Extent {
    /// One-past-the-end offset relative to the data area, if representable.
    #[must_use]
    pub fn end(self) -> Option<u64> {
        self.start.checked_add(self.length)
    }

    fn validate(self, allocatable: u64) -> Result<()> {
        if self.length == 0 {
            return Err(invalid("extent has zero length"));
        }
        if self.length % EXTENT_SIZE != 0 {
            return Err(invalid("extent length is not a multiple of 64 MiB"));
        }
        if self.start % EXTENT_SIZE != 0 {
            return Err(invalid("extent start is not 64 MiB aligned"));
        }
        if self.extent_id == 0 {
            return Err(invalid("extent has invalid zero extent ID"));
        }
        let end = self
            .start
            .checked_add(self.length)
            .ok_or_else(|| invalid("extent end overflows"))?;
        if end > allocatable {
            return Err(invalid("extent is outside device capacity"));
        }
        Ok(())
    }
}

/// Persistent device metadata decoded from the header region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceMetadata {
    /// Logical device identity.
    pub device_id: DeviceId,
    /// Logical capacity (address space) in bytes, independent of the
    /// container's physical length.
    pub capacity: u64,
    /// Allocatable data-area bytes (`capacity - data_start`).
    pub allocatable_bytes: u64,
    /// File offset where the data area starts.
    pub data_start: u64,
    /// Length of the snapshot region in bytes.
    pub snapshot_len: u64,
    /// Lifecycle state recorded when the header was persisted.
    pub lifecycle: Lifecycle,
    /// Whether initialization completed durably.
    pub initialized: bool,
}

/// Number of snapshot pages (and bytes) needed to persist `extents` entries.
fn snapshot_len_for_extents(extents: u64) -> Result<u64> {
    let entries = extents
        .checked_mul(SNAPSHOT_ENTRY_LEN)
        .ok_or_else(|| PlomidError::new(ErrorKind::InvalidArgument, "extent capacity overflows"))?;
    let needed = SNAPSHOT_PREFIX_LEN
        .checked_add(entries)
        .ok_or_else(|| PlomidError::new(ErrorKind::InvalidArgument, "extent capacity overflows"))?;
    let checksum = needed
        .checked_add(4)
        .ok_or_else(|| PlomidError::new(ErrorKind::InvalidArgument, "extent capacity overflows"))?;
    let pages = checksum
        .checked_add(SNAPSHOT_PAGE_LEN - 1)
        .ok_or_else(|| PlomidError::new(ErrorKind::InvalidArgument, "extent capacity overflows"))?
        / SNAPSHOT_PAGE_LEN;
    pages
        .checked_mul(SNAPSHOT_PAGE_LEN)
        .ok_or_else(|| PlomidError::new(ErrorKind::InvalidArgument, "extent capacity overflows"))
}

/// Resolves the metadata layout for a capacity: snapshot length, data start,
/// and allocatable bytes. Solves the fixpoint where more extents may need a
/// larger snapshot, which in turn reduces the allocatable area.
fn layout_for_capacity(capacity: u64) -> Result<(u64, u64, u64)> {
    validate_capacity_shape(capacity)?;
    // At most two iterations converge: the snapshot only grows when the
    // extent count crosses a page boundary.
    let mut snapshot_len = SNAPSHOT_PAGE_LEN;
    for _ in 0..8 {
        let data_start = DEVICE_HEADER_LEN
            .checked_add(snapshot_len)
            .ok_or_else(|| invalid("device capacity overflows"))?;
        if data_start >= capacity {
            return Err(invalid(
                "device capacity is smaller than required metadata plus one extent",
            ));
        }
        let allocatable = capacity - data_start;
        if allocatable % EXTENT_SIZE != 0 {
            return Err(invalid("device capacity is not extent aligned"));
        }
        let extents = allocatable / EXTENT_SIZE;
        if extents == 0 {
            return Err(invalid(
                "device capacity is smaller than minimum allocatable extent",
            ));
        }
        let needed = snapshot_len_for_extents(extents)?;
        if needed <= snapshot_len {
            return Ok((snapshot_len, data_start, allocatable));
        }
        snapshot_len = needed;
    }
    Err(invalid("device capacity overflows"))
}

/// Validates capacity shape without solving the snapshot fixpoint.
fn validate_capacity_shape(capacity: u64) -> Result<()> {
    if capacity == 0 {
        return Err(invalid("device capacity is zero"));
    }
    if capacity > MAX_DEVICE_CAPACITY {
        return Err(invalid("device capacity exceeds representable range"));
    }
    if capacity < MIN_DEVICE_CAPACITY {
        return Err(invalid(
            "device capacity is smaller than required metadata plus one extent",
        ));
    }
    if capacity % PAGE_SIZE != 0 {
        return Err(invalid("device capacity is not page aligned"));
    }
    Ok(())
}

/// Validates a device capacity and returns allocatable data-area bytes.
pub fn validate_capacity(capacity: u64) -> Result<u64> {
    let (_, _, allocatable) = layout_for_capacity(capacity)?;
    Ok(allocatable)
}

/// Returns the device capacity required for `extents` 64 MiB extents.
pub fn capacity_for_extents(extents: u64) -> Result<u64> {
    if extents == 0 {
        return Err(invalid("extent count is zero"));
    }
    let snapshot_len = snapshot_len_for_extents(extents)?;
    let data_start = DEVICE_HEADER_LEN
        .checked_add(snapshot_len)
        .ok_or_else(|| invalid("device capacity overflows"))?;
    let data = extents
        .checked_mul(EXTENT_SIZE)
        .ok_or_else(|| invalid("extent capacity overflows"))?;
    let capacity = data_start
        .checked_add(data)
        .ok_or_else(|| invalid("device capacity overflows"))?;
    validate_capacity(capacity)?;
    Ok(capacity)
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
        .map_err(|_| corrupt("invalid device field"))
}

fn le_u64(bytes: &[u8]) -> Result<u64> {
    bytes
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| corrupt("invalid device field"))
}

/// Best-fit free-space allocator over extent-aligned ranges.
///
/// Free space is indexed by start offset. Allocation picks the smallest free
/// range that satisfies the request (ties resolve to the lowest start for
/// deterministic behavior). Release coalesces with adjacent neighbors.
#[derive(Clone, Debug)]
struct ExtentAllocator {
    allocatable: u64,
    free: BTreeMap<u64, u64>,
    allocated: HashMap<u64, Extent>,
    next_extent_id: u64,
}

impl ExtentAllocator {
    fn new_empty(allocatable: u64) -> Self {
        let mut free = BTreeMap::new();
        free.insert(0, allocatable);
        Self {
            allocatable,
            free,
            allocated: HashMap::new(),
            next_extent_id: 1,
        }
    }

    fn allocate(&mut self, length: u64) -> Result<Extent> {
        if length == 0 {
            return Err(invalid("allocation length is zero"));
        }
        if length % EXTENT_SIZE != 0 {
            return Err(invalid("allocation length is not a multiple of 64 MiB"));
        }
        if length > self.allocatable {
            return Err(invalid("allocation length exceeds device capacity"));
        }
        let mut best: Option<(u64, u64)> = None;
        for (start, len) in &self.free {
            let better = match best {
                None => true,
                Some((best_start, best_len)) => {
                    *len < best_len || (*len == best_len && *start < best_start)
                }
            };
            if *len >= length && better {
                best = Some((*start, *len));
            }
        }
        let (start, len) = best
            .ok_or_else(|| PlomidError::new(ErrorKind::Conflict, "device allocation exhausted"))?;
        if self.next_extent_id == u64::MAX {
            return Err(PlomidError::new(
                ErrorKind::Internal,
                "extent ID space exhausted",
            ));
        }
        self.free.remove(&start);
        let remainder = len - length;
        if remainder > 0 {
            let next = start
                .checked_add(length)
                .ok_or_else(|| invalid("extent end overflows"))?;
            self.free.insert(next, remainder);
        }
        let extent = Extent {
            start,
            length,
            extent_id: self.next_extent_id,
        };
        self.next_extent_id += 1;
        extent.validate(self.allocatable)?;
        if self.allocated.insert(start, extent).is_some() {
            return Err(PlomidError::new(
                ErrorKind::Internal,
                "overlapping extent allocation detected",
            ));
        }
        Ok(extent)
    }

    fn release(&mut self, start: u64) -> Result<Extent> {
        let extent = self
            .allocated
            .remove(&start)
            .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "extent is not allocated"))?;
        extent.validate(self.allocatable)?;
        let mut new_start = extent.start;
        let mut new_len = extent.length;
        if let Some((&prev_start, &prev_len)) = self.free.range(..new_start).next_back() {
            let prev_end = prev_start
                .checked_add(prev_len)
                .ok_or_else(|| corrupt("free-space range overflows"))?;
            if prev_end == new_start {
                new_start = prev_start;
                new_len = new_len
                    .checked_add(prev_len)
                    .ok_or_else(|| corrupt("free-space range overflows"))?;
                self.free.remove(&prev_start);
            } else if prev_end > new_start {
                return Err(corrupt("free and allocated extents overlap"));
            }
        }
        let end = new_start
            .checked_add(new_len)
            .ok_or_else(|| corrupt("free-space range overflows"))?;
        if let Some(&next_len) = self.free.get(&end) {
            new_len = new_len
                .checked_add(next_len)
                .ok_or_else(|| corrupt("free-space range overflows"))?;
            self.free.remove(&end);
        }
        self.free.insert(new_start, new_len);
        Ok(extent)
    }

    fn validate_all(&self) -> Result<()> {
        let mut allocated: Vec<Extent> = self.allocated.values().copied().collect();
        allocated.sort_by_key(|extent| extent.start);
        let mut cursor = 0_u64;
        for extent in &allocated {
            extent.validate(self.allocatable)?;
            if extent.start < cursor {
                return Err(corrupt("allocated extents overlap"));
            }
            cursor = extent
                .end()
                .ok_or_else(|| corrupt("allocated extent overflows"))?;
        }
        let mut free_cursor = 0_u64;
        let mut accounted_free = 0_u64;
        for (start, len) in &self.free {
            if *len == 0 || *len % EXTENT_SIZE != 0 {
                return Err(corrupt("free extent has invalid length"));
            }
            if *start % EXTENT_SIZE != 0 {
                return Err(corrupt("free extent is misaligned"));
            }
            let end = start
                .checked_add(*len)
                .ok_or_else(|| corrupt("free extent overflows"))?;
            if end > self.allocatable {
                return Err(corrupt("free extent is outside device capacity"));
            }
            if *start < free_cursor {
                return Err(corrupt("free extents overlap"));
            }
            free_cursor = end;
            accounted_free = accounted_free
                .checked_add(*len)
                .ok_or_else(|| corrupt("free-space accounting overflows"))?;
        }
        for (start, len) in &self.free {
            let end = start + len;
            for extent in &allocated {
                let extent_end = extent
                    .end()
                    .ok_or_else(|| corrupt("allocated extent overflows"))?;
                if *start < extent_end && extent.start < end {
                    return Err(corrupt("free and allocated extents overlap"));
                }
            }
        }
        let allocated_bytes: u64 = allocated.iter().map(|extent| extent.length).sum();
        let total = allocated_bytes
            .checked_add(accounted_free)
            .ok_or_else(|| corrupt("extent accounting overflows"))?;
        if total != self.allocatable {
            return Err(corrupt(
                "free/allocated accounting does not match device capacity",
            ));
        }
        Ok(())
    }

    fn snapshot(&self) -> Vec<Extent> {
        let mut extents: Vec<Extent> = self.allocated.values().copied().collect();
        extents.sort_by_key(|extent| extent.start);
        extents
    }

    fn restore(&mut self, extents: &[Extent], next_extent_id: u64) -> Result<()> {
        if next_extent_id == 0 {
            return Err(corrupt("allocator snapshot has invalid next extent ID"));
        }
        let mut allocated = HashMap::new();
        let mut sorted = extents.to_vec();
        sorted.sort_by_key(|extent| extent.start);
        let mut cursor = 0_u64;
        let mut seen: Vec<u64> = Vec::with_capacity(sorted.len());
        for extent in &sorted {
            extent.validate(self.allocatable)?;
            if extent.start < cursor {
                return Err(corrupt("allocated extents overlap"));
            }
            if seen.contains(&extent.extent_id) {
                return Err(corrupt("duplicate extent in allocator snapshot"));
            }
            if extent.extent_id == 0 || extent.extent_id >= next_extent_id {
                return Err(corrupt("allocator snapshot has invalid next extent ID"));
            }
            seen.push(extent.extent_id);
            cursor = extent
                .end()
                .ok_or_else(|| corrupt("allocated extent overflows"))?;
            if allocated.insert(extent.start, *extent).is_some() {
                return Err(corrupt("duplicate extent in allocator snapshot"));
            }
        }
        let mut free = BTreeMap::new();
        let mut free_start = 0_u64;
        for extent in &sorted {
            if extent.start > free_start {
                free.insert(free_start, extent.start - free_start);
            }
            free_start = extent
                .end()
                .ok_or_else(|| corrupt("allocated extent overflows"))?;
        }
        if free_start < self.allocatable {
            free.insert(free_start, self.allocatable - free_start);
        }
        self.allocated = allocated;
        self.free = free;
        self.next_extent_id = next_extent_id;
        self.validate_all()?;
        Ok(())
    }

    fn encode_snapshot(
        &self,
        device_id: DeviceId,
        capacity: u64,
        snapshot_len: u64,
    ) -> Result<Vec<u8>> {
        let extents = self.snapshot();
        let needed = SNAPSHOT_PREFIX_LEN
            .checked_add(
                (extents.len() as u64)
                    .checked_mul(SNAPSHOT_ENTRY_LEN)
                    .ok_or_else(|| {
                        PlomidError::new(ErrorKind::Internal, "allocator snapshot overflows")
                    })?,
            )
            .and_then(|v| v.checked_add(4))
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "allocator snapshot overflows"))?;
        if needed > snapshot_len {
            return Err(PlomidError::new(
                ErrorKind::Internal,
                "allocator snapshot exceeds snapshot capacity",
            ));
        }
        let mut buffer = vec![0_u8; snapshot_len as usize];
        buffer[0..4].copy_from_slice(&ALLOCATOR_MAGIC);
        buffer[4..8].copy_from_slice(&ALLOCATOR_FORMAT_VERSION.to_le_bytes());
        buffer[8..16].copy_from_slice(&device_id.to_le_bytes());
        buffer[16..24].copy_from_slice(&capacity.to_le_bytes());
        buffer[24..32].copy_from_slice(&(extents.len() as u64).to_le_bytes());
        buffer[32..40].copy_from_slice(&self.next_extent_id.to_le_bytes());
        let mut offset = SNAPSHOT_PREFIX_LEN as usize;
        for extent in &extents {
            buffer[offset..offset + 8].copy_from_slice(&extent.start.to_le_bytes());
            buffer[offset + 8..offset + 16].copy_from_slice(&extent.length.to_le_bytes());
            buffer[offset + 16..offset + 24].copy_from_slice(&extent.extent_id.to_le_bytes());
            offset += SNAPSHOT_ENTRY_LEN as usize;
        }
        let checksum = compute_checksum(&buffer[..offset]);
        buffer[offset..offset + 4].copy_from_slice(&checksum.to_le_bytes());
        Ok(buffer)
    }

    fn decode_snapshot(
        bytes: &[u8],
        device_id: DeviceId,
        capacity: u64,
        allocatable: u64,
    ) -> Result<Self> {
        if bytes.len() < SNAPSHOT_PREFIX_LEN as usize + 4 {
            return Err(corrupt("truncated allocator snapshot"));
        }
        if bytes[0..4] != ALLOCATOR_MAGIC {
            return Err(corrupt("invalid allocator magic"));
        }
        let version = le_u32(&bytes[4..8])?;
        if version != ALLOCATOR_FORMAT_VERSION {
            return Err(corrupt(format!(
                "unsupported allocator format version {version}"
            )));
        }
        let stored_id = le_u64(&bytes[8..16])?;
        if DeviceId::new(stored_id) != device_id {
            return Err(corrupt("allocator snapshot device identity mismatch"));
        }
        if le_u64(&bytes[16..24])? != capacity {
            return Err(corrupt("allocator snapshot capacity mismatch"));
        }
        let count = le_u64(&bytes[24..32])?;
        let next_extent_id = le_u64(&bytes[32..40])?;
        let count_usize: usize = count
            .try_into()
            .map_err(|_| corrupt("invalid allocator snapshot count"))?;
        let entries_len = count_usize
            .checked_mul(SNAPSHOT_ENTRY_LEN as usize)
            .ok_or_else(|| corrupt("invalid allocator snapshot count"))?;
        let entries_end = (SNAPSHOT_PREFIX_LEN as usize)
            .checked_add(entries_len)
            .ok_or_else(|| corrupt("invalid allocator snapshot count"))?;
        let checksum_end = entries_end
            .checked_add(4)
            .ok_or_else(|| corrupt("invalid allocator snapshot count"))?;
        if checksum_end > bytes.len() {
            return Err(corrupt("truncated allocator snapshot"));
        }
        let stored = le_u32(&bytes[entries_end..checksum_end])?;
        verify_checksum(&bytes[..entries_end], stored)?;
        if !bytes[checksum_end..].iter().all(|b| *b == 0) {
            return Err(corrupt("allocator snapshot has trailing malformed data"));
        }
        let mut extents = Vec::with_capacity(count_usize);
        for index in 0..count_usize {
            let base = SNAPSHOT_PREFIX_LEN as usize + index * SNAPSHOT_ENTRY_LEN as usize;
            let start = le_u64(&bytes[base..base + 8])?;
            let length = le_u64(&bytes[base + 8..base + 16])?;
            let extent_id = le_u64(&bytes[base + 16..base + 24])?;
            extents.push(Extent {
                start,
                length,
                extent_id,
            });
        }
        let mut allocator = ExtentAllocator {
            allocatable,
            free: BTreeMap::new(),
            allocated: HashMap::new(),
            next_extent_id: 1,
        };
        allocator.restore(&extents, next_extent_id)?;
        Ok(allocator)
    }
}

impl DeviceMetadata {
    fn encode(&self, init: InitState) -> [u8; DEVICE_HEADER_LEN as usize] {
        let mut header = [0_u8; DEVICE_HEADER_LEN as usize];
        header[0..4].copy_from_slice(&DEVICE_MAGIC);
        header[4..8].copy_from_slice(&DEVICE_FORMAT_VERSION.to_le_bytes());
        header[8..16].copy_from_slice(&self.device_id.to_le_bytes());
        header[16..24].copy_from_slice(&self.capacity.to_le_bytes());
        header[24..32].copy_from_slice(&self.allocatable_bytes.to_le_bytes());
        header[32..40].copy_from_slice(&self.data_start.to_le_bytes());
        header[40..44].copy_from_slice(&self.snapshot_len.to_le_bytes()[..4]);
        header[44..48].copy_from_slice(&(self.lifecycle as u32).to_le_bytes());
        header[48..52].copy_from_slice(&(init as u32).to_le_bytes());
        let checksum = compute_checksum(&header[..HDR_CHECKSUMMED_END]);
        header[HDR_CHECKSUMMED_END..HDR_CHECKSUM_END].copy_from_slice(&checksum.to_le_bytes());
        header
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < DEVICE_HEADER_LEN as usize {
            return Err(corrupt("truncated device metadata"));
        }
        if bytes.len() != DEVICE_HEADER_LEN as usize {
            return Err(corrupt("device metadata has trailing malformed data"));
        }
        if bytes[0..4] != DEVICE_MAGIC {
            return Err(corrupt("invalid device magic"));
        }
        let version = le_u32(&bytes[4..8])?;
        if version != DEVICE_FORMAT_VERSION {
            return Err(corrupt(format!(
                "unsupported device format version {version}"
            )));
        }
        let raw_id = le_u64(&bytes[8..16])?;
        if raw_id == 0 {
            return Err(corrupt("invalid device identity"));
        }
        let capacity = le_u64(&bytes[16..24])?;
        let allocatable = le_u64(&bytes[24..32])?;
        let data_start = le_u64(&bytes[32..40])?;
        let snapshot_len = u64::from(le_u32(&bytes[40..44])?);
        let lifecycle = Lifecycle::from_u32(le_u32(&bytes[44..48])?)?;
        let init = InitState::from_u32(le_u32(&bytes[48..52])?)?;
        if bytes[52..HDR_CHECKSUMMED_END].iter().any(|b| *b != 0) {
            return Err(corrupt("device metadata has trailing malformed data"));
        }
        let stored = le_u32(&bytes[HDR_CHECKSUMMED_END..HDR_CHECKSUM_END])?;
        verify_checksum(&bytes[..HDR_CHECKSUMMED_END], stored)?;
        if !bytes[HDR_CHECKSUM_END..].iter().all(|b| *b == 0) {
            return Err(corrupt("device metadata has trailing malformed data"));
        }
        let (expected_snapshot, expected_data_start, expected_allocatable) =
            layout_for_capacity(capacity).map_err(|_| corrupt("invalid device capacity"))?;
        if snapshot_len != expected_snapshot
            || data_start != expected_data_start
            || allocatable != expected_allocatable
        {
            return Err(corrupt("device capacity mismatch in device metadata"));
        }
        Ok(Self {
            device_id: DeviceId::new(raw_id),
            capacity,
            allocatable_bytes: allocatable,
            data_start,
            snapshot_len,
            lifecycle,
            initialized: init == InitState::Complete,
        })
    }
}

struct DeviceInner {
    file: File,
    device_id: DeviceId,
    capacity: u64,
    allocatable: u64,
    data_start: u64,
    snapshot_len: u64,
    /// Current physical length, cached so the growth check stays a comparison
    /// instead of a `stat` per metadata write.
    physical_len: u64,
    state: Lifecycle,
    allocator: ExtentAllocator,
}

/// Grows the container to `len` bytes when it is currently shorter.
///
/// Only ever extends: a container that already covers `len` is left byte-for-
/// byte alone, so an existing data area is never truncated or shuffled by a
/// metadata write. The length change is made durable by the caller's normal
/// `sync_all` durability boundary.
fn grow_to(inner: &mut DeviceInner, len: u64) -> Result<()> {
    if inner.physical_len >= len {
        return Ok(());
    }
    inner.file.set_len(len).map_err(PlomidError::from)?;
    inner.physical_len = len;
    Ok(())
}

/// File-backed storage device with a persistent extent allocator.
pub struct StorageDevice {
    inner: Mutex<DeviceInner>,
}

impl StorageDevice {
    /// Creates a device container with the given identity and logical capacity.
    ///
    /// The container is sized to its metadata footprint (`data_start`), not to
    /// `capacity`: the declared address space costs no physical bytes until
    /// data is actually placed in it. No existing file is reused — the create
    /// path truncates, so callers that must not lose data use [`Self::open`].
    pub fn create(path: &Path, device_id: DeviceId, capacity: u64) -> Result<Self> {
        if device_id.is_zero() {
            return Err(invalid("invalid device identity"));
        }
        let (snapshot_len, data_start, allocatable) = layout_for_capacity(capacity)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(PlomidError::from)?;
        file.set_len(data_start).map_err(PlomidError::from)?;
        let mut inner = DeviceInner {
            file,
            device_id,
            capacity,
            allocatable,
            data_start,
            snapshot_len,
            physical_len: data_start,
            state: Lifecycle::Initializing,
            allocator: ExtentAllocator::new_empty(allocatable),
        };
        persist_inner(&mut inner, InitState::Incomplete)?;
        inner.file.sync_all().map_err(PlomidError::from)?;
        inner.state = Lifecycle::Online;
        persist_inner(&mut inner, InitState::Complete)?;
        inner.file.sync_all().map_err(PlomidError::from)?;
        validate_inner(&inner)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Opens an existing device file and reconstructs allocator state.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(PlomidError::from)?;
        let len = file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(PlomidError::from)?;
        if len < DEVICE_HEADER_LEN {
            return Err(corrupt("missing device metadata"));
        }
        let mut header = [0_u8; DEVICE_HEADER_LEN as usize];
        file.read_exact_at(&mut header, 0)
            .map_err(|_| corrupt("truncated device metadata"))?;
        let metadata = DeviceMetadata::decode(&header)?;
        if !metadata.initialized {
            return Err(corrupt("device initialization did not complete"));
        }
        // Physical length is demand-driven, so it is validated as a range and
        // never used as the capacity authority (that comes from the header):
        // the metadata footprint must be fully present, and a container may
        // not claim more physical bytes than its own logical capacity.
        if len < DEVICE_HEADER_LEN + metadata.snapshot_len {
            return Err(corrupt("truncated device metadata"));
        }
        if len > metadata.capacity {
            return Err(corrupt("device file is larger than its logical capacity"));
        }
        let mut snapshot = vec![0_u8; metadata.snapshot_len as usize];
        file.read_exact_at(&mut snapshot, DEVICE_HEADER_LEN)
            .map_err(|_| corrupt("truncated allocator snapshot"))?;
        let allocator = ExtentAllocator::decode_snapshot(
            &snapshot,
            metadata.device_id,
            metadata.capacity,
            metadata.allocatable_bytes,
        )?;
        let state = match metadata.lifecycle {
            Lifecycle::Failed => Lifecycle::Recovering,
            Lifecycle::Initializing => {
                return Err(corrupt("device initialization did not complete"));
            }
            Lifecycle::Discovered => {
                return Err(corrupt("device initialization did not complete"));
            }
            _ => Lifecycle::Online,
        };
        let inner = DeviceInner {
            file,
            device_id: metadata.device_id,
            capacity: metadata.capacity,
            allocatable: metadata.allocatable_bytes,
            data_start: metadata.data_start,
            snapshot_len: metadata.snapshot_len,
            physical_len: len,
            state,
            allocator,
        };
        validate_inner(&inner)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Returns the logical device identity.
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        self.lock().device_id
    }

    /// Returns the logical device capacity (address space) in bytes.
    ///
    /// Independent of the container's physical length: the declared capacity
    /// is what the extent allocator may hand out, not what the file occupies.
    #[must_use]
    pub fn capacity(&self) -> u64 {
        self.lock().capacity
    }

    /// Returns the container's current physical length in bytes.
    ///
    /// This is the metadata footprint plus whatever data-area bytes have been
    /// placed so far; it stays below [`Self::capacity`] until the declared
    /// address space is actually used.
    #[must_use]
    pub fn physical_len(&self) -> u64 {
        self.lock().physical_len
    }

    /// Grows the container so the data area covers `end` bytes.
    ///
    /// `end` is an offset relative to the start of the data area (the same
    /// coordinate system as [`Extent::start`]), so a caller that has reserved
    /// an extent and is about to place bytes in it calls this with
    /// `extent.start + bytes`. Growth is forward-only, idempotent and
    /// extent-bounded: requesting more than the device's allocatable bytes is
    /// a controlled [`ErrorKind::InvalidArgument`] error rather than a silent
    /// over-commit. The extension is made durable by the caller's normal data
    /// durability boundary; it performs no fsync of its own.
    pub fn ensure_data_capacity(&self, end: u64) -> Result<()> {
        let mut inner = self.lock();
        require_online(&inner)?;
        if end > inner.allocatable {
            return Err(invalid(format!(
                "requested data area end {end} exceeds allocatable {}",
                inner.allocatable
            )));
        }
        let target = inner
            .data_start
            .checked_add(end)
            .ok_or_else(|| invalid("device data area end overflows"))?;
        if target > inner.capacity {
            return Err(invalid("device data area end exceeds capacity"));
        }
        grow_to(&mut inner, target)
    }

    /// Returns the allocatable data-area capacity in bytes.
    #[must_use]
    pub fn allocatable_bytes(&self) -> u64 {
        self.lock().allocatable
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub fn state(&self) -> Lifecycle {
        self.lock().state
    }

    /// Returns the file offset where the data area starts.
    #[must_use]
    pub fn data_start(&self) -> u64 {
        self.lock().data_start
    }

    /// Returns the number of allocated extents.
    #[must_use]
    pub fn allocated_extent_count(&self) -> usize {
        self.lock().allocator.allocated.len()
    }

    /// Returns allocated bytes across all extents.
    #[must_use]
    pub fn allocated_bytes(&self) -> u64 {
        self.lock()
            .allocator
            .allocated
            .values()
            .map(|extent| extent.length)
            .sum()
    }

    /// Returns free bytes across all free ranges.
    #[must_use]
    pub fn free_bytes(&self) -> u64 {
        self.lock().allocator.free.values().sum::<u64>()
    }

    /// Lists allocated extents sorted by start offset.
    #[must_use]
    pub fn allocated_extents(&self) -> Vec<Extent> {
        self.lock().allocator.snapshot()
    }

    /// Validates metadata, capacity agreement, and allocation invariants.
    pub fn validate(&self) -> Result<DeviceMetadata> {
        let inner = self.lock();
        validate_inner(&inner)?;
        Ok(DeviceMetadata {
            device_id: inner.device_id,
            capacity: inner.capacity,
            allocatable_bytes: inner.allocatable,
            data_start: inner.data_start,
            snapshot_len: inner.snapshot_len,
            lifecycle: inner.state,
            initialized: true,
        })
    }

    /// Requests a state transition, persisting the new state on success.
    pub fn transition(&self, next: Lifecycle) -> Result<()> {
        let mut inner = self.lock();
        if !inner.state.can_enter(next) {
            return Err(invalid(format!(
                "invalid device lifecycle transition from {:?} to {:?}",
                inner.state, next
            )));
        }
        inner.state = next;
        persist_inner(&mut inner, InitState::Complete)?;
        inner.file.sync_all().map_err(PlomidError::from)?;
        Ok(())
    }

    /// Allocates one contiguous extent of `length` bytes.
    pub fn allocate(&self, length: u64) -> Result<Extent> {
        let mut inner = self.lock();
        require_online(&inner)?;
        let snapshot = inner.allocator.clone();
        let extent = inner.allocator.allocate(length)?;
        if persist_inner(&mut inner, InitState::Complete)
            .and_then(|()| inner.file.sync_all().map_err(PlomidError::from))
            .is_err()
        {
            inner.allocator = snapshot;
            persist_inner(&mut inner, InitState::Complete).ok();
            inner.file.sync_all().ok();
            return Err(PlomidError::new(
                ErrorKind::Io,
                "device allocation durability failure",
            ));
        }
        Ok(extent)
    }

    /// Releases the allocated extent starting at `start`.
    pub fn release(&self, start: u64) -> Result<Extent> {
        let mut inner = self.lock();
        require_online(&inner)?;
        let snapshot = inner.allocator.clone();
        let extent = inner.allocator.release(start)?;
        if persist_inner(&mut inner, InitState::Complete)
            .and_then(|()| inner.file.sync_all().map_err(PlomidError::from))
            .is_err()
        {
            inner.allocator = snapshot;
            persist_inner(&mut inner, InitState::Complete).ok();
            inner.file.sync_all().ok();
            return Err(PlomidError::new(
                ErrorKind::Io,
                "device release durability failure",
            ));
        }
        Ok(extent)
    }

    /// Persists metadata and makes file contents durable.
    pub fn flush(&self) -> Result<()> {
        let mut inner = self.lock();
        persist_inner(&mut inner, InitState::Complete)?;
        inner.file.sync_all().map_err(PlomidError::from)?;
        Ok(())
    }

    /// Makes file contents durable without rewriting metadata.
    pub fn sync(&self) -> Result<()> {
        let inner = self.lock();
        inner.file.sync_all().map_err(PlomidError::from)?;
        Ok(())
    }

    /// Flushes, records an orderly offline marker when possible, and closes.
    pub fn close(self) -> Result<()> {
        let mut inner = self.lock();
        if inner.state.can_enter(Lifecycle::Quiescing)
            && Lifecycle::Quiescing.can_enter(Lifecycle::Offline)
        {
            inner.state = Lifecycle::Quiescing;
            persist_inner(&mut inner, InitState::Complete)?;
            inner.state = Lifecycle::Offline;
            persist_inner(&mut inner, InitState::Complete)?;
            inner.file.sync_all().map_err(PlomidError::from)?;
        } else if inner.state.can_enter(Lifecycle::Offline) {
            inner.state = Lifecycle::Offline;
            persist_inner(&mut inner, InitState::Complete)?;
            inner.file.sync_all().map_err(PlomidError::from)?;
        } else {
            persist_inner(&mut inner, InitState::Complete)?;
            inner.file.sync_all().map_err(PlomidError::from)?;
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, DeviceInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn require_online(inner: &DeviceInner) -> Result<()> {
    if inner.state != Lifecycle::Online {
        return Err(invalid(format!(
            "device is not online (state {:?})",
            inner.state
        )));
    }
    Ok(())
}

fn persist_inner(inner: &mut DeviceInner, init: InitState) -> Result<()> {
    // Metadata lives entirely inside the metadata footprint, which creation
    // and open both guarantee: the snapshot region is sized for the maximum
    // extent count of the logical capacity. Keeping the write inside one
    // footprint is what lets the data area stay demand-driven.
    let metadata_region = DEVICE_HEADER_LEN
        .checked_add(inner.snapshot_len)
        .ok_or_else(|| invalid("device metadata region overflows"))?;
    grow_to(inner, metadata_region)?;
    let metadata = DeviceMetadata {
        device_id: inner.device_id,
        capacity: inner.capacity,
        allocatable_bytes: inner.allocatable,
        data_start: inner.data_start,
        snapshot_len: inner.snapshot_len,
        lifecycle: inner.state,
        initialized: init == InitState::Complete,
    };
    let header = metadata.encode(init);
    inner
        .file
        .write_all_at(&header, 0)
        .map_err(PlomidError::from)?;
    let snapshot =
        inner
            .allocator
            .encode_snapshot(inner.device_id, inner.capacity, inner.snapshot_len)?;
    inner
        .file
        .write_all_at(&snapshot, DEVICE_HEADER_LEN)
        .map_err(PlomidError::from)?;
    Ok(())
}

fn validate_inner(inner: &DeviceInner) -> Result<()> {
    let (snapshot_len, data_start, allocatable) =
        layout_for_capacity(inner.capacity).map_err(|_| corrupt("invalid device capacity"))?;
    if snapshot_len != inner.snapshot_len
        || data_start != inner.data_start
        || allocatable != inner.allocatable
    {
        return Err(corrupt("device capacity mismatch in device metadata"));
    }
    if inner.device_id.is_zero() {
        return Err(corrupt("invalid device identity"));
    }
    inner.allocator.validate_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical::BLOCK_SIZE as PHYS_BLOCK_SIZE;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;

    const E: u64 = EXTENT_SIZE;

    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("plomid-device-{name}-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("device.dat");
        (dir, path)
    }

    #[test]
    fn create_open_validate_close_reopen() {
        let (dir, path) = scratch("lifecycle");
        let capacity = capacity_for_extents(2).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(7), capacity).expect("create");
        assert_eq!(device.state(), Lifecycle::Online);
        assert_eq!(device.capacity(), capacity);
        device.validate().expect("validate");
        device.flush().expect("flush");
        device.close().expect("close");
        let reopened = StorageDevice::open(&path).expect("open");
        assert_eq!(reopened.device_id(), DeviceId::new(7));
        reopened.validate().expect("revalidate");
        drop(reopened);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A new device occupies only its metadata footprint: the declared
    /// capacity is address space, not allocated bytes.
    #[test]
    fn a_new_container_holds_only_its_metadata_footprint() {
        let (dir, path) = scratch("footprint");
        // 8 extents * 64 MiB = 512 MiB of declared address space.
        let capacity = capacity_for_extents(8).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(11), capacity).expect("create");
        assert_eq!(device.capacity(), capacity);
        assert_eq!(device.data_start(), device.physical_len());
        assert!(device.physical_len() < capacity);
        assert_eq!(
            std::fs::metadata(&path).expect("metadata").len(),
            device.physical_len()
        );
        drop(device);

        // Reopen does not require the file to be capacity-sized.
        let reopened = StorageDevice::open(&path).expect("open demand-grown container");
        assert_eq!(reopened.capacity(), capacity);
        assert_eq!(reopened.physical_len(), reopened.data_start());
        assert_eq!(reopened.allocated_extent_count(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Allocating extents costs no physical bytes; placing bytes in the data
    /// area is what grows the container, and the growth survives reopen.
    #[test]
    fn the_data_area_grows_only_when_bytes_are_placed() {
        let (dir, path) = scratch("grow");
        let capacity = capacity_for_extents(4).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(12), capacity).expect("create");
        let footprint = device.physical_len();

        // First allocation: metadata only, no data bytes written yet.
        let first = device.allocate(E).expect("allocate");
        assert_eq!(first.start, 0);
        assert_eq!(device.physical_len(), footprint);

        // Placing bytes grows the container to cover exactly that extent.
        let payload = b"PLOMID_DATA_AREA_PAYLOAD";
        device
            .ensure_data_capacity(first.start + payload.len() as u64)
            .expect("ensure data capacity");
        assert_eq!(device.physical_len(), footprint + payload.len() as u64);
        {
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("open file");
            let offset = device.data_start() + first.start;
            file.write_all_at(payload, offset).expect("place bytes");
            file.sync_all().expect("sync");
        }

        // Idempotent: repeating the same request changes nothing.
        device
            .ensure_data_capacity(first.start + payload.len() as u64)
            .expect("idempotent");
        assert_eq!(device.physical_len(), footprint + payload.len() as u64);

        drop(device);
        let reopened = StorageDevice::open(&path).expect("reopen");
        assert_eq!(reopened.capacity(), capacity);
        assert_eq!(reopened.physical_len(), footprint + payload.len() as u64);
        assert_eq!(reopened.allocated_extent_count(), 1);
        let mut read_back = vec![0_u8; payload.len()];
        {
            use crate::platform::FileExt as _;
            let file = OpenOptions::new()
                .read(true)
                .open(&path)
                .expect("open read");
            file.read_exact_at(&mut read_back, reopened.data_start() + first.start)
                .expect("read back");
        }
        assert_eq!(read_back, payload);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A caller that spans an extent boundary grows the file across both
    /// extents in one forward step, and the allocator state survives reopen.
    #[test]
    fn growth_across_an_extent_boundary_survives_reopen() {
        let (dir, path) = scratch("boundary");
        let capacity = capacity_for_extents(4).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(13), capacity).expect("create");
        let first = device.allocate(E).expect("first");
        let second = device.allocate(E).expect("second");
        assert_eq!(second.start, first.start + E);

        let end = second.start + E;
        device.ensure_data_capacity(end).expect("grow to boundary");
        assert_eq!(device.physical_len(), device.data_start() + end);
        drop(device);

        let reopened = StorageDevice::open(&path).expect("reopen");
        assert_eq!(reopened.allocated_extent_count(), 2);
        assert_eq!(reopened.allocated_bytes(), 2 * E);
        assert_eq!(reopened.physical_len(), reopened.data_start() + end);
        // A third allocation beyond the grown region is still logical only.
        let third = reopened.allocate(E).expect("third");
        assert_eq!(third.start, 2 * E);
        assert_eq!(reopened.physical_len(), reopened.data_start() + end);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Growth is bounded by the declared address space, so a caller cannot
    /// commit more data bytes than the device was created with.
    #[test]
    fn growth_beyond_the_declared_capacity_is_rejected() {
        let (dir, path) = scratch("overcommit");
        let capacity = capacity_for_extents(2).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(14), capacity).expect("create");
        let allocatable = device.allocatable_bytes();
        assert!(device.ensure_data_capacity(allocatable + 1).is_err());
        device
            .ensure_data_capacity(allocatable)
            .expect("exact bound is allowed");
        assert_eq!(device.physical_len(), device.data_start() + allocatable);
        assert_eq!(device.physical_len(), capacity);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_invalid_device_id_and_capacities() {
        let (dir, path) = scratch("capacity");
        let two = capacity_for_extents(2).expect("capacity");
        assert!(StorageDevice::create(&path, DeviceId::new(0), two).is_err());
        assert!(StorageDevice::create(&path, DeviceId::new(1), 0).is_err());
        assert!(StorageDevice::create(&path, DeviceId::new(1), 16 * 1024).is_err());
        assert!(StorageDevice::create(&path, DeviceId::new(1), E).is_err());
        assert!(
            StorageDevice::create(&path, DeviceId::new(1), MAX_DEVICE_CAPACITY + 4096).is_err()
        );
        assert!(validate_capacity(u64::MAX).is_err());
        assert!(capacity_for_extents(0).is_err());
        assert!(capacity_for_extents(u64::MAX).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn metadata_rejections_cover_required_matrix() {
        let (dir, path) = scratch("meta");
        let capacity = capacity_for_extents(2).expect("capacity");
        let _ = StorageDevice::create(&path, DeviceId::new(11), capacity).expect("create");
        let raw = std::fs::read(&path).expect("read");
        let mutate = |index: usize, value: u8| {
            let mut bytes = raw.clone();
            bytes[index] = value;
            std::fs::write(&path, &bytes).expect("write");
            let opened = StorageDevice::open(&path);
            std::fs::write(&path, &raw).expect("restore");
            opened
        };
        assert!(mutate(0, b'X').is_err());
        assert!(mutate(4, 0xFF).is_err());
        assert!(mutate(DEVICE_HEADER_LEN as usize, b'X').is_err());
        assert!(mutate(DEVICE_HEADER_LEN as usize + 4, 0xFF).is_err());
        {
            let mut bytes = raw.clone();
            bytes[8..16].fill(0);
            std::fs::write(&path, &bytes).expect("write");
            assert!(StorageDevice::open(&path).is_err());
            std::fs::write(&path, &raw).expect("restore");
        }
        {
            let mut bytes = raw.clone();
            bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
            std::fs::write(&path, &bytes).expect("write");
            assert!(StorageDevice::open(&path).is_err());
            std::fs::write(&path, &raw).expect("restore");
        }
        assert!(mutate(44, 0x7F).is_err());
        assert!(mutate(48, 0x7F).is_err());
        assert!(mutate(52, 0xAB).is_err());
        assert!(mutate(4096, 0xAB).is_err());
        std::fs::write(&path, &raw[..100]).expect("truncate");
        assert!(StorageDevice::open(&path).is_err());
        std::fs::write(&path, &raw).expect("restore");
        assert!(mutate(4095, 0xFF).is_err());
        assert!(StorageDevice::open(&dir.join("absent.dat")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lifecycle_transitions_and_interrupted_creation() {
        let (dir, path) = scratch("transition");
        let capacity = capacity_for_extents(2).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(21), capacity).expect("create");
        assert!(device.transition(Lifecycle::Offline).is_ok());
        assert!(device.transition(Lifecycle::Quiescing).is_err());
        assert!(device.transition(Lifecycle::Online).is_ok());
        assert!(device.transition(Lifecycle::Recovering).is_err());
        device.transition(Lifecycle::Quiescing).expect("quiesce");
        assert!(device.allocate(E).is_err());
        device.transition(Lifecycle::Offline).expect("offline");
        device.close().expect("close");
        let reopened = StorageDevice::open(&path).expect("open");
        assert_eq!(reopened.state(), Lifecycle::Online);
        drop(reopened);
        let partial = dir.join("partial.dat");
        let (snapshot_len, _, allocatable) = layout_for_capacity(capacity).expect("layout");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&partial)
            .expect("file");
        file.set_len(capacity).expect("size");
        let mut inner = DeviceInner {
            file,
            device_id: DeviceId::new(22),
            capacity,
            allocatable,
            data_start: DEVICE_HEADER_LEN + snapshot_len,
            snapshot_len,
            physical_len: capacity,
            state: Lifecycle::Initializing,
            allocator: ExtentAllocator::new_empty(allocatable),
        };
        persist_inner(&mut inner, InitState::Incomplete).expect("persist");
        inner.file.sync_all().expect("sync");
        drop(inner);
        assert!(StorageDevice::open(&partial).is_err());
        std::fs::write(&partial, vec![0_u8; 128]).expect("truncate");
        assert!(StorageDevice::open(&partial).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failed_device_reopens_as_recovering() {
        let (dir, path) = scratch("failed");
        let capacity = capacity_for_extents(2).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(23), capacity).expect("create");
        device.transition(Lifecycle::Failed).expect("fail");
        drop(device);
        let reopened = StorageDevice::open(&path).expect("open");
        assert_eq!(reopened.state(), Lifecycle::Recovering);
        reopened.transition(Lifecycle::Online).expect("recover");
        reopened.validate().expect("validate");
        drop(reopened);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn allocation_release_and_capacity_edges() {
        let (dir, path) = scratch("alloc");
        let capacity = capacity_for_extents(4).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(31), capacity).expect("create");
        assert!(device.allocate(0).is_err());
        assert!(device.allocate(1024).is_err());
        assert!(device.allocate(E + 1).is_err());
        let first = device.allocate(E).expect("first");
        assert_eq!(first.start, 0);
        assert_eq!(first.length, E);
        let large = device.allocate(2 * E).expect("large");
        assert_eq!(large.length, 2 * E);
        assert!(
            first.start + first.length <= large.start || large.start + large.length <= first.start
        );
        let last = device.allocate(E).expect("last");
        assert!(device.allocate(E).is_err());
        assert_eq!(device.free_bytes(), 0);
        assert!(device.release(9 * E).is_err());
        assert!(device.release(E).is_ok() || device.release(first.start).is_ok());
        let _ = (last, large);
        let reopened = StorageDevice::open(&path).expect("open");
        reopened.validate().expect("validate");
        drop(reopened);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_allocation_boundaries_and_misaligned_requests() {
        let (dir, path) = scratch("edges");
        let capacity = capacity_for_extents(3).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(32), capacity).expect("create");
        let start = device.allocate(3 * E).expect("full");
        assert_eq!(start.start, 0);
        assert!(device.allocate(E).is_err());
        device.release(start.start).expect("release");
        assert_eq!(device.free_bytes(), device.allocatable_bytes());
        let head = device.allocate(E).expect("head");
        assert_eq!(head.start, 0);
        let tail = device.allocate(E).expect("tail");
        let end = device.allocate(E).expect("end");
        assert_eq!(head.end(), Some(E));
        assert_eq!(end.start + end.length, device.allocatable_bytes());
        let _ = (head, tail, end);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn release_reuse_double_free_and_unknown() {
        let (dir, path) = scratch("reuse");
        let capacity = capacity_for_extents(2).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(41), capacity).expect("create");
        let first = device.allocate(E).expect("first");
        let released = device.release(first.start).expect("release");
        assert_eq!(released, first);
        assert!(device.release(first.start).is_err());
        assert!(device.release(7 * E).is_err());
        let reused = device.allocate(E).expect("reuse");
        assert_eq!(reused.start, first.start);
        assert_ne!(reused.extent_id, first.extent_id);
        device.validate().expect("validate");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fragmentation_coalescing_and_large_contiguous() {
        let (dir, path) = scratch("frag");
        let capacity = capacity_for_extents(8).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(51), capacity).expect("create");
        let a = device.allocate(E).expect("a");
        let b = device.allocate(E).expect("b");
        let c = device.allocate(E).expect("c");
        let rest = device.allocate(5 * E).expect("rest");
        // Free the middle; a small allocation must reuse it exactly.
        device.release(b.start).expect("free b");
        let small = device.allocate(E).expect("small");
        assert_eq!(small.start, b.start);
        device.release(small.start).expect("free small");
        // Free everything adjacent: A + B + C must coalesce into 3 extents.
        device.release(a.start).expect("free a");
        device.release(c.start).expect("free c");
        device.release(rest.start).expect("free rest");
        assert_eq!(device.free_bytes(), device.allocatable_bytes());
        let big = device.allocate(8 * E).expect("big");
        assert_eq!(big.start, 0);
        assert_eq!(big.length, 8 * E);
        // Non-adjacent frees must not merge across a live extent.
        device.release(big.start).expect("free big");
        let x = device.allocate(E).expect("x");
        let y = device.allocate(E).expect("y");
        let z = device.allocate(E).expect("z");
        device.release(x.start).expect("free x");
        device.release(z.start).expect("free z");
        assert!(device.allocate(3 * E).is_ok() || device.allocate(2 * E).is_ok());
        let _ = (x, y, z);
        device.validate().expect("validate");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persistence_round_trip_with_allocate_and_release() {
        let (dir, path) = scratch("persist");
        let capacity = capacity_for_extents(4).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(61), capacity).expect("create");
        let a = device.allocate(E).expect("a");
        let b = device.allocate(2 * E).expect("b");
        device.flush().expect("flush");
        drop(device);
        let reopened = StorageDevice::open(&path).expect("open");
        reopened.validate().expect("validate");
        assert_eq!(reopened.allocated_extent_count(), 2);
        let c = reopened.allocate(E).expect("c");
        reopened.release(a.start).expect("release a");
        let _ = (a, b, c);
        drop(reopened);
        let again = StorageDevice::open(&path).expect("open");
        again.validate().expect("validate");
        assert_eq!(again.allocated_extent_count(), 2);
        drop(again);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupted_snapshot_and_size_mismatch_fail_open() {
        let (dir, path) = scratch("corrupt");
        let capacity = capacity_for_extents(2).expect("capacity");
        let device = StorageDevice::create(&path, DeviceId::new(71), capacity).expect("create");
        device.allocate(E).expect("alloc");
        drop(device);
        let raw = std::fs::read(&path).expect("read");
        let snapshot_off = DEVICE_HEADER_LEN as usize;
        let mut bad = raw.clone();
        bad[snapshot_off + 24] ^= 0xFF;
        std::fs::write(&path, &bad).expect("write");
        assert!(StorageDevice::open(&path).is_err());
        std::fs::write(&path, &raw).expect("restore");
        {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("open file");
            file.set_len(capacity + PAGE_SIZE).expect("grow");
        }
        assert!(StorageDevice::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_allocate_release_stays_consistent() {
        let (dir, path) = scratch("concurrent");
        let capacity = capacity_for_extents(16).expect("capacity");
        let device =
            Arc::new(StorageDevice::create(&path, DeviceId::new(81), capacity).expect("c"));
        let allocatable = device.allocatable_bytes();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let device = Arc::clone(&device);
            handles.push(thread::spawn(move || {
                let mut owned = Vec::new();
                for _ in 0..4 {
                    match device.allocate(E) {
                        Ok(extent) => owned.push(extent.start),
                        Err(_) => break,
                    }
                }
                // Deterministic release order: reverse of acquisition.
                for start in owned.into_iter().rev() {
                    device.release(start).expect("release");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("thread");
        }
        device.validate().expect("validate");
        assert_eq!(device.allocated_extent_count(), 0);
        assert_eq!(device.free_bytes(), allocatable);
        // Interleaved allocate/release from two threads keeps invariants.
        let device2 = Arc::new(StorageDevice::open(&path).expect("open"));
        let first = device2.allocate(2 * E).expect("first");
        let worker = {
            let device2 = Arc::clone(&device2);
            thread::spawn(move || {
                for _ in 0..8 {
                    if let Ok(extent) = device2.allocate(E) {
                        device2.release(extent.start).ok();
                    }
                }
            })
        };
        for _ in 0..8 {
            if let Ok(extent) = device2.allocate(E) {
                device2.release(extent.start).ok();
            }
        }
        worker.join().expect("worker");
        device2.release(first.start).expect("free first");
        device2.validate().expect("validate");
        assert_eq!(device2.free_bytes(), allocatable);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn alignment_matches_storage_hierarchy() {
        assert_eq!(E % PHYS_BLOCK_SIZE, 0);
        assert_eq!(PHYS_BLOCK_SIZE % PAGE_SIZE, 0);
        assert_eq!(DEVICE_HEADER_LEN % PAGE_SIZE, 0);
        assert_eq!(SNAPSHOT_PAGE_LEN % PAGE_SIZE, 0);
        let capacity = capacity_for_extents(2).expect("capacity");
        assert_eq!(capacity % PAGE_SIZE, 0);
        let (dir, path) = scratch("align");
        let device = StorageDevice::create(&path, DeviceId::new(91), capacity).expect("create");
        let extent = device.allocate(E).expect("alloc");
        assert_eq!(extent.start % E, 0);
        assert_eq!(extent.length % E, 0);
        assert_eq!((device.data_start() + extent.start) % PHYS_BLOCK_SIZE, 0);
        assert_eq!((device.data_start() + extent.start) % PAGE_SIZE, 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
