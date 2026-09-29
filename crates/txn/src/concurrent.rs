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
//! Connection-safe facade over the single-writer production engine.
//!
//! Query preparation and transaction buffering happen without the database
//! mutex. The mutex is taken for point/range snapshots and for the short
//! WAL/data/commit publication boundary.

use crate::engine_impl::SharedVersionStore;
use crate::{PlomidStorageEngine, TransactionManager};
use plomid_core::{Result, TxnId};
use plomid_mvcc::{Snapshot, SCAN_CHUNK_ROWS};
use plomid_storage::{CommitResult, StorageEngine, StorageEngineTransaction, TransactionState};
use plomid_wal::DataOperation;
use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Instant;

/// Map of gate key -> gate, shared between the engine's gate tables and the
/// gates themselves (row gates carry a weak back-reference for eviction).
pub(crate) type GateMap = Mutex<HashMap<Vec<u8>, Arc<TransactionGate>>>;

/// Distinguishes a row-key lock domain from table-name domains, so a table
/// literally named like a row key can never alias a row gate.
fn row_domain(key: &[u8]) -> Vec<u8> {
    let mut domain = Vec::with_capacity(key.len() + 4);
    domain.extend_from_slice(b"\0R\0");
    domain.extend_from_slice(key);
    domain
}

/// Unique-reservation conflict domain: `\0U\0` + index bytes + `\0` + the
/// canonical normalized value bytes the index itself uses (`index_value_bytes`).
/// The index identity is part of the domain, so the same value under two
/// different unique indexes never shares a reservation.
fn unique_domain(index: &str, value: &[u8]) -> Vec<u8> {
    let mut domain = Vec::with_capacity(index.len() + value.len() + 5);
    domain.extend_from_slice(b"\0U\0");
    domain.extend_from_slice(index.as_bytes());
    domain.push(0);
    domain.extend_from_slice(value);
    domain
}

fn poisoned() -> plomid_core::PlomidError {
    plomid_core::PlomidError::new(plomid_core::ErrorKind::Internal, "storage mutex poisoned")
}

/// Database-wide write-lane domain for transactions that stage raw mutations
/// without a statement table (catalog bookkeeping, DDL bookkeeping).
const EMPTY_DOMAIN: &[u8] = b"";

/// How a gate is currently held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GateMode {
    /// Exclusive lane: a single writer; blocks every other acquisition.
    Exclusive,
    /// Shared lane: any number of concurrent holders; blocked only by an
    /// exclusive lane (and blocks new exclusive lanes while alive).
    Shared,
}

/// Gate occupancy: one rwlock-style state under a single mutex so waiters
/// and releasers can never deadlock against each other.
#[derive(Clone, Copy)]
struct GateState {
    exclusive: bool,
    shared: usize,
}

/// A small owned, per-database gate. It avoids a borrowed `MutexGuard`, so a
/// transaction can retain the lock while its executor performs reads and
/// stages writes outside the storage mutex.
pub(crate) struct TransactionGate {
    state: Mutex<GateState>,
    released: Condvar,
    /// Eviction identity for per-row gates: the owning map plus the key this
    /// gate is stored under. `None` for gates that live for the engine's
    /// lifetime (table gates are bounded by the table count).
    evict: Mutex<Option<(Weak<GateMap>, Vec<u8>)>>,
}

