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
//! Concurrent uniqueness semantics: independent keys proceed independently,
//! identical keys admit exactly one winner, and updates into taken values fail.
//!
//! These pin the SQL-visible contract that the unique-reservation gates and
//! the sealed-segment lookup path (including the negative-membership filter)
//! must preserve under threads.
use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::{ConcurrentPlomidStorageEngine, PlomidStorageEngine};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn dir(tag: &str) -> (PathBuf, PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-unique-conc-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!(
        "plomid-unique-conc-{tag}-wal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn shared_engine(tag: &str) -> Arc<Mutex<PlomidStorageEngine>> {
    let (storage, wal) = dir(tag);
    Arc::new(Mutex::new(
        PlomidStorageEngine::create(&storage, &wal, 64).expect("engine"),
    ))
}

fn session(shared: &Arc<Mutex<PlomidStorageEngine>>) -> Executor<ConcurrentPlomidStorageEngine> {
    Executor::new_shared(Arc::clone(shared)).expect("session")
}

fn count(session: &mut Executor<ConcurrentPlomidStorageEngine>, sql: &str) -> i64 {
    match session.execute(sql).expect("count query") {
        QueryResult::Rows { rows, .. } => match &rows[0][0] {
            Value::Int8(n) => *n,
            Value::Int4(n) => *n as i64,
            other => panic!("unexpected count value {other:?}"),
        },
        other => panic!("unexpected result {other:?}"),
    }
}

#[test]
fn distinct_unique_values_all_succeed_concurrently() {
    let shared = shared_engine("distinct");
    session(&shared)
        .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, u BIGINT UNIQUE NOT NULL)")
        .expect("ddl");
    let mut handles = Vec::new();
    for worker in 0..8i64 {
        let shared = Arc::clone(&shared);
        handles.push(std::thread::spawn(move || {
            let mut s = session(&shared);
            for i in 0..50 {
                let id = worker * 1000 + i;
                s.execute(&format!("INSERT INTO t VALUES ({id},{id})"))
                    .map(|_| ())
                    .map_err(|e| format!("{e:?}"))?;
            }
            Ok::<(), String>(())
        }));
    }
    let mut failures = Vec::new();
    for h in handles {
        if let Err(e) = h.join().expect("thread") {
            failures.push(e);
        }
    }
    assert!(
        failures.is_empty(),
        "independent keys must not conflict: {failures:?}"
    );
    let mut verify = session(&shared);
    assert_eq!(count(&mut verify, "SELECT COUNT(*) FROM t"), 400);
    let _ = std::fs::remove_dir_all(dir("distinct").0);
}

#[test]
fn identical_unique_value_admits_exactly_one_winner() {
    let shared = shared_engine("samekey");
    session(&shared)
        .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, u BIGINT UNIQUE NOT NULL)")
        .expect("ddl");
    let mut handles = Vec::new();
    for worker in 0..8i64 {
        let shared = Arc::clone(&shared);
        handles.push(std::thread::spawn(move || {
            let mut s = session(&shared);
            s.execute(&format!("INSERT INTO t VALUES ({worker}, 424242)"))
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        }));
    }
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    assert_eq!(
        outcomes.iter().filter(|o| o.is_ok()).count(),
        1,
        "exactly one winner: {outcomes:?}"
    );
    let mut verify = session(&shared);
    assert_eq!(
        count(&mut verify, "SELECT COUNT(*) FROM t WHERE u = 424242"),
        1
    );
    let _ = std::fs::remove_dir_all(dir("samekey").0);
}

#[test]
fn composite_unique_tuple_admits_exactly_one_winner() {
    let shared = shared_engine("composite");
    session(&shared)
        .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, a INT, b INT, UNIQUE (a, b))")
        .expect("ddl");
    let mut handles = Vec::new();
    for worker in 0..4i64 {
        let shared = Arc::clone(&shared);
        handles.push(std::thread::spawn(move || {
            let mut s = session(&shared);
            s.execute(&format!("INSERT INTO t VALUES ({worker}, 7, 9)"))
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        }));
    }
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    assert_eq!(
        outcomes.iter().filter(|o| o.is_ok()).count(),
        1,
        "exactly one composite winner: {outcomes:?}"
    );
    let mut verify = session(&shared);
    assert_eq!(
        count(&mut verify, "SELECT COUNT(*) FROM t WHERE a = 7 AND b = 9"),
        1
    );
    let _ = std::fs::remove_dir_all(dir("composite").0);
}

#[test]
fn concurrent_updates_into_same_unique_value_conflict() {
    let shared = shared_engine("updconflict");
    {
        let mut s = session(&shared);
        s.execute("CREATE TABLE t (id BIGINT PRIMARY KEY, u BIGINT UNIQUE NOT NULL)")
            .expect("ddl");
        s.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)")
            .expect("seed");
    }
    // Both threads move different rows onto 99: exactly one may win.
    let mut handles = Vec::new();
    for worker in [1i64, 2] {
        let shared = Arc::clone(&shared);
        handles.push(std::thread::spawn(move || {
            let mut s = session(&shared);
            s.execute(&format!("UPDATE t SET u = 99 WHERE id = {worker}"))
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        }));
    }
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    assert_eq!(
        outcomes.iter().filter(|o| o.is_ok()).count(),
        1,
        "exactly one update winner: {outcomes:?}"
    );
    let mut verify = session(&shared);
    assert_eq!(count(&mut verify, "SELECT COUNT(*) FROM t WHERE u = 99"), 1);
    assert_eq!(count(&mut verify, "SELECT COUNT(*) FROM t"), 3);
    let _ = std::fs::remove_dir_all(dir("updconflict").0);
}

#[test]
fn disjoint_updates_all_succeed_concurrently() {
    let shared = shared_engine("disjoint");
    {
        let mut s = session(&shared);
        s.execute("CREATE TABLE t (id BIGINT PRIMARY KEY, u BIGINT UNIQUE NOT NULL)")
            .expect("ddl");
        let seed: Vec<String> = (0..8).map(|i| format!("({i},{})", i * 10)).collect();
        s.execute(&format!("INSERT INTO t VALUES {}", seed.join(",")))
            .expect("seed");
    }
    let mut handles = Vec::new();
    for worker in 0..8i64 {
        let shared = Arc::clone(&shared);
        handles.push(std::thread::spawn(move || {
            let mut s = session(&shared);
            s.execute(&format!("UPDATE t SET u = u + 1 WHERE id = {worker}"))
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        }));
    }
    let mut failures = Vec::new();
    for h in handles {
        if let Err(e) = h.join().expect("thread") {
            failures.push(e);
        }
    }
    assert!(
        failures.is_empty(),
        "disjoint updates must not conflict: {failures:?}"
    );
    let mut verify = session(&shared);
    assert_eq!(count(&mut verify, "SELECT COUNT(*) FROM t"), 8);
    let _ = std::fs::remove_dir_all(dir("disjoint").0);
}
