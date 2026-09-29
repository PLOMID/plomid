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
//! Regression tests for transaction-local uniqueness enforcement on the
//! concurrent INSERT path.
//!
//! Every uniqueness probe must answer "does this key conflict with committed
//! state OR with anything this transaction has already staged?" without
//! walking the whole buffered operation list. These tests pin the SQL-visible
//! behavior that the transaction-local overlay must preserve:
//!
//! * committed duplicate rejection;
//! * same-statement duplicate rejection;
//! * duplicate rejection across statements of one transaction;
//! * rollback leaving no transaction-local unique entry;
//! * composite UNIQUE tuples;
//! * SQL NULL-not-equal semantics;
//! * UPDATE/DELETE interactions with staged unique keys;
//! * concurrent transactions contending on one unique value;
//! * large multi-row INSERT scaling (the O(N^2) regression guard).
//!
//! The ignored [`insert_scaling`] harness is the performance evidence: run it
//! with
//! `PLOMID_SCALE_ROWS=1000 cargo test -p plomid-executor --test insert_unique_scaling -- --ignored --nocapture`.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_storage::StorageEngine as _;
use plomid_txn::{ConcurrentPlomidStorageEngine, PlomidStorageEngine};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

type Engine = ConcurrentPlomidStorageEngine;

fn unique_dir(tag: &str) -> (PathBuf, PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-insert-scale-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!(
        "plomid-insert-scale-{tag}-wal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn open(tag: &str) -> (Executor<Engine>, PathBuf, PathBuf) {
    let (storage, wal) = unique_dir(tag);
    let engine = Engine::create(&storage, &wal, 64).unwrap_or_else(|e| panic!("open {tag}: {e}"));
    let executor = Executor::new(engine).unwrap_or_else(|e| panic!("executor {tag}: {e}"));
    (executor, storage, wal)
}

fn exec(executor: &mut Executor<Engine>, sql: &str) {
    executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"));
}

fn scalar(executor: &mut Executor<Engine>, sql: &str) -> i64 {
    match executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should execute: {e}"))
    {
        QueryResult::Rows { rows, .. } => {
            assert_eq!(rows.len(), 1, "{sql}: expected exactly one row");
            match &rows[0][0] {
                Value::Int2(v) => i64::from(*v),
                Value::Int4(v) => i64::from(*v),
                Value::Int8(v) => *v,
                other => panic!("{sql}: expected integer, got {other:?}"),
            }
        }
        other => panic!("{sql}: expected rows, got {other:?}"),
    }
}

/// Runs a multi-statement batch through one `execute_all` call so a BEGIN/COMMIT
/// group stays inside a single transaction.
fn batch(executor: &mut Executor<Engine>, sql: &str) -> Result<(), String> {
    executor
        .execute_all(sql)
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

fn expect_err(executor: &mut Executor<Engine>, sql: &str) {
    assert!(
        executor.execute(sql).is_err(),
        "{sql} should fail but succeeded"
    );
}

fn make_schema(executor: &mut Executor<Engine>) {
    exec(
        executor,
        "CREATE SCHEMA r;
         CREATE TABLE r.kv (id BIGINT PRIMARY KEY, u BIGINT UNIQUE NOT NULL, payload TEXT);",
    );
}

// ---------------------------------------------------------------------------
// Test 1 - committed unique conflict
// ---------------------------------------------------------------------------

#[test]
fn committed_duplicate_is_rejected() {
    let (mut executor, _, _) = open("committed-dup");
    make_schema(&mut executor);
    exec(
        &mut executor,
        "INSERT INTO r.kv (id, u, payload) VALUES (1, 10, 'a')",
    );
    expect_err(
        &mut executor,
        "INSERT INTO r.kv (id, u, payload) VALUES (2, 10, 'b')",
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), 1);
}

// ---------------------------------------------------------------------------
// Test 1b - duplicate values are legal without a UNIQUE/PRIMARY KEY constraint
// ---------------------------------------------------------------------------

/// Duplicate values must NOT be rejected merely because they match: only a
/// constraint/index that requires uniqueness makes a repeated value invalid.
#[test]
fn duplicate_values_without_unique_constraint_are_allowed() {
    let (mut executor, _, _) = open("no-unique");
    exec(
        &mut executor,
        "CREATE SCHEMA r;
         CREATE TABLE r.dups (id BIGINT PRIMARY KEY, u BIGINT);",
    );
    // Same statement and separate statements: `u` is not unique, so repeats
    // are ordinary data.
    exec(
        &mut executor,
        "INSERT INTO r.dups (id, u) VALUES (1, 10), (2, 10)",
    );
    exec(&mut executor, "INSERT INTO r.dups (id, u) VALUES (3, 10)");
    assert_eq!(
        scalar(&mut executor, "SELECT count(*) FROM r.dups WHERE u = 10"),
        3
    );
    // The PRIMARY KEY still rejects a duplicate key.
    expect_err(&mut executor, "INSERT INTO r.dups (id, u) VALUES (1, 99)");
}

// ---------------------------------------------------------------------------
// Test 2 - same-statement duplicate
// ---------------------------------------------------------------------------

#[test]
fn same_statement_duplicate_is_rejected() {
    let (mut executor, _, _) = open("same-stmt-dup");
    make_schema(&mut executor);
    expect_err(
        &mut executor,
        "INSERT INTO r.kv (id, u, payload) VALUES (1, 10, 'a'), (2, 10, 'b')",
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), 0);
}

