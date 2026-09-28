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
//! Regression tests for PostgreSQL `DELETE ... USING` support.
//!
//! Drives the same `Executor::execute` path used by the PostgreSQL wire
//! server: target alias (`DELETE FROM t d` / `DELETE FROM t AS d`), read-only
//! `USING` source relations, WHERE join qualification, `RETURNING`
//! (`d.*`, explicit lists, bare lists), zero-match, multiple source matches
//! (target deleted exactly once), plain-DELETE regression, and explicit
//! transactions.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-delete-using-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!(
        "plomid-delete-using-{tag}-wal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn exec<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor
        .execute(sql)
        .unwrap_or_else(|error| panic!("{sql} should execute: {error}"))
    {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

fn exec_ok<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) {
    executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"));
}

/// Executes a DML statement expecting the legacy row-count result.
fn exec_deleted<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> u64 {
    match executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"))
    {
        QueryResult::Deleted(count) => count,
        other => panic!("expected deleted count, got {other:?}"),
    }
}

/// Executes and returns the full project shape (column names, column types and
/// row values) of a `Rows` result.
fn rows_of<E: plomid_txn::StorageEngine>(
    executor: &mut Executor<E>,
    sql: &str,
) -> (
    Vec<String>,
    Vec<Option<plomid_sql::ColumnType>>,
    Vec<Vec<Value>>,
) {
    match executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should execute: {e}"))
    {
        QueryResult::Rows {
            columns,
            column_types,
            rows,
        } => (columns, column_types, rows),
        other => panic!("{sql} should return rows, got {other:?}"),
    }
}

#[test]
fn delete_using_basic() {
    let (storage, wal) = unique_engine("basic");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor, "t1");

    assert_eq!(
        exec_deleted(
            &mut executor,
            "DELETE FROM target_t1 t USING filter_t1 f WHERE t.id = f.id;"
        ),
        1
    );
    assert_eq!(
        exec(&mut executor, "SELECT * FROM target_t1 ORDER BY id;"),
        vec![
            vec![Value::Int4(1), Value::Text("one".into())],
            vec![Value::Int4(3), Value::Text("three".into())],
        ]
    );
    // USING source rows must not be deleted.
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM filter_t1;"),
        vec![vec![Value::Int8(1)]]
    );
}

#[test]
fn delete_using_aliases() {
    let (storage, wal) = unique_engine("aliases");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor, "t2");

    assert_eq!(
        exec_deleted(
            &mut executor,
            "DELETE FROM target_t2 AS t USING filter_t2 AS f WHERE t.id = f.id;"
        ),
        1
    );
    // Target alias also works without USING.
    assert_eq!(
        exec_deleted(&mut executor, "DELETE FROM target_t2 AS t WHERE t.id = 1;"),
        1
    );
    assert_eq!(
        exec(&mut executor, "SELECT * FROM target_t2 ORDER BY id;"),
        vec![vec![Value::Int4(3), Value::Text("three".into())]]
    );
}

#[test]
fn delete_using_returning() {
    let (storage, wal) = unique_engine("returning");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor, "t3");

    // Explicit list with qualified target references.
    let (columns, column_types, rows) = rows_of(
        &mut executor,
        "DELETE FROM target_t3 t USING filter_t3 f WHERE t.id = f.id RETURNING t.id, t.value;",
    );
    assert_eq!(columns, vec!["id", "value"]);
    assert_eq!(column_types.len(), 2);
    assert_eq!(rows, vec![vec![Value::Int4(2), Value::Text("two".into())]]);
    for row in &rows {
        assert_eq!(row.len(), columns.len());
        assert_eq!(row.len(), column_types.len());
    }
    setup(&mut executor, "t3");
    // Bare column references.
    let (_, _, rows) = rows_of(
        &mut executor,
        "DELETE FROM target_t3 t USING filter_t3 f WHERE t.id = f.id RETURNING id, value;",
    );
    assert_eq!(rows, vec![vec![Value::Int4(2), Value::Text("two".into())]]);
    setup(&mut executor, "t3");
    // `RETURNING *`.
    let (columns, column_types, rows) = rows_of(
        &mut executor,
        "DELETE FROM target_t3 t USING filter_t3 f WHERE t.id = f.id RETURNING *;",
    );
    assert_eq!(columns, vec!["id", "value"]);
    assert_eq!(column_types.len(), 2);
    assert_eq!(rows, vec![vec![Value::Int4(2), Value::Text("two".into())]]);
}

#[test]
fn delete_using_returning_qualified_star() {
    let (storage, wal) = unique_engine("qualstar");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor, "t4");

    // `RETURNING d.*` (torture-test section 38 shape).
    let (columns, column_types, rows) = rows_of(
        &mut executor,
        "DELETE FROM target_t4 d USING filter_t4 f WHERE d.id = f.id RETURNING d.*;",
    );
    assert_eq!(columns, vec!["id", "value"]);
    assert_eq!(column_types.len(), 2);
    assert_eq!(rows, vec![vec![Value::Int4(2), Value::Text("two".into())]]);
    for row in &rows {
        assert_eq!(row.len(), columns.len());
        assert_eq!(row.len(), column_types.len());
    }
}

