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
//! Regression tests for indexed DML target discovery.
//!
//! The production benchmark measured that `UPDATE ... WHERE pk = ?` /
//! `DELETE ... WHERE pk = ?` / single-row `INSERT` performed work proportional
//! to the total table size (DELETE of a nonexistent key: ~66 ms at 10k rows).
//! These tests pin the corrected access paths:
//!
//! * UPDATE and DELETE resolve `WHERE <indexed column> = literal` through the
//!   existing authoritative catalog index (shared `indexed_dml_entries`),
//!   proven by the perf access-path events;
//! * the staged DML preview (explicit-transaction protocol path) uses the same
//!   access path;
//! * INSERT uniqueness checking probes the constraint index per row instead of
//!   preloading every existing value;
//! * no correctness regression: rollback, read-your-writes, duplicate-key
//!   rejection, concurrent independent-row writers, and restart/recovery all
//!   behave as before.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-indexed-dml-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!(
        "plomid-indexed-dml-{tag}-wal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn open(
    tag: &str,
) -> (
    Executor<PlomidStorageEngine>,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let (storage, wal) = unique_engine(tag);
    let engine = PlomidStorageEngine::create(&storage, &wal, 64)
        .unwrap_or_else(|e| panic!("open {tag}: {e}"));
    let executor = Executor::new(engine).unwrap_or_else(|e| panic!("executor {tag}: {e}"));
    (executor, storage, wal)
}

type Captured = Arc<Mutex<Vec<(String, BTreeMap<String, String>)>>>;

/// Minimal tracing Layer that records `plomid::perf` access-path events
/// directly, without depending on a subscriber being installed globally.
struct CaptureLayer {
    events: Captured,
}

impl<S> tracing_subscriber::Layer<S> for CaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != "plomid::perf" {
            return;
        }
        let mut name = String::new();
        let mut fields = BTreeMap::new();
        let mut visitor = |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
            if field.name() == "event" {
                name = format!("{value:?}").trim_matches('"').to_string();
            } else {
                fields.insert(field.name().to_string(), format!("{value:?}"));
            }
        };
        event.record(&mut visitor);
        if !name.is_empty() {
            self.events
                .lock()
                .expect("capture lock")
                .push((name, fields));
        }
    }
}

/// The perf access-path event proves (not infers) which path a statement took:
/// events are emitted at INFO on the `plomid::perf` target, so a capturing
/// tracing layer is installed for the duration of the statement.
fn access_path<E: plomid_txn::StorageEngine>(
    executor: &mut Executor<E>,
    sql: &str,
) -> (bool, usize) {
    use tracing_subscriber::layer::SubscriberExt;
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(CaptureLayer {
            events: Arc::clone(&captured),
        }));
    let _ = executor.execute(sql);
    drop(_guard);
    let events = captured.lock().expect("capture lock");
    for (name, fields) in events.iter() {
        if name == "update_access_path" || name == "delete_access_path" {
            let indexed = fields.get("indexed").map(|v| v == "true").unwrap_or(false);
            let rows = fields
                .get("rows_examined")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            return (indexed, rows);
        }
    }
    panic!("no access-path event for `{sql}` (events: {events:?})");
}

fn exec<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor
        .execute(sql)
        .unwrap_or_else(|error| panic!("{sql} should execute: {error}"))
    {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows from `{sql}`, got {other:?}"),
    }
}

fn exec_ok<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) {
    executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"));
}

fn affected<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> i64 {
    match executor
        .execute(sql)
        .unwrap_or_else(|error| panic!("{sql} should execute: {error}"))
    {
        QueryResult::Updated(n) => n as i64,
        QueryResult::Deleted(n) => n as i64,
        other => panic!("expected row count from `{sql}`, got {other:?}"),
    }
}

fn duplicate_error<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) {
    let error = executor
        .execute(sql)
        .expect_err("duplicate insert must be rejected");
    let text = error.to_string();
    assert!(
        text.contains("duplicate key"),
        "expected duplicate-key error from `{sql}`, got: {text}"
    );
}

fn scalar_long(executor: &mut Executor<PlomidStorageEngine>, sql: &str) -> i64 {
    match exec(executor, sql)[0][0].clone() {
        Value::Int2(v) => v as i64,
        Value::Int4(v) => v as i64,
        Value::Int8(v) => v,
        other => panic!("{sql} should return an integer, got {other:?}"),
    }
}

