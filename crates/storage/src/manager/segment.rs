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
//! One durable storage segment: an on-disk B+Tree in a device's pack directory.
//!
//! A segment is the storage engine's unit of physical growth. Its bytes live in
//! the `packs/` directory of exactly one device, so a segment is the smallest
//! unit that is *placed*: it is never split across devices, which is what lets
//! a logical table span several devices while every segment keeps a single
//! physical owner.
//!
//! The segment owns its B+Tree and exposes only key-value operations to the
//! storage manager, so placement bookkeeping in the manager never has to know
//! how a segment stores bytes.

use crate::BTree;
use plomid_core::{DeviceId, Result};
use plomid_filters::XorFilter;
use std::path::PathBuf;

/// Lifecycle state of a segment inside one storage manager.
///
/// Exactly one segment is [`SegmentState::Active`]: new keys go there. Sealed
/// segments are immutable and are only read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentState {
    /// Receives new keys.
    Active,
    /// Complete and read-only.
    Sealed,
}

/// A durable segment: an independent on-disk B+Tree.
pub struct Segment {
    /// Logical identity of the segment, unique within the database.
    pub id: u64,
    /// Pack path owning the segment's bytes.
    pub path: PathBuf,
    /// Whether the segment still receives new keys.
    pub state: SegmentState,
    /// Device that physically owns this segment.
    pub device_id: DeviceId,
    tree: BTree,
    tracked_len: u64,
    /// Negative-membership filter over the segment's key set.
    ///
    /// `None` means unknown: the active segment (still mutating) and sealed
    /// segments whose sidecar is missing or unreadable. A present filter was
    /// built over exactly the keys sealed into this segment; sealed segments
    /// never gain keys afterwards (only replacements/deletes of existing
    /// keys), so the filter remains a superset forever and `false` is a
    /// definitive absence. Callers must still evaluate `true` exactly.
    key_filter: Option<XorFilter>,
    /// Last filesystem size observation, refreshed at most once per
    /// [`Self::stat_window`] `is_full` probes. `size()` is a `stat`
    /// syscall (~1.5µs); probing it on every put of a bulk batch made the
    /// rotation check alone ~10% of batch-insert cost.
    cached_len: u64,
    /// Puts observed since `cached_len` was refreshed.
    ops_since_stat: u32,
}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segment")
            .field("id", &self.id)
            .field("path", &self.path)
            .field("state", &self.state)
            .field("device_id", &self.device_id)
            .field("has_filter", &self.key_filter.is_some())
            .finish()
    }
}

impl Segment {
    /// Adaptive probes between filesystem size observations. A segment only
    /// grows by puts in its active phase, so staleness can only delay
    /// rotation (a soft threshold), never break correctness: `stat` remains
    /// the authority whenever the window lapses. The window scales with the
    /// limit so byte-sized test segments still rotate per probe while
    /// multi-megabyte production segments skip hundreds of `stat` syscalls
    /// per observation (~1.5µs each, ~10% of bulk-insert cost unbatched).
    fn stat_window(limit: u64) -> u32 {
        (limit / 8192).clamp(1, 512) as u32
    }

    /// Creates a new segment page file on its device.
    pub(crate) fn create(
        path: PathBuf,
        id: u64,
        device_id: DeviceId,
        pool_capacity: usize,
    ) -> Result<Self> {
        let tree = BTree::create(&path, pool_capacity)?;
        Ok(Self {
            id,
            path,
            state: SegmentState::Active,
            device_id,
            tree,
            tracked_len: 0,
            cached_len: 0,
            key_filter: None,
            // Prime the window as lapsed so the first probe prices one `stat`:
            // a freshly created segment already holds B-tree metadata pages,
            // and byte-sized test limits must observe that immediately.
            ops_since_stat: u32::MAX,
        })
    }

    /// Reopens an existing segment page file.
    pub(crate) fn open(
        path: PathBuf,
        id: u64,
        device_id: DeviceId,
        pool_capacity: usize,
    ) -> Result<Self> {
        let len = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
        let tree = BTree::open(&path, pool_capacity)?;
        let key_filter = Self::load_filter(&path);
        Ok(Self {
            id,
            path,
            state: SegmentState::Sealed,
            device_id,
            tree,
            tracked_len: len,
            cached_len: len,
            key_filter,
            // Same priming as `create`: the first probe revalidates against
            // the filesystem (one `stat` per segment lifetime).
            ops_since_stat: u32::MAX,
        })
    }

    /// Current on-disk size of the segment, plus buffered dirty bytes.
    ///
    /// The filesystem size alone lags the buffer pool: pages dirtied since
    /// the last flush are invisible to `stat`, so a pure file-size check
    /// lets batched fills overshoot the rotation limit by up to a poolful
    /// before any observation catches up. Dirty bytes over-count rewritten
    /// pages (safe direction for a soft threshold).
    #[must_use]
    pub(crate) fn size(&self) -> u64 {
        let file =
            std::fs::metadata(&self.path).map_or(self.tracked_len, |metadata| metadata.len());
        // `tree` is borrowed through `&self` only for the read-only counter.
        file.saturating_add(self.tree.dirty_bytes())
    }