impl TransactionGate {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(GateState {
                exclusive: false,
                shared: 0,
            }),
            released: Condvar::new(),
            evict: Mutex::new(None),
        }
    }

    /// Creates a gate registered for orphan eviction from `table` under `key`.
    pub(crate) fn new_evictable(key: Vec<u8>, table: &Arc<GateMap>) -> Self {
        Self {
            state: Mutex::new(GateState {
                exclusive: false,
                shared: 0,
            }),
            released: Condvar::new(),
            evict: Mutex::new(Some((Arc::downgrade(table), key))),
        }
    }

    /// Removes this gate from its owning table once the last guard is gone
    /// and no waiter holds a clone. The strong-count check and the removal
    /// both run under the table lock, so a concurrent lookup either cloned
    /// the gate first (count > 2, entry stays and the waiter proceeds) or
    /// creates a fresh gate after removal — two live holders of one row can
    /// never end up on different gate objects.
    fn evict_if_orphaned(self: &Arc<Self>) {
        let Ok(evict) = self.evict.lock() else {
            return;
        };
        let Some((table, key)) = evict.as_ref() else {
            return;
        };
        let Some(table) = table.upgrade() else {
            return;
        };
        let Ok(mut gates) = table.lock() else {
            return;
        };
        // Expected live clones when the last guard drops: the map entry plus
        // this guard's clone. Anything more means a waiter holds a clone.
        if Arc::strong_count(self) != 2 {
            return;
        }
        if gates.get(key).is_some_and(|gate| Arc::ptr_eq(gate, self)) {
            gates.remove(key);
        }
    }

    fn acquire(self: &Arc<Self>) -> Result<TransactionGateGuard> {
        self.acquire_exclusive_inner(Some(Instant::now()))
    }

    fn acquire_exclusive_inner(
        self: &Arc<Self>,
        wait_started: Option<Instant>,
    ) -> Result<TransactionGateGuard> {
        let wait_started = wait_started.unwrap_or_else(Instant::now);
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        while state.exclusive || state.shared > 0 {
            state = self.released.wait(state).map_err(|_| poisoned())?;
        }
        state.exclusive = true;
        Ok(TransactionGateGuard {
            gate: Arc::clone(self),
            domain: Vec::new(),
            mode: GateMode::Exclusive,
            acquired_at: Instant::now(),
            wait_us: wait_started.elapsed().as_micros() as u64,
        })
    }

    /// Non-blocking acquisition for transactions that already hold another
    /// write lane. Blocking here while holding a lane could form a wait cycle
    /// across tables (A holds X wants Y while B holds Y wants X), so a
    /// transaction never waits for a second lane: on contention it fails and
    /// the executor aborts it, releasing every held lane.
    fn try_acquire(self: &Arc<Self>, domain: &[u8]) -> Result<Option<TransactionGateGuard>> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        if state.exclusive || state.shared > 0 {
            return Ok(None);
        }
        state.exclusive = true;
        Ok(Some(TransactionGateGuard {
            gate: Arc::clone(self),
            domain: domain.to_vec(),
            mode: GateMode::Exclusive,
            acquired_at: Instant::now(),
            wait_us: 0,
        }))
    }

    /// Blocking SHARED acquisition: any number of concurrent holders; waits
    /// only while an exclusive lane (conservative table writer) is held.
    /// Unique-key reservations use this so independent unique values on the
    /// same table never wait on each other, only on the conservative writer.
    fn acquire_shared(self: &Arc<Self>) -> Result<TransactionGateGuard> {
        let wait_started = Instant::now();
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        while state.exclusive {
            state = self.released.wait(state).map_err(|_| poisoned())?;
        }
        state.shared += 1;
        Ok(TransactionGateGuard {
            gate: Arc::clone(self),
            domain: Vec::new(),
            mode: GateMode::Shared,
            acquired_at: Instant::now(),
            wait_us: wait_started.elapsed().as_micros() as u64,
        })
    }

    fn release(&self, mode: GateMode) {
        if let Ok(mut state) = self.state.lock() {
            match mode {
                GateMode::Shared => {
                    state.shared = state.shared.saturating_sub(1);
                }
                GateMode::Exclusive => {
                    state.exclusive = false;
                }
            }
            self.released.notify_all();
        }
    }
}

struct TransactionGateGuard {
    gate: Arc<TransactionGate>,
    /// Statement domain this guard covers (the target table bytes), used for
    /// re-entrancy checks when a transaction touches the same table again.
    domain: Vec<u8>,
    mode: GateMode,
    acquired_at: Instant,
    wait_us: u64,
}

impl Drop for TransactionGateGuard {
    fn drop(&mut self) {
        // High-frequency (one event per gate acquire/release): DEBUG/TRACE
        // opt-in only, so bulk DML does not emit per-row INFO records.
        if tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf") {
            let row = self.domain.starts_with(b"\0R\0");
            let kind = if row {
                "row"
            } else if self.domain.starts_with(b"\0U\0") {
                "unique"
            } else {
                "table"
            };
            let mode = if self.mode == GateMode::Shared {
                "shared"
            } else {
                "exclusive"
            };
            tracing::debug!(
                target: "plomid::perf",
                event = "write_gate",
                kind,
                mode,
                wait_us = self.wait_us,
                hold_us = self.acquired_at.elapsed().as_micros() as u64,
            );
        }
        self.gate.release(self.mode);
        self.gate.evict_if_orphaned();
    }
}

pub struct ConcurrentPlomidStorageEngine {
    shared: Arc<Mutex<PlomidStorageEngine>>,
    /// Read-side state, resolved once here so a read costs neither the database
    /// mutex nor a per-read handle lookup.
    versions: SharedVersionStore,
    manager: Arc<TransactionManager>,
    complete: Arc<AtomicBool>,
    root: PathBuf,
}

impl ConcurrentPlomidStorageEngine {
    pub fn from_shared(shared: Arc<Mutex<PlomidStorageEngine>>) -> Result<Self> {
        let (root, versions, manager, complete) = {
            let engine = shared.lock().map_err(|_| poisoned())?;
            (
                engine.root().to_path_buf(),
                engine.versions_handle(),
                engine.manager_handle(),
                engine.versions_complete_flag(),
            )
        };
        Ok(Self {
            shared,
            versions,
            manager,
            complete,
            root,
        })
    }

