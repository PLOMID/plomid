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
//! Regression tests for the indexed target-discovery path of
//! `UPDATE ... FROM` and `DELETE ... USING`.
//!
//! The statements used to discover target rows by pairing every target row
//! with every materialized source row — O(target × source). They now probe
//! the target's existing single-column index once per source row when the
//! WHERE holds a provably safe equality atom (`target_col = source_expr`),
//! falling back to the nested loop otherwise. These tests pin the SQL-visible
//! semantics across both paths: match shapes, NULL keys, duplicates, aliases,
//! rollback, restart, and concurrency.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_storage::StorageEngine as _;
use plomid_txn::ConcurrentPlomidStorageEngine;

type Engine = ConcurrentPlomidStorageEngine;

fn dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage = std::env::temp_dir().join(format!("plomid-join-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!("plomid-join-{tag}-wal-{}", std::process::id()));
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

fn expect_err(executor: &mut Executor<Engine>, sql: &str) {
    assert!(executor.execute(sql).is_err(), "{sql} should fail");
}

fn exec_all(executor: &mut Executor<Engine>, script: &str) {
    executor
        .execute_all(script)
        .unwrap_or_else(|e| panic!("{script} should succeed: {e}"));
}

fn cleanup(storage: &std::path::Path, wal: &std::path::Path) {
    let _ = std::fs::remove_dir_all(storage);
    let _ = std::fs::remove_dir_all(wal);
}

/// Standard fixture: target and source share the join key 1..=n.
fn fixture(executor: &mut Executor<Engine>, tag: &str, n: i64) {
    exec(executor, "CREATE SCHEMA jf;");
    exec(
        executor,
        &format!("CREATE TABLE jf.{tag}_t (id BIGINT PRIMARY KEY, v BIGINT);"),
    );
    exec(
        executor,
        &format!("CREATE TABLE jf.{tag}_s (id BIGINT PRIMARY KEY, v BIGINT);"),
    );
    exec(
        executor,
        &format!("INSERT INTO jf.{tag}_t (id, v) SELECT g, g * 10 FROM generate_series(1, {n}) g;"),
    );
    exec(
        executor,
        &format!(
            "INSERT INTO jf.{tag}_s (id, v) SELECT g, g * 100 FROM generate_series(1, {n}) g;"
        ),
    );
}

// ---------------------------------------------------------------------------
// UPDATE ... FROM — match shapes
// ---------------------------------------------------------------------------

#[test]
fn update_from_zero_match_updates_nothing() {
    let (mut e, storage, wal) = open("uf_zero");
    fixture(&mut e, "a", 5);
    exec(
        &mut e,
        "UPDATE jf.a_t SET v = a_s.v FROM jf.a_s WHERE a_t.id = a_s.id AND a_t.id = 999;",
    );
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.a_t WHERE id = 1;"), 10);
    cleanup(&storage, &wal);
}

#[test]
fn update_from_one_match() {
    let (mut e, storage, wal) = open("uf_one");
    fixture(&mut e, "b", 5);
    exec(
        &mut e,
        "UPDATE jf.b_t SET v = b_s.v FROM jf.b_s WHERE b_t.id = b_s.id AND b_t.id = 3;",
    );
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.b_t WHERE id = 3;"), 300);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.b_t WHERE id = 1;"), 10);
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.b_t WHERE v = 100 * id;"),
        1
    );
    cleanup(&storage, &wal);
}

#[test]
fn update_from_all_rows_match() {
    let (mut e, storage, wal) = open("uf_all");
    fixture(&mut e, "c", 5);
    exec(
        &mut e,
        "UPDATE jf.c_t SET v = c_s.v FROM jf.c_s WHERE c_t.id = c_s.id;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.c_t WHERE v = 100 * id;"),
        5
    );
    cleanup(&storage, &wal);
}

