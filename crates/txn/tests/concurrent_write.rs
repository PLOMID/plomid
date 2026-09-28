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
//! Correctness tests for the concurrent write path: row-level locking,
//! per-statement write lanes, lost-update prevention, gate release on abort,
//! and row-gate reclamation.
//!
//! These exercise the same primitives the SQL UPDATE/DELETE executor uses
//! (`lock_rows`/`lock_for_write` -> re-read -> stage -> commit) without going
//! through the SQL layer, so a regression in the locking protocol fails here.

use plomid_core::Result;
use plomid_storage::{StorageEngine, StorageEngineTransaction};
use plomid_txn::{ConcurrentPlomidStorageEngine, PlomidStorageEngine};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

type Shared = Arc<Mutex<PlomidStorageEngine>>;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn temp_root(label: &str) -> PathBuf {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "plomid-concurrent-{label}-{}-{id}",
        std::process::id()
    ))
}

fn shared_engine(label: &str) -> (Shared, PathBuf) {
    let root = temp_root(label);
    std::fs::create_dir_all(&root).expect("create temp root");
    let engine = PlomidStorageEngine::create(&root, &root.join("wal"), 512)
        .expect("create concurrent engine");
    (Arc::new(Mutex::new(engine)), root)
}

fn cleanup(root: &PathBuf) {
    let _ = std::fs::remove_dir_all(root);
}

fn session(shared: &Shared) -> Result<ConcurrentPlomidStorageEngine> {
    ConcurrentPlomidStorageEngine::from_shared(Arc::clone(shared))
}

fn counter(bytes: Option<Vec<u8>>) -> i64 {
    match bytes {
        Some(bytes) => i64::from_le_bytes(bytes.try_into().expect("counter width")),
        None => 0,
    }
}

/// One read-modify-write of a single row under its row lock, matching the
/// executor's UPDATE protocol: acquire the row lock, re-read committed state,
/// stage the new value, commit while still holding the lock.
fn increment_row(shared: &Shared, key: &[u8]) -> Result<()> {
    let mut engine = session(shared)?;
    let mut txn = engine.begin()?;
    txn.lock_rows(&[key.to_vec()])?;
    let current = counter(txn.get(key)?);
    txn.put(key, &(current + 1).to_le_bytes())?;
    txn.commit()?;
    Ok(())
}