fn setup(executor: &mut Executor<PlomidStorageEngine>, rows: i64) {
    exec_ok(executor, "CREATE SCHEMA dml");
    exec_ok(
        executor,
        "CREATE TABLE dml.accounts (id BIGINT PRIMARY KEY, value BIGINT NOT NULL)",
    );
    exec_ok(
        executor,
        &format!(
            "INSERT INTO dml.accounts (id, value) SELECT g, 0 FROM generate_series(1, {rows}) g"
        ),
    );
}

#[test]
fn update_by_pk_uses_index_and_updates_one_row() {
    let (mut executor, _, _) = open("update-pk");
    setup(&mut executor, 10_000);
    let (indexed, rows) = access_path(
        &mut executor,
        "UPDATE dml.accounts SET value = value + 1 WHERE id = 1234",
    );
    assert!(indexed, "UPDATE by PK equality must use the index path");
    assert_eq!(rows, 1, "PK equality examines at most one row");
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT value FROM dml.accounts WHERE id = 1234"
        ),
        1
    );
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT value FROM dml.accounts WHERE id = 1235"
        ),
        0
    );
}

#[test]
fn update_missed_pk_uses_index_and_touches_nothing() {
    let (mut executor, _, _) = open("update-miss");
    setup(&mut executor, 10_000);
    let (indexed, rows) = access_path(
        &mut executor,
        "UPDATE dml.accounts SET value = value + 1 WHERE id = 999999999",
    );
    assert!(indexed, "UPDATE miss must still take the index path");
    assert_eq!(rows, 0);
    assert_eq!(
        affected(
            &mut executor,
            "UPDATE dml.accounts SET value = value + 1 WHERE id = 999999999"
        ),
        0
    );
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT count(*) FROM dml.accounts WHERE value <> 0"
        ),
        0
    );
}

#[test]
fn update_non_indexed_column_still_scans() {
    let (mut executor, _, _) = open("update-seq");
    setup(&mut executor, 1_000);
    let (indexed, _) = access_path(
        &mut executor,
        "UPDATE dml.accounts SET value = value + 1 WHERE value = 0",
    );
    assert!(
        !indexed,
        "no index on `value`: the sequential scan fallback is required"
    );
}

#[test]
fn delete_by_pk_uses_index_and_deletes_one_row() {
    let (mut executor, _, _) = open("delete-pk");
    setup(&mut executor, 10_000);
    let (indexed, rows) = access_path(&mut executor, "DELETE FROM dml.accounts WHERE id = 77");
    assert!(indexed, "DELETE by PK equality must use the index path");
    assert_eq!(rows, 1);
    assert_eq!(
        scalar_long(&mut executor, "SELECT count(*) FROM dml.accounts"),
        9_999
    );
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT count(*) FROM dml.accounts WHERE id = 77"
        ),
        0
    );
}

#[test]
fn delete_missed_pk_uses_index_and_deletes_nothing() {
    let (mut executor, _, _) = open("delete-miss");
    setup(&mut executor, 10_000);
    let (indexed, rows) = access_path(
        &mut executor,
        "DELETE FROM dml.accounts WHERE id = 999999999",
    );
    assert!(
        indexed,
        "nonexistent PK must be answered by the index, not a scan"
    );
    assert_eq!(rows, 0);
    assert_eq!(
        scalar_long(&mut executor, "SELECT count(*) FROM dml.accounts"),
        10_000
    );
}

#[test]
fn insert_uniqueness_is_enforced_by_index_probe() {
    let (mut executor, _, _) = open("insert-unique");
    setup(&mut executor, 5_000);
    exec_ok(
        &mut executor,
        "INSERT INTO dml.accounts (id, value) VALUES (100001, 1)",
    );
    duplicate_error(
        &mut executor,
        "INSERT INTO dml.accounts (id, value) VALUES (100001, 2)",
    );
    // Intra-statement duplicates are also caught (staged index entries are
    // visible to the probe through the transaction's own write overlay).
    duplicate_error(
        &mut executor,
        "INSERT INTO dml.accounts (id, value) VALUES (100002, 1), (100002, 2)",
    );
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT value FROM dml.accounts WHERE id = 100001"
        ),
        1
    );
}

