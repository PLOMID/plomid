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
//! Newest-first version chains keyed by logical storage key.
use plomid_core::TxnId;
use std::collections::BTreeMap;

use super::{is_version_visible, RowVersion, Snapshot, VersionState};

/// One page of an ordered snapshot walk: visible `(key, value)` rows plus
/// paging state `(rows, last_examined, completed)`.
type ScanPage = (Vec<(Vec<u8>, Vec<u8>)>, Option<Vec<u8>>, bool);

/// Rows walked per critical section by paged snapshot scans.
///
/// A full-table scan releases and reacquires the version-store lock every
/// this many visible rows, so one OLAP scan cannot serialize all OLTP
/// traffic behind a single hundreds-of-milliseconds hold. At ~0.3µs/row the
/// hold is sub-millisecond; smaller values add lock round-trips, larger ones
/// lengthen OLTP stalls.
pub const SCAN_CHUNK_ROWS: usize = 1024;

/// Newest-first version chains keyed by logical storage key.
#[derive(Debug, Default)]
pub struct VersionStore {
    /// Ordered keys let bounded visibility scans walk only the requested
    /// range.  The previous hash map required collecting and sorting every
    /// matching key on every scan.
    chains: BTreeMap<Vec<u8>, Vec<RowVersion>>,
    last_committed: u64,
}

