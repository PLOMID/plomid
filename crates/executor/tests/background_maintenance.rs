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
//! Background maintenance worker regression (Blocker 3).
//!
//! Proves the worker absorbs generation passes off committing sessions
//! without changing results: with a link installed the session runs zero
//! inline passes while the worker completes them; without a link the same
//! workload runs inline passes (proving the workload actually triggers
//! maintenance, so the first assertion is not vacuous). Timing comparisons
//! belong to release benchmarks, not to debug assertions.

use plomid_executor::{Executor, MaintenanceWorker};
use plomid_sql::QueryResult;
use plomid_txn::PlomidStorageEngine;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn scratch(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("plomid-bgmaint-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
    path
}

fn cleanup(root: &std::path::Path) {
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root.with_extension("wal"));
}

fn count(session: &mut Executor<plomid_txn::ConcurrentPlomidStorageEngine>) -> i64 {
    match session.execute("SELECT COUNT(*) FROM t;").unwrap() {
        QueryResult::Rows { rows, .. } => match rows[0][0] {
            plomid_sql::Value::Int8(n) => n,
            _ => panic!("count shape"),
        },
        other => panic!("expected rows, got {other:?}"),
    }
}

fn drive_inserts(
    session: &mut Executor<plomid_txn::ConcurrentPlomidStorageEngine>,
    base: i64,
    n: i64,
) {
    for i in 0..n {
        session
            .execute(&format!("INSERT INTO t VALUES ({}, 1);", base + i))
            .unwrap();
    }
}

#[test]
fn worker_absorbs_passes_off_the_commit_path() {
    let root = scratch("absorb");
    let wal = root.with_extension("wal");
    let engine = PlomidStorageEngine::create(&root, &wal, 64).expect("create");
    let shared = Arc::new(Mutex::new(engine));
    let worker = MaintenanceWorker::spawn(Arc::clone(&shared));
    let link = worker.link();
    let mut session = Executor::new_shared(Arc::clone(&shared)).expect("session");
    session.set_background_maintenance(link.clone());
    session
        .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
        .unwrap();
    // 130 single-row commits cross the default 64-mutation bound twice.
    drive_inserts(&mut session, 0, 130);
    // Wait for the worker to finish what was submitted (timing-tolerant poll;
    // the assertions below are exact, not timing-based).
    let deadline = Instant::now() + Duration::from_secs(120);
    while link.stats().1 < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let (submitted, completed, inline_fallbacks) = link.stats();
    assert!(completed >= 1, "worker completed at least one pass");
    assert_eq!(
        session.maintenance_passes("public.t"),
        0,
        "no inline pass ran on the committing session"
    );
    assert_eq!(count(&mut session), 130, "every committed row visible");
    // Freshness was established by the worker's pass: analytical reads agree.
    match session.execute("SELECT COUNT(*), SUM(v) FROM t;").unwrap() {
        QueryResult::Rows { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][0], plomid_sql::Value::Int8(130));
            assert_eq!(rows[0][1], plomid_sql::Value::Int8(130));
        }
        other => panic!("expected rows, got {other:?}"),
    }
    eprintln!(
        "worker stats: submitted={submitted} completed={completed} inline={inline_fallbacks}"
    );
    worker.shutdown();
    cleanup(&root);
}

#[test]
fn same_workload_runs_inline_without_a_link() {
    // Control: without an installed link the identical workload performs
    // inline passes on the committing session (proves the absorb test above
    // actually exercises maintenance).
    let root = scratch("control");
    let wal = root.with_extension("wal");
    let engine = PlomidStorageEngine::create(&root, &wal, 64).expect("create");
    let shared = Arc::new(Mutex::new(engine));
    let mut session = Executor::new_shared(Arc::clone(&shared)).expect("session");
    session
        .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
        .unwrap();
    drive_inserts(&mut session, 0, 130);
    assert!(
        session.maintenance_passes("public.t") >= 1,
        "control session runs inline passes"
    );
    assert_eq!(count(&mut session), 130);
    cleanup(&root);
}