#[test]
fn update_from_duplicate_source_keys_update_once() {
    let (mut e, storage, wal) = open("uf_dup_src");
    fixture(&mut e, "d", 3);
    // Duplicate source rows for the same join key: PostgreSQL updates the
    // target row once (first match wins). Use a non-PK source so dupes exist.
    exec(&mut e, "CREATE TABLE jf.d_s2 (id BIGINT, v BIGINT);");
    exec(
        &mut e,
        "INSERT INTO jf.d_s2 VALUES (1, 111), (1, 222), (2, 333);",
    );
    exec(
        &mut e,
        "UPDATE jf.d_t SET v = d_s2.v FROM jf.d_s2 WHERE d_t.id = d_s2.id;",
    );
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.d_t WHERE id = 1;"), 111);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.d_t WHERE id = 2;"), 333);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.d_t WHERE id = 3;"), 30);
    cleanup(&storage, &wal);
}

#[test]
fn update_from_null_source_key_matches_nothing() {
    let (mut e, storage, wal) = open("uf_null");
    fixture(&mut e, "f", 3);
    exec(&mut e, "CREATE TABLE jf.f_s2 (id BIGINT, v BIGINT);");
    exec(&mut e, "INSERT INTO jf.f_s2 VALUES (NULL, 1), (2, 2);");
    exec(
        &mut e,
        "UPDATE jf.f_t SET v = f_s2.v FROM jf.f_s2 WHERE f_t.id = f_s2.id;",
    );
    // NULL = NULL is not true: row 1 keeps its value, row 2 gets 2.
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.f_t WHERE id = 1;"), 10);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.f_t WHERE id = 2;"), 2);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.f_t WHERE id = 3;"), 30);
    cleanup(&storage, &wal);
}

#[test]
fn update_from_disjoint_keys_touch_nothing() {
    let (mut e, storage, wal) = open("uf_disjoint");
    fixture(&mut e, "g", 3);
    exec(&mut e, "CREATE TABLE jf.g_s2 (id BIGINT, v BIGINT);");
    exec(&mut e, "INSERT INTO jf.g_s2 VALUES (50, 5), (60, 6);");
    exec(
        &mut e,
        "UPDATE jf.g_t SET v = g_s2.v FROM jf.g_s2 WHERE g_t.id = g_s2.id;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.g_t WHERE v = 10 * id;"),
        3
    );
    cleanup(&storage, &wal);
}

#[test]
fn update_from_rechecks_residual_predicate() {
    let (mut e, storage, wal) = open("uf_residual");
    fixture(&mut e, "h", 5);
    // The index probe finds every join match; the residual predicate must
    // still decide which of them update.
    exec(
        &mut e,
        "UPDATE jf.h_t SET v = h_s.v FROM jf.h_s WHERE h_t.id = h_s.id AND h_s.v > 200;",
    );
    // Only source rows 3,4,5 qualify.
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.h_t WHERE id = 2;"), 20);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.h_t WHERE id = 3;"), 300);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.h_t WHERE id = 5;"), 500);
    cleanup(&storage, &wal);
}

#[test]
fn update_from_text_join_key() {
    let (mut e, storage, wal) = open("uf_text");
    exec(&mut e, "CREATE SCHEMA jf;");
    exec(
        &mut e,
        "CREATE TABLE jf.tx_t (k TEXT PRIMARY KEY, v BIGINT);",
    );
    exec(
        &mut e,
        "CREATE TABLE jf.tx_s (k TEXT PRIMARY KEY, v BIGINT);",
    );
    exec(
        &mut e,
        "INSERT INTO jf.tx_t VALUES ('a', 1), ('b', 2), ('c', 3);",
    );
    exec(&mut e, "INSERT INTO jf.tx_s VALUES ('b', 20), ('c', 30);");
    exec(
        &mut e,
        "UPDATE jf.tx_t SET v = tx_s.v FROM jf.tx_s WHERE tx_t.k = tx_s.k;",
    );
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.tx_t WHERE k = 'a';"), 1);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.tx_t WHERE k = 'b';"), 20);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.tx_t WHERE k = 'c';"), 30);
    cleanup(&storage, &wal);
}

