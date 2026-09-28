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
//! Regression tests for the OLTP correctness/concurrency hardening pass.
//!
//! Each test encodes *required* PostgreSQL-visible semantics for one of the
//! defects found by the forensic investigation:
//!
//! * A — composite `UNIQUE(a, b)` must be a tuple constraint, not
//!   `UNIQUE(a)` + `UNIQUE(b)`.
//! * B — expression unique indexes must behave identically to plain ones
//!   (correctness; reservation eligibility is a concurrency property).
//! * C — `SERIAL` / `BIGSERIAL` column metadata must survive catalog
//!   persistence and allocate distinct values.
//! * D — `ON CONFLICT` must keep its semantics on every arbiter shape.
//! * E/F — `UPDATE ... FROM` / `DELETE ... USING` qualifier binding must never
//!   silently resolve an unknown qualifier against the target row.
//! * G — `DELETE FROM t` with no `WHERE` must delete every visible row.
//!
//! These tests drive the process-long executor with the concurrent engine
//! (the same engine the PostgreSQL wire server uses).

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_storage::StorageEngine as _;
use plomid_txn::ConcurrentPlomidStorageEngine;

type Engine = ConcurrentPlomidStorageEngine;

fn dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage = std::env::temp_dir().join(format!("plomid-oltp-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!("plomid-oltp-{tag}-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn open(tag: &str) -> (Executor<Engine>, std::path::PathBuf, std::path::PathBuf) {
    let (storage, wal) = dirs(tag);
    let engine = Engine::create(&storage, &wal, 64).unwrap_or_else(|e| panic!("open {tag}: {e}"));
    let executor = Executor::new(engine).unwrap_or_else(|e| panic!("executor {tag}: {e}"));
    (executor, storage, wal)
}

fn reopen(tag: &str, storage: &std::path::Path, wal: &std::path::Path) -> Executor<Engine> {
    let engine = Engine::open(storage, wal, 64).unwrap_or_else(|e| panic!("reopen {tag}: {e}"));
    Executor::new(engine).unwrap_or_else(|e| panic!("executor {tag}: {e}"))
}

fn exec(executor: &mut Executor<Engine>, sql: &str) {
    executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"));
}

fn rows(executor: &mut Executor<Engine>, sql: &str) -> Vec<Vec<Value>> {
    match executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
    {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("{sql}: expected rows, got {other:?}"),
    }
}

fn scalar(executor: &mut Executor<Engine>, sql: &str) -> i64 {
    let rows = rows(executor, sql);
    assert_eq!(rows.len(), 1, "{sql}: expected one row");
    assert_eq!(rows[0].len(), 1, "{sql}: expected one column");
    match &rows[0][0] {
        Value::Int2(v) => i64::from(*v),
        Value::Int4(v) => i64::from(*v),
        Value::Int8(v) => *v,
        other => panic!("{sql}: expected integer, got {other:?}"),
    }
}

fn expect_err(executor: &mut Executor<Engine>, sql: &str) -> String {
    match executor.execute(sql) {
        Err(e) => format!("{e:?}"),
        Ok(other) => panic!("{sql} should fail, got {other:?}"),
    }
}

fn cleanup(storage: &std::path::Path, wal: &std::path::Path) {
    let _ = std::fs::remove_dir_all(storage);
    let _ = std::fs::remove_dir_all(wal);
}

// ---------------------------------------------------------------------------
// A — composite UNIQUE is a tuple constraint
// ---------------------------------------------------------------------------

#[test]
fn composite_unique_is_a_tuple_constraint() {
    let (mut e, storage, wal) = open("cmp_tuple");
    exec(&mut e, "CREATE SCHEMA s;");
    exec(
        &mut e,
        "CREATE TABLE s.t (tenant_id BIGINT, email TEXT, note TEXT, UNIQUE (tenant_id, email));",
    );

    // Different tuples that share a column value are all allowed.
    exec(&mut e, "INSERT INTO s.t VALUES (1, 'a', 'x');");
    exec(&mut e, "INSERT INTO s.t VALUES (1, 'b', 'y');");
    exec(&mut e, "INSERT INTO s.t VALUES (2, 'a', 'z');");
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM s.t;"), 3);

    // The exact duplicate tuple is rejected.
    let err = expect_err(&mut e, "INSERT INTO s.t VALUES (1, 'a', 'dup');");
    assert!(
        err.to_lowercase().contains("duplicate") || err.to_lowercase().contains("conflict"),
        "expected duplicate-key error, got {err}"
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM s.t;"), 3);

    cleanup(&storage, &wal);
}

#[test]
fn composite_unique_null_never_conflicts() {
    let (mut e, storage, wal) = open("cmp_null");
    exec(&mut e, "CREATE TABLE t (a BIGINT, b TEXT, UNIQUE (a, b));");
    // NULL in any component makes the tuple non-conflicting (SQL semantics).
    exec(&mut e, "INSERT INTO t VALUES (1, NULL);");
    exec(&mut e, "INSERT INTO t VALUES (1, NULL);");
    exec(&mut e, "INSERT INTO t VALUES (NULL, 'x');");
    exec(&mut e, "INSERT INTO t VALUES (NULL, 'x');");
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 4);

    cleanup(&storage, &wal);
}

#[test]
fn composite_unique_survives_restart() {
    let (mut e, storage, wal) = open("cmp_restart");
    exec(&mut e, "CREATE TABLE t (a BIGINT, b TEXT, UNIQUE (a, b));");
    exec(&mut e, "INSERT INTO t VALUES (1, 'a');");
    exec(&mut e, "INSERT INTO t VALUES (1, 'b');");
    drop(e);

    let mut e = reopen("cmp_restart", &storage, &wal);
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 2);
    // The recreated durable index still rejects the exact duplicate tuple.
    let err = expect_err(&mut e, "INSERT INTO t VALUES (1, 'a');");
    assert!(
        err.to_lowercase().contains("duplicate") || err.to_lowercase().contains("conflict"),
        "expected duplicate-key error after restart, got {err}"
    );
    // ... while a distinct tuple is still accepted.
    exec(&mut e, "INSERT INTO t VALUES (2, 'a');");
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 3);

    cleanup(&storage, &wal);
}