    /// Resolves a committed read from the shared version store without the
    /// database mutex, when the completeness invariant holds.
    ///
    /// While [`PlomidStorageEngine::versions_complete_flag`] is true the store
    /// mirrors every committed key, so `store.visible`/`store.scan_visible`
    /// under `manager.snapshot(TxnId(0))` return exactly what the engine-level
    /// `get_snapshot`/`scan_snapshot` return — the same snapshot source, the
    /// same visibility function, the same payloads. The database mutex is only
    /// needed before the mount-time bootstrap completes, where the engine is
    /// the only place that can bootstrap a key from the committed image.
    ///
    /// Returns `None` when the caller must fall back to the engine.
    fn committed_read(
        &self,
    ) -> Result<
        Option<(
            std::sync::MutexGuard<'_, plomid_mvcc::VersionStore>,
            Snapshot,
        )>,
    > {
        if !self.complete.load(Ordering::Acquire) {
            return Ok(None);
        }
        let snapshot = self.manager.snapshot(TxnId::new(0))?;
        let store = self.versions.lock().map_err(|_| poisoned())?;
        Ok(Some((store, snapshot)))
    }

    /// Ordered snapshot walk paging through `scan_step`, releasing the shared
    /// version lock between chunks. `limit` caps visible rows overall.
    /// Callers pass a snapshot taken up front; see `scan_snapshot_limit` for
    /// why lock churn between chunks cannot change results.
    fn scan_paged(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        snapshot: &Snapshot,
        limit: Option<usize>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut out = Vec::new();
        let mut bound = start.map_or(Bound::Unbounded, |s| Bound::Included(s.to_vec()));
        let end_owned = end.map(|e| e.to_vec());
        loop {
            if limit.is_some_and(|cap| out.len() >= cap) {
                break;
            }
            let chunk_cap = limit
                .map(|cap| (cap - out.len()).min(SCAN_CHUNK_ROWS))
                .unwrap_or(SCAN_CHUNK_ROWS)
                .max(1);
            let (chunk, resume, completed) = {
                let store = self.versions.lock().map_err(|_| poisoned())?;
                store.scan_step(bound, end_owned.as_deref(), snapshot, chunk_cap)
            };
            out.extend(chunk);
            if completed {
                break;
            }
            match resume {
                Some(key) => {
                    bound = Bound::Excluded(key);
                }
                None => break,
            }
        }
        if let Some(cap) = limit {
            out.truncate(cap);
        }
        Ok(out)
    }
}

pub struct ConcurrentTransaction {
    shared: Arc<Mutex<PlomidStorageEngine>>,
    txn_id: TxnId,
    snapshot: Snapshot,
    /// This transaction's write overlay, keyed by storage key: the *last*
    /// staged operation per key (`Some` = put, `None` = delete). Because it is
    /// key-indexed, `get` and range `scan` resolve a key (or a range) without
    /// walking every buffered operation, and unique-index probes go through
    /// `scan` with the index-entry prefix, so a multi-row statement no longer
    /// costs O(rows) per row. Commit derives its write set from this map, so
    /// there is no second copy of the same operations to keep in sync.
    pending: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    state: TransactionState,
    /// Write lanes held by this transaction, one per distinct statement
    /// domain. A transaction holds at least one lane once it has staged a
    /// write, and every lane is released at commit/abort.
    write_gates: Vec<TransactionGateGuard>,
    /// The domains in [`Self::write_gates`], so the re-entrancy check every
    /// reservation performs is an O(1) set lookup. Scanning the gate list
    /// instead made a multi-row reservation-mode INSERT O(N^2): the list grows
    /// by every reserved unique value and row key.
    held_domains: std::collections::HashSet<Vec<u8>>,
    /// Read-side handles for committing-state reads that bypass the database
    /// mutex (see `ConcurrentPlomidStorageEngine`).
    versions: SharedVersionStore,
    complete: Arc<AtomicBool>,
}

impl ConcurrentTransaction {
    /// Committed rows in `[start, end)`: straight from the version store while
    /// it is complete, otherwise through the engine, which establishes
    /// completeness for the range before answering.
    ///
    /// This is an inherent helper rather than a trait method: it is the shared
    /// body of [`StorageEngineTransaction::scan`], which needs the committed
    /// image before applying this transaction's own overlay, and it is not part
    /// of the storage-engine contract other implementors must satisfy.
    fn committed_range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        if self.complete.load(Ordering::Acquire) {
            // Paged like the engine scans: the transaction's snapshot is
            // fixed, so lock churn between chunks cannot change results.
            let mut out = Vec::new();
            let mut bound = start.map_or(Bound::Unbounded, |s| Bound::Included(s.to_vec()));
            let end_owned = end.map(|e| e.to_vec());
            loop {
                let (chunk, resume, completed) = {
                    let store = self.versions.lock().map_err(|_| poisoned())?;
                    store.scan_step(bound, end_owned.as_deref(), &self.snapshot, SCAN_CHUNK_ROWS)
                };
                out.extend(chunk);
                if completed {
                    break;
                }
                match resume {
                    Some(key) => {
                        bound = Bound::Excluded(key);
                    }
                    None => break,
                }
            }
            return Ok(out);
        }
        self.shared
            .lock()
            .map_err(|_| poisoned())?
            .scan_snapshot(start, end, &self.snapshot)
    }
}