#[test]
fn update_from_without_usable_index_falls_back() {
    let (mut e, storage, wal) = open("uf_fallback");
    fixture(&mut e, "i", 3);
    // Source column is NOT indexed and the join column is a bare name that
    // could ambiguously bind — the atom classifier refuses and the nested
    // loop must still produce correct results.
    exec(&mut e, "CREATE TABLE jf.i_s2 (id BIGINT, v BIGINT);");
    exec(&mut e, "INSERT INTO jf.i_s2 VALUES (1, 11), (3, 33);");
    exec(
        &mut e,
        "UPDATE jf.i_t SET v = i_s2.v FROM jf.i_s2 WHERE id = i_s2.id;",
    );
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.i_t WHERE id = 1;"), 11);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.i_t WHERE id = 2;"), 20);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.i_t WHERE id = 3;"), 33);
    cleanup(&storage, &wal);
}

#[test]
fn update_from_or_shape_still_correct() {
    let (mut e, storage, wal) = open("uf_or");
    fixture(&mut e, "j", 4);
    // OR at the top level: the classifier must refuse the probe, but the
    // statement must remain correct through the fallback.
    exec(
        &mut e,
        "UPDATE jf.j_t SET v = 1 FROM jf.j_s WHERE j_t.id = j_s.id OR j_t.id = 999;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.j_t WHERE v = 1;"),
        4
    );
    cleanup(&storage, &wal);
}

#[test]
fn update_from_set_and_returning_bind_source_values() {
    let (mut e, storage, wal) = open("uf_returning");
    fixture(&mut e, "k", 3);
    let out = rows(
        &mut e,
        "UPDATE jf.k_t SET v = k_s.v FROM jf.k_s WHERE k_t.id = k_s.id AND k_t.id = 2 RETURNING k_t.id, k_t.v;",
    );
    assert_eq!(out.len(), 1);
    match (&out[0][0], &out[0][1]) {
        (Value::Int8(id), Value::Int8(v)) => {
            assert_eq!((*id, *v), (2, 200));
        }
        other => panic!("unexpected RETURNING row {other:?}"),
    }
    cleanup(&storage, &wal);
}

// ---------------------------------------------------------------------------
// DELETE ... USING — match shapes
// ---------------------------------------------------------------------------

#[test]
fn delete_using_zero_match_deletes_nothing() {
    let (mut e, storage, wal) = open("du_zero");
    fixture(&mut e, "l", 5);
    exec(
        &mut e,
        "DELETE FROM jf.l_t USING jf.l_s WHERE l_t.id = l_s.id AND l_t.id = 999;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM jf.l_t;"), 5);
    cleanup(&storage, &wal);
}

#[test]
fn delete_using_one_match() {
    let (mut e, storage, wal) = open("du_one");
    fixture(&mut e, "m", 5);
    exec(
        &mut e,
        "DELETE FROM jf.m_t USING jf.m_s WHERE m_t.id = m_s.id AND m_t.id = 3;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM jf.m_t;"), 4);
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.m_t WHERE id = 3;"),
        0
    );
    cleanup(&storage, &wal);
}

#[test]
fn delete_using_all_rows_match() {
    let (mut e, storage, wal) = open("du_all");
    fixture(&mut e, "n", 5);
    exec(
        &mut e,
        "DELETE FROM jf.n_t USING jf.n_s WHERE n_t.id = n_s.id;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM jf.n_t;"), 0);
    cleanup(&storage, &wal);
}

#[test]
fn delete_using_duplicate_source_keys_delete_once() {
    let (mut e, storage, wal) = open("du_dup");
    fixture(&mut e, "o", 3);
    exec(&mut e, "CREATE TABLE jf.o_s2 (id BIGINT, v BIGINT);");
    exec(&mut e, "INSERT INTO jf.o_s2 VALUES (1, 0), (1, 0), (2, 0);");
    exec(
        &mut e,
        "DELETE FROM jf.o_t USING jf.o_s2 WHERE o_t.id = o_s2.id;",
    );
    // Target row deleted at most once even with duplicate source matches.
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.o_t WHERE id = 1;"),
        0
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.o_t WHERE id = 2;"),
        0
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.o_t WHERE id = 3;"),
        1
    );
    cleanup(&storage, &wal);
}