#[test]
fn composite_unique_rollback_releases_tuple() {
    let (mut e, storage, wal) = open("cmp_rollback");
    exec(&mut e, "CREATE TABLE t (a BIGINT, b TEXT, UNIQUE (a, b));");
    let results = e
        .execute_all("BEGIN; INSERT INTO t VALUES (1, 'a'); ROLLBACK;")
        .unwrap();
    let _ = results;
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 0);
    exec(&mut e, "INSERT INTO t VALUES (1, 'a');");
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 1);

    cleanup(&storage, &wal);
}

#[test]
fn composite_unique_update_moves_tuple() {
    let (mut e, storage, wal) = open("cmp_update");
    exec(
        &mut e,
        "CREATE TABLE t (id BIGINT PRIMARY KEY, a BIGINT, b TEXT, UNIQUE (a, b));",
    );
    exec(&mut e, "INSERT INTO t VALUES (1, 10, 'a');");
    exec(&mut e, "INSERT INTO t VALUES (2, 10, 'b');");

    // Moving row 1 onto row 2's tuple is rejected.
    let err = expect_err(&mut e, "UPDATE t SET b = 'b' WHERE id = 1;");
    assert!(
        err.to_lowercase().contains("duplicate") || err.to_lowercase().contains("conflict"),
        "expected duplicate-key error, got {err}"
    );
    // Moving it to a free tuple succeeds.
    exec(&mut e, "UPDATE t SET b = 'c' WHERE id = 1;");
    assert_eq!(
        rows(&mut e, "SELECT b FROM t WHERE id = 1;")[0][0].to_sql_text(),
        "c"
    );

    cleanup(&storage, &wal);
}

// ---------------------------------------------------------------------------
// B — expression unique indexes
// ---------------------------------------------------------------------------

#[test]
fn expression_unique_index_enforces_normalized_values() {
    let (mut e, storage, wal) = open("expr_unique");
    exec(
        &mut e,
        "CREATE TABLE t (id BIGINT PRIMARY KEY, email TEXT);",
    );
    exec(
        &mut e,
        "CREATE UNIQUE INDEX t_lower_email ON t ((lower(email)));",
    );
    exec(&mut e, "INSERT INTO t VALUES (1, 'A@x.com');");
    // Same normalized value under a different spelling must conflict.
    let err = expect_err(&mut e, "INSERT INTO t VALUES (2, 'a@X.COM');");
    assert!(
        err.to_lowercase().contains("duplicate") || err.to_lowercase().contains("conflict"),
        "expected duplicate-key error, got {err}"
    );
    // A different normalized value is fine.
    exec(&mut e, "INSERT INTO t VALUES (3, 'b@x.com');");
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 2);

    cleanup(&storage, &wal);
}