impl<'a> StorageEngineTransaction<'a> for ConcurrentTransaction {
    fn lock_for_write(&mut self, domain: &[u8]) -> Result<()> {
        self.ensure_active()?;
        if self.holds_domain(domain) {
            return Ok(());
        }
        let gate = {
            let engine = self.shared.lock().map_err(|_| poisoned())?;
            engine.gate_for(domain)?
        };
        // A transaction blocks only while it holds NO lane. Once it holds a
        // lane, further domains are acquired with try_acquire: blocking could
        // form a wait cycle across tables (A holds X wants Y while B holds Y
        // wants X). On contention the statement fails with a conflict and the
        // executor aborts the transaction, releasing every held lane. The
        // domain gate still guarantees that two writers on the SAME table are
        // always mutually exclusive, so no write-write conflict is ever lost.
        let guard = if self.write_gates.is_empty() {
            let mut guard = gate.acquire()?;
            guard.domain = domain.to_vec();
            guard
        } else {
            match gate.try_acquire(domain)? {
                Some(guard) => guard,
                None => {
                    return Err(plomid_core::PlomidError::with_detail(
                        plomid_core::ErrorKind::Conflict,
                        "write conflict: transaction already holds another table's write lane",
                        format!(
                            "txn_id={} held_domains={}",
                            self.txn_id.get(),
                            self.write_gates
                                .iter()
                                .map(|gate| String::from_utf8_lossy(&gate.domain).into_owned())
                                .collect::<Vec<_>>()
                                .join(",")
                        ),
                    ));
                }
            }
        };
        // Re-read the committed state at write-lane acquisition so this
        // statement's read/modify/write observes every transaction that
        // committed before the lane became ours (lost-update protection).
        self.snapshot = self
            .shared
            .lock()
            .map_err(|_| poisoned())?
            .refresh_concurrent_snapshot(self.txn_id)?;
        self.record_gate(guard);
        Ok(())
    }

    fn lock_rows(&mut self, keys: &[Vec<u8>]) -> Result<()> {
        self.ensure_active()?;
        // Canonical ascending acquisition order: within one call every key is
        // acquired strictly ascending, so any holder-waiter chain waits on a
        // key greater than every key its holder already owns — a wait-for
        // cycle would need the total byte order to cycle, which is impossible.
        let mut sorted = keys.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let pending: Vec<Vec<u8>> = sorted
            .into_iter()
            .map(|key| row_domain(&key))
            .filter(|domain| !self.holds_domain(domain))
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        // Fetch the gates under one short engine-mutex acquisition; the gates
        // themselves are acquired with NO lock held.
        let gates: Vec<Arc<TransactionGate>> = {
            let engine = self.shared.lock().map_err(|_| poisoned())?;
            let mut gates = Vec::with_capacity(pending.len());
            for domain in &pending {
                gates.push(engine.row_gate_for(domain)?);
            }
            gates
        };
        for (gate, domain) in gates.into_iter().zip(pending) {
            // Only an empty-handed transaction may block: waiting while
            // holding another lane could form a wait cycle. A transaction
            // that already holds lanes acquires further rows with
            // try_acquire and fails with a conflict on contention; the
            // executor aborts the whole transaction, releasing every lane.
            let guard = if self.write_gates.is_empty() {
                let mut guard = gate.acquire()?;
                guard.domain = domain;
                guard
            } else {
                match gate.try_acquire(&domain)? {
                    Some(mut guard) => {
                        guard.domain = domain;
                        guard
                    }
                    None => {
                        return Err(plomid_core::PlomidError::with_detail(
                            plomid_core::ErrorKind::Conflict,
                            "write conflict: transaction already holds another row's write lock",
                            format!("txn_id={}", self.txn_id.get()),
                        ));
                    }
                }
            };
            self.record_gate(guard);
        }
        // The snapshot is re-read AFTER the row locks are ours so the
        // statement's read/modify/write observes every transaction that held
        // these rows before (read-committed / EvalPlanQual semantics).
        self.snapshot = self
            .shared
            .lock()
            .map_err(|_| poisoned())?
            .refresh_concurrent_snapshot(self.txn_id)?;
        Ok(())
    }

    fn lock_shared(&mut self, domain: &[u8]) -> Result<()> {
        self.ensure_active()?;
        if self.holds_domain(domain) {
            return Ok(());
        }
        let gate = {
            let engine = self.shared.lock().map_err(|_| poisoned())?;
            engine.gate_for(domain)?
        };
        // Shared mode never blocks on other shared holders, so this is safe
        // even while the transaction already holds other lanes.
        let mut guard = gate.acquire_shared()?;
        guard.domain = domain.to_vec();
        self.record_gate(guard);
        Ok(())
    }