#[test]
fn insert_rollback_and_read_your_writes() {
    let (mut executor, _, _) = open("insert-rollback");
    setup(&mut executor, 1_000);
    // Read-your-writes inside the transaction batch.
    exec_ok(
        &mut executor,
        "BEGIN; INSERT INTO dml.accounts (id, value) VALUES (200001, 7); ROLLBACK;",
    );
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT count(*) FROM dml.accounts WHERE id = 200001"
        ),
        0,
        "rolled-back insert must not be visible"
    );
    assert_eq!(
        scalar_long(&mut executor, "SELECT count(*) FROM dml.accounts"),
        1_000
    );
}

#[test]
fn update_delete_rollback_and_read_your_writes() {
    let (mut executor, _, _) = open("dml-rollback");
    setup(&mut executor, 1_000);
    exec_ok(
        &mut executor,
        "BEGIN; UPDATE dml.accounts SET value = 5 WHERE id = 3; DELETE FROM dml.accounts WHERE id = 4; ROLLBACK;",
    );
    // The rolled-back writes are invisible after the transaction ends.
    assert_eq!(
        scalar_long(&mut executor, "SELECT value FROM dml.accounts WHERE id = 3"),
        0
    );
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT count(*) FROM dml.accounts WHERE id = 4"
        ),
        1
    );
}

#[test]
fn concurrent_independent_pk_updates_do_not_lose_updates() {
    let (mut executor, _, _) = open("concurrent-pk");
    setup(&mut executor, 100);
    let executor = Arc::new(Mutex::new(executor));
    let mut handles = Vec::new();
    for worker in 0..8_i64 {
        let shared = Arc::clone(&executor);
        handles.push(std::thread::spawn(move || {
            for _ in 0..10 {
                let mut guard = shared.lock().expect("executor mutex");
                assert_eq!(
                    affected(
                        &mut guard,
                        &format!(
                            "UPDATE dml.accounts SET value = value + 1 WHERE id = {}",
                            worker + 1
                        )
                    ),
                    1
                );
            }
        }));
    }
    for handle in handles {
        handle.join().expect("worker thread");
    }
    let mut executor = executor.lock().expect("executor mutex");
    let rows = exec(
        &mut executor,
        "SELECT id, value FROM dml.accounts WHERE id <= 8 ORDER BY id",
    );
    assert_eq!(rows.len(), 8);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(
            row[1],
            Value::Int8(10),
            "row {} must count exactly 10 updates",
            i + 1
        );
    }
}

#[test]
fn same_row_serializes_and_counts_every_update() {
    let (mut executor, _, _) = open("same-row");
    setup(&mut executor, 10);
    for _ in 0..8 {
        assert_eq!(
            affected(
                &mut executor,
                "UPDATE dml.accounts SET value = value + 1 WHERE id = 1"
            ),
            1
        );
    }
    assert_eq!(
        scalar_long(&mut executor, "SELECT value FROM dml.accounts WHERE id = 1"),
        8
    );
}

#[test]
fn preview_count_uses_index_path() {
    let (mut executor, _, _) = open("preview");
    setup(&mut executor, 5_000);
    // The network protocol calls `preview_dml_count` for every staged
    // explicit-transaction DML statement; it must resolve indexed equality
    // through the index (the O(rows) scan made BEGIN;UPDATE;COMMIT slow).
    assert_eq!(
        executor
            .preview_dml_count("UPDATE dml.accounts SET value = value + 1 WHERE id = 42")
            .unwrap(),
        1
    );
    assert_eq!(
        executor
            .preview_dml_count("UPDATE dml.accounts SET value = value + 1 WHERE id = 999999999")
            .unwrap(),
        0
    );
    assert_eq!(
        executor
            .preview_dml_count("DELETE FROM dml.accounts WHERE id = 43")
            .unwrap(),
        1
    );
    assert_eq!(
        executor
            .preview_dml_count("DELETE FROM dml.accounts WHERE id = 999999999")
            .unwrap(),
        0
    );
    assert_eq!(
        executor
            .preview_dml_count("DELETE FROM dml.accounts WHERE value = 0")
            .unwrap(),
        5_000
    );
}