// ---------------------------------------------------------------------------
// C — SERIAL / BIGSERIAL metadata durability and allocation
// ---------------------------------------------------------------------------

#[test]
fn serial_allocates_distinct_values() {
    let (mut e, storage, wal) = open("serial_alloc");
    exec(&mut e, "CREATE TABLE t (id SERIAL, v BIGINT);");
    exec(&mut e, "INSERT INTO t(v) VALUES (1);");
    exec(&mut e, "INSERT INTO t(v) VALUES (2);");
    let ids = rows(&mut e, "SELECT id FROM t ORDER BY id;");
    assert_eq!(ids.len(), 2, "expected two rows");
    let first = ids[0][0].to_sql_text();
    let second = ids[1][0].to_sql_text();
    assert_ne!(first, second, "SERIAL must allocate distinct values");
    assert_ne!(first, "NULL", "SERIAL must not leave the column NULL");

    cleanup(&storage, &wal);
}

#[test]
fn bigserial_primary_key_allocates_and_survives_restart() {
    let (mut e, storage, wal) = open("bigserial_pk");
    exec(
        &mut e,
        "CREATE TABLE t (id BIGSERIAL PRIMARY KEY, v BIGINT);",
    );
    exec(&mut e, "INSERT INTO t(v) VALUES (1);");
    exec(&mut e, "INSERT INTO t(v) VALUES (2);");
    let before = scalar(&mut e, "SELECT count(DISTINCT id) FROM t;");
    assert_eq!(before, 2, "BIGSERIAL must allocate distinct ids");
    drop(e);

    // The serial flag must survive catalog persistence; the third row is
    // allocated by the recovered definition.
    let mut e = reopen("bigserial_pk", &storage, &wal);
    exec(&mut e, "INSERT INTO t(v) VALUES (3);");
    assert_eq!(
        scalar(&mut e, "SELECT count(DISTINCT id) FROM t;"),
        3,
        "recovered BIGSERIAL must still allocate a fresh id"
    );

    cleanup(&storage, &wal);
}

/// Recovery must rebuild the durable uniqueness authority: a primary key
/// committed before restart still rejects a duplicate afterwards.
#[test]
fn duplicate_primary_key_after_restart_is_rejected() {
    let (mut e, storage, wal) = open("pk_restart");
    exec(&mut e, "CREATE DATABASE app;");
    exec(&mut e, "USE app;");
    exec(&mut e, "CREATE SCHEMA analytics;");
    exec(
        &mut e,
        "CREATE TABLE analytics.events (id BIGINT PRIMARY KEY, ts BIGINT, value BIGINT);",
    );
    exec(
        &mut e,
        "INSERT INTO analytics.events VALUES (1, 100, 42), (2, 200, 84);",
    );
    drop(e);

    let mut e = reopen("pk_restart", &storage, &wal);
    exec(&mut e, "USE app;");
    let duplicate = e.execute("INSERT INTO analytics.events VALUES (1, 999, 999);");
    assert!(
        duplicate.is_err(),
        "a duplicate primary key must be rejected after restart, got {duplicate:?}"
    );
    // The legal insert still works, and the committed rows are intact.
    exec(&mut e, "INSERT INTO analytics.events VALUES (3, 300, 126);");
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM analytics.events;"),
        3,
        "recovery must keep the committed rows"
    );

    cleanup(&storage, &wal);
}

// ---------------------------------------------------------------------------
// D — ON CONFLICT semantics
// ---------------------------------------------------------------------------