// ---------------------------------------------------------------------------
// Test 3 - duplicate across statements of one transaction
// ---------------------------------------------------------------------------

#[test]
fn duplicate_across_statements_in_one_transaction_is_rejected() {
    let (mut executor, _, _) = open("txn-dup");
    make_schema(&mut executor);
    let result = batch(
        &mut executor,
        "BEGIN;
         INSERT INTO r.kv (id, u, payload) VALUES (1, 10, 'a');
         INSERT INTO r.kv (id, u, payload) VALUES (2, 10, 'b');
         COMMIT;",
    );
    assert!(
        result.is_err(),
        "transaction-local duplicate must fail: {result:?}"
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), 0);
}

// ---------------------------------------------------------------------------
// Test 3b - distinct unique keys across statements of one transaction succeed
// ---------------------------------------------------------------------------

#[test]
fn distinct_unique_values_across_statements_in_one_transaction_succeed() {
    let (mut executor, _, _) = open("txn-distinct");
    make_schema(&mut executor);
    batch(
        &mut executor,
        "BEGIN;
         INSERT INTO r.kv (id, u, payload) VALUES (1, 10, 'a');
         INSERT INTO r.kv (id, u, payload) VALUES (2, 20, 'b');
         COMMIT;",
    )
    .expect("distinct transaction-local unique values must succeed");
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), 2);
}

// ---------------------------------------------------------------------------
// Test 4 - rollback must discard transaction-local unique state
// ---------------------------------------------------------------------------

#[test]
fn rollback_discards_transaction_local_unique_state() {
    let (mut executor, _, _) = open("txn-rollback");
    make_schema(&mut executor);
    batch(
        &mut executor,
        "BEGIN;
         INSERT INTO r.kv (id, u, payload) VALUES (1, 10, 'a');
         ROLLBACK;",
    )
    .expect("rollback batch");
    // The rolled-back key must be free in a later transaction.
    exec(
        &mut executor,
        "INSERT INTO r.kv (id, u, payload) VALUES (1, 10, 'a')",
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), 1);
}

// ---------------------------------------------------------------------------
// Test 5 - composite UNIQUE
// ---------------------------------------------------------------------------

