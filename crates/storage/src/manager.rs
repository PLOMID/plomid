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
//! Segment-oriented storage management built on the existing durable B+Tree.
//!
//! Each segment is an independent page file (an on-disk B+Tree). The manager
//! routes reads to all segments and directs new keys to the active segment,
//! rotating it once its configured on-disk size is reached.
//!
//! # Physical layout
//!
//! Segments live inside the physical device tree, never under the logical
//! object tree. A segment belongs to exactly one device, and devices are
//! discovered from the layout. No manifest or volume registry exists: the
//! filesystem listing of each device's `packs/` directory is the durable
//! segment inventory, combined with the device records the registry validates.
//!
//! ```text
//! PLOMID_DATA/
//!   devices/
//!     D-0000000000000001/
//!       packs/
//!         SEG-00000000000000000001.dat   <- segment 1 page file
//!     D-0000000000000002/
//!       packs/
//!         SEG-00000000000000000003.dat   <- segment 3 page file
//! ```
//!
//! Logical table/generation metadata references segments by segment ID. The
//! manager never reads logical metadata: it only owns the physical segment
//! page files and their placement on devices.
//!
//! # Device placement
//!
//! The manager never assumes a device. Every segment is placed by the device
//! allocator, which selects among the devices the registry reports. A segment
//! commits its maximum size on the selected device when it is created — the
//! device reserves that capacity as one contiguous extent — so a device can
//! never be oversubscribed and a full device simply makes another eligible
//! device the choice. The capacity of the database is therefore the sum of its
//! devices rather than the capacity of any single one, and one logical table
//! can span several devices through its segments.

use crate::device::{
    default_device_capacity, register_device, round_up_to_extent, DeviceAllocator, DeviceRegistry,
};
use crate::layout::{reject_legacy_layout, DatabaseLayout, STAGING_SUFFIX};
use placement::segment_from_file_name;
use plomid_core::{ErrorKind, GenerationId, PlomidError, Result, SegmentId};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub(crate) mod placement;
mod segment;

pub use placement::storage_generation;
pub(crate) use placement::{durable_segment_ids, segment_id_exhausted};
pub use segment::{Segment, SegmentState};

/// Segment-oriented physical storage manager.
pub struct StorageManager {
    layout: DatabaseLayout,
    pool_capacity: usize,
    segment_size_bytes: u64,
    segments: Vec<Segment>,
    next_segment_id: u64,
    allocator: DeviceAllocator,
    /// In-memory ownership map for keys already seen by this manager.  The
    /// durable B+Trees remain authoritative; this is only a routing cache so
    /// mutations do not probe every sealed generation before touching a key.
    key_segments: Option<HashMap<Vec<u8>, usize>>,
    /// Sealed-segment probes attempted (exact B-tree descents).
    filter_probes: u64,
    /// Sealed-segment probes skipped by the negative-membership filter.
    filter_skips: u64,
}

impl std::fmt::Debug for StorageManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageManager")
            .field("segment_count", &self.segments.len())
            .field("next_segment_id", &self.next_segment_id)
            .finish()
    }
}

impl StorageManager {
    /// Creates a storage database at `root`, or reopens the one already there.
    ///
    /// Database creation is the only path that registers a device implicitly: a
    /// database cannot hold anything without physical capacity. The device it
    /// creates for itself takes the lowest free device identity, so no specific
    /// device number is special, and further devices are registered explicitly.
    ///
    /// Creation is idempotent: a root that already owns durable segments is
    /// rediscovered and reopened instead of accumulating a second segment 1, so
    /// running creation against an existing database can never truncate the
    /// pages of a segment that already holds data.
    pub fn create(root: &Path, pool_capacity: usize, segment_size_bytes: u64) -> Result<Self> {
        let layout = DatabaseLayout::new(root);
        reject_legacy_layout(&layout)?;
        layout.initialize()?;

        let registry = DeviceRegistry::discover(&layout)?;
        if registry.is_empty() {
            let capacity = default_device_capacity()?;
            register_device(&layout, registry.next_free_id(), capacity)?;
        }

        let segments = discover_segments(&layout, pool_capacity)?;
        let mut manager = Self::restore(layout, pool_capacity, segment_size_bytes, segments);
        if manager.segments.is_empty() {
            manager.allocate_segment()?;
        }
        Ok(manager)
    }