/// The `ON CONFLICT` arbiter must be decided by the arbiter's own unique index
/// (an O(log n) probe), for every arbiter shape the planner supports.
#[test]
fn on_conflict_arbiter_shapes_keep_semantics() {
    let (mut e, storage, wal) = open("on_conflict_shapes");
    exec(
        &mut e,
        "CREATE TABLE users (id BIGINT PRIMARY KEY, email TEXT UNIQUE, v BIGINT);",
    );
    exec(
        &mut e,
        "INSERT INTO users VALUES (1, 'a@x', 10), (2, 'b@x', 20);",
    );

    // Explicit `(email)` arbiter: a hit must not insert, a miss must insert.
    exec(
        &mut e,
        "INSERT INTO users VALUES (9, 'a@x', 99) ON CONFLICT (email) DO NOTHING;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM users;"),
        2,
        "a UNIQUE arbiter hit must not insert"
    );
    exec(
        &mut e,
        "INSERT INTO users VALUES (3, 'c@x', 30) ON CONFLICT (email) DO NOTHING;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM users;"), 3);

    // `DO UPDATE` through the UNIQUE arbiter, with `EXCLUDED` substitution.
    exec(
        &mut e,
        "INSERT INTO users VALUES (3, 'c@x', 31) ON CONFLICT (email) DO UPDATE SET v = EXCLUDED.v;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT v FROM users WHERE email = 'c@x';"),
        31
    );

    // The inferred arbiter (two unique columns) must also stay correct.
    exec(
        &mut e,
        "INSERT INTO users VALUES (4, 'b@x', 40) ON CONFLICT DO NOTHING;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM users;"),
        3,
        "the inferred arbiter must detect the UNIQUE conflict"
    );

    // Two rows in one statement that conflict with each other: the second must
    // observe the first as already staged.
    exec(
        &mut e,
        "INSERT INTO users VALUES (5, 'd@x', 50), (6, 'd@x', 60) ON CONFLICT (email) DO NOTHING;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM users WHERE email = 'd@x';"),
        1,
        "an intra-statement arbiter conflict must insert exactly one row"
    );

    // Rollback must release the aborted row so the key is free again.
    e.execute_all(
        "BEGIN; INSERT INTO users VALUES (7, 'e@x', 70) ON CONFLICT (email) DO NOTHING; ROLLBACK;",
    )
    .expect("the aborted upsert rolls back");
    exec(
        &mut e,
        "INSERT INTO users VALUES (8, 'e@x', 80) ON CONFLICT (email) DO NOTHING;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM users WHERE email = 'e@x';"),
        1
    );

    // Restart: the arbiter is still answered from the durable unique index.
    drop(e);
    let mut e = reopen("on_conflict_shapes", &storage, &wal);
    exec(
        &mut e,
        "INSERT INTO users VALUES (10, 'a@x', 1000) ON CONFLICT (email) DO NOTHING;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM users WHERE email = 'a@x';"),
        1,
        "the recovered unique index must still answer the arbiter"
    );
    exec(
        &mut e,
        "INSERT INTO users VALUES (10, 'a@x', 1000) ON CONFLICT (email) DO UPDATE SET v = 111;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT v FROM users WHERE email = 'a@x';"),
        111
    );

    cleanup(&storage, &wal);
}

#[test]
fn on_conflict_semantics_hold() {
    let (mut e, storage, wal) = open("on_conflict");
    exec(&mut e, "CREATE TABLE t (id BIGINT PRIMARY KEY, v INTEGER);");
    exec(&mut e, "INSERT INTO t VALUES (1, 10);");

    // DO NOTHING: hit and miss.
    exec(
        &mut e,
        "INSERT INTO t VALUES (1, 99) ON CONFLICT (id) DO NOTHING;",
    );
    assert_eq!(scalar(&mut e, "SELECT v FROM t WHERE id = 1;"), 10);
    exec(
        &mut e,
        "INSERT INTO t VALUES (2, 20) ON CONFLICT (id) DO NOTHING;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 2);

    // DO UPDATE with EXCLUDED: hit.
    exec(
        &mut e,
        "INSERT INTO t VALUES (2, 25) ON CONFLICT (id) DO UPDATE SET v = EXCLUDED.v;",
    );
    assert_eq!(scalar(&mut e, "SELECT v FROM t WHERE id = 2;"), 25);

    // DO UPDATE: miss inserts.
    exec(
        &mut e,
        "INSERT INTO t VALUES (3, 30) ON CONFLICT (id) DO UPDATE SET v = EXCLUDED.v;",
    );
    assert_eq!(scalar(&mut e, "SELECT v FROM t WHERE id = 3;"), 30);

    // Intra-statement conflict: first row inserted, second conflicts with it.
    exec(
        &mut e,
        "INSERT INTO t VALUES (4, 40), (4, 41) ON CONFLICT (id) DO NOTHING;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t WHERE id = 4;"), 1);

    // RETURNING on the conflict-update path.
    let out = rows(
        &mut e,
        "INSERT INTO t VALUES (3, 33) ON CONFLICT (id) DO UPDATE SET v = EXCLUDED.v RETURNING v;",
    );
    assert_eq!(out[0][0].to_sql_text(), "33");

    cleanup(&storage, &wal);
}