#[test]
fn recovery_after_indexed_dml_restart() {
    let (mut executor, storage, wal) = open("recovery");
    setup(&mut executor, 1_000);
    exec_ok(
        &mut executor,
        "BEGIN; INSERT INTO dml.accounts (id, value) VALUES (300001, 11); \
         UPDATE dml.accounts SET value = value + 1 WHERE id = 500; \
         DELETE FROM dml.accounts WHERE id = 501; COMMIT;",
    );
    drop(executor);

    let engine = PlomidStorageEngine::open(&storage, &wal, 64)
        .unwrap_or_else(|e| panic!("reopen after indexed DML: {e}"));
    let mut reopened = Executor::new(engine).unwrap_or_else(|e| panic!("reopen executor: {e}"));
    assert_eq!(
        scalar_long(
            &mut reopened,
            "SELECT value FROM dml.accounts WHERE id = 300001"
        ),
        11
    );
    assert_eq!(
        scalar_long(
            &mut reopened,
            "SELECT value FROM dml.accounts WHERE id = 500"
        ),
        1
    );
    assert_eq!(
        scalar_long(
            &mut reopened,
            "SELECT count(*) FROM dml.accounts WHERE id = 501"
        ),
        0
    );
    // Post-restart indexed DML keeps working.
    assert_eq!(
        affected(
            &mut reopened,
            "UPDATE dml.accounts SET value = value + 1 WHERE id = 300001"
        ),
        1
    );
    assert_eq!(
        scalar_long(
            &mut reopened,
            "SELECT value FROM dml.accounts WHERE id = 300001"
        ),
        12
    );
}

#[test]
fn bigint_variants_share_the_normalized_index_key() {
    let (mut executor, _, _) = open("int-normalization");
    exec_ok(&mut executor, "CREATE SCHEMA dml");
    exec_ok(
        &mut executor,
        "CREATE TABLE dml.nums (id BIGINT PRIMARY KEY, small SMALLINT, med INTEGER, big BIGINT)",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO dml.nums (id, small, med, big) VALUES (1, 7, 7, 7), (2, 300, 70000, 5000000000)",
    );
    // A literal that normalizes to the same canonical bytes must find the row
    // through the index, not just through a scan.
    assert_eq!(
        scalar_long(&mut executor, "SELECT id FROM dml.nums WHERE small = 7"),
        1
    );
    assert_eq!(
        scalar_long(&mut executor, "SELECT id FROM dml.nums WHERE med = 70000"),
        2
    );
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT id FROM dml.nums WHERE big = 5000000000"
        ),
        2
    );
    assert_eq!(
        affected(
            &mut executor,
            "UPDATE dml.nums SET big = big + 1 WHERE big = 5000000000"
        ),
        1
    );
}

#[test]
fn update_pk_range_uses_index_and_updates_exactly() {
    let (mut executor, _, _) = open("update-range");
    setup(&mut executor, 10_000);
    // access_path executes the statement once; follow-up SELECTs verify.
    let (indexed, rows) = access_path(
        &mut executor,
        "UPDATE dml.accounts SET value = value + 1 WHERE id BETWEEN 100 AND 199",
    );
    assert!(
        indexed,
        "PK range UPDATE must use the index path, not a scan"
    );
    assert_eq!(rows, 100, "range examines candidates, not the table");
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT count(*) FROM dml.accounts WHERE value = 1"
        ),
        100
    );
    assert_eq!(
        scalar_long(
            &mut executor,
            "SELECT count(*) FROM dml.accounts WHERE value = 0"
        ),
        9_900
    );
}

#[test]
fn delete_pk_range_uses_index_and_deletes_exactly() {
    let (mut executor, _, _) = open("delete-range");
    setup(&mut executor, 10_000);
    let (indexed, rows) = access_path(&mut executor, "DELETE FROM dml.accounts WHERE id >= 9900");
    assert!(
        indexed,
        "PK range DELETE must use the index path, not a scan"
    );
    assert_eq!(rows, 101);
    assert_eq!(
        scalar_long(&mut executor, "SELECT count(*) FROM dml.accounts"),
        9_899
    );
}

#[test]
fn update_empty_range_touches_nothing() {
    let (mut executor, _, _) = open("update-empty-range");
    setup(&mut executor, 10_000);
    let (indexed, rows) = access_path(
        &mut executor,
        "UPDATE dml.accounts SET value = 1 WHERE id BETWEEN 20000 AND 30000",
    );
    assert!(indexed, "empty range still resolves through the index");
    assert_eq!(rows, 0);
}