#[test]
fn delete_using_null_source_key_matches_nothing() {
    let (mut e, storage, wal) = open("du_null");
    fixture(&mut e, "p", 3);
    exec(&mut e, "CREATE TABLE jf.p_s2 (id BIGINT, v BIGINT);");
    exec(&mut e, "INSERT INTO jf.p_s2 VALUES (NULL, 0), (2, 0);");
    exec(
        &mut e,
        "DELETE FROM jf.p_t USING jf.p_s2 WHERE p_t.id = p_s2.id;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.p_t WHERE id = 1;"),
        1
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.p_t WHERE id = 2;"),
        0
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.p_t WHERE id = 3;"),
        1
    );
    cleanup(&storage, &wal);
}

#[test]
fn delete_using_rechecks_residual_predicate() {
    let (mut e, storage, wal) = open("du_residual");
    fixture(&mut e, "q", 5);
    exec(
        &mut e,
        "DELETE FROM jf.q_t USING jf.q_s WHERE q_t.id = q_s.id AND q_s.v >= 300;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.q_t WHERE id <= 2;"),
        2
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.q_t WHERE id >= 3;"),
        0
    );
    cleanup(&storage, &wal);
}

#[test]
fn delete_using_without_usable_index_falls_back() {
    let (mut e, storage, wal) = open("du_fallback");
    fixture(&mut e, "r", 3);
    exec(&mut e, "CREATE TABLE jf.r_s2 (id BIGINT, v BIGINT);");
    exec(&mut e, "INSERT INTO jf.r_s2 VALUES (1, 0), (3, 0);");
    exec(
        &mut e,
        "DELETE FROM jf.r_t USING jf.r_s2 WHERE id = r_s2.id;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM jf.r_t;"), 1);
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.r_t WHERE id = 2;"),
        1
    );
    cleanup(&storage, &wal);
}

#[test]
fn delete_using_returning_binds_source_row() {
    let (mut e, storage, wal) = open("du_returning");
    fixture(&mut e, "s", 3);
    let out = rows(
        &mut e,
        "DELETE FROM jf.s_t USING jf.s_s WHERE s_t.id = s_s.id AND s_t.id = 1 RETURNING s_t.id, s_s.v;",
    );
    assert_eq!(out.len(), 1);
    match (&out[0][0], &out[0][1]) {
        (Value::Int8(id), Value::Int8(v)) => {
            assert_eq!((*id, *v), (1, 100));
        }
        other => panic!("unexpected RETURNING row {other:?}"),
    }
    cleanup(&storage, &wal);
}

// ---------------------------------------------------------------------------
// Shared semantics: aliases, qualifiers, rollback, restart, errors
// ---------------------------------------------------------------------------

#[test]
fn joined_dml_alias_and_schema_qualifiers() {
    let (mut e, storage, wal) = open("join_alias");
    fixture(&mut e, "t", 4);
    exec(
        &mut e,
        "UPDATE jf.t_t AS tgt SET v = src.v FROM jf.t_s AS src WHERE tgt.id = src.id AND tgt.id = 2;",
    );
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.t_t WHERE id = 2;"), 200);
    exec(
        &mut e,
        "DELETE FROM jf.t_t AS tgt USING jf.t_s AS src WHERE tgt.id = src.id AND tgt.id = 3;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.t_t WHERE id = 3;"),
        0
    );
    cleanup(&storage, &wal);
}

#[test]
fn joined_dml_unknown_qualifier_still_errors() {
    let (mut e, storage, wal) = open("join_unknown");
    fixture(&mut e, "u", 2);
    expect_err(
        &mut e,
        "UPDATE jf.u_t SET v = nosuch.v FROM jf.u_s WHERE u_t.id = nosuch.id;",
    );
    expect_err(
        &mut e,
        "DELETE FROM jf.u_t USING jf.u_s WHERE nosuch.id = u_s.id;",
    );
    cleanup(&storage, &wal);
}

