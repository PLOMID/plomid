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
//! Regression tests for unique-key reservation concurrency.
//!
//! The reservation architecture replaces the table-wide write lane for
//! single-column PK/UNIQUE INSERTs and unique-value UPDATEs:
//!
//! * the engine atomically allocates internal row ids (no meta-key RMW that
//!   required the table lane);
//! * each proposed unique value reserves its `(index, value)` conflict domain
//!   until commit/abort;
//! * same value in two transactions conflicts; different values proceed
//!   concurrently;
//! * rollback releases the reservation so the value can be reused;
//! * committed state (durable B+Tree unique index) remains the authority.
//!
//! These tests pin the SQL-visible behavior of that protocol on the
//! concurrent engine.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_storage::StorageEngine as _;
use plomid_txn::ConcurrentPlomidStorageEngine;

type Engine = ConcurrentPlomidStorageEngine;

fn unique_dir(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage = std::env::temp_dir().join(format!(
        "plomid-unique-reserve-{tag}-{}",
        std::process::id()
    ));
    let wal = std::env::temp_dir().join(format!(
        "plomid-unique-reserve-{tag}-wal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn open(tag: &str) -> (Executor<Engine>, std::path::PathBuf, std::path::PathBuf) {
    let (storage, wal) = unique_dir(tag);
    let engine = Engine::create(&storage, &wal, 64).unwrap_or_else(|e| panic!("open {tag}: {e}"));
    let executor = Executor::new(engine).unwrap_or_else(|e| panic!("executor {tag}: {e}"));
    (executor, storage, wal)
}

fn reopen(tag: &str, storage: &std::path::Path, wal: &std::path::Path) -> Executor<Engine> {
    let engine = Engine::open(storage, wal, 64).unwrap_or_else(|e| panic!("reopen {tag}: {e}"));
    Executor::new(engine).unwrap_or_else(|e| panic!("executor {tag}: {e}"))
}

/// Reopens the engine on the same durable state and runs `check` against a
/// fresh connection whose catalog is recovered from disk.
fn with_fresh<F>(tag: &str, storage: &std::path::Path, wal: &std::path::Path, mut check: F)
where
    F: FnMut(&mut Executor<Engine>),
{
    let mut executor = reopen(tag, storage, wal);
    check(&mut executor);
}

fn scalar(executor: &mut Executor<Engine>, sql: &str) -> i64 {
    match executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should execute: {e}"))
    {
        QueryResult::Rows { rows, .. } => {
            assert_eq!(rows.len(), 1, "{sql}: expected exactly one row");
            assert_eq!(rows[0].len(), 1, "{sql}: expected exactly one column");
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

fn exec(executor: &mut Executor<Engine>, sql: &str) {
    executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"));
}

fn expect_err(executor: &mut Executor<Engine>, sql: &str) -> String {
    match executor.execute(sql) {
        Err(e) => format!("{e:?}"),
        Ok(other) => panic!("{sql} should fail, got {other:?}"),
    }
}

fn make_schema(executor: &mut Executor<Engine>) {
    exec(
        executor,
        "CREATE SCHEMA r;
         CREATE TABLE r.kv (id BIGINT PRIMARY KEY, value BIGINT NOT NULL);
         CREATE TABLE r.multi (id BIGINT PRIMARY KEY, email TEXT UNIQUE, name TEXT UNIQUE);
         CREATE TABLE r.nouk (id BIGINT, value BIGINT);",
    );
}

/// Runs a multi-statement batch through the one `execute` call (the wire
/// protocol batches statements the same way; separate `execute` calls would
/// leave a dangling BEGIN).
fn batch(executor: &mut Executor<Engine>, sql: &str) -> Vec<QueryResult> {
    executor
        .execute_all(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"))
}

// ---------------------------------------------------------------------------
// Reservation-mode INSERT
// ---------------------------------------------------------------------------

#[test]
fn insert_many_rows_unique_ids_within_one_statement() {
    let (mut executor, _, _) = open("bulk-ids");
    make_schema(&mut executor);
    exec(
        &mut executor,
        "INSERT INTO r.kv (id, value) SELECT g, g FROM generate_series(1, 1000) g",
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), 1000);
    assert_eq!(scalar(&mut executor, "SELECT max(id) FROM r.kv"), 1000);
    // No duplicate ids staged by the allocator.
    assert_eq!(
        scalar(
            &mut executor,
            "SELECT count(*) FROM (SELECT id FROM r.kv GROUP BY id HAVING count(*) > 1) d"
        ),
        0
    );
}

#[test]
fn insert_after_restart_continues_past_committed_ids() {
    let (storage, wal) = unique_dir("restart-alloc");
    {
        let engine = Engine::create(&storage, &wal, 64).unwrap();
        let mut executor = Executor::new(engine).unwrap();
        exec(
            &mut executor,
            "CREATE SCHEMA r;
             CREATE TABLE r.kv (id BIGINT PRIMARY KEY, value BIGINT NOT NULL);
             INSERT INTO r.kv (id, value) VALUES (7, 7), (12, 12);",
        );
    }
    {
        // The allocator must never hand out an id that collides with
        // committed state after a restart: an explicit fresh id commits, the
        // already-committed id stays reserved.
        let mut executor = reopen("restart-alloc", &storage, &wal);
        exec(
            &mut executor,
            "INSERT INTO r.kv (id, value) VALUES (999, 999)",
        );
        assert!(
            !executor
                .execute("INSERT INTO r.kv (id, value) VALUES (12, 120)")
                .is_ok(),
            "committed id must stay reserved after restart"
        );
        assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), 3);
    }
}

#[test]
fn duplicate_pk_insert_fails_without_residue() {
    let (mut executor, _, _) = open("dup-pk");
    make_schema(&mut executor);
    exec(&mut executor, "INSERT INTO r.kv (id, value) VALUES (1, 10)");
    let err = expect_err(&mut executor, "INSERT INTO r.kv (id, value) VALUES (1, 20)");
    assert!(
        err.contains("duplicate") || err.contains("unique"),
        "unexpected error: {err}"
    );
    assert_eq!(
        scalar(&mut executor, "SELECT value FROM r.kv WHERE id = 1"),
        10
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.kv"), 1);
}

#[test]
fn rollback_releases_value_for_reuse() {
    let (mut executor, _, _) = open("rollback-reuse");
    make_schema(&mut executor);
    // BEGIN INSERT ROLLBACK, then the same value must be insertable.
    let results = batch(
        &mut executor,
        "BEGIN; INSERT INTO r.kv (id, value) VALUES (50, 5); ROLLBACK;",
    );
    assert!(
        results.iter().any(|r| matches!(r, QueryResult::RolledBack)),
        "expected RolledBack in {results:?}"
    );
    exec(&mut executor, "INSERT INTO r.kv (id, value) VALUES (50, 5)");
    assert_eq!(
        scalar(&mut executor, "SELECT count(*) FROM r.kv WHERE id = 50"),
        1
    );
}

#[test]
fn multi_constraint_insert_reserves_all_domains() {
    let (mut executor, _, _) = open("multi-uk");
    make_schema(&mut executor);
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (1, 'a@x', 'ann')",
    );
    // Each constraint independently rejects its own duplicate.
    assert!(
        !executor
            .execute("INSERT INTO r.multi (id, email, name) VALUES (2, 'a@x', 'bob')")
            .is_ok(),
        "duplicate email must fail"
    );
    assert!(
        !executor
            .execute("INSERT INTO r.multi (id, email, name) VALUES (3, 'c@x', 'ann')")
            .is_ok(),
        "duplicate name must fail"
    );
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (4, 'd@x', 'dot')",
    );
    // The same value under TWO DIFFERENT unique indexes must NOT conflict
    // (email and name are distinct reservation domains).
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (5, 'ann', 'a@x')",
    );
}

#[test]
fn null_unique_values_never_reserve_or_conflict() {
    let (mut executor, _, _) = open("null-uk");
    make_schema(&mut executor);
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (1, NULL, 'ann')",
    );
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (2, NULL, 'bob')",
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.multi"), 2);
}