// ---------------------------------------------------------------------------
// E/F — UPDATE ... FROM / DELETE ... USING qualifier binding
// ---------------------------------------------------------------------------

#[test]
fn update_from_schema_qualified_source_binds_to_source() {
    let (mut e, storage, wal) = open("upd_from_bind");
    exec(&mut e, "CREATE SCHEMA s;");
    exec(&mut e, "CREATE TABLE s.dst (id INTEGER, v INTEGER);");
    exec(&mut e, "CREATE TABLE s.src (id INTEGER, v INTEGER);");
    exec(&mut e, "INSERT INTO s.dst VALUES (1, 0), (2, 0), (3, 0);");
    exec(&mut e, "INSERT INTO s.src VALUES (2, 22);");

    exec(
        &mut e,
        "UPDATE s.dst SET v = s.src.v FROM s.src WHERE s.dst.id = s.src.id;",
    );

    // Exactly the joined row changed; the previous binder defect rewrote all
    // three rows with the (tautological) fallback.
    assert_eq!(
        rows(&mut e, "SELECT v FROM s.dst WHERE id = 1;")[0][0].to_sql_text(),
        "0"
    );
    assert_eq!(
        rows(&mut e, "SELECT v FROM s.dst WHERE id = 2;")[0][0].to_sql_text(),
        "22"
    );
    assert_eq!(
        rows(&mut e, "SELECT v FROM s.dst WHERE id = 3;")[0][0].to_sql_text(),
        "0"
    );

    cleanup(&storage, &wal);
}

#[test]
fn update_from_with_aliases_binds_to_source() {
    let (mut e, storage, wal) = open("upd_from_alias");
    exec(&mut e, "CREATE SCHEMA s;");
    exec(&mut e, "CREATE TABLE s.dst (id INTEGER, v INTEGER);");
    exec(&mut e, "CREATE TABLE s.src (id INTEGER, v INTEGER);");
    exec(&mut e, "INSERT INTO s.dst VALUES (1, 0), (2, 0);");
    exec(&mut e, "INSERT INTO s.src VALUES (2, 22);");

    exec(
        &mut e,
        "UPDATE s.dst AS d SET v = src.v FROM s.src AS src WHERE d.id = src.id;",
    );
    assert_eq!(
        rows(&mut e, "SELECT v FROM s.dst WHERE id = 1;")[0][0].to_sql_text(),
        "0"
    );
    assert_eq!(
        rows(&mut e, "SELECT v FROM s.dst WHERE id = 2;")[0][0].to_sql_text(),
        "22"
    );

    cleanup(&storage, &wal);
}

#[test]
fn update_from_unknown_qualifier_is_an_error() {
    let (mut e, storage, wal) = open("upd_from_err");
    exec(&mut e, "CREATE TABLE dst (id INTEGER, v INTEGER);");
    exec(&mut e, "CREATE TABLE src (id INTEGER, v INTEGER);");
    exec(&mut e, "INSERT INTO dst VALUES (1, 0);");
    exec(&mut e, "INSERT INTO src VALUES (1, 5);");

    // `nosuch.v` names neither relation: it must be an error, never a silent
    // fallback to the target row (which made the predicate a tautology).
    let err = expect_err(
        &mut e,
        "UPDATE dst SET v = nosuch.v FROM src WHERE dst.id = src.id;",
    );
    assert!(
        err.to_lowercase().contains("nosuch") || err.to_lowercase().contains("column"),
        "expected an unknown-qualifier error, got {err}"
    );
    // The target row must be untouched.
    assert_eq!(
        rows(&mut e, "SELECT v FROM dst WHERE id = 1;")[0][0].to_sql_text(),
        "0"
    );

    cleanup(&storage, &wal);
}

