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
//! Bounded background maintenance worker.
//!
//! Post-commit generation passes (columnar flush, compaction, index builds)
//! cost tens of milliseconds even in release builds — running them on the
//! committing connection inflates write p99 ~13× (measured 12ms → 153ms).
//! This module moves that work off the foreground commit path without
//! changing what the work does or when it is safe.
//!
//! # Design
//!
//! * One dedicated thread per worker, driven by a bounded channel. Sessions
//!   submit `(database, table)` items when their policy says a table is due;
//!   the worker runs the same [`maintain_table`](crate::ddl::maintain_table)
//!   the foreground path ran, through its own engine facade and a freshly
//!   loaded catalog (so DDL-visible state is never stale).
//! * Single ownership: a process-wide in-flight set coalesces duplicate
//!   submissions — at most one queued-or-running item per table, so a write
//!   burst cannot pile up redundant full materializations and memory stays
//!   bounded by (tables × one item).
//! * Failures are non-fatal exactly like foreground passes: the item is
//!   dropped, the in-flight mark is released, and the submitting sessions'
//!   accounting still says "due", so the next commit resubmits. Nothing is
//!   lost; nothing is retried blindly.
//! * Backpressure without debt: a full channel makes `submit` return false,
//!   and the caller runs the pass inline — precisely today's foreground
//!   behavior. Overload therefore degrades to the status quo, never to
//!   unbounded accumulation.
//! * Crash safety is inherited, not reimplemented: the pass is the same
//!   WAL + atomic-publication sequence the kill-9 tests cover. A kill
//!   mid-pass recovers like a kill mid-`VACUUM`. Unprocessed queued items
//!   vanish with the process; new sessions re-derive the debt from write
//!   activity and resubmit.
//! * Shutdown is cooperative and bounded by one pass: the flag stops intake,
//!   the loop drains nothing new, and `shutdown` joins the thread. A pass in
//!   flight finishes (it is crash-safe at every boundary, so waiting is
//!   always sound).
//!
//! The worker never touches session ART (a per-session derived cache, rebuilt
//! lazily on demand) and never changes transaction, snapshot, WAL, or
//! durability behavior: foreground commits still WAL-sync before responding.

use crate::ddl::maintain_table;
use crate::maintenance::{
    auto_pass_allowed, last_pass_duration, maintenance_claim_key, record_auto_pass,
    MaintenanceClaim, MaintenancePolicy,
};
use plomid_sql::load_catalog;
use plomid_storage::DatabaseLayout;
use plomid_txn::{ConcurrentPlomidStorageEngine, PlomidStorageEngine, StorageEngine};
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{sync_channel, SyncSender},
    Arc, Mutex,
};

/// Default bound on queued-but-unstarted maintenance items.
pub const DEFAULT_WORKER_QUEUE_BOUND: usize = 1024;

/// One maintenance request: bring `table` in `database` up to a maintained
/// generation.
struct WorkItem {
    database: String,
    table: String,
    /// Earliest time this item may run. `None` for fresh submissions;
    /// gate-denied items carry the moment the burst interval lapses so the
    /// worker sleeps through the remainder instead of hot-spinning.
    not_before: Option<std::time::Instant>,
}

/// Shared submission endpoint. Cloneable; every session holds one when the
/// server installs background maintenance.
#[derive(Clone)]
pub struct WorkerLink {
    tx: SyncSender<WorkItem>,
    /// Storage root this worker serves. Submissions for other roots decline
    /// so those sessions run inline passes (e.g. secondary databases).
    root: Arc<std::path::PathBuf>,
    /// Tables currently queued or running (coalescing set).
    inflight: Arc<Mutex<HashSet<(String, String)>>>,
    /// Set when the worker is shutting down: submissions decline so callers
    /// fall back to inline passes instead of queueing into a dead worker.
    stopped: Arc<AtomicBool>,
    submitted: Arc<AtomicU64>,
    completed: Arc<AtomicU64>,
    inline_fallbacks: Arc<AtomicU64>,
}