fn setup<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, tag: &str) {
    exec_ok(executor, &format!("DROP TABLE IF EXISTS target_{tag};"));
    exec_ok(executor, &format!("DROP TABLE IF EXISTS filter_{tag};"));
    exec_ok(
        executor,
        &format!("CREATE TABLE target_{tag} (id INTEGER PRIMARY KEY, value TEXT);"),
    );
    exec_ok(
        executor,
        &format!("CREATE TABLE filter_{tag} (id INTEGER);"),
    );
    exec_ok(
        executor,
        &format!("INSERT INTO target_{tag} VALUES (1, 'one'), (2, 'two'), (3, 'three');"),
    );
    exec_ok(executor, &format!("INSERT INTO filter_{tag} VALUES (2);"));
}

#[test]
fn delete_using_no_match() {
    let (storage, wal) = unique_engine("nomatch");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor, "t5");

    // Row count result: DELETE 0.
    assert_eq!(
        exec_deleted(
            &mut executor,
            "DELETE FROM target_t5 t USING filter_t5 f WHERE t.id = f.id AND t.id = 99999;"
        ),
        0
    );
    // RETURNING result: zero rows, connection still usable afterwards.
    let (_, _, rows) = rows_of(
        &mut executor,
        "DELETE FROM target_t5 t USING filter_t5 f WHERE t.id = f.id AND t.id = 99999 RETURNING t.*;",
    );
    assert!(rows.is_empty());
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM target_t5;"),
        vec![vec![Value::Int8(3)]]
    );
}

#[test]
fn delete_using_multiple_source_matches() {
    let (storage, wal) = unique_engine("multimatch");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor, "t6");
    exec_ok(&mut executor, "INSERT INTO filter_t6 VALUES (2), (2);");

    // Several USING rows match one target row: deleted exactly once, one
    // RETURNING row.
    let (_, _, rows) = rows_of(
        &mut executor,
        "DELETE FROM target_t6 t USING filter_t6 f WHERE t.id = f.id RETURNING t.id, t.value;",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], vec![Value::Int4(2), Value::Text("two".into())]);
    assert_eq!(
        exec(&mut executor, "SELECT * FROM target_t6 ORDER BY id;"),
        vec![
            vec![Value::Int4(1), Value::Text("one".into())],
            vec![Value::Int4(3), Value::Text("three".into())],
        ]
    );
}

#[test]
fn plain_delete_regression() {
    let (storage, wal) = unique_engine("plain");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor, "t7");

    // Ordinary DELETE without alias/USING.
    assert_eq!(
        exec_deleted(&mut executor, "DELETE FROM target_t7 WHERE id = 1;"),
        1
    );
    // DELETE ... RETURNING *.
    let (_, _, rows) = rows_of(
        &mut executor,
        "DELETE FROM target_t7 WHERE id = 2 RETURNING *;",
    );
    assert_eq!(rows, vec![vec![Value::Int4(2), Value::Text("two".into())]]);
    // DELETE inside an explicit transaction (single execute call, rolled back).
    let result = executor
        .execute("BEGIN; DELETE FROM target_t7 WHERE id = 3; ROLLBACK;")
        .unwrap_or_else(|e| panic!("transaction should succeed: {e}"));
    assert_eq!(result, QueryResult::RolledBack);
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM target_t7;"),
        vec![vec![Value::Int8(1)]]
    );
}

#[test]
fn delete_using_in_transaction() {
    let (storage, wal) = unique_engine("txn");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor, "t8");

    // Committed transaction: source rows inserted inside the same
    // transaction are visible to USING; the target row is deleted.
    let result = executor
        .execute(
            "BEGIN; INSERT INTO filter_t8 VALUES (1); \
             DELETE FROM target_t8 d USING filter_t8 f WHERE d.id = f.id; \
             COMMIT;",
        )
        .unwrap_or_else(|e| panic!("transaction should succeed: {e}"));
    assert_eq!(result, QueryResult::Committed);
    // `filter_t8` already contains 2 from setup, and the transaction adds 1,
    // so target rows 1 and 2 qualify for deletion.
    assert_eq!(
        exec(&mut executor, "SELECT * FROM target_t8 ORDER BY id;"),
        vec![vec![Value::Int4(3), Value::Text("three".into())]]
    );

    // Rolled-back transaction: nothing deleted.
    setup(&mut executor, "t8");
    let result = executor
        .execute(
            "BEGIN; DELETE FROM target_t8 d USING filter_t8 f WHERE d.id = f.id; \
             ROLLBACK;",
        )
        .unwrap_or_else(|e| panic!("transaction should succeed: {e}"));
    assert_eq!(result, QueryResult::RolledBack);
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM target_t8;"),
        vec![vec![Value::Int8(3)]]
    );
}