#[test]
fn delete_using_schema_qualified_source_deletes_only_joined_rows() {
    let (mut e, storage, wal) = open("del_using_bind");
    exec(&mut e, "CREATE SCHEMA s;");
    exec(&mut e, "CREATE TABLE s.dst (id INTEGER, v INTEGER);");
    exec(&mut e, "CREATE TABLE s.src (id INTEGER, v INTEGER);");
    exec(&mut e, "INSERT INTO s.dst VALUES (1, 0), (2, 0), (3, 0);");
    exec(&mut e, "INSERT INTO s.src VALUES (2, 0);");

    exec(
        &mut e,
        "DELETE FROM s.dst USING s.src WHERE s.dst.id = s.src.id;",
    );
    // The previous binder defect deleted every row.
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM s.dst;"), 2);
    assert_eq!(
        rows(&mut e, "SELECT id FROM s.dst ORDER BY id;")
            .iter()
            .map(|r| r[0].to_sql_text())
            .collect::<Vec<_>>(),
        vec!["1", "3"]
    );

    cleanup(&storage, &wal);
}

#[test]
fn delete_using_alias_and_zero_and_multi_match() {
    let (mut e, storage, wal) = open("del_using_alias");
    exec(&mut e, "CREATE TABLE dst (id INTEGER, v INTEGER);");
    exec(&mut e, "CREATE TABLE src (id INTEGER, v INTEGER);");
    exec(&mut e, "INSERT INTO dst VALUES (1, 0), (2, 0), (3, 0);");
    // Two source rows match target 2: it must still be deleted exactly once.
    exec(&mut e, "INSERT INTO src VALUES (2, 0), (2, 0);");

    exec(
        &mut e,
        "DELETE FROM dst AS d USING src AS s WHERE d.id = s.id;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM dst;"), 2);

    cleanup(&storage, &wal);
}

#[test]
fn delete_using_unknown_qualifier_is_an_error() {
    let (mut e, storage, wal) = open("del_using_err");
    exec(&mut e, "CREATE TABLE dst (id INTEGER, v INTEGER);");
    exec(&mut e, "CREATE TABLE src (id INTEGER, v INTEGER);");
    exec(&mut e, "INSERT INTO dst VALUES (1, 0);");
    exec(&mut e, "INSERT INTO src VALUES (1, 0);");

    let err = expect_err(
        &mut e,
        "DELETE FROM dst USING src WHERE nosuch.id = src.id;",
    );
    assert!(
        err.to_lowercase().contains("nosuch") || err.to_lowercase().contains("column"),
        "expected an unknown-qualifier error, got {err}"
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM dst;"), 1);

    cleanup(&storage, &wal);
}

// ---------------------------------------------------------------------------
// G — DELETE without WHERE
// ---------------------------------------------------------------------------

#[test]
fn delete_without_where_deletes_every_row() {
    let (mut e, storage, wal) = open("del_no_where");
    exec(
        &mut e,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);",
    );
    exec(
        &mut e,
        "INSERT INTO t VALUES (1,1),(2,2),(3,3),(4,4),(5,5),(6,6),(7,7),(8,8),(9,9),(10,10);",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 10);

    exec(&mut e, "DELETE FROM t;");
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 0);
    cleanup(&storage, &wal);
}

#[test]
fn delete_without_where_on_empty_table_is_a_noop() {
    let (mut e, storage, wal) = open("del_no_where_empty");
    exec(&mut e, "CREATE TABLE t (id INTEGER);");
    exec(&mut e, "DELETE FROM t;");
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 0);
    cleanup(&storage, &wal);
}

#[test]
fn delete_without_where_matches_where_true() {
    let (mut e, storage, wal) = open("del_no_where_true");
    exec(&mut e, "CREATE TABLE t (id INTEGER);");
    exec(&mut e, "INSERT INTO t VALUES (1),(2),(3);");
    exec(&mut e, "DELETE FROM t WHERE TRUE;");
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 0);
    cleanup(&storage, &wal);
}

#[test]
fn delete_without_where_rolls_back() {
    let (mut e, storage, wal) = open("del_no_where_rb");
    exec(&mut e, "CREATE TABLE t (id INTEGER);");
    exec(&mut e, "INSERT INTO t VALUES (1),(2),(3);");
    e.execute_all("BEGIN; DELETE FROM t; ROLLBACK;").unwrap();
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 3);
    cleanup(&storage, &wal);
}

#[test]
fn delete_without_where_survives_restart() {
    let (mut e, storage, wal) = open("del_no_where_restart");
    exec(&mut e, "CREATE TABLE t (id INTEGER);");
    exec(&mut e, "INSERT INTO t VALUES (1),(2),(3);");
    exec(&mut e, "DELETE FROM t;");
    drop(e);
    let mut e = reopen("del_no_where_restart", &storage, &wal);
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM t;"), 0);
    cleanup(&storage, &wal);
}
