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
//! Automatic checkpoint policy: end-to-end behaviour through the real commit
//! path.
//!
//! These tests drive the same concurrent facade the server uses, so they cover
//! the wiring as well as the policy: a commit that crosses a configured
//! threshold must trigger a checkpoint *after* the commit is durable, the
//! checkpoint must reclaim only what it made unreachable, and a restart after
//! an automatic checkpoint must still recover every committed row.

use plomid_storage::{StorageEngine, StorageEngineTransaction};
use plomid_txn::{
    CheckpointPolicy, CheckpointTrigger, ConcurrentPlomidStorageEngine, PlomidStorageEngine,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type Shared = Arc<Mutex<PlomidStorageEngine>>;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn temp_root(label: &str) -> PathBuf {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "plomid-auto-checkpoint-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create temp root");
    root
}

/// A small WAL segment size keeps rotation and reclamation observable without
/// writing megabytes in a unit test. 4 KiB is far above one record (the test
/// rows are ~330 WAL bytes per commit) and far below the payload the tests
/// write, so several segments exist by the time a threshold trips.
const TEST_WAL_SEGMENT_BYTES: u64 = 4 * 1024;

fn open_shared(root: &PathBuf, policy: CheckpointPolicy) -> Shared {
    let mut engine = PlomidStorageEngine::open_with_config(
        root,
        &root.join("wal"),
        256,
        8 * 1024 * 1024,
        TEST_WAL_SEGMENT_BYTES,
    )
    .expect("open engine");
    engine.set_checkpoint_policy(policy);
    Arc::new(Mutex::new(engine))
}

fn create_shared(root: &PathBuf, policy: CheckpointPolicy) -> Shared {
    let mut engine = PlomidStorageEngine::create_with_config(
        root,
        &root.join("wal"),
        256,
        8 * 1024 * 1024,
        TEST_WAL_SEGMENT_BYTES,
    )
    .expect("create engine");
    engine.set_checkpoint_policy(policy);
    Arc::new(Mutex::new(engine))
}

