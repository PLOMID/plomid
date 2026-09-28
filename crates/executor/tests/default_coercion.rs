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
//! Regression test for PLOMID torture-test section 35 (`INSERT ... RETURNING *`
//! with omitted `DEFAULT` columns).
//!
//! PostgreSQL semantics under test: a `DEFAULT` expression is evaluated and
//! then *assigned* to the column, so it must travel through the exact same
//! type-coercion path as an explicitly supplied `INSERT` value. A string
//! literal such as `'active'` parses as an untyped/`TEXT` runtime value and
//! must be coerced to the declared column type (e.g. `VARCHAR(30)`); an
//! integer literal such as `100` must be coerced to `NUMERIC(p,s)`, and so on.
//!
//! The original bug: omitted `status VARCHAR(30) DEFAULT 'active'` failed with
//! `column "status" expects varchar but received active`, while explicitly
//! passing the same `'active'` value succeeded. The fix re-coerces every
//! materialized default (literal `default_value` and evaluated `default_expr`
//! such as `CURRENT_TIMESTAMP`) via `coerce_value_for_column` in
//! `crates/executor/src/row.rs::apply_defaults_and_validate`.
//!
//! Coverage below mirrors the task requirements:
//! - VARCHAR / NUMERIC / BOOLEAN defaults coerced to the declared column type.
//! - DATE / TIMESTAMP defaults (evaluated default expressions).
//! - TIMESTAMP(p) typmod/precision still respected after coercion.
//! - `DEFAULT NULL` stays NULL for nullable columns; NOT NULL still enforced.
//! - `RETURNING *` surfaces the coerced defaults (the section-35 shape).
use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

// Build a fresh isolated engine in the OS temp dir so each test gets its own
// storage + WAL files and cannot leak rows into other tests. The `tag`
// keeps filenames unique per test; the PID guards against parallel workers.
fn fresh_executor(tag: &str) -> Executor<PlomidStorageEngine> {
    let storage =
        std::env::temp_dir().join(format!("plomid-defcoerce-{tag}-{}", std::process::id()));
    let wal =
        std::env::temp_dir().join(format!("plomid-defcoerce-{tag}-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    Executor::new(engine).unwrap()
}

// Run a DDL/DML statement that must succeed, discarding its result.
// Panics with the offending SQL + engine error to make failures diagnosable.
fn exec_ok<E: plomid_txn::StorageEngine>(exe: &mut Executor<E>, sql: &str) {
    let result = exe.execute(sql);
    assert!(
        result.is_ok(),
        "statement should succeed: {sql}\ngot {result:?}"
    );
}

fn select_rows<E: plomid_txn::StorageEngine>(exe: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match exe.execute(sql).expect("sql should execute") {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

#[test]
fn default_values_coerced_to_column_types() {
    let mut e = fresh_executor("main");
    exec_ok(
        &mut e,
        "CREATE TABLE default_coercion_test (id INTEGER PRIMARY KEY, status VARCHAR(30) NOT NULL DEFAULT 'active', amount NUMERIC(10,2) DEFAULT 100, enabled BOOLEAN DEFAULT TRUE)",
    );
    let rows = select_rows(
        &mut e,
        "INSERT INTO default_coercion_test (id) VALUES (1) RETURNING *",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int4(1));
    assert_eq!(rows[0][1], Value::VarChar("active".to_string()));
    // NUMERIC(10,2) default 100 must survive as numeric.
    assert!(
        matches!(rows[0][2], Value::Numeric(_)),
        "expected numeric default, got {:?}",
        rows[0][2]
    );
    assert!(
        rows[0][2].to_sql_text() == "100" || rows[0][2].to_sql_text() == "100.00",
        "expected numeric 100 default, got {:?}",
        rows[0][2]
    );
    assert_eq!(rows[0][3], Value::Bool(true));
}

#[test]
fn default_temporal_precision_respected() {
    let mut e = fresh_executor("temporal");
    exec_ok(
        &mut e,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, ts TIMESTAMP(3) DEFAULT TIMESTAMP '2026-01-01 12:34:56.123456')",
    );
    exec_ok(&mut e, "INSERT INTO t (id) VALUES (1)");
    let rows = select_rows(&mut e, "SELECT ts FROM t");
    assert_eq!(rows.len(), 1);
    // TIMESTAMP(3) must truncate 12:34:56.123456 -> 12:34:56.123 (PostgreSQL
    // renders with exactly 3 fractional digits; PLOMID pads to 6).
    let text = rows[0][0].to_sql_text();
    assert!(
        text.starts_with("2026-01-01 12:34:56.123"),
        "TIMESTAMP(3) precision must be respected, got {text}"
    );
    assert!(
        !text.starts_with("2026-01-01 12:34:56.1234"),
        "TIMESTAMP(3) must not keep extra precision, got {text}"
    );
}

#[test]
fn default_date_and_timestamp() {
    let mut e = fresh_executor("datets");
    exec_ok(
        &mut e,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, d DATE DEFAULT DATE '2026-01-01', ts TIMESTAMP DEFAULT TIMESTAMP '2026-01-02 03:04:05')",
    );
    exec_ok(&mut e, "INSERT INTO t (id) VALUES (1)");
    let rows = select_rows(&mut e, "SELECT d, ts FROM t");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].to_sql_text(), "2026-01-01");
    assert_eq!(rows[0][1].to_sql_text(), "2026-01-02 03:04:05");
}

#[test]
fn default_null_and_not_null() {
    let mut e = fresh_executor("null");
    exec_ok(
        &mut e,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, value VARCHAR(30) DEFAULT NULL)",
    );
    exec_ok(&mut e, "INSERT INTO t (id) VALUES (1)");
    let rows = select_rows(&mut e, "SELECT * FROM t");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Null);

    exec_ok(
        &mut e,
        "CREATE TABLE n (id INTEGER PRIMARY KEY, s VARCHAR(10) NOT NULL)",
    );
    let result = e.execute("INSERT INTO n (id) VALUES (1)");
    assert!(result.is_err(), "NOT NULL must still be enforced");
}