    fn lock_unique(&mut self, keys: &[(String, Vec<u8>)]) -> Result<()> {
        self.ensure_active()?;
        // Deterministic acquisition order: sort the (index, canonical value)
        // domains bytewise. Two transactions reserving the same key pair in
        // different orders would otherwise be able to form a wait cycle.
        let mut sorted: Vec<(String, Vec<u8>)> = keys.to_vec();
        sorted.sort();
        sorted.dedup();
        let pending: Vec<(String, Vec<u8>)> = sorted
            .into_iter()
            .filter(|(index, value)| {
                let domain = unique_domain(index, value);
                !self.holds_domain(&domain)
            })
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        let gates: Vec<(Arc<TransactionGate>, Vec<u8>)> = {
            let engine = self.shared.lock().map_err(|_| poisoned())?;
            let mut gates = Vec::with_capacity(pending.len());
            for (index, value) in &pending {
                let domain = unique_domain(index, value);
                gates.push((engine.unique_gate_for(&domain)?, domain));
            }
            gates
        };
        for (gate, domain) in gates {
            // EXCLUSIVE reservation on the conflict domain: while T1 owns it,
            // T2 either waits (empty-handed — no wait cycle is possible since
            // an empty-handed transaction holds nothing another waiter needs)
            // or fails with a conflict when it already holds other lanes (the
            // executor aborts it, releasing everything). After acquiring, the
            // statement's probe re-reads committed state, so a waiter sees the
            // winner's committed row (duplicate-key error) or the loser's
            // rollback (value free). Re-entrancy is handled by the pending
            // filter above: a transaction's own reservations are already in
            // its write_gates list.
            let guard = if self.write_gates.is_empty() {
                let mut guard = gate.acquire()?;
                guard.domain = domain;
                guard
            } else {
                match gate.try_acquire(&domain)? {
                    Some(guard) => guard,
                    None => {
                        return Err(plomid_core::PlomidError::with_detail(
                            plomid_core::ErrorKind::Conflict,
                            "write conflict: unique key is reserved by an active transaction",
                            format!("txn_id={}", self.txn_id.get()),
                        ));
                    }
                }
            };
            self.record_gate(guard);
        }
        // Re-read committed state after the reservations are ours so the
        // statement's probe sees every transaction that committed before
        // them (same read-committed reread the row locks provide).
        self.snapshot = self
            .shared
            .lock()
            .map_err(|_| poisoned())?
            .refresh_concurrent_snapshot(self.txn_id)?;
        Ok(())
    }

    fn allocate_rowid(&mut self, table: &str) -> Result<i64> {
        self.ensure_active()?;
        let row_id = self
            .shared
            .lock()
            .map_err(|_| poisoned())?
            .allocate_concurrent_rowid(table)?;
        Ok(row_id)
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.ensure_write_lane()?;
        self.pending.insert(key.to_vec(), Some(value.to_vec()));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.ensure_write_lane()?;
        self.pending.insert(key.to_vec(), None);
        Ok(())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.ensure_active()?;
        // Read-your-own-writes: the indexed overlay holds the last staged
        // operation for the key, so the newest buffered operation is one
        // O(log N) lookup away instead of a reverse walk of every operation.
        if let Some(staged) = self.pending.get(key) {
            return Ok(staged.clone());
        }
        // Fast path: resolve against the MVCC version store without the
        // database mutex. Once the store mirrors committed storage, its answer
        // for this key *is* the engine's answer, so the read needs nothing the
        // engine holds. Taking the database mutex here serialised every read on
        // every connection, which is why throughput previously peaked at four
        // concurrent readers and then fell.
        if self.complete.load(Ordering::Acquire) {
            if let Ok(store) = self.versions.lock() {
                if let Some(visible) = store.visible_or_unknown(key, &self.snapshot) {
                    return Ok(visible);
                }
            }
        }
        // Fallback: the store has never seen this key (the window before the
        // mount-time bootstrap finishes) or its mutex is poisoned. Only the
        // engine can bootstrap, so it is the only correct place to answer.
        self.shared
            .lock()
            .map_err(|_| poisoned())?
            .get_snapshot(key, &self.snapshot)
    }