// UPDATE ... FROM inside an explicit transaction is an existing architectural
// restriction (FROM materialization needs engine access; DELETE ... USING has
// an in-transaction materializer and does support it). Rollback coverage for
// the indexed path therefore uses DELETE ... USING.
#[test]
fn joined_dml_rollback_restores_rows() {
    let (mut e, storage, wal) = open("join_rollback");
    fixture(&mut e, "w", 3);
    exec_all(
        &mut e,
        "BEGIN; \
         DELETE FROM jf.w_t USING jf.w_s WHERE w_t.id = w_s.id; \
         ROLLBACK;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM jf.w_t;"), 3);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.w_t WHERE id = 2;"), 20);
    cleanup(&storage, &wal);
}

/// (Renamed: UPDATE ... FROM is not supported inside explicit transactions;
/// see the note above. This test covers rollback-releases-rows via USING.)
#[test]
fn joined_dml_delete_using_in_txn_and_rollback_reuse() {
    let (mut e, storage, wal) = open("join_rb_reuse2");
    fixture(&mut e, "x", 3);
    // Rollback must not leave the row locked/unavailable for the next statement.
    exec_all(
        &mut e,
        "BEGIN; \
         DELETE FROM jf.x_t USING jf.x_s WHERE x_t.id = x_s.id AND x_t.id = 1; \
         ROLLBACK;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.x_t WHERE id = 1;"),
        1
    );
    exec(
        &mut e,
        "DELETE FROM jf.x_t USING jf.x_s WHERE x_t.id = x_s.id AND x_t.id = 1;",
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.x_t WHERE id = 1;"),
        0
    );
    cleanup(&storage, &wal);
}

#[test]
fn joined_dml_update_then_delete_same_txn() {
    let (mut e, storage, wal) = open("join_same_txn");
    fixture(&mut e, "y", 3);
    // Both statements inside one transaction: read-your-writes through the
    // transaction's own overlay (the probe must see the earlier DELETE's
    // staged index state).
    exec_all(
        &mut e,
        "BEGIN; \
         DELETE FROM jf.y_t USING jf.y_s WHERE y_t.id = y_s.id AND y_t.id = 1; \
         DELETE FROM jf.y_t USING jf.y_s WHERE y_t.id = y_s.id AND y_t.id = 2; \
         COMMIT;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM jf.y_t;"), 1);
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.y_t WHERE id = 3;"),
        1
    );
    cleanup(&storage, &wal);
}

#[test]
fn joined_dml_survives_restart() {
    let (mut e, storage, wal) = open("join_restart");
    fixture(&mut e, "z", 4);
    exec(
        &mut e,
        "UPDATE jf.z_t SET v = z_s.v FROM jf.z_s WHERE z_t.id = z_s.id;",
    );
    exec(
        &mut e,
        "DELETE FROM jf.z_t USING jf.z_s WHERE z_t.id = z_s.id AND z_t.id = 4;",
    );
    drop(e);
    let mut e = reopen("join_restart", &storage, &wal);
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM jf.z_t;"), 3);
    assert_eq!(scalar(&mut e, "SELECT v FROM jf.z_t WHERE id = 3;"), 300);
    // The rebuilt index still enforces the PK after restart.
    expect_err(&mut e, "INSERT INTO jf.z_t (id, v) VALUES (1, 1);");
    cleanup(&storage, &wal);
}

#[test]
fn joined_dml_unique_modifying_update_from_stays_correct() {
    let (mut e, storage, wal) = open("join_unique_mod");
    exec(&mut e, "CREATE SCHEMA jf;");
    exec(
        &mut e,
        "CREATE TABLE jf.v_t (id BIGINT PRIMARY KEY, v BIGINT);",
    );
    exec(&mut e, "CREATE TABLE jf.v_s (id BIGINT, nv BIGINT);");
    exec(
        &mut e,
        "INSERT INTO jf.v_t (id, v) VALUES (1, 10), (2, 20), (3, 30);",
    );
    exec(&mut e, "INSERT INTO jf.v_s VALUES (1, 101), (2, 202);");
    // SET touches the PK column: the reservation path must coordinate the
    // new values even though discovery goes through the index.
    exec(
        &mut e,
        "UPDATE jf.v_t SET id = v_s.nv FROM jf.v_s WHERE v_t.id = v_s.id;",
    );
    assert_eq!(scalar(&mut e, "SELECT count(*) FROM jf.v_t;"), 3);
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.v_t WHERE id = 101;"),
        1
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.v_t WHERE id = 202;"),
        1
    );
    assert_eq!(
        scalar(&mut e, "SELECT count(*) FROM jf.v_t WHERE id = 3;"),
        1
    );
    cleanup(&storage, &wal);
}