    /// Whether the segment reached its configured size and must be rotated.
    ///
    /// The filesystem observation is cached across a limit-adaptive window
    /// of probes (see [`Self::stat_window`]) instead of `stat`-ing on every
    /// put.
    #[must_use]
    pub(crate) fn is_full(&mut self, limit: u64) -> bool {
        if self.ops_since_stat >= Self::stat_window(limit) {
            self.cached_len = self.size();
            self.ops_since_stat = 0;
        }
        self.ops_since_stat = self.ops_since_stat.saturating_add(1);
        self.cached_len >= limit
    }

    /// Sibling sidecar path holding this segment's persisted key filter.
    ///
    /// Colocated with the segment file so placement, device ownership, and
    /// lifecycle stay identical; segment discovery only recognizes the
    /// strict `SEG-<20 digits>.dat` shape, so the sidecar is invisible to
    /// inventory scans.
    fn filter_path(&self) -> PathBuf {
        self.path.with_extension("xor")
    }

    /// Loads a persisted key filter, if one exists and validates.
    ///
    /// Any failure (missing file, torn write, version mismatch, checksum
    /// failure) yields `None`: the caller falls back to exact evaluation
    /// and rebuilds lazily. A corrupt sidecar can therefore never cause a
    /// wrong answer, only a rebuild.
    fn load_filter(path: &std::path::Path) -> Option<XorFilter> {
        let bytes = std::fs::read(path.with_extension("xor")).ok()?;
        XorFilter::deserialize(&bytes).ok()
    }

    /// Whether `key` may be present in this segment.
    ///
    /// `false` is definitive (the key is absent); `true` requires exact
    /// evaluation. Unknown (`None`) always answers `true`.
    #[must_use]
    pub(crate) fn may_contain(&self, key: &[u8]) -> bool {
        self.key_filter
            .as_ref()
            .is_none_or(|filter| filter.contains(key))
    }

    /// Whether a usable key filter is installed.
    #[must_use]
    pub(crate) fn has_filter(&self) -> bool {
        self.key_filter.is_some()
    }

    /// Builds and installs the key filter by enumerating the segment's keys.
    ///
    /// Only meaningful for sealed segments (the active segment mutates, so a
    /// filter built over it would go stale in the unsafe direction... more
    /// precisely it would MISS keys added after the build, which is a false
    /// negative and therefore forbidden). Sealed segments only lose keys
    /// (replaces/deletes of existing keys), so a filter built at seal time
    /// stays a superset forever. The image is persisted best-effort; a failed
    /// write keeps the in-memory filter, which already fixes steady state.
    /// Build failures (element budget, construction) leave `None`: exact
    /// evaluation, no behavior change.
    pub(crate) fn ensure_filter(&mut self) {
        if self.state != SegmentState::Sealed || self.key_filter.is_some() {
            return;
        }
        let keys: Vec<Vec<u8>> = match self.tree.range(None, None) {
            Ok(pairs) => pairs.into_iter().map(|(key, _)| key).collect(),
            Err(_) => return,
        };
        let filter = match XorFilter::build(keys.iter().map(Vec::as_slice)) {
            Ok(filter) => filter,
            Err(_) => return,
        };
        let path = self.filter_path();
        // Best-effort persistence: a torn file fails validation on load and
        // triggers a rebuild, so no atomic dance is required for safety.
        let _ = std::fs::write(&path, filter.serialize());
        self.key_filter = Some(filter);
    }

    /// Reads the value of `key`, if the segment holds it.
    pub(crate) fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.tree.get(key)
    }

    /// Inserts or replaces one entry.
    pub(crate) fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.tree.insert(key, value)
    }

    pub(crate) fn replace_if_present(&mut self, key: &[u8], value: &[u8]) -> Result<bool> {
        self.tree.replace_if_present(key, value)
    }

    /// Deletes one entry, returning whether it existed.
    pub(crate) fn delete(&mut self, key: &[u8]) -> Result<bool> {
        self.tree.delete(key)
    }

    /// Returns every entry between `start` and `end` in key order.
    pub(crate) fn range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.tree.range(start, end)
    }

    /// Makes every written page of the segment durable.
    pub(crate) fn sync(&mut self) -> Result<()> {
        self.tree.sync()
    }

    /// Seals the segment so it stops receiving new keys, making its pages
    /// durable before it becomes read-only.
    pub(crate) fn seal(&mut self) -> Result<()> {
        self.sync()?;
        self.state = SegmentState::Sealed;
        // The key set is now frozen (only replacements/deletes of existing
        // keys follow, which preserve the superset property), so build the
        // negative-membership filter now and persist it beside the segment.
        self.ensure_filter();
        Ok(())
    }
}