    fn get_many(&mut self, keys: &[Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>> {
        self.ensure_active()?;
        let mut values: Vec<Option<Vec<u8>>> = vec![None; keys.len()];
        // Keys the statement's own overlay cannot answer, by result slot.
        let mut unresolved: Vec<usize> = Vec::with_capacity(keys.len());
        for (slot, key) in keys.iter().enumerate() {
            match self.pending.get(key) {
                Some(staged) => values[slot] = staged.clone(),
                None => unresolved.push(slot),
            }
        }
        if unresolved.is_empty() {
            return Ok(values);
        }
        // One version-store acquisition resolves the whole batch, so an index
        // probe returning K rows pays one lock instead of K. Keys the store has
        // never seen fall through to the engine, which bootstraps them.
        let mut unknown: Vec<usize> = Vec::new();
        if self.complete.load(Ordering::Acquire) {
            if let Ok(store) = self.versions.lock() {
                for &slot in &unresolved {
                    match store.visible_or_unknown(&keys[slot], &self.snapshot) {
                        Some(visible) => values[slot] = visible,
                        None => unknown.push(slot),
                    }
                }
            } else {
                unknown = unresolved.clone();
            }
        } else {
            unknown = unresolved.clone();
        }
        // The store guard is released above before the fallback takes the
        // database mutex: that path locks the store while holding the mutex, so
        // the two must never be held in the opposite order.
        if unknown.is_empty() {
            return Ok(values);
        }
        let mut engine = self.shared.lock().map_err(|_| poisoned())?;
        for &slot in &unknown {
            values[slot] = engine.get_snapshot(&keys[slot], &self.snapshot)?;
        }
        Ok(values)
    }

    fn scan(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.ensure_active()?;
        let mut rows = self.committed_range(start, end)?;
        let mut overlay = BTreeMap::from_iter(rows.drain(..));
        // Only overlay entries inside `[start, end)` can affect a row the
        // committed scan already restricted to that range, so the indexed
        // overlay is consulted for that sub-range alone: O(log N + staged-in-
        // range) instead of O(all buffered operations).
        let lower = start.map_or(Bound::Unbounded, Bound::Included);
        let upper = end.map_or(Bound::Unbounded, Bound::Excluded);
        for (key, value) in self.pending.range::<[u8], _>((lower, upper)) {
            match value {
                Some(value) => {
                    overlay.insert(key.clone(), value.clone());
                }
                None => {
                    overlay.remove(key);
                }
            }
        }
        Ok(overlay.into_iter().collect())
    }

    fn commit(&mut self) -> Result<CommitResult> {
        self.ensure_active()?;
        // Transaction-local overlay state dies with the transaction: the map
        // is drained here, so nothing can stay visible as stale state after
        // commit, and a key superseded within the transaction is written once.
        let operations: Vec<DataOperation> = std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(key, value)| match value {
                Some(value) => DataOperation::Put { key, value },
                None => DataOperation::Delete { key },
            })
            .collect();
        let encode_started = Instant::now();
        let encoded = operations
            .iter()
            .map(|operation| plomid_wal::encode_data(self.txn_id, operation))
            .collect::<Result<Vec<_>>>()?;
        let encode_us = encode_started.elapsed().as_micros() as u64;
        // Phase 1 - under the engine mutex: reserve the commit timestamp,
        // append the WAL batch, and build the write set. Short and CPU-only:
        // no fsync happens here.
        let mutex_wait_started = Instant::now();
        let mut engine = self.shared.lock().map_err(|_| poisoned())?;
        let mutex_wait_us = mutex_wait_started.elapsed().as_micros() as u64;
        let prepare_hold_started = Instant::now();
        let prepared = engine.commit_prepare(self.txn_id, operations, encoded)?;
        let group = engine.group_handle();
        let prepare_hold_us = prepare_hold_started.elapsed().as_micros() as u64;
        drop(engine);
        // Phase 2 - durability barrier with NO locks held. Concurrent commits
        // (any domain) coalesce into one WAL flush here.
        let durability_started = Instant::now();
        group.wait_durable(prepared.commit_lsn)?;
        let durability_wait_us = durability_started.elapsed().as_micros() as u64;
        // Phase 3 - under the engine mutex: apply data pages, publish the
        // commit timestamp, install MVCC versions. The write lane stays held
        // until publication so same-domain statements keep their serialized
        // read/modify/write semantics.
        let publish_wait_started = Instant::now();
        let mut engine = self.shared.lock().map_err(|_| poisoned())?;
        let publish_mutex_wait_us = publish_wait_started.elapsed().as_micros() as u64;
        let publish_hold_started = Instant::now();
        let result = engine.commit_publish(prepared)?;
        let publish_hold_us = publish_hold_started.elapsed().as_micros() as u64;
        drop(engine);
        if tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf") {
            tracing::debug!(
                target: "plomid::perf",
                event = "txn_commit",
                txn_id = self.txn_id.get(),
                encode_us,
                mutex_wait_us,
                prepare_hold_us,
                durability_wait_us,
                publish_mutex_wait_us,
                publish_hold_us,
            );
        }
        self.state = TransactionState::Committed;
        self.write_gates.clear();
        // Phase 4 - automatic checkpoint policy. The commit above is durable
        // and published, so a checkpoint here can only reclaim what the
        // durable boundary proves unreachable; it never precedes durability.
        // It runs on this thread with no gate held, and the due check reads
        // live state under the engine mutex, so a concurrent committer that
        // sees the same policy simply observes a no-op. Failures are recorded
        // by the engine and never surface as commit failures.
        {
            let mut engine = self.shared.lock().map_err(|_| poisoned())?;
            let _ = engine.checkpoint_if_due();
        }
        self.held_domains.clear();
        Ok(result)
    }