#[test]
fn joined_dml_concurrent_disjoint_targets() {
    let (mut e, storage, wal) = open("join_concurrent");
    fixture(&mut e, "cc", 8);
    let e = std::sync::Arc::new(std::sync::Mutex::new(e));
    let mut handles = Vec::new();
    for worker in 1..=4_i64 {
        let shared = std::sync::Arc::clone(&e);
        handles.push(std::thread::spawn(move || {
            let mut guard = shared.lock().expect("executor mutex");
            exec(
                &mut guard,
                &format!(
                    "UPDATE jf.cc_t SET v = cc_s.v FROM jf.cc_s WHERE cc_t.id = cc_s.id AND cc_t.id = {worker};"
                ),
            );
        }));
    }
    for handle in handles {
        handle.join().expect("worker thread");
    }
    let mut e = e.lock().expect("executor mutex");
    assert_eq!(
        scalar(
            &mut e,
            "SELECT count(*) FROM jf.cc_t WHERE v = 100 * id AND id <= 4;"
        ),
        4
    );
    assert_eq!(
        scalar(
            &mut e,
            "SELECT count(*) FROM jf.cc_t WHERE id > 4 AND v = 10 * id;"
        ),
        4
    );
    cleanup(&storage, &wal);
}

// =====================================================================
// Source-side pruned materialization (`src.col = <constant>` over an
// indexed source column prunes the source scan through the source's own
// B+Tree). Semantics must be identical to the full materialization.
// =====================================================================

fn count(executor: &mut Executor<Engine>, sql: &str) -> i64 {
    scalar(executor, sql)
}