impl WorkerLink {
    /// Submits one table for background maintenance.
    ///
    /// Returns true when the worker owns the item (queued or already
    /// in-flight for it). Returns false when the worker is stopped, serves a
    /// different storage root, or the queue is full — the caller must run the
    /// pass inline, which is exactly the historical foreground behavior.
    pub fn submit(&self, root: &std::path::Path, database: &str, table: &str) -> bool {
        if self.stopped.load(Ordering::Acquire) {
            return false;
        }
        if root != self.root.as_path() {
            return false;
        }
        let key = (database.to_string(), table.to_string());
        {
            let mut inflight = match self.inflight.lock() {
                Ok(guard) => guard,
                Err(_) => return false,
            };
            if !inflight.insert(key.clone()) {
                // Already queued or running: the owner will finish it, and
                // the session's own accounting still says "due" so nothing
                // is lost if this particular pass is skipped.
                return true;
            }
        }
        if self
            .tx
            .try_send(WorkItem {
                database: key.0.clone(),
                table: key.1.clone(),
                not_before: None,
            })
            .is_err()
        {
            // Bounded queue is full (or the worker died): release the mark
            // and report inline so the caller degrades to foreground work.
            if let Ok(mut inflight) = self.inflight.lock() {
                inflight.remove(&key);
            }
            self.inline_fallbacks.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.submitted.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Tables currently queued or running (bounded by design).
    pub fn queue_depth(&self) -> usize {
        self.inflight.lock().ok().map(|set| set.len()).unwrap_or(0)
    }

    /// Re-queues a worker-owned item (burst-gate deferral) without touching
    /// the in-flight set: the item is already owned by the worker, so no
    /// dedup decision is needed. Returns false when the bounded queue is
    /// full, in which case the caller must release the mark (a held mark
    /// with no queued item would black-hole future submissions).
    fn requeue(&self, item: WorkItem) -> bool {
        self.tx.try_send(item).is_ok()
    }

    /// Releases one in-flight mark (queue-full fallback for deferred items).
    fn release_mark(&self, database: &str, table: &str) {
        release(&self.inflight, &(database.to_string(), table.to_string()));
    }

    /// Lifetime counters for observability: submitted, completed, and
    /// submissions declined back to inline execution.
    pub fn stats(&self) -> (u64, u64, u64) {
        (
            self.submitted.load(Ordering::Relaxed),
            self.completed.load(Ordering::Relaxed),
            self.inline_fallbacks.load(Ordering::Relaxed),
        )
    }
}

/// Handle to a running background maintenance worker.
pub struct MaintenanceWorker {
    link: WorkerLink,
    stopped: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MaintenanceWorker {
    /// Spawns the worker over the shared authoritative engine.
    ///
    /// The worker derives its own concurrent facade, so its passes never
    /// borrow session state. Catalogs are loaded fresh per item.
    pub fn spawn(engine: Arc<Mutex<PlomidStorageEngine>>) -> Self {
        Self::spawn_bounded(engine, DEFAULT_WORKER_QUEUE_BOUND)
    }

    /// Spawns the worker with an explicit queue bound (tests use small
    /// bounds to prove backpressure deterministically).
    pub fn spawn_bounded(engine: Arc<Mutex<PlomidStorageEngine>>, bound: usize) -> Self {
        let (tx, rx) = sync_channel::<WorkItem>(bound.max(1));
        let stopped = Arc::new(AtomicBool::new(false));
        // Snapshot the served root once: submissions for other roots (e.g.
        // secondary-database engines) decline to inline passes.
        let worker_engine = Arc::clone(&engine);
        let root = engine
            .lock()
            .map(|guard| guard.root().to_path_buf())
            .unwrap_or_default();
        let link = WorkerLink {
            tx,
            root: Arc::new(root),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            stopped: Arc::clone(&stopped),
            submitted: Arc::new(AtomicU64::new(0)),
            completed: Arc::new(AtomicU64::new(0)),
            inline_fallbacks: Arc::new(AtomicU64::new(0)),
        };
        let thread_link = link.clone();
        let thread_stopped = Arc::clone(&stopped);
        let thread = std::thread::Builder::new()
            .name("plomid-maintenance".to_string())
            .spawn(move || {
                worker_loop(worker_engine, rx, thread_link, thread_stopped);
            })
            .expect("maintenance worker thread spawns");
        Self {
            link,
            stopped,
            thread: Some(thread),
        }
    }

    /// Submission endpoint handed to sessions.
    pub fn link(&self) -> WorkerLink {
        self.link.clone()
    }

    /// Signals shutdown and waits for the in-flight pass, if any.
    ///
    /// Queued-but-unstarted items are dropped; their debt re-derives from
    /// session write accounting after restart. Joining is bounded by one
    /// pass, which is crash-safe at every boundary.
    pub fn shutdown(mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn worker_loop(
    engine: Arc<Mutex<PlomidStorageEngine>>,
    rx: std::sync::mpsc::Receiver<WorkItem>,
    link: WorkerLink,
    stopped: Arc<AtomicBool>,
) {
    let facade = ConcurrentPlomidStorageEngine::from_shared(engine);
    let mut facade = match facade {
        Ok(facade) => facade,
        Err(error) => {
            tracing::warn!(
                target: "sql::maintenance",
                "background worker cannot attach to shared engine error={}",
                error
            );
            return;
        }
    };
    let policy = MaintenancePolicy::production();
    loop {
        if stopped.load(Ordering::Acquire) {
            break;
        }
        let item = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(item) => item,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // Deferred items (burst-gate denials) cycle to the back until their
        // deadline; the 100ms cadence bounds the spin and other tables'
        // items are unaffected in between.
        if item
            .not_before
            .is_some_and(|at| std::time::Instant::now() < at)
        {
            let (database, table) = (item.database.clone(), item.table.clone());
            if !link.requeue(item) {
                // Queue full with the mark held would black-hole
                // submissions; release so the next commit resubmits
                // (historical drop path).
                link.release_mark(&database, &table);
            }
            continue;
        }
        process_item(&mut facade, &policy, &item, &link);
    }
}

/// Runs one queued item: fresh catalog, single-flight claim, burst gate,
/// then the same [`maintain_table`] the foreground path runs.
fn process_item(
    engine: &mut ConcurrentPlomidStorageEngine,
    policy: &MaintenancePolicy,
    item: &WorkItem,
    link: &WorkerLink,
) {
    let key = (item.database.clone(), item.table.clone());
    // Fresh catalog per item: tables created after worker spawn must resolve.
    // A catalog that cannot load aborts this item; sessions keep their own
    // accounting and will resubmit on later commits.
    let catalog = match load_catalog(engine) {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(
                target: "sql::maintenance",
                "background worker catalog load failed database={} table={} error={}",
                item.database,
                item.table,
                error
            );
            release(&link.inflight, &key);
            return;
        }
    };
    let root = engine.root().to_path_buf();
    let layout = DatabaseLayout::new(&root);
    let claim_key = maintenance_claim_key(&root, &item.database, &item.table);
    // Same non-blocking discipline as foreground passes: claim conflicts and
    // burst-gate denials skip (the submitting sessions still say "due" and
    // will resubmit), never wait.
    let Some(_claim) = MaintenanceClaim::try_acquire(claim_key.clone()) else {
        release(&link.inflight, &key);
        return;
    };
    if !auto_pass_allowed(&claim_key, policy) {
        // Burst-gate denial is not a drop: with no further commits nothing
        // would ever resubmit, leaving a bulk-loaded table unfresh forever.
        // Requeue for the moment the interval lapses (dated from the last
        // completed pass, so repeated denials never drift later), keeping
        // the in-flight mark so concurrent submissions coalesce into this
        // single pending pass. The worker loop cycles it to the back until
        // the deadline; a full queue releases the mark (historical drop).
        let not_before = crate::maintenance::last_pass_at(&claim_key).map(|last| {
            last + crate::maintenance::effective_auto_interval(
                policy,
                last_pass_duration(&claim_key),
            )
        });
        let item = WorkItem {
            database: item.database.clone(),
            table: item.table.clone(),
            not_before,
        };
        if !link.requeue(item) {
            release(&link.inflight, &key);
        }
        return;
    }
    let started = std::time::Instant::now();
    // Write watermark for the staleness check below: commits landing after
    // this point are not covered by the coming pass.
    let freshness_key =
        crate::columnar_freshness::freshness_key(&root, &item.database, &item.table);
    let writes_before = crate::columnar_freshness::writes_since_boot(&freshness_key);
    let outcome = maintain_table(
        engine,
        &catalog,
        &layout,
        &item.database,
        &item.table,
        plomid_columnar::ColumnarFailPoint::None,
    );
    match outcome {
        Ok(()) => {
            record_auto_pass(&claim_key, started.elapsed());
            link.completed.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                target: "sql::maintenance",
                "background pass complete database={} table={} elapsed_ms={}",
                item.database,
                item.table,
                started.elapsed().as_millis() as u64,
            );
            // Stale-success resubmit: commits landed inside this pass's
            // window, so its result cannot establish freshness — and with
            // no further commits nothing would ever resubmit. Re-queue one
            // follow-up pass (after releasing, so it queues normally); the
            // burst gate on the next run throttles chains under continuous
            // write load. Failures never resubmit: a deterministically
            // failing pass must not loop, and later commits resubmit anyway.
            if crate::columnar_freshness::writes_since_boot(&freshness_key) != writes_before {
                release(&link.inflight, &key);
                link.submit(&root, &item.database, &item.table);
                return;
            }
        }
        Err(error) => {
            // Failures are recorded, never raised: committed work never
            // depends on maintenance, and later commits resubmit.
            tracing::warn!(
                target: "sql::maintenance",
                "background pass failed database={} table={} error={}",
                item.database,
                item.table,
                error
            );
        }
    }
    release(&link.inflight, &key);
}

fn release(inflight: &Mutex<HashSet<(String, String)>>, key: &(String, String)) {
    if let Ok(mut guard) = inflight.lock() {
        guard.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columnar_freshness;

    const TEST_ROOT: &str = "/test-root";
    fn test_root() -> std::path::PathBuf {
        std::path::PathBuf::from(TEST_ROOT)
    }
    fn test_link(bound: usize) -> (WorkerLink, std::sync::mpsc::Receiver<WorkItem>) {
        let (tx, rx) = sync_channel::<WorkItem>(bound.max(1));
        let link = WorkerLink {
            tx,
            root: Arc::new(test_root()),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            stopped: Arc::new(AtomicBool::new(false)),
            submitted: Arc::new(AtomicU64::new(0)),
            completed: Arc::new(AtomicU64::new(0)),
            inline_fallbacks: Arc::new(AtomicU64::new(0)),
        };
        (link, rx)
    }

    #[test]
    fn submit_dedupes_inflight_tables() {
        // No worker thread drains here, so submissions accumulate
        // deterministically: five submits for one table queue one item.
        let (link, _rx) = test_link(64);
        for _ in 0..5 {
            assert!(link.submit(&test_root(), "db", "t"), "submit accepts");
        }
        assert_eq!(link.queue_depth(), 1, "one in-flight entry max per table");
        assert!(
            link.submit(&test_root(), "db", "other"),
            "distinct table queues too"
        );
        assert_eq!(link.queue_depth(), 2);
        let (submitted, _, _) = link.stats();
        assert_eq!(submitted, 2, "only first submissions queue work");
    }

    #[test]
    fn full_queue_falls_back_to_inline() {
        let (link, _rx) = test_link(1);
        assert!(link.submit(&test_root(), "db", "t1"), "first item fits");
        // Same table coalesces even when full (no new queue slot needed).
        assert!(
            link.submit(&test_root(), "db", "t1"),
            "coalesced submit succeeds"
        );
        // Distinct table with a full queue declines to inline execution.
        assert!(
            !link.submit(&test_root(), "db", "t2"),
            "full queue declines"
        );
        let (_, _, inline_fallbacks) = link.stats();
        assert_eq!(inline_fallbacks, 1);
        // The declined table was released from the set: a later submit (once
        // a worker drains) may proceed.
        assert_eq!(link.queue_depth(), 1);
    }

    #[test]
    fn stopped_link_declines() {
        let (link, _rx) = test_link(64);
        link.stopped.store(true, Ordering::Release);
        assert!(
            !link.submit(&test_root(), "db", "t"),
            "submissions after shutdown decline to inline"
        );
    }

    #[test]
    fn foreign_root_declines_to_inline() {
        let (link, _rx) = test_link(64);
        assert!(
            !link.submit(&std::path::PathBuf::from("/other-root"), "db", "t"),
            "foreign roots run inline"
        );
        assert_eq!(link.queue_depth(), 0);
    }

    #[test]
    fn worker_executes_pass_and_marks_fresh() {
        // End-to-end through a real thread: the worker runs the pass for a
        // submitted table and the shared freshness map records it, all
        // without any session running inline maintenance.
        let root = std::env::temp_dir().join(format!("plomid-worker-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(root.with_extension("wal"));
        let wal = root.with_extension("wal");
        let engine = PlomidStorageEngine::create(&root, &wal, 32).expect("engine");
        let shared = Arc::new(Mutex::new(engine));
        {
            let mut setup =
                crate::executor::Executor::new_shared(Arc::clone(&shared)).expect("setup");
            setup
                .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
                .unwrap();
            setup
                .execute("INSERT INTO t VALUES (1, 10), (2, 20);")
                .unwrap();
        }
        let key = columnar_freshness::freshness_key(&root, "plomid", "public.t");
        columnar_freshness::forget_table(&key);
        let worker = MaintenanceWorker::spawn(Arc::clone(&shared));
        let link = worker.link();
        assert!(link.submit(&root, "plomid", "public.t"));
        // Poll for completion (generous bound; the pass itself is fast).
        let mut completed = false;
        for _ in 0..300 {
            if link.stats().1 >= 1 {
                completed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(completed, "worker completed the pass");
        assert!(
            columnar_freshness::clean_generation(&key).is_some(),
            "pass marked the table fresh"
        );
        worker.shutdown();
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(wal);
    }
}