    /// Opens an existing storage database and rediscovers its segments.
    pub fn open(root: &Path, pool_capacity: usize, segment_size_bytes: u64) -> Result<Self> {
        let layout = DatabaseLayout::new(root);
        layout.validate()?;

        let registry = DeviceRegistry::discover(&layout)?;
        if registry.is_empty() {
            return Err(PlomidError::with_detail(
                ErrorKind::NotFound,
                "no physical devices are registered",
                format!("root={}", root.display()),
            ));
        }

        let segments = discover_segments(&layout, pool_capacity)?;
        let mut manager = Self::restore(layout, pool_capacity, segment_size_bytes, segments);
        if manager.segments.is_empty() {
            manager.allocate_segment()?;
        }
        Ok(manager)
    }

    /// Assembles a manager around the segments a root already owns.
    ///
    /// The newest segment continues to receive keys; older segments stay sealed,
    /// which is the state a clean shutdown leaves behind.
    fn restore(
        layout: DatabaseLayout,
        pool_capacity: usize,
        segment_size_bytes: u64,
        mut segments: Vec<Segment>,
    ) -> Self {
        segments.sort_unstable_by_key(|segment| segment.id);
        if let Some(active) = segments.last_mut() {
            active.state = SegmentState::Active;
        }
        let next_segment_id = segments
            .iter()
            .map(|segment| segment.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        Self {
            layout,
            pool_capacity,
            segment_size_bytes: segment_size_bytes.max(1),
            segments,
            next_segment_id,
            allocator: DeviceAllocator::new(),
            // Start empty.  Existing keys are routed through the authoritative
            // fallback on first mutation after restart; eagerly scanning every
            // segment here made the first bulk INSERT pay for a full database
            // read before doing any writes.
            key_segments: Some(HashMap::new()),
            filter_probes: 0,
            filter_skips: 0,
        }
    }

    /// Cumulative sealed-segment probe statistics `(attempted, skipped)`.
    ///
    /// `skipped` counts probes avoided by the negative-membership filter;
    /// `attempted` counts exact descents performed. Used by benchmarks and
    /// operators to verify the filter engages; not a correctness signal.
    #[must_use]
    pub fn filter_stats(&self) -> (u64, u64) {
        (self.filter_probes, self.filter_skips)
    }

    /// Whether sealed segment `index` must be probed exactly for `key`.
    ///
    /// Builds the segment's filter lazily on first need (one full key scan
    /// per segment lifetime) and records the decision in [`Self::filter_stats`].
    /// A `false` answer is definitive (the key is absent); `true` requires
    /// exact evaluation. Must only be called for sealed segments: the active
    /// segment has no filter and always probes.
    fn probe_needed(&mut self, index: usize, key: &[u8]) -> bool {
        let segment = &mut self.segments[index];
        debug_assert_ne!(segment.state, SegmentState::Active);
        if !segment.has_filter() {
            segment.ensure_filter();
        }
        if segment.may_contain(key) {
            self.filter_probes += 1;
            true
        } else {
            self.filter_skips += 1;
            false
        }
    }

    /// Root directory of this storage database.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.layout.root()
    }

    /// Durable storage-image generation: the highest committed segment ID.
    #[must_use]
    pub fn storage_generation(&self) -> GenerationId {
        GenerationId::new(
            self.segments
                .iter()
                .map(|segment| segment.id)
                .max()
                .unwrap_or(0),
        )
    }

    /// Number of segments the manager currently holds open.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Every segment, in creation order.
    pub fn segments(&self) -> impl Iterator<Item = &Segment> {
        self.segments.iter()
    }