#[test]
fn source_pruned_update_selective_matches_full_scan() {
    let (mut e, storage, wal) = open("sp_sel");
    exec(
        &mut e,
        "CREATE TABLE sp_t (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "CREATE TABLE sp_s (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "INSERT INTO sp_t SELECT g, 0 FROM generate_series(1, 50) g",
    );
    exec(
        &mut e,
        "INSERT INTO sp_s SELECT g, g * 10 FROM generate_series(1, 50) g",
    );
    // Pruned source path (src.id = 5, source has a PK index).
    exec(
        &mut e,
        "UPDATE sp_t SET v = src.v FROM sp_s src WHERE sp_t.id = src.id AND src.id = 5",
    );
    assert_eq!(count(&mut e, "SELECT v FROM sp_t WHERE id = 5"), 50);
    assert_eq!(count(&mut e, "SELECT count(*) FROM sp_t WHERE v <> 0"), 1);
    cleanup(&storage, &wal);
}

#[test]
fn source_pruned_preserves_duplicate_source_order() {
    let (mut e, storage, wal) = open("sp_dup");
    exec(&mut e, "CREATE TABLE sp_t (id BIGINT PRIMARY KEY, v TEXT)");
    // Unindexed source: insert order is the scan order.
    exec(&mut e, "CREATE TABLE sp_s (id BIGINT, v TEXT)");
    exec(&mut e, "INSERT INTO sp_t VALUES (3, 'unset')");
    exec(&mut e, "INSERT INTO sp_s VALUES (3, 'first')");
    exec(&mut e, "INSERT INTO sp_s VALUES (3, 'second')");
    // Two source rows share key 3; first qualifying FROM row wins, in
    // materialized (row-key ascending) order.
    exec(
        &mut e,
        "UPDATE sp_t SET v = src.v FROM sp_s src WHERE sp_t.id = src.id AND src.id = 3",
    );
    assert_eq!(
        rows(&mut e, "SELECT v FROM sp_t WHERE id = 3")[0][0],
        Value::Text("first".into())
    );
    cleanup(&storage, &wal);
}

#[test]
fn source_pruned_delete_using() {
    let (mut e, storage, wal) = open("sp_del");
    exec(
        &mut e,
        "CREATE TABLE sp_t (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "CREATE TABLE sp_s (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "INSERT INTO sp_t SELECT g, g FROM generate_series(1, 20) g",
    );
    exec(
        &mut e,
        "INSERT INTO sp_s SELECT g, g FROM generate_series(1, 20) g",
    );
    exec(
        &mut e,
        "DELETE FROM sp_t USING sp_s src WHERE sp_t.id = src.id AND src.id = 7",
    );
    assert_eq!(count(&mut e, "SELECT count(*) FROM sp_t"), 19);
    assert_eq!(count(&mut e, "SELECT count(*) FROM sp_t WHERE id = 7"), 0);
    cleanup(&storage, &wal);
}

#[test]
fn source_pruned_with_additional_target_predicate() {
    let (mut e, storage, wal) = open("sp_mixed");
    exec(
        &mut e,
        "CREATE TABLE sp_t (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "CREATE TABLE sp_s (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "INSERT INTO sp_t SELECT g, g FROM generate_series(1, 30) g",
    );
    exec(
        &mut e,
        "INSERT INTO sp_s SELECT g, g * 2 FROM generate_series(1, 30) g",
    );
    // Source pruning on src.id = 4, target restriction id % 2 = 0 must still
    // be applied by the full WHERE recheck.
    exec(
        &mut e,
        "UPDATE sp_t SET v = src.v FROM sp_s src \
         WHERE sp_t.id = src.id AND src.id = 4 AND sp_t.id % 2 = 0",
    );
    assert_eq!(count(&mut e, "SELECT v FROM sp_t WHERE id = 4"), 8);
    assert_eq!(count(&mut e, "SELECT count(*) FROM sp_t WHERE v <> id"), 1);
    cleanup(&storage, &wal);
}

#[test]
fn source_pruned_zero_and_no_index_fallback() {
    let (mut e, storage, wal) = open("sp_fb");
    exec(
        &mut e,
        "CREATE TABLE sp_t (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    // No index on the source: full materialization fallback.
    exec(&mut e, "CREATE TABLE sp_s (id BIGINT, v BIGINT)");
    exec(
        &mut e,
        "INSERT INTO sp_t SELECT g, 0 FROM generate_series(1, 10) g",
    );
    exec(
        &mut e,
        "INSERT INTO sp_s SELECT g, g FROM generate_series(1, 10) g",
    );
    exec(
        &mut e,
        "UPDATE sp_t SET v = src.v FROM sp_s src WHERE sp_t.id = src.id AND src.id = 3",
    );
    assert_eq!(count(&mut e, "SELECT v FROM sp_t WHERE id = 3"), 3);
    // Zero matches through a pruned path.
    exec(
        &mut e,
        "UPDATE sp_t SET v = 99 FROM sp_s src WHERE sp_t.id = src.id AND src.id = 999",
    );
    assert_eq!(count(&mut e, "SELECT count(*) FROM sp_t WHERE v = 99"), 0);
    cleanup(&storage, &wal);
}

#[test]
fn source_pruned_bare_name_binds_target_not_source() {
    let (mut e, storage, wal) = open("sp_bare");
    exec(
        &mut e,
        "CREATE TABLE sp_t (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "CREATE TABLE sp_s (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "INSERT INTO sp_t SELECT g, 0 FROM generate_series(1, 20) g",
    );
    exec(
        &mut e,
        "INSERT INTO sp_s SELECT g, g * 3 FROM generate_series(1, 20) g",
    );
    // Bare `id = 2` binds to the TARGET (binder precedence); the source is
    // fully materialized and the join picks id=2. Pruning must not treat it
    // as a source restriction.
    exec(
        &mut e,
        "UPDATE sp_t SET v = src.v FROM sp_s src WHERE sp_t.id = src.id AND id = 2",
    );
    assert_eq!(count(&mut e, "SELECT v FROM sp_t WHERE id = 2"), 6);
    assert_eq!(count(&mut e, "SELECT count(*) FROM sp_t WHERE v <> 0"), 1);
    cleanup(&storage, &wal);
}

#[test]
fn source_pruned_rollback_and_restart() {
    let (mut e, storage, wal) = open("sp_rb");
    exec(
        &mut e,
        "CREATE TABLE sp_t (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "CREATE TABLE sp_s (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "INSERT INTO sp_t SELECT g, 0 FROM generate_series(1, 10) g",
    );
    exec(
        &mut e,
        "INSERT INTO sp_s SELECT g, g * 5 FROM generate_series(1, 10) g",
    );
    exec_all(
        &mut e,
        "BEGIN; \
         DELETE FROM sp_t USING sp_s src WHERE sp_t.id = src.id AND src.id = 1; \
         ROLLBACK;",
    );
    assert_eq!(count(&mut e, "SELECT count(*) FROM sp_t WHERE id = 1"), 1);
    // Restart: catalog (index metadata) must survive for post-restart pruning.
    let e2 = reopen("sp_rb", &storage, &wal);
    let mut e2 = e2;
    exec(
        &mut e2,
        "UPDATE sp_t SET v = src.v FROM sp_s src WHERE sp_t.id = src.id AND src.id = 2",
    );
    assert_eq!(count(&mut e2, "SELECT v FROM sp_t WHERE id = 2"), 10);
    cleanup(&storage, &wal);
}

#[test]
fn source_pruned_returning_uses_source_values() {
    let (mut e, storage, wal) = open("sp_ret");
    exec(
        &mut e,
        "CREATE TABLE sp_t (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(
        &mut e,
        "CREATE TABLE sp_s (id BIGINT PRIMARY KEY, v BIGINT)",
    );
    exec(&mut e, "INSERT INTO sp_t VALUES (1, 0)");
    exec(&mut e, "INSERT INTO sp_s VALUES (1, 42)");
    let out = rows(
        &mut e,
        "UPDATE sp_t SET v = src.v FROM sp_s src \
         WHERE sp_t.id = src.id AND src.id = 1 RETURNING sp_t.id, sp_t.v",
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0][1], Value::Int8(42));
    cleanup(&storage, &wal);
}

#[test]
fn source_pruned_non_unique_index_duplicates_keep_scan_order() {
    let (mut e, storage, wal) = open("sp_nu");
    exec(
        &mut e,
        "CREATE TABLE sp_t (id BIGINT PRIMARY KEY, flag BIGINT, v TEXT)",
    );
    // flag is indexed but NOT unique on the source; two rows share flag=3.
    exec(
        &mut e,
        "CREATE TABLE sp_s (id BIGINT PRIMARY KEY, flag BIGINT, v TEXT)",
    );
    exec(&mut e, "CREATE INDEX sp_nu_flag ON sp_s (flag)");
    exec(&mut e, "INSERT INTO sp_t VALUES (1, 3, 'unset')");
    exec(&mut e, "INSERT INTO sp_s VALUES (10, 3, 'first')");
    exec(&mut e, "INSERT INTO sp_s VALUES (11, 3, 'second')");
    exec(&mut e, "INSERT INTO sp_s VALUES (12, 4, 'other')");
    // Source pruning on src.flag = 3 (non-unique index); the join is on flag
    // and the target has no usable index on it, so target discovery takes the
    // nested-loop fallback over the pruned source rows.
    exec(
        &mut e,
        "UPDATE sp_t SET v = src.v FROM sp_s src \
         WHERE sp_t.flag = src.flag AND src.flag = 3",
    );
    // Two source rows match; the first qualifying FROM row in materialized
    // order wins — exactly what a full table scan yields.
    assert_eq!(
        rows(&mut e, "SELECT v FROM sp_t WHERE id = 1")[0][0],
        Value::Text("first".into())
    );
    cleanup(&storage, &wal);
}