#[test]
fn independent_rows_commit_concurrently() {
    let (shared, root) = shared_engine("indep");
    let result = (|| -> Result<()> {
        let mut handles = Vec::new();
        for (key, value) in [
            (b"a".to_vec(), b"va".to_vec()),
            (b"b".to_vec(), b"vb".to_vec()),
        ] {
            let shared = Arc::clone(&shared);
            handles.push(thread::spawn(move || -> Result<()> {
                let mut engine = session(&shared)?;
                let mut txn = engine.begin()?;
                txn.lock_rows(std::slice::from_ref(&key))?;
                txn.put(&key, &value)?;
                txn.commit()?;
                Ok(())
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread panicked")?;
        }
        let mut engine = session(&shared)?;
        assert_eq!(engine.get(b"a")?, Some(b"va".to_vec()));
        assert_eq!(engine.get(b"b")?, Some(b"vb".to_vec()));
        Ok(())
    })();
    cleanup(&root);
    assert!(result.is_ok(), "independent rows failed: {result:?}");
}

#[test]
fn same_row_increments_are_not_lost() {
    let (shared, root) = shared_engine("lost-update");
    let key = b"counter-1".to_vec();
    let workers = 4;
    let per_worker = 25;
    let result = (|| -> Result<()> {
        // Seed the row so every increment updates an existing value.
        let mut engine = session(&shared)?;
        let mut txn = engine.begin()?;
        txn.lock_rows(std::slice::from_ref(&key))?;
        txn.put(&key, &0_i64.to_le_bytes())?;
        txn.commit()?;

        let mut handles = Vec::new();
        for _ in 0..workers {
            let shared = Arc::clone(&shared);
            let key = key.clone();
            handles.push(thread::spawn(move || -> Result<()> {
                for _ in 0..per_worker {
                    increment_row(&shared, &key)?;
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread panicked")?;
        }

        let mut engine = session(&shared)?;
        assert_eq!(
            counter(engine.get(&key)?),
            (workers * per_worker) as i64,
            "lost update under same-row contention"
        );
        Ok(())
    })();
    cleanup(&root);
    assert!(result.is_ok(), "same-row increments failed: {result:?}");
}

#[test]
fn abort_releases_row_gate() {
    let (shared, root) = shared_engine("abort-release");
    let result = (|| -> Result<()> {
        let key = b"victim".to_vec();
        let mut engine = session(&shared)?;
        let mut txn = engine.begin()?;
        txn.lock_rows(std::slice::from_ref(&key))?;
        txn.put(&key, b"aborted")?;
        txn.abort()?;

        // If the abort leaked the row gate this second transaction would block
        // forever instead of completing.
        let mut engine = session(&shared)?;
        let mut txn = engine.begin()?;
        txn.lock_rows(std::slice::from_ref(&key))?;
        txn.put(&key, b"committed")?;
        txn.commit()?;
        assert_eq!(engine.get(&key)?, Some(b"committed".to_vec()));
        assert_eq!(
            shared.lock().expect("engine mutex").row_gate_count(),
            0,
            "row gates must be released after commit/abort"
        );
        Ok(())
    })();
    cleanup(&root);
    assert!(result.is_ok(), "abort gate release failed: {result:?}");
}

#[test]
fn row_gates_do_not_accumulate_per_row() {
    let (shared, root) = shared_engine("gate-cleanup");
    let result = (|| -> Result<()> {
        let mut engine = session(&shared)?;
        for index in 0..64_u32 {
            let key = format!("row-{index}").into_bytes();
            let mut txn = engine.begin()?;
            txn.lock_rows(std::slice::from_ref(&key))?;
            txn.put(&key, &index.to_le_bytes())?;
            txn.commit()?;
        }
        assert_eq!(
            shared.lock().expect("engine mutex").row_gate_count(),
            0,
            "row gates must not accumulate per historical row"
        );
        Ok(())
    })();
    cleanup(&root);
    assert!(result.is_ok(), "row gate cleanup failed: {result:?}");
}

#[test]
fn transaction_touching_multiple_tables_does_not_deadlock() {
    let (shared, root) = shared_engine("multi-table");
    let result = (|| -> Result<()> {
        // Two workers acquire the same two table lanes in OPPOSITE order, so
        // they necessarily contend on their second lane. A transaction that
        // already holds a lane must never block for a second one (that is the
        // wait cycle); contention must surface as a Conflict that releases
        // every lane, letting the other worker finish. A deadlock would hang
        // this test instead of failing it.
        let mut handles = Vec::new();
        for worker in 0..2_u32 {
            let shared = Arc::clone(&shared);
            handles.push(thread::spawn(move || -> Result<()> {
                let (first, second) = if worker == 0 {
                    (b"table_a".as_slice(), b"table_b".as_slice())
                } else {
                    (b"table_b".as_slice(), b"table_a".as_slice())
                };
                let mut engine = session(&shared)?;
                for round in 0..64_u32 {
                    let mut txn = engine.begin()?;
                    // Acquire the first lane, blocking until it is free.
                    loop {
                        match txn.lock_for_write(first) {
                            Ok(()) => break,
                            Err(error) if error.kind() == plomid_core::ErrorKind::Conflict => {
                                txn.abort()?;
                                txn = engine.begin()?;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    match txn.lock_for_write(second) {
                        Ok(()) => {
                            txn.put(format!("{}-{round}", worker).as_bytes(), b"v")?;
                            txn.commit()?;
                        }
                        // Non-blocking second lane: back off, release, retry.
                        Err(error) if error.kind() == plomid_core::ErrorKind::Conflict => {
                            txn.abort()?;
                            std::thread::sleep(std::time::Duration::from_micros(20));
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread panicked")?;
        }
        assert_eq!(
            shared.lock().expect("engine mutex").row_gate_count(),
            0,
            "table lanes must not leave row gates behind"
        );
        Ok(())
    })();
    cleanup(&root);
    assert!(
        result.is_ok(),
        "multi-table deadlock test failed: {result:?}"
    );
}