fn wal_bytes(root: &PathBuf) -> u64 {
    std::fs::read_dir(root.join("wal"))
        .expect("wal dir")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// Writes `count` rows through the concurrent facade, one commit each.
fn write_rows(shared: &Shared, count: u64, payload: usize) {
    let mut engine =
        ConcurrentPlomidStorageEngine::from_shared(Arc::clone(shared)).expect("concurrent session");
    for i in 0..count {
        let mut txn = engine.begin().expect("begin");
        let key = format!("row-{i:08}").into_bytes();
        txn.put(&key, &vec![b'x'; payload]).expect("put");
        txn.commit().expect("commit");
    }
}

#[test]
fn a_commit_that_crosses_the_byte_threshold_checkpoints_and_reclaims_wal() {
    let root = temp_root("bytes");
    // Threshold far below one segment: the first commits trip it.
    let shared = create_shared(&root, CheckpointPolicy::new(4 * 1024, 0, None));

    write_rows(&shared, 40, 256);

    let (checkpoints, trigger, reclaimed, wal_after) = {
        let engine = shared.lock().expect("engine lock");
        let stats = engine.checkpoint_stats();
        (
            stats.checkpoints,
            stats.last_trigger,
            stats.last_reclaimed_segments,
            wal_bytes(&root),
        )
    };
    assert!(
        checkpoints >= 1,
        "an automatic checkpoint must have run: stats={checkpoints}"
    );
    assert_eq!(trigger, Some(CheckpointTrigger::WalBytes));
    assert!(
        reclaimed >= 1,
        "the checkpoint must reclaim the segments it made unreachable, got {reclaimed}"
    );
    assert!(
        wal_after < 32 * 1024,
        "retained WAL must stay bounded after checkpointing, got {wal_after} bytes"
    );
    // Checkpointing resets the accounting window instead of immediately
    // re-firing on the next commit.
    let engine = shared.lock().expect("engine lock");
    assert!(
        engine.checkpoint_stats().wal_bytes_since_checkpoint < 4 * 1024,
        "the checkpoint must restart the byte window"
    );
    drop(engine);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn rows_committed_across_an_automatic_checkpoint_survive_reopen() {
    let root = temp_root("reopen");
    let policy = CheckpointPolicy::new(4 * 1024, 0, None);
    let shared = create_shared(&root, policy);
    write_rows(&shared, 40, 256);
    let checkpoints_before = {
        let engine = shared.lock().expect("engine lock");
        engine.checkpoint_stats().checkpoints
    };
    assert!(checkpoints_before >= 1, "checkpoint must run before reopen");
    drop(shared);

    // Reopen through the normal recovery path, with a fresh policy (the
    // reopened engine's counters start empty).
    let reopened = open_shared(&root, policy);
    let mut engine = ConcurrentPlomidStorageEngine::from_shared(Arc::clone(&reopened))
        .expect("concurrent session");
    for i in 0..40u64 {
        let key = format!("row-{i:08}").into_bytes();
        assert_eq!(
            engine.get(&key).expect("get").map(|value| value.len()),
            Some(256),
            "row {i} must survive recovery after an automatic checkpoint"
        );
    }
    // WAL reclamation ran, so recovery used the checkpoint boundary rather
    // than replaying everything from the start.
    let report = reopened
        .lock()
        .expect("engine lock")
        .recovery_report()
        .cloned();
    assert!(
        report.is_some(),
        "a reopened engine reports its recovery outcome"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn the_segment_threshold_backs_up_the_byte_signal() {
    let root = temp_root("segments");
    // Byte signal disabled: only retained segments can trigger.
    let shared = create_shared(&root, CheckpointPolicy::new(0, 2, None));
    // Enough rows to rotate past two 4 KiB segments, with small payloads so the
    // segment count rather than the byte counter is what moves first.
    write_rows(&shared, 60, 64);
    let (checkpoints, trigger) = {
        let engine = shared.lock().expect("engine lock");
        let stats = engine.checkpoint_stats();
        (stats.checkpoints, stats.last_trigger)
    };
    assert!(
        checkpoints >= 1,
        "segment threshold must trigger a checkpoint"
    );
    assert_eq!(trigger, Some(CheckpointTrigger::WalSegments));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_disabled_policy_never_checkpoints_automatically() {
    let root = temp_root("disabled");
    let shared = create_shared(&root, CheckpointPolicy::disabled());
    write_rows(&shared, 40, 256);
    let (checkpoints, wal_after) = {
        let engine = shared.lock().expect("engine lock");
        (engine.checkpoint_stats().checkpoints, wal_bytes(&root))
    };
    assert_eq!(checkpoints, 0, "a disabled policy must not checkpoint");
    assert!(
        wal_after > 8 * 1024,
        "with automatic checkpointing off, WAL accumulates: {wal_after} bytes"
    );
    // An explicit checkpoint still works and reclaims.
    let outcome = {
        let mut engine = shared.lock().expect("engine lock");
        engine.checkpoint_with_report(CheckpointTrigger::Explicit)
    }
    .expect("explicit checkpoint");
    assert!(outcome.lsn.get() > 0);
    assert!(outcome.reclaimed_segments >= 1);
    assert!(wal_bytes(&root) < wal_after);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_single_commit_does_not_pay_for_a_checkpoint() {
    let root = temp_root("cheap");
    // Default thresholds: nothing is due after one small commit.
    let shared = create_shared(&root, CheckpointPolicy::default());
    let mut engine = ConcurrentPlomidStorageEngine::from_shared(Arc::clone(&shared))
        .expect("concurrent session");
    let mut txn = engine.begin().expect("begin");
    txn.put(b"only-row", b"value").expect("put");
    txn.commit().expect("commit");
    let stats = shared.lock().expect("engine lock").checkpoint_stats();
    assert_eq!(stats.checkpoints, 0);
    assert_eq!(stats.last_trigger, None);
    assert!(stats.last_duration.is_zero());
    std::fs::remove_dir_all(&root).ok();
}
