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
//! Focused regression tests for temporal precision (TIME(p) / TIMESTAMP(p) /
//! TIMESTAMPTZ(p)) through the full engine: parse → AST → type → value →
//! formatting → table round-trip → comparison. Mirrors PostgreSQL 17 semantics.
//!
//! Not a time-series/storage test; it only exercises scalar temporal values
//! and plain table storage via the existing MVCC/WAL path.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str, id: u64) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-tprec-{tag}-{id}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!(
        "plomid-tprec-{tag}-wal-{id}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn fresh_executor(tag: &str, id: u64) -> Executor<PlomidStorageEngine> {
    let (storage, wal) = unique_engine(tag, id);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    Executor::new(engine).unwrap()
}

fn select_rows<E: plomid_txn::StorageEngine>(exe: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match exe.execute(sql).expect("sql should execute") {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

fn exec_err<E: plomid_txn::StorageEngine>(exe: &mut Executor<E>, sql: &str) {
    let result = exe.execute(sql);
    assert!(result.is_err(), "expected error for: {sql}\ngot {result:?}");
}

/// Runs a statement (CREATE/INSERT/SET...) that should succeed, discarding the result.
fn exec_ok<E: plomid_txn::StorageEngine>(exe: &mut Executor<E>, sql: &str) {
    let result = exe.execute(sql);
    assert!(
        result.is_ok(),
        "statement should succeed: {sql}\ngot {result:?}"
    );
}

/// Returns the single scalar cell produced by `sql`.
fn one<E: plomid_txn::StorageEngine>(exe: &mut Executor<E>, sql: &str) -> Value {
    let rows = select_rows(exe, sql);
    assert_eq!(rows.len(), 1, "expected one row for {sql}");
    assert_eq!(rows[0].len(), 1, "expected one column for {sql}");
    rows[0][0].clone()
}

const FULL: &[(&str, &str)] = &[
    ("TIME", "12:34:56.123456"),
    ("TIMESTAMP", "2026-01-15 12:34:56.123456"),
    ("TIMESTAMPTZ", "2026-01-15 12:34:56.123456+00"),
];
#[test]
fn typed_literal_precision_0_to_6() {
    let mut e = fresh_executor("lit", 1);
    // (precision, expected fractional text using PLOMID's 6-digit micros)
    let cases: &[(&str, &str)] = &[
        ("0", ".000000"),
        ("1", ".100000"),
        ("2", ".120000"),
        ("3", ".123000"),
        ("4", ".123500"),
        ("5", ".123460"),
        ("6", ".123456"),
    ];
    // Note: at precision 4, PostgreSQL 17 rounds .123456 to .123500 (nearest
    // 100 µs), which is what the engine reproduces.
    let n = FULL.len();
    for i in 0..n {
        let (ty, text) = FULL[i];
        let m = cases.len();
        for j in 0..m {
            let (p, frac) = cases[j];
            let sql = format!("SELECT {ty}({p}) '{text}';");
            let out = one(&mut e, &sql).to_sql_text();
            let expected = if p == "0" {
                // precision 0: no fractional seconds
                text.split('.').next().unwrap().to_string()
            } else {
                let stem = text.split('.').next().unwrap();
                format!("{stem}{frac}")
            };
            assert_eq!(out, expected, "{sql}: expected {expected}, got {out}");
        }
    }
}

#[test]
fn typed_literal_multiword_and_shorthand_equivalence() {
    let mut e = fresh_executor("multiword", 2);
    let sql = "2026-01-15 12:34:56.123456";
    let a = one(
        &mut e,
        &format!("SELECT TIMESTAMP WITH TIME ZONE(3) '{sql}+00';"),
    );
    let b = one(
        &mut e,
        &format!("SELECT TIMESTAMP(3) WITH TIME ZONE '{sql}+00';"),
    );
    let c = one(&mut e, &format!("SELECT TIMESTAMPTZ(3) '{sql}+00';"));
    let d = one(&mut e, &format!("SELECT TIMESTAMP(3) '{sql}';"));
    assert_eq!(a, b, "both multiword forms must resolve equivalently");
    assert_eq!(
        a, c,
        "shorthand TIMESTAMPTZ(3) must match TIMESTAMP WITH TIME ZONE(3)"
    );
    // TIMESTAMPTZ(3) is a different type family from TIMESTAMP(3) even when the
    // stored micros match, so compare their formatted text instead of equality.
    assert_eq!(a.to_sql_text(), d.to_sql_text());
}

#[test]
fn cast_preserves_precision() {
    let mut e = fresh_executor("cast", 3);
    assert_eq!(
        one(&mut e, "SELECT '12:34:56.123456'::TIME(3);").to_sql_text(),
        "12:34:56.123000",
    );
    assert_eq!(
        one(&mut e, "SELECT '2026-01-15 12:34:56.123456'::TIMESTAMP(3);").to_sql_text(),
        "2026-01-15 12:34:56.123000",
    );
    assert_eq!(
        one(
            &mut e,
            "SELECT '2026-01-15 12:34:56.123456+00'::TIMESTAMPTZ(3);"
        )
        .to_sql_text(),
        "2026-01-15 12:34:56.123000",
    );
}

#[test]
fn boundary_rounding_carries_into_next_unit() {
    let mut e = fresh_executor("boundary", 4);
    // time: 23:59:59.999999 rounds up through midnight (PostgreSQL 17).
    assert_eq!(
        one(&mut e, "SELECT TIME(0) '23:59:59.999999';").to_sql_text(),
        "24:00:00"
    );
    assert_eq!(
        one(&mut e, "SELECT TIME(3) '23:59:59.999999';").to_sql_text(),
        "24:00:00"
    );
    assert_eq!(
        one(&mut e, "SELECT TIME(6) '23:59:59.999999';").to_sql_text(),
        "23:59:59.999999",
    );
    // timestamp: carries into the next day.
    assert_eq!(
        one(&mut e, "SELECT TIMESTAMP(0) '2026-01-15 23:59:59.999999';").to_sql_text(),
        "2026-01-16 00:00:00",
    );
    assert_eq!(
        one(&mut e, "SELECT TIMESTAMP(3) '2026-01-15 23:59:59.999999';").to_sql_text(),
        "2026-01-16 00:00:00",
    );
    assert_eq!(
        one(&mut e, "SELECT TIMESTAMP(6) '2026-01-15 23:59:59.999999';").to_sql_text(),
        "2026-01-15 23:59:59.999999",
    );
}

#[test]
fn table_storage_preserves_declared_precision() {
    let mut e = fresh_executor("table", 5);
    exec_ok(
        &mut e,
        "CREATE TABLE temporal_precision_test (
            t0 TIME(0), t3 TIME(3), t6 TIME(6),
            ts0 TIMESTAMP(0), ts3 TIMESTAMP(3), ts6 TIMESTAMP(6),
            tz0 TIMESTAMPTZ(0), tz3 TIMESTAMPTZ(3), tz6 TIMESTAMPTZ(6)
        );",
    );
    exec_ok(
        &mut e,
        "INSERT INTO temporal_precision_test VALUES (
            '12:34:56.123456', '12:34:56.123456', '12:34:56.123456',
            '2026-01-15 12:34:56.123456', '2026-01-15 12:34:56.123456', '2026-01-15 12:34:56.123456',
            '2026-01-15 12:34:56.123456+00', '2026-01-15 12:34:56.123456+00', '2026-01-15 12:34:56.123456+00'
        );",
    );
    let rows = select_rows(&mut e, "SELECT * FROM temporal_precision_test;");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.len(), 9);
    assert_eq!(row[0].to_sql_text(), "12:34:56", "t0");
    assert_eq!(row[1].to_sql_text(), "12:34:56.123000", "t3");
    assert_eq!(row[2].to_sql_text(), "12:34:56.123456", "t6");
    assert_eq!(row[3].to_sql_text(), "2026-01-15 12:34:56", "ts0");
    assert_eq!(row[4].to_sql_text(), "2026-01-15 12:34:56.123000", "ts3");
    assert_eq!(row[5].to_sql_text(), "2026-01-15 12:34:56.123456", "ts6");
    assert_eq!(row[6].to_sql_text(), "2026-01-15 12:34:56", "tz0");
    assert_eq!(row[7].to_sql_text(), "2026-01-15 12:34:56.123000", "tz3");
    assert_eq!(row[8].to_sql_text(), "2026-01-15 12:34:56.123456", "tz6");
}

#[test]
fn comparison_semantics() {
    let mut e = fresh_executor("cmp", 6);
    assert_eq!(
        one(
            &mut e,
            "SELECT TIMESTAMP(3) '2026-01-15 12:34:56.123'
             = TIMESTAMP(6) '2026-01-15 12:34:56.123000';",
        ),
        Value::Bool(true),
    );
    assert_eq!(
        one(
            &mut e,
            "SELECT TIMESTAMP(6) '2026-01-15 12:34:56.123456'
             > TIMESTAMP(6) '2026-01-15 12:34:56.123000';",
        ),
        Value::Bool(true),
    );
}

#[test]
fn invalid_precision_rejected() {
    let mut e = fresh_executor("invalid", 7);
    exec_err(&mut e, "SELECT TIME(7) '12:34:56';");
    exec_err(&mut e, "SELECT TIMESTAMP(7) '2026-01-15 12:34:56';");
    exec_err(&mut e, "SELECT TIMESTAMPTZ(7) '2026-01-15 12:34:56+00';");
    exec_err(&mut e, "SELECT TIMESTAMP(-1) '2026-01-15 12:34:56';");
    exec_err(&mut e, "SELECT TIME(100) '12:34:56';");
}
