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
//! Regression tests for unqualified table resolution inside explicit
//! transactions (torture test Section 40).
//!
//! Transactional DML on the wire path is validated with
//! `Executor::validate_dml_strict_with_search_path`, which must resolve
//! unqualified names against the connection's `search_path` — exactly like the
//! autocommit path (`execute_all_with_search_path`) — instead of the
//! executor's default `public` path.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage = std::env::temp_dir().join(format!(
        "plomid-txn-search-path-{tag}-{}",
        std::process::id()
    ));
    let wal = std::env::temp_dir().join(format!(
        "plomid-txn-search-path-{tag}-wal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn exec_rows(executor: &mut Executor<PlomidStorageEngine>, sql: &str, path: &[String]) -> u64 {
    let (results, _) = executor
        .execute_all_with_search_path(sql, path)
        .unwrap_or_else(|error| panic!("{sql} should execute: {error}"));
    results
        .into_iter()
        .map(|result| match result {
            QueryResult::Rows { rows, .. } => rows.len() as u64,
            _ => 0,
        })
        .sum()
}

fn count_customers(executor: &mut Executor<PlomidStorageEngine>, path: &[String]) -> u64 {
    let (results, _) = executor
        .execute_all_with_search_path("SELECT COUNT(*) FROM customers", path)
        .unwrap();
    match &results[0] {
        QueryResult::Rows { rows, .. } => match &rows[0][0] {
            Value::Int8(n) => *n as u64,
            other => panic!("unexpected count value {other:?}"),
        },
        other => panic!("expected rows, got {other:?}"),
    }
}

fn cleanup(paths: &[std::path::PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

/// The minimal reproduction of the torture-test Section 40 failure: a custom
/// `search_path` session, then transactional DML against an unqualified table
/// in that schema.
#[test]
fn transactional_dml_resolves_unqualified_table_via_search_path() {
    let (storage, wal) = unique_engine("insert");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let path = ["plomid_torture".to_string(), "public".to_string()];

    exec_rows(
        &mut executor,
        "CREATE SCHEMA plomid_torture",
        &["public".to_string()],
    );
    exec_rows(
        &mut executor,
        "CREATE TABLE customers (id BIGINT PRIMARY KEY)",
        &path,
    );
    // Autocommit insert resolves through the session search_path.
    exec_rows(&mut executor, "INSERT INTO customers VALUES (1)", &path);
    assert_eq!(count_customers(&mut executor, &path), 1);

    // Transactional DML validation must use the same resolution rules.
    let staged = executor
        .validate_dml_strict_with_search_path("INSERT INTO customers VALUES (2)", &path)
        .unwrap();
    assert_eq!(staged, 1);
    // The throwaway validation transaction must not persist the row.
    assert_eq!(count_customers(&mut executor, &path), 1);

    // Full BEGIN/COMMIT through the transaction path.
    exec_rows(
        &mut executor,
        "BEGIN; INSERT INTO customers VALUES (200); COMMIT;",
        &path,
    );
    assert_eq!(count_customers(&mut executor, &path), 2);

    // ROLLBACK discards the transactional write.
    exec_rows(
        &mut executor,
        "BEGIN; INSERT INTO customers VALUES (201); ROLLBACK;",
        &path,
    );
    assert_eq!(count_customers(&mut executor, &path), 2);

    // Schema-qualified access keeps working independently of search_path.
    exec_rows(
        &mut executor,
        "BEGIN; INSERT INTO plomid_torture.customers VALUES (202); COMMIT;",
        &["public".to_string()],
    );
    assert_eq!(count_customers(&mut executor, &path), 3);

    cleanup(&[storage, wal]);
}

/// Tables created after temporary tables exist must still resolve inside a
/// transaction (torture test Sections 38 → 40 ordering).
#[test]
fn transactional_dml_after_temp_table_creation() {
    let (storage, wal) = unique_engine("temp");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let path = ["plomid_torture".to_string(), "public".to_string()];

    exec_rows(
        &mut executor,
        "CREATE SCHEMA plomid_torture",
        &["public".to_string()],
    );
    exec_rows(
        &mut executor,
        "CREATE TABLE customers (id BIGINT PRIMARY KEY)",
        &path,
    );
    exec_rows(
        &mut executor,
        "CREATE TEMP TABLE delete_test (id INTEGER)",
        &path,
    );
    exec_rows(&mut executor, "INSERT INTO delete_test VALUES (1)", &path);

    let staged = executor
        .validate_dml_strict_with_search_path("INSERT INTO customers VALUES (200)", &path)
        .unwrap();
    assert_eq!(staged, 1);
    exec_rows(
        &mut executor,
        "BEGIN; INSERT INTO customers VALUES (200); COMMIT;",
        &path,
    );
    assert_eq!(count_customers(&mut executor, &path), 1);

    cleanup(&[storage, wal]);
}

/// Without the session search_path installed, transactional DML against an
/// unqualified table outside the default path must fail — pinning down the
/// resolution semantics this regression suite protects.
#[test]
fn default_path_validation_does_not_see_other_schemas() {
    let (storage, wal) = unique_engine("default");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let path = ["plomid_torture".to_string(), "public".to_string()];

    exec_rows(
        &mut executor,
        "CREATE SCHEMA plomid_torture",
        &["public".to_string()],
    );
    exec_rows(
        &mut executor,
        "CREATE TABLE customers (id BIGINT PRIMARY KEY)",
        &path,
    );
    assert!(executor
        .validate_dml_strict_with_search_path(
            "INSERT INTO customers VALUES (1)",
            &["public".to_string()]
        )
        .is_err());

    cleanup(&[storage, wal]);
}