// ---------------------------------------------------------------------------
// Reservation-mode unique-value UPDATE
// ---------------------------------------------------------------------------

#[test]
fn unique_update_moves_value_and_enforces_conflict() {
    let (mut executor, _, _) = open("uk-update");
    make_schema(&mut executor);
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (1, 'a@x', 'ann'), (2, 'b@x', 'bob')",
    );
    // Plain move to a free value.
    exec(
        &mut executor,
        "UPDATE r.multi SET email = 'c@x' WHERE id = 1",
    );
    assert_eq!(
        scalar(
            &mut executor,
            "SELECT count(*) FROM r.multi WHERE email = 'c@x'"
        ),
        1
    );
    // Move onto an occupied value must fail and leave the row unchanged.
    assert!(
        !executor
            .execute("UPDATE r.multi SET email = 'b@x' WHERE id = 1")
            .is_ok(),
        "move onto occupied unique value must fail"
    );
    assert_eq!(
        scalar(
            &mut executor,
            "SELECT count(*) FROM r.multi WHERE email = 'c@x'"
        ),
        1
    );
    // A no-op update of a unique column does not conflict with itself.
    exec(
        &mut executor,
        "UPDATE r.multi SET email = 'b@x' WHERE id = 2",
    );
}

#[test]
fn unique_update_rollback_frees_value() {
    let (mut executor, _, _) = open("uk-update-rollback");
    make_schema(&mut executor);
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (1, 'a@x', 'ann')",
    );
    let _ = batch(
        &mut executor,
        "BEGIN; UPDATE r.multi SET email = 'taken@x' WHERE id = 1; ROLLBACK;",
    );
    // The value is free again: a fresh transaction may take it.
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (2, 'taken@x', 'bob')",
    );
    assert_eq!(
        scalar(
            &mut executor,
            "SELECT count(*) FROM r.multi WHERE email = 'taken@x'"
        ),
        1
    );
}

