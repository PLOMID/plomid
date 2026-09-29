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
//! Composite index DDL regression (Blocker 6).
//!
//! `CREATE INDEX ... ON t (a, b)` parses the full ordered column list and
//! registers one tuple index: uniqueness is a property of the whole tuple
//! (never of each column independently), NULL in any component voids the
//! conflict, DML maintains entries on UPDATE/DELETE, and the definition
//! survives restart via the catalog encoding.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::{ConcurrentPlomidStorageEngine, PlomidStorageEngine};
use std::sync::{Arc, Mutex};

fn scratch(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "plomid-composite-idx-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
    path
}

fn open(label: &str) -> (Executor<ConcurrentPlomidStorageEngine>, std::path::PathBuf) {
    let dir = scratch(label);
    let wal = dir.with_extension("wal");
    let engine = PlomidStorageEngine::create(&dir, &wal, 64).expect("create");
    let executor = Executor::new_shared(Arc::new(Mutex::new(engine))).expect("executor");
    (executor, dir)
}

fn count(session: &mut Executor<ConcurrentPlomidStorageEngine>) -> i64 {
    match session.execute("SELECT COUNT(*) FROM t;").unwrap() {
        QueryResult::Rows { rows, .. } => match rows[0][0] {
            Value::Int8(n) => n,
            Value::Int4(n) => n as i64,
            ref other => panic!("count shape: {other:?}"),
        },
        other => panic!("count shape: {other:?}"),
    }
}

fn is_conflict(result: Result<QueryResult, plomid_executor::SqlError>) -> bool {
    match result {
        Ok(_) => false,
        Err(error) => error.to_string().contains("duplicate key"),
    }
}

#[test]
fn composite_unique_enforces_tuple_domain() {
    let (mut session, dir) = open("tuple");
    session
        .execute("CREATE TABLE t (a INTEGER, b INTEGER, v INTEGER);")
        .unwrap();
    session
        .execute("CREATE UNIQUE INDEX ab_u ON t (a, b);")
        .unwrap();
    session.execute("INSERT INTO t VALUES (1, 1, 10);").unwrap();
    // Same `a`, different `b`: no conflict (columns are not independent).
    session.execute("INSERT INTO t VALUES (1, 2, 20);").unwrap();
    // Same `b`, different `a`: no conflict.
    session.execute("INSERT INTO t VALUES (2, 1, 30);").unwrap();
    // Identical tuple: conflict.
    assert!(
        is_conflict(session.execute("INSERT INTO t VALUES (1, 1, 99);")),
        "duplicate (a, b) tuple must conflict"
    );
    // NULL in any component voids the conflict (SQL NULL-not-equal).
    session
        .execute("INSERT INTO t VALUES (NULL, 1, 40);")
        .unwrap();
    session
        .execute("INSERT INTO t VALUES (NULL, 1, 41);")
        .unwrap();
    session
        .execute("INSERT INTO t VALUES (1, NULL, 42);")
        .unwrap();
    session
        .execute("INSERT INTO t VALUES (1, NULL, 43);")
        .unwrap();
    assert_eq!(count(&mut session), 7);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.with_extension("wal"));
}

#[test]
fn composite_backfill_and_dml_maintenance() {
    let (mut session, dir) = open("maint");
    session
        .execute("CREATE TABLE t (a INTEGER, b INTEGER, v INTEGER);")
        .unwrap();
    session
        .execute("INSERT INTO t VALUES (1, 1, 10), (1, 2, 20), (2, 1, 30);")
        .unwrap();
    // Backfill over existing rows: creating the unique index succeeds, and a
    // duplicate tuple afterwards conflicts (the backfilled entries exist).
    session
        .execute("CREATE UNIQUE INDEX ab_u ON t (a, b);")
        .unwrap();
    assert!(is_conflict(
        session.execute("INSERT INTO t VALUES (1, 2, 99);")
    ));
    // UPDATE moving a row onto an existing tuple conflicts; moving off it frees it.
    assert!(is_conflict(
        session.execute("UPDATE t SET a = 1 WHERE a = 2 AND b = 1;")
    ));
    session
        .execute("UPDATE t SET a = 3 WHERE a = 2 AND b = 1;")
        .unwrap();
    session.execute("INSERT INTO t VALUES (2, 1, 31);").unwrap();
    // DELETE frees the tuple for reuse.
    session
        .execute("DELETE FROM t WHERE a = 1 AND b = 1;")
        .unwrap();
    session.execute("INSERT INTO t VALUES (1, 1, 11);").unwrap();
    assert_eq!(count(&mut session), 4);
    // DROP INDEX removes enforcement.
    session.execute("DROP INDEX ab_u;").unwrap();
    session.execute("INSERT INTO t VALUES (1, 1, 12);").unwrap();
    assert_eq!(count(&mut session), 5);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.with_extension("wal"));
}

#[test]
fn composite_unique_backfill_rejects_existing_duplicates() {
    let (mut session, dir) = open("dupbackfill");
    session
        .execute("CREATE TABLE t (a INTEGER, b INTEGER);")
        .unwrap();
    session
        .execute("INSERT INTO t VALUES (1, 1), (1, 1);")
        .unwrap();
    assert!(
        is_conflict(session.execute("CREATE UNIQUE INDEX ab_u ON t (a, b);")),
        "backfill must observe the duplicate tuple"
    );
    // Non-unique composite over the same data succeeds.
    session.execute("CREATE INDEX ab_nu ON t (a, b);").unwrap();
    session.execute("INSERT INTO t VALUES (1, 1);").unwrap();
    assert_eq!(count(&mut session), 3);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.with_extension("wal"));
}

#[test]
fn composite_definition_survives_restart() {
    let dir = scratch("restart");
    let wal = dir.with_extension("wal");
    {
        let engine = PlomidStorageEngine::create(&dir, &wal, 64).expect("create");
        let mut session = Executor::new_shared(Arc::new(Mutex::new(engine))).expect("executor");
        session
            .execute("CREATE TABLE t (a INTEGER, b INTEGER);")
            .unwrap();
        session
            .execute("CREATE UNIQUE INDEX ab_u ON t (a, b);")
            .unwrap();
        session.execute("INSERT INTO t VALUES (1, 1);").unwrap();
    }
    let engine = PlomidStorageEngine::open(&dir, &wal, 64).expect("reopen");
    let mut session = Executor::new_shared(Arc::new(Mutex::new(engine))).expect("executor");
    assert!(
        is_conflict(session.execute("INSERT INTO t VALUES (1, 1);")),
        "composite enforcement survives restart"
    );
    session.execute("INSERT INTO t VALUES (1, 2);").unwrap();
    assert_eq!(count(&mut session), 2);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(wal);
}

#[test]
fn composite_ddl_rejects_bad_keys() {
    let (mut session, dir) = open("badkeys");
    session
        .execute("CREATE TABLE t (a INTEGER, b INTEGER, v INTEGER);")
        .unwrap();
    // Unknown column: registration refuses rather than recording an index
    // that can never be maintained.
    let error = session
        .execute("CREATE INDEX bad_idx ON t (a, nope);")
        .unwrap_err();
    assert!(
        error.to_string().contains("nope"),
        "unknown column named, got: {error}"
    );
    // Expression keys cannot be combined with plain columns.
    let error = session
        .execute("CREATE INDEX expr_idx ON t ((a + b), a);")
        .unwrap_err();
    assert!(
        error.to_string().contains("cannot be combined"),
        "clear expression-combination error, got: {error}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.with_extension("wal"));
}