    fn abort(&mut self) -> Result<()> {
        self.ensure_active()?;
        self.shared
            .lock()
            .map_err(|_| poisoned())?
            .abort_concurrent(self.txn_id)?;
        self.pending.clear();
        self.state = TransactionState::Aborted;
        self.write_gates.clear();
        self.held_domains.clear();
        Ok(())
    }

    fn abort_preview(&mut self) -> Result<()> {
        self.ensure_active()?;
        self.shared
            .lock()
            .map_err(|_| poisoned())?
            .abort_concurrent_preview(self.txn_id)?;
        self.pending.clear();
        self.state = TransactionState::Aborted;
        self.write_gates.clear();
        self.held_domains.clear();
        Ok(())
    }

    fn txn_id(&self) -> TxnId {
        self.txn_id
    }
    fn state(&self) -> TransactionState {
        self.state
    }
}

impl ConcurrentTransaction {
    fn ensure_active(&self) -> Result<()> {
        if self.state == TransactionState::Active {
            Ok(())
        } else {
            Err(plomid_core::PlomidError::with_detail(
                plomid_core::ErrorKind::Transaction,
                "transaction is no longer active",
                format!("txn_id={}", self.txn_id.get()),
            ))
        }
    }

    /// Ensures the transaction holds SOME write lane before staging a raw
    /// mutation. Statements call [`Self::lock_for_write`] with their table
    /// domain first; direct puts from non-DML paths (catalog/DDL bookkeeping)
    /// fall back to the database-wide domain here. A transaction therefore
    /// holds at most one lane per domain, so lane acquisition can never
    /// deadlock across domains.
    fn ensure_write_lane(&mut self) -> Result<()> {
        if self.write_gates.is_empty() {
            self.lock_for_write(EMPTY_DOMAIN)?;
        }
        Ok(())
    }

    /// True when this transaction already holds `domain`.
    fn holds_domain(&self, domain: &[u8]) -> bool {
        self.held_domains.contains(domain)
    }

    /// Retains `guard` and records its domain for O(1) re-entrancy checks.
    fn record_gate(&mut self, guard: TransactionGateGuard) {
        self.held_domains.insert(guard.domain.clone());
        self.write_gates.push(guard);
    }
}

impl StorageEngine for ConcurrentPlomidStorageEngine {
    type Transaction<'a> = ConcurrentTransaction;

    fn open(storage_path: &Path, wal_path: &Path, pool_capacity: usize) -> Result<Self> {
        let engine = PlomidStorageEngine::open(storage_path, wal_path, pool_capacity)?;
        Self::from_shared(Arc::new(Mutex::new(engine)))
    }

    fn create(storage_path: &Path, wal_path: &Path, pool_capacity: usize) -> Result<Self> {
        let engine = PlomidStorageEngine::create(storage_path, wal_path, pool_capacity)?;
        Self::from_shared(Arc::new(Mutex::new(engine)))
    }