    /// Reads `key`, searching the newest segment first.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        for index in (0..self.segments.len()).rev() {
            // The active segment has no filter (it mutates); sealed segments
            // consult theirs, building it lazily on first need.
            if index + 1 < self.segments.len() && !self.probe_needed(index, key) {
                continue;
            }
            if let Some(value) = self.segments[index].get(key)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Inserts `key` and makes it durable.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.insert_buffered(key, value)?;
        self.sync()
    }

    /// Inserts `key` without forcing a flush; the caller owns durability.
    pub fn insert_buffered(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.apply_batch(std::slice::from_ref(&(key.to_vec(), Some(value.to_vec()))))
    }

    /// Applies a transaction's storage mutations as one logical batch.
    ///
    /// The old path searched every segment for every put, then descended the
    /// active tree again for new keys.  This path builds a lazy ownership map,
    /// routes known keys directly to their authoritative segment, and handles
    /// unknown keys with one replacement traversal before inserting.  The
    /// map is an optimization only: a cache miss still performs the complete
    /// durable search, so replacement semantics remain unchanged.
    pub fn apply_batch(&mut self, operations: &[(Vec<u8>, Option<Vec<u8>>)]) -> Result<()> {
        let perf = tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf");
        let total_started = std::time::Instant::now();
        let mut routed_us = 0_u64;
        let mut probe_us = 0_u64;
        let mut insert_us = 0_u64;
        let mut probes = 0_u64;
        let mut inserts = 0_u64;
        let mut rotates = 0_u64;
        let mut filter_skipped = 0_u64;
        self.ensure_key_segments()?;
        for (key, value) in operations {
            if let Some(&index) = self.key_segments.as_ref().and_then(|m| m.get(key)) {
                if let Some(segment) = self.segments.get_mut(index) {
                    let routed_started = std::time::Instant::now();
                    match value {
                        Some(value) => {
                            if segment.replace_if_present(key, value)? {
                                routed_us += routed_started.elapsed().as_micros() as u64;
                                continue;
                            }
                        }
                        None => {
                            if segment.delete(key)? {
                                routed_us += routed_started.elapsed().as_micros() as u64;
                                if let Some(map) = self.key_segments.as_mut() {
                                    map.remove(key);
                                }
                                continue;
                            }
                        }
                    }
                    routed_us += routed_started.elapsed().as_micros() as u64;
                }
            }

            // A cache miss can be caused by a key written before the cache was
            // initialized, or by a stale location after recovery. Probe the
            // SEALED segments only, newest first, so the copy the reads would
            // resolve (`get` searches newest segment first) is the one
            // replaced.
            //
            // The active segment is deliberately excluded from the probe: its
            // write path (`BTree::insert`, reached through `Segment::insert`)
            // is already an upsert, so probing it here and then descending it
            // again to insert would double the tree descents for every new key
            // in a batch. The probe's only job is to find a copy in a segment
            // that must not be shadowed; the active segment is handled by the
            // single upsert descent below.
            let probe_started = std::time::Instant::now();
            let active = self.segments.len().checked_sub(1);
            let mut found = None;
            if let Some(active) = active {
                for index in (0..active).rev() {
                    // Negative prefilter: a sealed segment that definitively
                    // lacks the key is skipped without a B-tree descent. A
                    // `false` answer is exact (the filter is a superset built
                    // at seal time over an only-shrinking key set); `true`
                    // falls through to exact evaluation below.
                    if !self.probe_needed(index, key) {
                        filter_skipped += 1;
                        continue;
                    }
                    probes += 1;
                    let segment = &mut self.segments[index];
                    match value {
                        Some(value) if segment.replace_if_present(key, value)? => {
                            found = Some(index);
                            break;
                        }
                        None if segment.delete(key)? => {
                            found = Some(index);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            probe_us += probe_started.elapsed().as_micros() as u64;
            if let Some(index) = found {
                if let Some(map) = self.key_segments.as_mut() {
                    if value.is_some() {
                        map.insert(key.clone(), index);
                    } else {
                        map.remove(key);
                    }
                }
                continue;
            }
            let insert_started = std::time::Instant::now();
            if value.is_none() {
                // A deletion the sealed probe did not satisfy may still match
                // the active segment.
                if let Some(index) = active {
                    if self.segments[index].delete(key)? {
                        insert_us += insert_started.elapsed().as_micros() as u64;
                        if let Some(map) = self.key_segments.as_mut() {
                            map.remove(key);
                        }
                    }
                }
                continue;
            }
            if self
                .segments
                .last_mut()
                .is_none_or(|segment| segment.is_full(self.segment_size_bytes))
            {
                self.rotate()?;
                rotates += 1;
            }
            let index = self.segments.len().checked_sub(1).ok_or_else(|| {
                PlomidError::new(ErrorKind::Internal, "storage manager has no active segment")
            })?;
            // One descent: `BTree::insert` replaces an existing entry in place
            // and inserts a new one otherwise, covering the active segment
            // without a preceding probe.
            self.segments[index].insert(key, value.as_deref().unwrap_or_default())?;
            insert_us += insert_started.elapsed().as_micros() as u64;
            inserts += 1;
            if let Some(map) = self.key_segments.as_mut() {
                map.insert(key.clone(), index);
            }
        }
        if perf {
            tracing::debug!(
                target: "plomid::perf",
                event = "storage_apply",
                ops = operations.len(),
                segments = self.segments.len(),
                routed_us,
                probe_us,
                insert_us,
                probes,
                filter_skipped,
                inserts,
                rotates,
                total_us = total_started.elapsed().as_micros() as u64,
            );
        }
        Ok(())
    }

    /// Deletes `key` and makes the deletion durable.
    pub fn delete(&mut self, key: &[u8]) -> Result<bool> {
        let removed = self.delete_buffered(key)?;
        self.sync()?;
        Ok(removed)
    }

    /// Deletes `key` without forcing a flush.
    pub fn delete_buffered(&mut self, key: &[u8]) -> Result<bool> {
        self.ensure_key_segments()?;
        if let Some(index) = self.key_segments.as_ref().and_then(|m| m.get(key)).copied() {
            if self.segments[index].delete(key)? {
                self.key_segments
                    .as_mut()
                    .expect("key map initialized")
                    .remove(key);
                return Ok(true);
            }
        }
        for index in (0..self.segments.len()).rev() {
            // Sealed segments consult the negative filter first (the active
            // segment always probes exactly).
            if index + 1 < self.segments.len() && !self.probe_needed(index, key) {
                continue;
            }
            if self.segments[index].delete(key)? {
                self.key_segments
                    .as_mut()
                    .expect("key map initialized")
                    .remove(key);
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn ensure_key_segments(&mut self) -> Result<()> {
        // The routing map is intentionally lazy.  It is populated by every
        // mutation performed during this process; a key absent from it still
        // takes the complete durable lookup path in apply_batch().
        if self.key_segments.is_none() {
            self.key_segments = Some(HashMap::new());
        }
        Ok(())
    }

    /// Returns the ordered union of `[start, end)` across every segment.
    pub fn range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut values = Vec::new();
        for segment in &mut self.segments {
            values.extend(segment.range(start, end)?);
        }
        values.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        values.dedup_by(|left, right| left.0 == right.0);
        Ok(values)
    }

    /// Makes every written page of every segment durable.
    pub fn sync(&mut self) -> Result<()> {
        for segment in &mut self.segments {
            segment.sync()?;
        }
        Ok(())
    }

    /// Seals the active segment and opens a new one for subsequent keys.
    fn rotate(&mut self) -> Result<()> {
        if let Some(active) = self.segments.last_mut() {
            active.seal()?;
        }
        self.allocate_segment()
    }

    /// Places a new segment on a device and opens it.
    ///
    /// The device is chosen by the allocator, never by the manager: the manager
    /// only performs the placement the allocator decided. Capacity is committed
    /// before the segment file is created, so a failed allocation leaves the
    /// device set unchanged rather than half-committed.
    fn allocate_segment(&mut self) -> Result<()> {
        let id = self.next_segment_id;
        self.next_segment_id = self
            .next_segment_id
            .checked_add(1)
            .ok_or_else(segment_id_exhausted)?;

        // A segment may grow up to its configured size, so that is the capacity
        // the device must be able to commit for it.
        let request = round_up_to_extent(self.segment_size_bytes)?;
        // Devices are rediscovered for every segment: a device registered after
        // the manager was opened becomes eligible without reopening the
        // database, and this happens once per segment, not once per write.
        let registry = DeviceRegistry::discover(&self.layout)?;
        if registry.is_empty() {
            return Err(PlomidError::new(
                ErrorKind::NotFound,
                "no physical device is available for a new segment",
            ));
        }
        let target = self.allocator.select_device(&registry, request)?;
        let device_id = target.device_id;
        // The reservation is durable in the device's allocator: the extent
        // belongs to this segment until segment reclamation releases it.
        let _reservation = target.device.allocate(request)?;

        let path = placement::segment_path(&self.layout, device_id, SegmentId::new(id));
        let segment =
            Segment::create(path.clone(), id, device_id, self.pool_capacity).map_err(|error| {
                // A segment that cannot be created on a device is a placement
                // failure, not an anonymous I/O fault: report the device and the
                // pack path so the operator can see which physical capacity failed.
                PlomidError::with_detail(
                    error.kind(),
                    "new storage segment could not be created on its device",
                    format!(
                        "segment_id={id} device_id={} path={} error={}",
                        device_id.get(),
                        path.display(),
                        error.message()
                    ),
                )
            })?;
        self.segments.push(segment);
        Ok(())
    }
}

/// Reports whether a storage root already owns durable physical state.
///
/// A root with no registered device and no durable segment has never stored
/// anything, so a database may be created there. Any other root is an existing
/// database: it must be opened, never rebuilt over, because creating segments
/// again could truncate the pages of a segment that already holds data. This is
/// the explicit test a first-boot path uses instead of a silent fallback.
pub fn has_durable_state(root: &Path) -> Result<bool> {
    let layout = DatabaseLayout::new(root);
    if !layout.devices_dir().is_dir() {
        return Ok(false);
    }
    if !DeviceRegistry::discover(&layout)?.is_empty() {
        return Ok(true);
    }
    Ok(!durable_segment_ids(root)?.is_empty())
}

/// Discovers the durable segments of a database root, by device.
///
/// The pack listing of each registered device is the durable segment inventory;
/// a segment ID that appears on more than one device is corruption, because a
/// segment has exactly one physical owner.
fn discover_segments(layout: &DatabaseLayout, pool_capacity: usize) -> Result<Vec<Segment>> {
    let mut all_segments: BTreeMap<u64, Segment> = BTreeMap::new();
    for device_id in layout.discover_device_ids()? {
        let packs_dir = layout.device_packs_dir(device_id);
        if !packs_dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&packs_dir).map_err(PlomidError::from)? {
            let entry = entry.map_err(PlomidError::from)?;
            if !entry.file_type().map_err(PlomidError::from)?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // A staging artifact is an unpublished segment and is never part of
            // the durable inventory.
            if name.ends_with(STAGING_SUFFIX) {
                continue;
            }
            let Some(segment_id) = segment_from_file_name(&name) else {
                continue;
            };
            let path = entry.path();
            if let Some(existing) = all_segments.get(&segment_id.get()) {
                return Err(PlomidError::with_detail(
                    ErrorKind::Corruption,
                    "duplicate segment ID across devices",
                    format!(
                        "segment_id={} first={} duplicate={}",
                        segment_id.get(),
                        existing.path.display(),
                        path.display()
                    ),
                ));
            }
            let segment = Segment::open(path.clone(), segment_id.get(), device_id, pool_capacity)
                .map_err(|error| {
                // A durable segment exists in the device listing but cannot be
                // reopened: report which device and pack file is unreadable
                // instead of surfacing an anonymous I/O error.
                PlomidError::with_detail(
                    error.kind(),
                    "durable storage segment could not be reopened",
                    format!(
                        "segment_id={} device_id={} path={} error={}",
                        segment_id.get(),
                        device_id.get(),
                        path.display(),
                        error.message()
                    ),
                )
            })?;
            all_segments.insert(segment_id.get(), segment);
        }
    }
    Ok(all_segments.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::StorageManager;
    use crate::device::{register_device, DeviceRegistry};
    use crate::layout::DatabaseLayout;
    use plomid_core::DeviceId;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn root() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "plomid-storage-manager-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn rotates_real_segments_and_reopens_them() {
        let path = root();
        let result = (|| {
            let mut manager = StorageManager::create(&path, 32, 1)?;
            manager.insert(b"alpha", b"one")?;
            manager.insert(b"beta", b"two")?;
            assert!(manager.segment_count() >= 2);
            manager.sync()?;
            drop(manager);
            let mut reopened = StorageManager::open(&path, 32, 1)?;
            assert_eq!(reopened.get(b"alpha")?, Some(b"one".to_vec()));
            assert_eq!(reopened.get(b"beta")?, Some(b"two".to_vec()));
            assert!(reopened.segment_count() >= 2);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = std::fs::remove_dir_all(path);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn updating_a_key_in_a_sealed_segment_does_not_shadow_it() {
        let path = root();
        let result = (|| {
            // A 1-byte segment rotates on every insert, so `alpha` is owned by
            // a sealed segment by the time it is updated.
            let mut manager = StorageManager::create(&path, 32, 1)?;
            manager.insert(b"alpha", b"one")?;
            manager.insert(b"beta", b"two")?;
            assert!(manager.segment_count() >= 2);
            manager.insert(b"alpha", b"updated")?;
            manager.sync()?;
            drop(manager);

            let mut reopened = StorageManager::open(&path, 32, 1)?;
            assert_eq!(reopened.get(b"alpha")?, Some(b"updated".to_vec()));
            let rows = reopened.range(None, None)?;
            assert_eq!(
                rows.iter().filter(|(key, _)| key == b"alpha").count(),
                1,
                "replacing a sealed key must not leave a second copy"
            );
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = std::fs::remove_dir_all(path);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn every_segment_belongs_to_a_registered_device() {
        use crate::capacity_for_extents;

        let path = root();
        let result = (|| {
            let mut manager = StorageManager::create(&path, 32, 1)?;
            let second = DeviceId::new(7);
            let layout = DatabaseLayout::new(&path);
            // A capacity within one allocator snapshot page and larger than the
            // device the database created for itself (8 192 extents).
            let capacity = capacity_for_extents(10_000).expect("capacity");
            register_device(&layout, second, capacity)?;
            manager.insert(b"first", b"one")?;
            manager.insert(b"second", b"two")?;
            // Placement is capacity-first: the device with the most free
            // capacity receives the next segment, so a device registered after
            // creation wins as soon as it has the most room.
            assert!(
                manager
                    .segments()
                    .any(|segment| segment.device_id == second),
                "the newly registered device receives segments"
            );
            // Every segment lives in the pack directory of its own device, and
            // never in the logical object tree.
            for segment in manager.segments() {
                assert!(segment
                    .path
                    .starts_with(layout.device_packs_dir(segment.device_id)));
                assert!(!segment.path.starts_with(layout.objects_dir()));
            }
            assert!(DeviceRegistry::discover(&layout)?.contains(second));
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = std::fs::remove_dir_all(path);
        assert!(result.is_ok(), "{result:?}");
    }
}