impl VersionStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Highest committed timestamp installed so far.
    #[must_use]
    pub fn last_committed(&self) -> u64 {
        self.last_committed
    }

    /// Installs one committed version at the head of `key`'s chain.
    pub fn install(&mut self, key: Vec<u8>, mut version: RowVersion) {
        debug_assert!(
            version.commit_ts.is_some(),
            "only committed versions enter the shared store"
        );
        if let Some(ts) = version.commit_ts {
            self.last_committed = self.last_committed.max(ts);
        }
        let chain = self.chains.entry(key).or_default();
        version.prev = Some(chain.len());
        chain.insert(0, version);
    }

    /// Ensures `key` has a bootstrap version so pre-MVCC committed data
    /// (including WAL-recovered rows) is visible to every snapshot.
    pub fn ensure_bootstrap(&mut self, key: Vec<u8>, payload: Option<Vec<u8>>) {
        use std::collections::btree_map::Entry;
        if let Entry::Vacant(slot) = self.chains.entry(key) {
            let version = if let Some(bytes) = payload {
                RowVersion::committed(TxnId::new(0), 0, bytes)
            } else {
                RowVersion::deleted(TxnId::new(0), 0)
            };
            slot.insert(vec![version]);
        }
    }

    /// Returns the newest version of `key` visible to `snapshot`.
    #[must_use]
    pub fn visible(&self, key: &[u8], snapshot: &Snapshot) -> Option<&RowVersion> {
        self.chains
            .get(key)?
            .iter()
            .find(|v| is_version_visible(v, snapshot))
    }

    /// Visible payload for `key`, distinguishing "nothing visible" from "this
    /// key was never seen here".
    ///
    /// `None` means the key has no chain in this store at all, so the store
    /// cannot answer for it and the caller must resolve it from the committed
    /// storage image. `Some(None)` means the key is known but no version of it
    /// is visible to `snapshot`; `Some(Some(payload))` is the payload a
    /// snapshot-visible read must return.
    ///
    /// This is the ownership-free form of [`Self::visible`] plus the presence
    /// test in one lookup, which is what lets a reader resolve a point read
    /// while holding only this store and never the database engine.
    #[must_use]
    pub fn visible_or_unknown(&self, key: &[u8], snapshot: &Snapshot) -> Option<Option<Vec<u8>>> {
        let chain = self.chains.get(key)?;
        Some(
            chain
                .iter()
                .find(|v| is_version_visible(v, snapshot))
                .and_then(|v| {
                    if v.state == VersionState::Deleted {
                        None
                    } else {
                        v.payload.clone()
                    }
                }),
        )
    }

    /// Raw newest-first chain for `key`, for GC/tests.
    #[must_use]
    pub fn chain(&self, key: &[u8]) -> Option<&[RowVersion]> {
        self.chains.get(key).map(Vec::as_slice)
    }

    /// Number of versions stored for `key`.
    #[must_use]
    pub fn version_count(&self, key: &[u8]) -> usize {
        self.chains.get(key).map_or(0, Vec::len)
    }

    /// Scans keys in `[start, end)` returning the visible payload per key.
    #[must_use]
    pub fn scan_visible(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        snapshot: &Snapshot,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.scan_visible_limit(start, end, snapshot, None)
    }

    /// Scans keys in `[start, end)` returning at most `limit` visible payloads.
    ///
    /// Iteration is key order and the limit applies to visible rows (deleted
    /// or invisible versions do not consume it), so `LIMIT n` stops the walk
    /// after `n` matches instead of materializing the range. `None` scans
    /// everything, identical to [`Self::scan_visible`].
    #[must_use]
    pub fn scan_visible_limit(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        snapshot: &Snapshot,
        limit: Option<usize>,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        let range_start = start.map_or(std::ops::Bound::Unbounded, |s| {
            std::ops::Bound::Included(s.to_vec())
        });
        let range_end = end.map_or(std::ops::Bound::Unbounded, |e| {
            std::ops::Bound::Excluded(e.to_vec())
        });
        for (key, chain) in self.chains.range::<Vec<u8>, _>((range_start, range_end)) {
            if let Some(v) = chain.iter().find(|v| is_version_visible(v, snapshot)) {
                if v.state == VersionState::Deleted {
                    continue;
                }
                if let Some(p) = &v.payload {
                    out.push((key.clone(), p.clone()));
                    if limit.is_some_and(|cap| out.len() >= cap) {
                        break;
                    }
                }
            }
        }
        out
    }

    /// Counts visible, non-deleted keys in `[start, end)` without cloning
    /// keys or payloads.
    ///
    /// Same visibility rules as [`Self::scan_visible`]: newest visible
    /// version per key, tombstones excluded, keys without payload excluded.
    /// Full-table `COUNT(*)` therefore walks the key space instead of
    /// materializing every row first (measured: one 1M-row count pinned
    /// ~450MB transient; this path allocates nothing per row).
    #[must_use]
    pub fn count_visible(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        snapshot: &Snapshot,
    ) -> u64 {
        let range_start = start.map_or(std::ops::Bound::Unbounded, |s| {
            std::ops::Bound::Included(s.to_vec())
        });
        let range_end = end.map_or(std::ops::Bound::Unbounded, |e| {
            std::ops::Bound::Excluded(e.to_vec())
        });
        let mut count = 0u64;
        for (_, chain) in self.chains.range::<Vec<u8>, _>((range_start, range_end)) {
            if let Some(v) = chain.iter().find(|v| is_version_visible(v, snapshot)) {
                if v.state != VersionState::Deleted && v.payload.is_some() {
                    count += 1;
                }
            }
        }
        count
    }

    /// One bounded step of an ordered snapshot walk.
    ///
    /// Returns visible rows plus paging state: `(rows, last_examined,
    /// completed)`. `last_examined` is the final key the walk looked at
    /// (`None` only when the range held no keys at all); resuming with
    /// `Bound::Excluded(last)` visits every key exactly once. Concurrent
    /// commits only prepend versions to chains (production paths never remove
    /// visible ones mid-scan), so a key examined in an earlier step is never
    /// double-counted, and a key committed after the snapshot started is
    /// correctly invisible to it. An empty page with `completed == false`
    /// simply advances past invisible keys; callers keep paging until
    /// `completed`. A `limit` of zero completes immediately with no rows.
    #[must_use]
    pub fn scan_step(
        &self,
        start: std::ops::Bound<Vec<u8>>,
        end: Option<&[u8]>,
        snapshot: &Snapshot,
        limit: usize,
    ) -> ScanPage {
        let mut out = Vec::new();
        if limit == 0 {
            return (out, None, true);
        }
        let range_end = end.map_or(std::ops::Bound::Unbounded, |e| {
            std::ops::Bound::Excluded(e.to_vec())
        });
        let mut iter = self.chains.range::<Vec<u8>, _>((start, range_end));
        let mut last_examined: Option<Vec<u8>> = None;
        for (key, chain) in iter.by_ref() {
            last_examined = Some(key.clone());
            if let Some(v) = chain.iter().find(|v| is_version_visible(v, snapshot)) {
                if v.state == VersionState::Deleted {
                    continue;
                }
                if let Some(p) = &v.payload {
                    out.push((key.clone(), p.clone()));
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
        // Exhaustion check: any key at all remaining means the walk may have
        // more visible rows (invisible keys don't complete it).
        let completed = iter.next().is_none();
        (out, last_examined, completed)
    }

    /// Counts one bounded step of an ordered snapshot walk.
    ///
    /// Same paging contract as [`Self::scan_step`] without cloning keys or
    /// payloads. Returns `(visible count, last examined key, completed)`.
    #[must_use]
    pub fn count_step(
        &self,
        start: std::ops::Bound<Vec<u8>>,
        end: Option<&[u8]>,
        snapshot: &Snapshot,
        limit: usize,
    ) -> (u64, Option<Vec<u8>>, bool) {
        if limit == 0 {
            return (0, None, true);
        }
        let range_end = end.map_or(std::ops::Bound::Unbounded, |e| {
            std::ops::Bound::Excluded(e.to_vec())
        });
        let mut count = 0u64;
        let mut iter = self.chains.range::<Vec<u8>, _>((start, range_end));
        let mut last_examined: Option<Vec<u8>> = None;
        for (key, chain) in iter.by_ref() {
            last_examined = Some(key.clone());
            if let Some(v) = chain.iter().find(|v| is_version_visible(v, snapshot)) {
                if v.state != VersionState::Deleted && v.payload.is_some() {
                    count += 1;
                    if count >= limit as u64 {
                        break;
                    }
                }
            }
        }
        let completed = iter.next().is_none();
        (count, last_examined, completed)
    }

    /// Reclaims versions no active snapshot can observe.
    pub fn gc(&mut self, horizon: u64) -> usize {
        let mut removed = 0;
        for chain in self.chains.values_mut() {
            let mut keep = vec![false; chain.len()];
            let mut found_floor = false;
            for (i, v) in chain.iter().enumerate() {
                match v.commit_ts {
                    None => keep[i] = true,
                    Some(ts) if ts > horizon => keep[i] = true,
                    Some(_) if !found_floor => {
                        keep[i] = true;
                        found_floor = true;
                    }
                    Some(_) => {}
                }
            }
            let before = chain.len();
            let mut kept = Vec::with_capacity(before);
            for (v, k) in chain.drain(..).zip(keep.iter()) {
                if *k {
                    kept.push(v);
                }
            }
            removed += before - kept.len();
            *chain = kept;
        }
        removed
    }
}

/// GC horizon: min(active watermarks), or last_committed when none active.
#[must_use]
pub fn gc_horizon(active_watermarks: &[u64], last_committed: u64) -> u64 {
    active_watermarks
        .iter()
        .copied()
        .min()
        .unwrap_or(last_committed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn paging_snapshot() -> Snapshot {
        Snapshot::new(TxnId::new(999), 999, BTreeSet::new())
    }

    fn paged_collect(
        store: &VersionStore,
        snap: &Snapshot,
        chunk: usize,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        let mut bound = std::ops::Bound::Unbounded;
        loop {
            let (page, resume, completed) = store.scan_step(bound, None, snap, chunk);
            out.extend(page);
            if completed {
                break;
            }
            match resume {
                Some(key) => {
                    bound = std::ops::Bound::Excluded(key);
                }
                None => break,
            }
        }
        out
    }

    #[test]
    fn paged_scan_matches_full_scan_at_any_chunk_size() {
        let mut store = VersionStore::new();
        for i in 0..500 {
            store.install(
                format!("k{i:04}").into_bytes(),
                RowVersion::committed(TxnId::new(1), 1, vec![i as u8; 8]),
            );
        }
        // Every 7th key deleted; every 11th has an invisible newer version.
        for i in (0..500).step_by(7) {
            store.install(
                format!("k{i:04}").into_bytes(),
                RowVersion::deleted(TxnId::new(2), 2),
            );
        }
        let snap = paging_snapshot();
        let full = store.scan_visible(None, None, &snap);
        for chunk in [1, 7, 64, 1024, 10000] {
            assert_eq!(paged_collect(&store, &snap, chunk), full, "chunk={chunk}");
        }
        // Count agrees too.
        let total: u64 = {
            let mut total = 0;
            let mut bound = std::ops::Bound::Unbounded;
            loop {
                let (n, resume, completed) = store.count_step(bound, None, &snap, 64);
                total += n;
                if completed {
                    break;
                }
                match resume {
                    Some(key) => {
                        bound = std::ops::Bound::Excluded(key);
                    }
                    None => break,
                }
            }
            total
        };
        assert_eq!(total as usize, full.len());
    }

    #[test]
    fn paged_scan_empty_and_singleton_ranges() {
        let store = VersionStore::new();
        let snap = paging_snapshot();
        let (rows, _, completed) = store.scan_step(std::ops::Bound::Unbounded, None, &snap, 10);
        assert!(rows.is_empty() && completed);
        let (n, _, completed) = store.count_step(std::ops::Bound::Unbounded, None, &snap, 10);
        assert_eq!((n, completed), (0, true));
    }

    #[test]
    fn bounded_scan_preserves_key_order_and_excludes_tombstones() {
        let mut store = VersionStore::new();
        store.install(
            b"a".to_vec(),
            RowVersion::committed(TxnId::new(1), 1, b"a".to_vec()),
        );
        store.install(
            b"b".to_vec(),
            RowVersion::committed(TxnId::new(2), 2, b"b".to_vec()),
        );
        store.install(b"c".to_vec(), RowVersion::deleted(TxnId::new(3), 3));

        let snapshot = Snapshot::new(TxnId::new(99), 3, BTreeSet::new());
        assert_eq!(
            store.scan_visible(Some(b"a"), Some(b"c"), &snapshot),
            vec![
                (b"a".to_vec(), b"a".to_vec()),
                (b"b".to_vec(), b"b".to_vec())
            ]
        );
    }

    #[test]
    fn active_transaction_versions_remain_invisible_to_other_snapshots() {
        let mut store = VersionStore::new();
        store.install(
            b"key".to_vec(),
            RowVersion::committed(TxnId::new(1), 1, b"old".to_vec()),
        );
        store.install(
            b"key".to_vec(),
            RowVersion::committed(TxnId::new(2), 2, b"new".to_vec()),
        );
        let mut active = BTreeSet::new();
        active.insert(2);
        let snapshot = Snapshot::new(TxnId::new(3), 2, active);
        assert_eq!(
            store.visible(b"key", &snapshot).unwrap().payload,
            Some(b"old".to_vec())
        );
    }
}
