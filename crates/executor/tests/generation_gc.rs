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
//! Snapshot-aware generation + segment reclamation tests.
//!
//! Each test drives the real SQL path (`INSERT`/`VACUUM`/reads) and then
//! proves physical facts: generation directories on disk, slice payloads in
//! storage, and query results. Temporary probe file from the investigation
//! was folded into these regression tests.

use plomid_columnar::ColumnarStore;
use plomid_executor::{Executor, MaintenancePolicy};
use plomid_txn::StorageEngineTransaction;
use plomid_txn::{ConcurrentPlomidStorageEngine, PlomidStorageEngine, StorageEngine};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("plomid-gen-gc-{label}-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root.with_extension("wal"));
}

fn gen_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if e.file_name().to_string_lossy().starts_with("GEN-") {
                    out.push(p.clone());
                }
                walk(&p, out);
            }
        }
    }
    walk(&root.join("objects"), &mut out);
    out.sort();
    out
}

/// Manifest + slice keys currently present in storage for `root`.
fn live_segments(session: &mut Executor<ConcurrentPlomidStorageEngine>, root: &Path) -> Vec<u64> {
    let store = ColumnarStore::open(root).expect("store");
    store
        .list_segments(session.engine_mut())
        .expect("list")
        .into_iter()
        .map(|id| id.get())
        .collect()
}

fn rows(session: &mut Executor<ConcurrentPlomidStorageEngine>) -> Vec<(i64, i64)> {
    match session.execute("SELECT id, v FROM t ORDER BY id").unwrap() {
        plomid_sql::QueryResult::Rows { rows, .. } => rows
            .into_iter()
            .map(|row| match &row[..] {
                [plomid_sql::Value::Int8(a), plomid_sql::Value::Int4(b)] => (*a, *b as i64),
                [plomid_sql::Value::Int8(a), plomid_sql::Value::Int8(b)] => (*a, *b),
                other => panic!("unexpected row {other:?}"),
            })
            .collect(),
        other => panic!("unexpected result {other:?}"),
    }
}

#[test]
fn vacuum_reclaims_superseded_generations_files_and_slices() {
    let dir = scratch("basic");
    let shared = Arc::new(Mutex::new(
        PlomidStorageEngine::create(&dir, &dir.with_extension("wal"), 32).expect("engine"),
    ));
    let mut session = Executor::new_shared(shared.clone()).expect("session");
    session
        .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, v INTEGER)")
        .unwrap();
    let mut live_rows = Vec::new();
    for round in 0..3 {
        session
            .execute(&format!("INSERT INTO t VALUES ({},{})", 100 + round, round))
            .unwrap();
        live_rows.push((100 + round as i64, round as i64));
        session.execute("VACUUM t").unwrap();
    }
    live_rows.sort();
    // Exactly one generation directory (the current one) may remain.
    let gens = gen_dirs(&dir);
    assert_eq!(
        gens.len(),
        1,
        "only the current generation dir remains: {gens:?}"
    );
    assert!(
        gens[0].join("META.dat").is_file(),
        "current generation metadata survives"
    );
    // And exactly one live columnar segment: superseded payloads are gone.
    let live = live_segments(&mut session, &dir);
    assert_eq!(live.len(), 1, "only the current segment survives: {live:?}");
    // Live data intact and current generation readable through the store.
    let mut check: Vec<(i64, i64)> = rows(&mut session);
    check.sort();
    assert_eq!(check, live_rows);
    cleanup(&dir);
}

#[test]
fn active_transaction_pins_superseded_generations() {
    let dir = scratch("pin");
    let shared = Arc::new(Mutex::new(
        PlomidStorageEngine::create(&dir, &dir.with_extension("wal"), 32).expect("engine"),
    ));
    let mut session = Executor::new_shared(shared.clone()).expect("session");
    session
        .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, v INTEGER)")
        .unwrap();
    session.execute("INSERT INTO t VALUES (1,1)").unwrap();
    session.execute("VACUUM t").unwrap();
    assert_eq!(gen_dirs(&dir).len(), 1);

    // T1 begins (old mark) and stays open across the next vacuum.
    let mut holder = ConcurrentPlomidStorageEngine::from_shared(shared.clone()).expect("holder");
    let mut pinned = holder.begin().expect("begin");
    session.execute("INSERT INTO t VALUES (2,2)").unwrap();
    session.execute("VACUUM t").unwrap();
    // Old generation retained while the pre-vacuum transaction lives (plus
    // the fresh flush and compact outputs: three generations total)...
    assert_eq!(gen_dirs(&dir).len(), 3, "pinned generation must survive");
    let pinned_live = live_segments(&mut session, &dir);
    assert_eq!(
        pinned_live.len(),
        3,
        "pinned payload must survive: {pinned_live:?}"
    );
    // ...and reads still work everywhere.
    assert_eq!(rows(&mut session), vec![(1, 1), (2, 2)]);
    // Release the pin; the next vacuum reclaims.
    pinned.abort().expect("abort");
    session.execute("VACUUM t").unwrap();
    assert_eq!(
        gen_dirs(&dir).len(),
        1,
        "generation reclaimed after pin release"
    );
    let freed = live_segments(&mut session, &dir);
    assert_eq!(
        freed.len(),
        1,
        "payload reclaimed after pin release: {freed:?}"
    );
    assert_eq!(rows(&mut session), vec![(1, 1), (2, 2)]);
    cleanup(&dir);
}

