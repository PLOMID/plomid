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
//! Regression tests for the PostgreSQL-compatible window-function subsystem.
//!
//! These drive the same Executor path as the network wire server
//! (lexer → parser → planner → executor). The fixture matches the mandatory
//! correctness tests in the window-function specification:
//!
//! ```sql
//! -- orders (id, customer_id, total_amount)
//! -- (1,1,1300) (2,1,850) (3,2,500) (4,4,1200) (5,5,100) (6,2,700) (7,4,300) (8,5,600)
//! ```

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-windowfn-{tag}-{}", std::process::id()));
    let wal =
        std::env::temp_dir().join(format!("plomid-windowfn-{tag}-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn setup_orders<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>) {
    exec_ok(
        executor,
        "CREATE TABLE orders (id BIGINT PRIMARY KEY, customer_id BIGINT NOT NULL, total_amount NUMERIC(12,2) NOT NULL);",
    );
    for (id, customer, amount) in [
        (1, 1, 1300),
        (2, 1, 850),
        (3, 2, 500),
        (4, 4, 1200),
        (5, 5, 100),
        (6, 2, 700),
        (7, 4, 300),
        (8, 5, 600),
    ] {
        exec_ok(
            executor,
            &format!("INSERT INTO orders VALUES ({id}, {customer}, {amount});"),
        );
    }
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

fn engine(
    tag: &str,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    Executor<PlomidStorageEngine>,
) {
    let (storage, wal) = unique_engine(tag);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_orders(&mut executor);
    (storage, wal, executor)
}

#[test]
fn running_sum_window() {
    let (storage, wal, mut executor) = engine("orders-1");
    let rows = exec(
        &mut executor,
        "SELECT id, SUM(total_amount) OVER (ORDER BY id) FROM orders ORDER BY id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "1300"],
            vec!["2", "2150"],
            vec!["3", "2650"],
            vec!["4", "3850"],
            vec!["5", "3950"],
            vec!["6", "4650"],
            vec!["7", "4950"],
            vec!["8", "5550"],
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn full_frame_last_value() {
    let (storage, wal, mut executor) = engine("orders-2");
    let rows = exec(
        &mut executor,
        "SELECT id, LAST_VALUE(total_amount) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) FROM orders ORDER BY id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "600"],
            vec!["2", "600"],
            vec!["3", "600"],
            vec!["4", "600"],
            vec!["5", "600"],
            vec!["6", "600"],
            vec!["7", "600"],
            vec!["8", "600"],
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn running_last_value() {
    let (storage, wal, mut executor) = engine("orders-3");
    let rows = exec(
        &mut executor,
        "SELECT id, LAST_VALUE(total_amount) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM orders ORDER BY id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "1300"],
            vec!["2", "850"],
            vec!["3", "500"],
            vec!["4", "1200"],
            vec!["5", "100"],
            vec!["6", "700"],
            vec!["7", "300"],
            vec!["8", "600"],
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn first_value_default_frame() {
    let (storage, wal, mut executor) = engine("orders-4");
    let rows = exec(
        &mut executor,
        "SELECT id, FIRST_VALUE(total_amount) OVER (ORDER BY id) FROM orders ORDER BY id;",
    );
    // Default frame (with ORDER BY) is UNBOUNDED PRECEDING .. CURRENT ROW, so the
    // first value never changes from the earliest visible row.
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "1300"],
            vec!["2", "1300"],
            vec!["3", "1300"],
            vec!["4", "1300"],
            vec!["5", "1300"],
            vec!["6", "1300"],
            vec!["7", "1300"],
            vec!["8", "1300"],
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn bounded_frame_sum() {
    let (storage, wal, mut executor) = engine("orders-5");
    let rows = exec(
        &mut executor,
        "SELECT id, SUM(total_amount) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) FROM orders ORDER BY id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "2150"],
            vec!["2", "2650"],
            vec!["3", "2550"],
            vec!["4", "1800"],
            vec!["5", "2000"],
            vec!["6", "1100"],
            vec!["7", "1600"],
            vec!["8", "900"],
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn partitioned_window() {
    let (storage, wal, mut executor) = engine("orders-6");
    let rows = exec(
        &mut executor,
        "SELECT id, customer_id, SUM(total_amount) OVER (PARTITION BY customer_id ORDER BY id) FROM orders ORDER BY customer_id, id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "1", "1300"],
            vec!["2", "1", "2150"],
            vec!["3", "2", "500"],
            vec!["6", "2", "1200"],
            vec!["4", "4", "1200"],
            vec!["7", "4", "1500"],
            vec!["5", "5", "100"],
            vec!["8", "5", "700"],
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn ranking_with_peers() {
    let (storage, wal) = unique_engine("rank");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER, v INTEGER);");
    for (id, v) in [(1, 10), (2, 20), (3, 20), (4, 30)] {
        exec_ok(&mut executor, &format!("INSERT INTO t VALUES ({id}, {v});"));
    }
    let rows = exec(
        &mut executor,
        "SELECT id, RANK() OVER (ORDER BY v), DENSE_RANK() OVER (ORDER BY v) FROM t ORDER BY id;",
    );
    // v: 10,20,20,30 -> ranks 1,2,2,4 ; dense 1,2,2,3
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "1", "1"],
            vec!["2", "2", "2"],
            vec!["3", "2", "2"],
            vec!["4", "4", "3"],
        ]
    );
    let rows = exec(
        &mut executor,
        "SELECT id, PERCENT_RANK() OVER (ORDER BY v), CUME_DIST() OVER (ORDER BY v) FROM t ORDER BY id;",
    );
    let rows = text_rows(&rows);
    assert_eq!(rows[0][1], "0");
    assert_eq!(rows[3][1], "1");
    assert_eq!(rows[3][2], "1");
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn ntile_test() {
    let (storage, wal) = unique_engine("ntile");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER);");
    for id in 1..=8 {
        exec_ok(&mut executor, &format!("INSERT INTO t VALUES ({id});"));
    }
    let rows = exec(
        &mut executor,
        "SELECT id, NTILE(4) OVER (ORDER BY id) FROM t ORDER BY id;",
    );
    let rows = text_rows(&rows);
    for (i, row) in rows.iter().enumerate() {
        let expected = if i < 2 {
            "1"
        } else if i < 4 {
            "2"
        } else if i < 6 {
            "3"
        } else {
            "4"
        };
        assert_eq!(row[1], expected, "ntile bucket row {i}");
    }
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn lag_lead_with_defaults() {
    let (storage, wal, mut executor) = engine("orders-7");
    let rows = exec(
        &mut executor,
        "SELECT id, LAG(total_amount) OVER (ORDER BY id), LEAD(total_amount) OVER (ORDER BY id), LAG(total_amount, 2, 0) OVER (ORDER BY id) FROM orders ORDER BY id;",
    );
    let rows = text_rows(&rows);
    // LAG: previous ; LEAD: next ; LAG(x,2,0): 2 back, 0 default
    assert_eq!(rows[0][1], "NULL");
    assert_eq!(rows[0][2], "850");
    assert_eq!(rows[0][3], "0");
    assert_eq!(rows[1][1], "1300");
    assert_eq!(rows[7][2], "NULL");
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn frame_boundary_clipping() {
    let (storage, wal, mut executor) = engine("orders-8");
    let rows = exec(
        &mut executor,
        "SELECT id, SUM(total_amount) OVER (ORDER BY id ROWS BETWEEN 10 PRECEDING AND 10 FOLLOWING) FROM orders ORDER BY id;",
    );
    let rows = text_rows(&rows);
    // Whole partition for every row (10 preceding/following clamps to ends).
    assert_eq!(rows[0][1], "5550");
    assert_eq!(rows[7][1], "5550");
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn null_handling_window() {
    let (storage, wal) = unique_engine("nulls");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER, v INTEGER);");
    for (id, v) in [(1, 10), (2, 20), (3, 30)] {
        exec_ok(&mut executor, &format!("INSERT INTO t VALUES ({id}, {v});"));
    }
    exec_ok(&mut executor, "INSERT INTO t VALUES (4, NULL);");
    exec_ok(&mut executor, "INSERT INTO t VALUES (5, 50);");
    let rows = exec(
        &mut executor,
        "SELECT id, COUNT(v) OVER (ORDER BY id), COUNT(*) OVER (ORDER BY id), SUM(v) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING), AVG(v) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING), LAST_VALUE(v) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM t ORDER BY id;",
    );
    let rows = text_rows(&rows);
    // id1: count(v)=1, count(*)=1
    assert_eq!(rows[0][1], "1");
    assert_eq!(rows[0][2], "1");
    // id4 (NULL): count(v)=3 (10,20,30), count(*)=4
    assert_eq!(rows[3][1], "3");
    assert_eq!(rows[3][2], "4");
    // Full-frame SUM = 110, AVG = 27.5 (rendered at the engine's numeric scale).
    assert_eq!(rows[3][3], "110");
    assert_eq!(rows[3][4], "27.5");
    assert_eq!(rows[3][5], "NULL");
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn invalid_frame_errors_cleanly() {
    let (storage, wal, mut executor) = engine("orders-9");
    let err = exec_err(
        &mut executor,
        "SELECT SUM(total_amount) OVER (ORDER BY id ROWS BETWEEN 5 FOLLOWING AND 2 PRECEDING) FROM orders;",
    );
    assert!(
        err.contains("frame") || err.contains("expected"),
        "got: {err}"
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn nth_value_within_frame() {
    let (storage, wal) = unique_engine("nth");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER, v INTEGER);");
    for (id, v) in [(1, 10), (2, 20), (3, 30), (4, 40)] {
        exec_ok(&mut executor, &format!("INSERT INTO t VALUES ({id}, {v});"));
    }
    let rows = exec(
        &mut executor,
        "SELECT id, NTH_VALUE(v, 2) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM t ORDER BY id;",
    );
    // Frame grows 1..4 rows; the 2nd frame value is NULL until 2 rows exist.
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "NULL"],
            vec!["2", "20"],
            vec!["3", "20"],
            vec!["4", "20"],
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn range_frame_uses_peer_groups() {
    let (storage, wal) = unique_engine("range");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER, v INTEGER);");
    for (id, v) in [(1, 10), (2, 20), (3, 20), (4, 30)] {
        exec_ok(&mut executor, &format!("INSERT INTO t VALUES ({id}, {v});"));
    }
    // RANGE CURRENT ROW never splits a peer group: both v=20 rows share a frame.
    let rows = exec(
        &mut executor,
        "SELECT id, SUM(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS r, SUM(v) OVER (ORDER BY v RANGE CURRENT ROW) AS g FROM t ORDER BY id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            // Running frame: 10 | 10+20+20 | 10+20+20 | 10+20+20+30
            vec!["1", "10", "10"],
            vec!["2", "50", "40"],
            vec!["3", "50", "40"],
            vec!["4", "80", "30"],
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn range_offset_frames_are_refused() {
    let (storage, wal) = unique_engine("range-offset");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER, v INTEGER);");
    for (id, v) in [(1, 10), (2, 20)] {
        exec_ok(&mut executor, &format!("INSERT INTO t VALUES ({id}, {v});"));
    }
    // PostgreSQL RANGE offsets are VALUE offsets; PLOMID refuses them rather
    // than silently approximating with row offsets.
    let err = exec_err(
        &mut executor,
        "SELECT id, SUM(v) OVER (ORDER BY v RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t;",
    );
    assert!(
        err.contains("RANGE") || err.contains("offset"),
        "got: {err}"
    );
    let err = exec_err(
        &mut executor,
        "SELECT id, SUM(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING AND 1 FOLLOWING) FROM t;",
    );
    assert!(
        err.contains("RANGE") || err.contains("offset"),
        "got: {err}"
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn groups_frame_moves_by_peer_groups() {
    let (storage, wal) = unique_engine("groups");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER, v INTEGER);");
    for (id, v) in [(1, 10), (2, 20), (3, 20), (4, 30)] {
        exec_ok(&mut executor, &format!("INSERT INTO t VALUES ({id}, {v});"));
    }
    let rows = exec(
        &mut executor,
        "SELECT id, SUM(v) OVER (ORDER BY v GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t ORDER BY id;",
    );
    // Peer groups by v: [10], [20,20], [30]. 1 PRECEDING moves one whole
    // group, so the frame is the previous group plus the current group.
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "10"],
            vec!["2", "50"],
            vec!["3", "50"],
            vec!["4", "70"],
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn multiple_window_specs_are_independent() {
    let (storage, wal, mut executor) = engine("multi");
    let rows = exec(
        &mut executor,
        "SELECT id, SUM(total_amount) OVER (ORDER BY id) AS running, SUM(total_amount) OVER (PARTITION BY customer_id ORDER BY id) AS per_customer FROM orders ORDER BY id;",
    );
    let rows = text_rows(&rows);
    // Global running total vs. independent per-partition running totals.
    assert_eq!(rows[0][1], "1300");
    assert_eq!(rows[0][2], "1300");
    assert_eq!(rows[5][1], "4650");
    assert_eq!(rows[5][2], "1200");
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn window_functions_with_where_and_limit() {
    let (storage, wal, mut executor) = engine("clauses");
    // WHERE must filter rows before window computation.
    let rows = exec(
        &mut executor,
        "SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM orders WHERE customer_id = 2 ORDER BY id;",
    );
    assert_eq!(text_rows(&rows), vec![vec!["3", "1"], vec!["6", "2"],]);
    // LIMIT applies after the window computation.
    let rows = exec(
        &mut executor,
        "SELECT id, COUNT(*) OVER () FROM orders ORDER BY id LIMIT 2;",
    );
    assert_eq!(text_rows(&rows), vec![vec!["1", "8"], vec!["2", "8"],]);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

fn drop<E: plomid_txn::StorageEngine>(executor: Executor<E>) {
    let _ = executor;
}
