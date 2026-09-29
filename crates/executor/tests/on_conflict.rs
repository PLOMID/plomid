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
//! Regression tests for `INSERT ... ON CONFLICT (...)` support.
//!
//! The immediate requirement is the Section 36 / 39 torture-test pattern:
//! `ON CONFLICT (id) DO UPDATE SET col = EXCLUDED.col RETURNING *`. These tests
//! lock in PostgreSQL-compatible semantics for conflict detection, DO UPDATE
//! (with bare/qualified/EXCLUDED column resolution), DO NOTHING, RETURNING, and
//! the row-count reported by the command tag.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage = std::env::temp_dir().join(format!("plomid-onc-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!("plomid-onc-{tag}-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn rows<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor.execute(sql).expect("sql should execute") {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

/// Runs a DML statement discarding its result tag — for INSERT/UPDATE/DELETE
/// statements that do not carry `RETURNING` (which report a row count instead).
fn exec_dml<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) {
    let _ = executor.execute(sql).expect("dml should execute");
}

fn setup<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>) {
    executor.execute("DROP TABLE IF EXISTS t;").expect("drop t");
    executor
        .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, value INTEGER);")
        .expect("create t");
    executor
        .execute("INSERT INTO t VALUES (1, 10);")
        .expect("seed (1,10)");
}

#[test]
fn on_conflict_do_update_matrix() {
    let (storage, wal) = unique_engine("matrix");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor);

    // A. conflict -> DO UPDATE from existing row's value (10 + 10 = 20).
    let out = rows(
        &mut executor,
        "INSERT INTO t VALUES (1, 20) ON CONFLICT (id) DO UPDATE SET value = value + 10 RETURNING *;",
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0][0].to_sql_text(), "1");
    assert_eq!(
        out[0][1].to_sql_text(),
        "20",
        "conflict should add 10 to existing 10"
    );
    assert_eq!(
        rows(&mut executor, "SELECT value FROM t WHERE id = 1;")[0][0].to_sql_text(),
        "20"
    );

    // B. no conflict -> normal insert of (2, 30).
    exec_dml(
        &mut executor,
        "INSERT INTO t VALUES (2, 30) ON CONFLICT (id) DO UPDATE SET value = value + 10;",
    );
    let all = rows(&mut executor, "SELECT id, value FROM t ORDER BY id;");
    assert_eq!(all.len(), 2);
    assert_eq!(all[1][0].to_sql_text(), "2");
    assert_eq!(all[1][1].to_sql_text(), "30");

    // C. DO NOTHING leaves the conflicting row unchanged.
    exec_dml(
        &mut executor,
        "INSERT INTO t VALUES (1, 999) ON CONFLICT (id) DO NOTHING;",
    );
    assert_eq!(
        rows(&mut executor, "SELECT value FROM t WHERE id = 1;")[0][0].to_sql_text(),
        "20"
    );

    // D. RETURNING projects the post-update row (20 + 10 = 30), width 2 cols.
    let d = rows(
        &mut executor,
        "INSERT INTO t VALUES (1, 50) ON CONFLICT (id) DO UPDATE SET value = value + 10 RETURNING id, value;",
    );
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].len(), 2);
    assert_eq!(d[0][0].to_sql_text(), "1");
    assert_eq!(d[0][1].to_sql_text(), "30");

    // E. EXCLUDED.<col> resolves to the proposed row's value (50).
    let e = rows(
        &mut executor,
        "INSERT INTO t VALUES (1, 50) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value RETURNING *;",
    );
    assert_eq!(e.len(), 1);
    assert_eq!(
        e[0][1].to_sql_text(),
        "50",
        "EXCLUDED.value should be the proposed 50"
    );

    // F. multi-row: (1) conflicts -> updated to 100 via EXCLUDED, (3) inserts.
    let f = rows(
        &mut executor,
        "INSERT INTO t VALUES (1, 100), (3, 300) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value RETURNING *;",
    );
    assert_eq!(
        f.len(),
        2,
        "both conflicting and non-conflicting rows appear in RETURNING"
    );
    let mut returned_values: Vec<String> = f.iter().map(|row| row[1].to_sql_text()).collect();
    returned_values.sort();
    assert_eq!(returned_values, ["100", "300"]);

    let sorted = rows(&mut executor, "SELECT id, value FROM t ORDER BY id;");
    assert_eq!(sorted.len(), 3);
    assert_eq!(sorted[0][1].to_sql_text(), "100");
    assert_eq!(sorted[2][0].to_sql_text(), "3");
    assert_eq!(sorted[2][1].to_sql_text(), "300");

    drop(&mut executor, &storage, &wal);
}

#[test]
fn on_conflict_where_guard_and_do_nothing_count() {
    let (storage, wal) = unique_engine("where");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor);

    // WHERE true -> update runs (10 + 10 = 20).
    exec_dml(
        &mut executor,
        "INSERT INTO t VALUES (1, 0) ON CONFLICT (id) DO UPDATE SET value = value + 10 WHERE value < 5000;",
    );
    assert_eq!(
        rows(&mut executor, "SELECT value FROM t WHERE id = 1;")[0][0].to_sql_text(),
        "20"
    );

    // WHERE false -> no update, no insert, row unchanged.
    exec_dml(
        &mut executor,
        "INSERT INTO t VALUES (1, 0) ON CONFLICT (id) DO UPDATE SET value = value + 10 WHERE value > 10000;",
    );
    assert_eq!(
        rows(&mut executor, "SELECT value FROM t WHERE id = 1;")[0][0].to_sql_text(),
        "20"
    );

    // DO NOTHING on conflict: command-tag count stays 0.
    match executor
        .execute("INSERT INTO t VALUES (1, 999) ON CONFLICT (id) DO NOTHING;")
        .expect("do nothing")
    {
        QueryResult::Inserted(n) => assert_eq!(n, 0, "DO NOTHING conflict must report 0"),
        other => panic!("expected Inserted tag, got {other:?}"),
    }

    // DO UPDATE on conflict: command-tag count is 1 (inserted + updated).
    match executor
        .execute(
            "INSERT INTO t VALUES (1, 5) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value;",
        )
        .expect("do update")
    {
        QueryResult::Inserted(n) => assert_eq!(n, 1, "DO UPDATE conflict must report 1"),
        other => panic!("expected Inserted tag, got {other:?}"),
    }
    assert_eq!(
        rows(&mut executor, "SELECT value FROM t WHERE id = 1;")[0][0].to_sql_text(),
        "5"
    );

    drop(&mut executor, &storage, &wal);
}

#[test]
fn on_conflict_through_transaction() {
    let (storage, wal) = unique_engine("txn");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor);

    let results = executor
        .execute_all(
            "BEGIN; \
             INSERT INTO t VALUES (1, 7) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value; \
             INSERT INTO t VALUES (1, 8) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value; \
             COMMIT;",
        )
        .expect("transactional upsert");
    assert!(results.len() >= 1);
    assert_eq!(
        rows(&mut executor, "SELECT value FROM t WHERE id = 1;")[0][0].to_sql_text(),
        "8"
    );

    drop(&mut executor, &storage, &wal);
}

fn drop<E: plomid_txn::StorageEngine>(
    executor: &mut Executor<E>,
    storage: &std::path::PathBuf,
    wal: &std::path::PathBuf,
) {
    let _ = executor.execute("DROP TABLE IF EXISTS t;");
    let _ = std::fs::remove_file(storage);
    let _ = std::fs::remove_file(wal);
}