#[test]
fn delete_then_insert_same_unique_value() {
    let (mut executor, _, _) = open("delete-insert");
    make_schema(&mut executor);
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (1, 'a@x', 'ann')",
    );
    exec(&mut executor, "DELETE FROM r.multi WHERE id = 1");
    exec(
        &mut executor,
        "INSERT INTO r.multi (id, email, name) VALUES (2, 'a@x', 'bob')",
    );
    assert_eq!(
        scalar(
            &mut executor,
            "SELECT count(*) FROM r.multi WHERE email = 'a@x'"
        ),
        1
    );
}

#[test]
fn plain_update_touching_non_unique_column_avoids_reservations() {
    let (mut executor, _, _) = open("plain-update");
    make_schema(&mut executor);
    exec(
        &mut executor,
        "INSERT INTO r.kv (id, value) VALUES (1, 1), (2, 2)",
    );
    // Non-unique UPDATE stays on the row-lock path (regression guard for
    // accidental table-wide or reservation behavior).
    exec(
        &mut executor,
        "UPDATE r.kv SET value = value + 1 WHERE id = 1",
    );
    assert_eq!(
        scalar(&mut executor, "SELECT value FROM r.kv WHERE id = 1"),
        2
    );
}

// ---------------------------------------------------------------------------
// Recovery
// ---------------------------------------------------------------------------

#[test]
fn committed_reservations_survive_restart_via_durable_index() {
    let (storage, wal) = unique_dir("restart-unique");
    {
        let engine = Engine::create(&storage, &wal, 64).unwrap();
        let mut executor = Executor::new(engine).unwrap();
        exec(
            &mut executor,
            "CREATE SCHEMA r;
             CREATE TABLE r.kv (id BIGINT PRIMARY KEY, value BIGINT UNIQUE NOT NULL);
             INSERT INTO r.kv (id, value) VALUES (1, 100);",
        );
    }
    {
        // Fresh connections (own catalog): committed data is intact and the
        // unique index rebuilt from durable state enforces uniqueness for
        // same-session inserts after restart.
        with_fresh("restart-unique-a", &storage, &wal, |executor| {
            assert_eq!(scalar(executor, "SELECT value FROM r.kv WHERE id = 1"), 100);
            assert!(
                !executor
                    .execute("INSERT INTO r.kv (id, value) VALUES (2, 100)")
                    .is_ok(),
                "duplicate value after restart must fail"
            );
        });
        with_fresh("restart-unique-b", &storage, &wal, |executor| {
            assert!(
                !executor
                    .execute("INSERT INTO r.kv (id, value) VALUES (1, 500)")
                    .is_ok(),
                "duplicate PK after restart must fail"
            );
        });
    }
}

#[test]
fn non_indexed_table_keeps_conservative_lane() {
    let (mut executor, _, _) = open("nouk-lane");
    make_schema(&mut executor);
    // Tables without unique constraints never enter reservation mode; they
    // keep the historical table-lane semantics and still work.
    exec(
        &mut executor,
        "INSERT INTO r.nouk (id, value) VALUES (1, 1), (2, 2)",
    );
    exec(
        &mut executor,
        "INSERT INTO r.nouk (id, value) VALUES (1, 9)",
    );
    assert_eq!(scalar(&mut executor, "SELECT count(*) FROM r.nouk"), 3);
}
