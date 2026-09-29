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
//! Focused regression tests for FOREIGN KEY enforcement and advanced GROUP BY
//! (GROUPING SETS, ROLLUP, CUBE), driven through the same Executor path the
//! PostgreSQL wire server uses.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage = std::env::temp_dir().join(format!("plomid-fkgb-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!("plomid-fkgb-{tag}-wal-{}", std::process::id()));
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

fn exec_err<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> String {
    match executor.execute(sql) {
        Ok(QueryResult::Rows { rows, .. }) => panic!("{sql} should fail, got rows={rows:?}"),
        Ok(other) => panic!("{sql} should fail, got {other:?}"),
        Err(e) => e.to_string(),
    }
}

fn text_rows(rows: &[Vec<Value>]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| row.iter().map(Value::to_sql_text).collect())
        .collect()
}

#[test]
fn foreign_keys_valid_invalid_null_and_atomicity() {
    let (storage, wal) = unique_engine("fk");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE parent (id BIGINT PRIMARY KEY);",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE child (
             id BIGINT PRIMARY KEY,
             parent_id BIGINT,
             CONSTRAINT child_parent_fk
                 FOREIGN KEY (parent_id)
                 REFERENCES parent(id)
         );",
    );

    // Test A - valid FK succeeds.
    exec_ok(&mut executor, "INSERT INTO parent VALUES (1);");
    exec_ok(&mut executor, "INSERT INTO child VALUES (1, 1);");

    // Test B - invalid FK fails with 23503, and row is not visible.
    let err = exec_err(&mut executor, "INSERT INTO child VALUES (2, 999);");
    assert!(
        err.to_ascii_lowercase().contains("foreign"),
        "expected foreign key violation, got: {err}"
    );
    let rows = exec(&mut executor, "SELECT COUNT(*) FROM child;");
    assert_eq!(text_rows(&rows), vec![vec!["1".to_string()]]);

    // Test C - NULL FK succeeds under MATCH SIMPLE.
    exec_ok(&mut executor, "INSERT INTO child VALUES (3, NULL);");
    let rows = exec(&mut executor, "SELECT COUNT(*) FROM child;");
    assert_eq!(text_rows(&rows), vec![vec!["2".to_string()]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn foreign_keys_catalog_metadata() {
    let (storage, wal) = unique_engine("fkcatalog");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE parent (id BIGINT PRIMARY KEY);",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE child (
             id BIGINT PRIMARY KEY,
             parent_id BIGINT,
             CONSTRAINT child_parent_fk
                 FOREIGN KEY (parent_id)
                 REFERENCES parent(id)
         );",
    );

    let rows = exec(
        &mut executor,
        "SELECT contype, conrelid, confrelid
         FROM pg_catalog.pg_constraint
         WHERE conname = 'child_parent_fk';",
    );
    assert_eq!(rows.len(), 1, "expected one FK row in pg_constraint");
    assert_eq!(rows[0][0], Value::Text("f".into()), "contype should be 'f'");
    assert!(rows[0][2] != Value::Oid(0), "confrelid should be non-zero");

    let rows = exec(
        &mut executor,
        "SELECT constraint_type FROM information_schema.table_constraints
         WHERE constraint_name = 'child_parent_fk';",
    );
    assert_eq!(text_rows(&rows), vec![vec!["FOREIGN KEY".to_string()]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

fn dt(executor: &mut Executor<PlomidStorageEngine>) {
    exec_ok(executor, "CREATE TABLE t (country TEXT, status TEXT);");
    exec_ok(
        executor,
        "INSERT INTO t VALUES ('US','a'),('US','b'),('CA','a');",
    );
}

#[test]
fn grouping_sets_parse_and_execute() {
    let (storage, wal) = unique_engine("gsets");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    dt(&mut executor);

    let rows = exec(
        &mut executor,
        "SELECT country, status, COUNT(*)
         FROM t
         GROUP BY GROUPING SETS ((country), (status), ())
         ORDER BY country NULLS LAST, status NULLS LAST;",
    );
    let got = text_rows(&rows);
    let mut nonnull: Vec<Vec<String>> = got
        .iter()
        .filter(|r| r[0] != "NULL" || r[1] != "NULL")
        .map(|r| r.clone())
        .collect();
    let grand_total = got
        .iter()
        .find(|r| r[0] == "NULL" && r[1] == "NULL")
        .cloned();
    assert_eq!(
        grand_total,
        Some(vec![
            "NULL".to_string(),
            "NULL".to_string(),
            "3".to_string()
        ])
    );
    nonnull.sort();
    assert_eq!(
        nonnull,
        vec![
            vec!["CA".to_string(), "NULL".to_string(), "1".to_string()],
            vec!["NULL".to_string(), "a".to_string(), "2".to_string()],
            vec!["NULL".to_string(), "b".to_string(), "1".to_string()],
            vec!["US".to_string(), "NULL".to_string(), "2".to_string()],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn rollup_parse_and_execute() {
    let (storage, wal) = unique_engine("rollup");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    dt(&mut executor);

    let rows = exec(
        &mut executor,
        "SELECT country, status, COUNT(*)
         FROM t
         GROUP BY ROLLUP(country, status)
         ORDER BY country NULLS LAST, status NULLS LAST;",
    );
    let got = text_rows(&rows);
    // Expected: CA a 1, CA NULL 1, US a 1, US b 1, US NULL 2, NULL NULL 3 = 6 rows
    assert_eq!(got.len(), 6, "ROLLUP should produce 6 rows, got {got:#?}");
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![
            vec!["CA".to_string(), "NULL".to_string(), "1".to_string()],
            vec!["CA".to_string(), "a".to_string(), "1".to_string()],
            vec!["NULL".to_string(), "NULL".to_string(), "3".to_string()],
            vec!["US".to_string(), "NULL".to_string(), "2".to_string()],
            vec!["US".to_string(), "a".to_string(), "1".to_string()],
            vec!["US".to_string(), "b".to_string(), "1".to_string()],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn cube_parse_and_execute() {
    let (storage, wal) = unique_engine("cube");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    dt(&mut executor);

    let rows = exec(
        &mut executor,
        "SELECT country, status, COUNT(*)
         FROM t
         GROUP BY CUBE(country, status)
         ORDER BY country NULLS LAST, status NULLS LAST;",
    );
    let got = text_rows(&rows);
    // Expected: 3 detail + 2 country + 2 status + 1 grand total = 8 rows
    assert_eq!(got.len(), 8, "CUBE should produce 8 rows, got {got:#?}");
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![
            vec!["CA".to_string(), "NULL".to_string(), "1".to_string()],
            vec!["CA".to_string(), "a".to_string(), "1".to_string()],
            vec!["NULL".to_string(), "NULL".to_string(), "3".to_string()],
            vec!["NULL".to_string(), "a".to_string(), "2".to_string()],
            vec!["NULL".to_string(), "b".to_string(), "1".to_string()],
            vec!["US".to_string(), "NULL".to_string(), "2".to_string()],
            vec!["US".to_string(), "a".to_string(), "1".to_string()],
            vec!["US".to_string(), "b".to_string(), "1".to_string()],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn legacy_group_by_regression() {
    let (storage, wal) = unique_engine("legacy");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE customers (id INT PRIMARY KEY, email TEXT);",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE orders (id INT PRIMARY KEY, customer_id INT);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO customers VALUES (1,'a@x.com'),(2,'b@x.com');",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO orders VALUES (10,1),(20,1),(30,2);",
    );

    let rows = exec(
        &mut executor,
        "SELECT customer_id, COUNT(*) FROM orders GROUP BY customer_id ORDER BY customer_id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1".to_string(), "2".to_string()],
            vec!["2".to_string(), "1".to_string()]
        ]
    );

    let rows = exec(
        &mut executor,
        "SELECT c.id, c.email, COUNT(o.id)
         FROM customers c
         LEFT JOIN orders o ON o.customer_id = c.id
         GROUP BY c.id, c.email
         ORDER BY c.id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1".to_string(), "a@x.com".to_string(), "2".to_string()],
            vec!["2".to_string(), "b@x.com".to_string(), "1".to_string()],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