    fn begin(&mut self) -> Result<Self::Transaction<'_>> {
        let (txn_id, snapshot) = self
            .shared
            .lock()
            .map_err(|_| poisoned())?
            .begin_concurrent()?;
        Ok(ConcurrentTransaction {
            shared: Arc::clone(&self.shared),
            txn_id,
            snapshot,
            pending: BTreeMap::new(),
            state: TransactionState::Active,
            write_gates: Vec::new(),
            held_domains: std::collections::HashSet::new(),
            versions: Arc::clone(&self.versions),
            complete: Arc::clone(&self.complete),
        })
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        // Fast path: the store mirrors committed storage, so the answer needs
        // nothing the database mutex protects. This is the read every
        // autocommit statement in the server takes, so leaving it on the
        // global mutex serialized all reads on all connections.
        if let Some((store, snapshot)) = self.committed_read()? {
            return Ok(match store.visible(key, &snapshot) {
                Some(version) if version.state == plomid_mvcc::VersionState::Deleted => None,
                Some(version) => version.payload.clone(),
                None => None,
            });
        }
        let mut engine = self.shared.lock().map_err(|_| poisoned())?;
        let snapshot = engine.committed_snapshot()?;
        engine.get_snapshot(key, &snapshot)
    }

    fn scan(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        if self.complete.load(Ordering::Acquire) {
            // Paged walk: one snapshot up front, version lock released per
            // chunk, so a large scan never holds the shared lock across its
            // whole walk (see `scan_snapshot_limit` for the safety argument).
            let snapshot = self.manager.snapshot(TxnId::new(0))?;
            return self.scan_paged(start, end, &snapshot, None);
        }
        let mut engine = self.shared.lock().map_err(|_| poisoned())?;
        let snapshot = engine.committed_snapshot()?;
        engine.scan_snapshot(start, end, &snapshot)
    }

    fn get_many(&mut self, keys: &[Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>> {
        // The whole candidate batch resolves under one snapshot and one store
        // acquisition: an index probe returning K rows costs one lock instead
        // of K, and all K are guaranteed the same commit horizon.
        if let Some((store, snapshot)) = self.committed_read()? {
            return Ok(keys
                .iter()
                .map(|key| match store.visible(key, &snapshot) {
                    Some(version) if version.state == plomid_mvcc::VersionState::Deleted => None,
                    Some(version) => version.payload.clone(),
                    None => None,
                })
                .collect());
        }
        let mut engine = self.shared.lock().map_err(|_| poisoned())?;
        let snapshot = engine.committed_snapshot()?;
        engine.get_many_snapshot(keys, &snapshot)
    }

    fn sync(&mut self) -> Result<()> {
        self.shared.lock().map_err(|_| poisoned())?.sync()
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn mvcc_safety(&self) -> Result<(Vec<u64>, u64)> {
        self.shared.lock().map_err(|_| poisoned())?.mvcc_safety()
    }

    fn txn_issue_mark(&self) -> Result<u64> {
        self.shared.lock().map_err(|_| poisoned())?.txn_issue_mark()
    }

    fn scan_limit(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        // Fast path: resolve under one snapshot with the lock released per
        // chunk, stopping the walk after `limit` visible rows.
        if self.complete.load(Ordering::Acquire) {
            let snapshot = self.manager.snapshot(TxnId::new(0))?;
            return self.scan_paged(start, end, &snapshot, Some(limit));
        }
        let mut engine = self.shared.lock().map_err(|_| poisoned())?;
        let snapshot = engine.committed_snapshot()?;
        engine.scan_snapshot_limit(start, end, &snapshot, Some(limit))
    }

    fn count_range(&mut self, start: Option<&[u8]>, end: Option<&[u8]>) -> Result<u64> {
        if self.complete.load(Ordering::Acquire) {
            // Same paging discipline as scans: count in bounded chunks so a
            // full-table COUNT(*) never holds the shared lock across 1M rows.
            let snapshot = self.manager.snapshot(TxnId::new(0))?;
            let mut total = 0u64;
            let mut bound = start.map_or(Bound::Unbounded, |s| Bound::Included(s.to_vec()));
            let end_owned = end.map(|e| e.to_vec());
            loop {
                let (n, resume, completed) = {
                    let store = self.versions.lock().map_err(|_| poisoned())?;
                    store.count_step(bound, end_owned.as_deref(), &snapshot, SCAN_CHUNK_ROWS)
                };
                total += n;
                if completed {
                    break;
                }
                match resume {
                    Some(key) => {
                        bound = Bound::Excluded(key);
                    }
                    None => break,
                }
            }
            return Ok(total);
        }
        let mut engine = self.shared.lock().map_err(|_| poisoned())?;
        let snapshot = engine.committed_snapshot()?;
        engine.count_snapshot(start, end, &snapshot)
    }

    fn scan_for_each(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        chunk_size: usize,
        f: &mut dyn FnMut(&[(Vec<u8>, Vec<u8>)]) -> Result<bool>,
    ) -> Result<()> {
        // Autocommit analytical path: one snapshot up front, version lock
        // released per chunk, so a 1M-row scan never holds the shared lock
        // across its whole walk and never materializes the full table.
        // With staged writes pending the overlay could affect visibility, so
        // fall back to the exact full-scan behavior (chunked at the callback
        // boundary only).
        if self.complete.load(Ordering::Acquire) {
            let snapshot = self.manager.snapshot(TxnId::new(0))?;
            let chunk_cap = chunk_size.max(1);
            let mut bound = start.map_or(Bound::Unbounded, |s| Bound::Included(s.to_vec()));
            let end_owned = end.map(|e| e.to_vec());
            loop {
                let (chunk, resume, completed) = {
                    let store = self.versions.lock().map_err(|_| poisoned())?;
                    store.scan_step(bound, end_owned.as_deref(), &snapshot, chunk_cap)
                };
                if !chunk.is_empty() && !f(&chunk)? {
                    break;
                }
                if completed {
                    break;
                }
                match resume {
                    Some(key) => {
                        bound = Bound::Excluded(key);
                    }
                    None => break,
                }
                if chunk.is_empty() {
                    continue;
                }
            }
            return Ok(());
        }
        let rows = self.scan(start, end)?;
        let chunk = chunk_size.max(1);
        for window in rows.chunks(chunk) {
            if !f(window)? {
                break;
            }
        }
        Ok(())
    }
}