#[test]
fn composite_unique_duplicate_and_distinct() {
    let (mut executor, _, _) = open("composite");
    exec(
        &mut executor,
        "CREATE SCHEMA r;
         CREATE TABLE r.comp (a INT, b INT, UNIQUE (a, b));",
    );
    exec(&mut executor, "INSERT INTO r.comp (a, b) VALUES (1, 2)");
    // Duplicate tuple, including the same statement.
    expect_err(&mut executor, "INSERT INTO r.comp (a, b) VALUES (1, 2)");
    expect_err(
        &mut executor,
        "INSERT INTO r.comp (a, b) VALUES (3, 3), (3, 3)",
    );
    // Distinct tuples sharing a leading or trailing component are fine.
    exec(
        &mut executor,
        "INSERT INTO r.comp (a, b) VALUES (1, 3), (2, 2)",
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.comp"), 3);
}

// ---------------------------------------------------------------------------
// Test 6 - NULL semantics
// ---------------------------------------------------------------------------

#[test]
fn null_unique_values_never_conflict() {
    let (mut executor, _, _) = open("nulls");
    exec(
        &mut executor,
        "CREATE SCHEMA r;
         CREATE TABLE r.n (id BIGINT PRIMARY KEY, u BIGINT UNIQUE);",
    );
    exec(
        &mut executor,
        "INSERT INTO r.n (id, u) VALUES (1, NULL), (2, NULL)",
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.n"), 2);
}

// ---------------------------------------------------------------------------
// Test 7 - UPDATE into a transaction-local unique key
// ---------------------------------------------------------------------------

#[test]
fn update_into_transaction_local_unique_key_is_rejected() {
    let (mut executor, _, _) = open("update-dup");
    make_schema(&mut executor);
    let result = batch(
        &mut executor,
        "BEGIN;
         INSERT INTO r.kv (id, u, payload) VALUES (1, 10, 'a');
         INSERT INTO r.kv (id, u, payload) VALUES (2, 20, 'b');
         UPDATE r.kv SET u = 10 WHERE id = 2;
         COMMIT;",
    );
    assert!(
        result.is_err(),
        "UPDATE onto a transaction-local unique key must fail: {result:?}"
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), 0);
}

// ---------------------------------------------------------------------------
// Test 8 - DELETE followed by INSERT of the same unique value in one txn
// ---------------------------------------------------------------------------

#[test]
fn delete_then_insert_same_unique_value_in_one_transaction() {
    let (mut executor, _, _) = open("delete-insert");
    make_schema(&mut executor);
    batch(
        &mut executor,
        "BEGIN;
         INSERT INTO r.kv (id, u, payload) VALUES (1, 10, 'a');
         DELETE FROM r.kv WHERE id = 1;
         INSERT INTO r.kv (id, u, payload) VALUES (2, 10, 'a');
         COMMIT;",
    )
    .expect("delete then insert of a freed unique value must succeed");
    assert_eq!(
        scalar(&mut executor, "SELECT count(*) FROM r.kv WHERE u = 10"),
        1
    );
}

// ---------------------------------------------------------------------------
// Test 9 - concurrent transactions contending on one unique value
// ---------------------------------------------------------------------------

#[test]
fn concurrent_transactions_cannot_both_take_one_unique_value() {
    let (storage, wal) = unique_dir("concurrent-uk");
    let shared = Arc::new(Mutex::new(
        PlomidStorageEngine::create(&storage, &wal, 64).expect("engine"),
    ));
    {
        let mut executor = Executor::new_shared(Arc::clone(&shared)).expect("bootstrap executor");
        exec(
            &mut executor,
            "CREATE SCHEMA r;
             CREATE TABLE r.kv (id BIGINT PRIMARY KEY, u BIGINT UNIQUE NOT NULL, payload TEXT);",
        );
    }

    let mut handles = Vec::new();
    for worker in 0..2_i64 {
        let shared = Arc::clone(&shared);
        handles.push(std::thread::spawn(move || {
            let mut executor = Executor::new_shared(shared).expect("session executor");
            // Distinct PKs, the SAME unique value: at most one can win.
            executor
                .execute(&format!(
                    "INSERT INTO r.kv (id, u, payload) VALUES ({}, 777, 'p')",
                    worker + 1
                ))
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        }));
    }
    let outcomes: Vec<Result<(), String>> = handles
        .into_iter()
        .map(|handle| handle.join().expect("worker thread panicked"))
        .collect();
    let successes = outcomes.iter().filter(|outcome| outcome.is_ok()).count();

    let mut verify = Executor::new_shared(Arc::clone(&shared)).expect("verify executor");
    let rows = scalar(&mut verify, "SELECT count(*) FROM r.kv WHERE u = 777");
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    assert_eq!(
        successes, 1,
        "exactly one concurrent writer must win: {outcomes:?}"
    );
    assert_eq!(rows, 1);
}

// ---------------------------------------------------------------------------
// Test 10 - large multi-row INSERT scaling (performance regression guard)
// ---------------------------------------------------------------------------

/// Inserts `PLOMID_SCALE_ROWS` (default 1000) rows of a table with a BIGINT
/// PRIMARY KEY and a BIGINT UNIQUE column in a single statement/transaction,
/// then prints the wall time and throughput.
///
/// Before the transaction-local overlay index the uniqueness probe walked the
/// whole buffered operation list once per row, so this harness scaled
/// quadratically. It is `#[ignore]`d because it is a measurement, not a fast
/// correctness gate; run it explicitly with `--ignored`.
#[test]
#[ignore = "performance harness; run with PLOMID_SCALE_ROWS"]
fn insert_scaling() {
    let rows: i64 = std::env::var("PLOMID_SCALE_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1000);
    let (mut executor, storage, wal) = open(&format!("scale-{rows}"));
    exec(
        &mut executor,
        "CREATE SCHEMA r;
         CREATE TABLE r.kv (id BIGINT PRIMARY KEY, u BIGINT UNIQUE NOT NULL, payload TEXT);",
    );
    let started = Instant::now();
    exec(
        &mut executor,
        &format!(
            "INSERT INTO r.kv (id, u, payload)
             SELECT g, g, 'payload' FROM generate_series(1, {rows}) g"
        ),
    );
    let elapsed = started.elapsed();
    let rate = rows as f64 / elapsed.as_secs_f64();
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), rows);
    // Duplicate of the last staged value must still be rejected after the bulk
    // statement: the committed index remains the authority.
    expect_err(
        &mut executor,
        &format!(
            "INSERT INTO r.kv (id, u, payload) VALUES ({}, {rows}, 'dup')",
            rows + 1
        ),
    );
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    println!(
        "PLOMID_INSERT_SCALE rows={rows} elapsed_s={:.6} rows_per_sec={:.0}",
        elapsed.as_secs_f64(),
        rate
    );
}

// Keep the unused-path helpers referenced so the harness compiles cleanly even
// when only a subset of tests is built.
#[allow(dead_code)]
fn _unused(_: &Path, _: &[Value]) {}