#[test]
fn reclamation_survives_restart() {
    let dir = scratch("restart");
    let wal = dir.with_extension("wal");
    {
        let shared = Arc::new(Mutex::new(
            PlomidStorageEngine::create(&dir, &wal, 32).expect("engine"),
        ));
        let mut session = Executor::new_shared(shared).expect("session");
        session.set_maintenance_policy(MaintenancePolicy::new(u64::MAX));
        session
            .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, v INTEGER)")
            .unwrap();
        for round in 0..3 {
            session
                .execute(&format!("INSERT INTO t VALUES ({},{})", round, round * 10))
                .unwrap();
            session.execute("VACUUM t").unwrap();
        }
        assert_eq!(gen_dirs(&dir).len(), 1);
    }
    {
        let shared = Arc::new(Mutex::new(
            PlomidStorageEngine::open(&dir, &wal, 32).expect("reopen"),
        ));
        let mut session = Executor::new_shared(shared).expect("session");
        session.set_maintenance_policy(MaintenancePolicy::new(u64::MAX));
        assert_eq!(rows(&mut session), vec![(0, 0), (1, 10), (2, 20)]);
        assert_eq!(gen_dirs(&dir).len(), 1, "no resurrection after restart");
        // Post-restart vacuum still works and keeps a single generation.
        session.execute("INSERT INTO t VALUES (9,90)").unwrap();
        session.execute("VACUUM t").unwrap();
        assert_eq!(gen_dirs(&dir).len(), 1);
    }
    cleanup(&dir);
}

#[test]
fn dropped_table_reclaims_slices() {
    let dir = scratch("drop");
    let wal = dir.with_extension("wal");
    let shared = Arc::new(Mutex::new(
        PlomidStorageEngine::create(&dir, &wal, 32).expect("engine"),
    ));
    let mut session = Executor::new_shared(shared.clone()).expect("session");
    session
        .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, v INTEGER)")
        .unwrap();
    session
        .execute("INSERT INTO t VALUES (1,1),(2,2),(3,3)")
        .unwrap();
    session.execute("VACUUM t").unwrap();
    let store = ColumnarStore::open(&dir).expect("store");
    let mut probe = ConcurrentPlomidStorageEngine::from_shared(shared.clone()).expect("probe");
    let before = store.list_segments(&mut probe).expect("list");
    assert!(!before.is_empty(), "vacuum must persist segments");
    session.execute("DROP TABLE t").unwrap();
    let after = store.list_segments(&mut probe).expect("list");
    assert!(
        after.is_empty(),
        "dropped table segments reclaimed: {after:?}"
    );
    cleanup(&dir);
}

#[test]
fn concurrent_readers_writer_and_vacuum_stay_correct() {
    let dir = scratch("conc");
    let shared = Arc::new(Mutex::new(
        PlomidStorageEngine::create(&dir, &dir.with_extension("wal"), 64).expect("engine"),
    ));
    {
        let mut session = Executor::new_shared(shared.clone()).expect("session");
        session
            .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, v INTEGER)")
            .unwrap();
        session.execute("INSERT INTO t VALUES (0,0)").unwrap();
    }
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let (shared, errors, stop) = (shared.clone(), errors.clone(), stop.clone());
        handles.push(std::thread::spawn(move || {
            let mut session = Executor::new_shared(shared).expect("session");
            session.set_maintenance_policy(MaintenancePolicy::new(u64::MAX));
            while !stop.load(Ordering::Relaxed) {
                match session.execute("SELECT COUNT(*) FROM t") {
                    Ok(_) => {}
                    Err(e) => {
                        errors.lock().unwrap().push(format!("read: {e:?}"));
                    }
                }
            }
        }));
    }
    {
        let (shared, errors, stop) = (shared.clone(), errors.clone(), stop.clone());
        handles.push(std::thread::spawn(move || {
            let mut session = Executor::new_shared(shared).expect("session");
            session.set_maintenance_policy(MaintenancePolicy::new(u64::MAX));
            let mut n = 1i64;
            while !stop.load(Ordering::Relaxed) && n < 200 {
                match session.execute(&format!("INSERT INTO t VALUES ({n},{n})")) {
                    Ok(_) => n += 1,
                    Err(e) => {
                        errors.lock().unwrap().push(format!("op: {e:?}"));
                    }
                }
            }
        }));
    }
    {
        let (shared, errors, stop) = (shared.clone(), errors.clone(), stop.clone());
        handles.push(std::thread::spawn(move || {
            let mut session = Executor::new_shared(shared).expect("session");
            session.set_maintenance_policy(MaintenancePolicy::new(u64::MAX));
            for _ in 0..10 {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match session.execute("VACUUM t") {
                    Ok(_) => {}
                    Err(e) => {
                        errors.lock().unwrap().push(format!("op: {e:?}"));
                    }
                }
            }
        }));
    }
    std::thread::sleep(std::time::Duration::from_secs(8));
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().expect("thread");
    }
    let errors = errors.lock().unwrap();
    assert!(errors.is_empty(), "no concurrent errors: {errors:?}");
    // Quiescent vacuum converges to a single generation with correct data.
    let mut session = Executor::new_shared(shared).expect("session");
    session.set_maintenance_policy(MaintenancePolicy::new(u64::MAX));
    session.execute("VACUUM t").unwrap();
    assert_eq!(gen_dirs(&dir).len(), 1);
    match session.execute("SELECT COUNT(*) FROM t").unwrap() {
        plomid_sql::QueryResult::Rows { rows, .. } => assert!(!rows.is_empty()),
        other => panic!("unexpected {other:?}"),
    }
    cleanup(&dir);
}
