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
//! Regression tests for PostgreSQL-compatible aggregate semantics:
//!
//! * `FILTER (WHERE ...)` applied generically to every aggregate
//!   (COUNT, COUNT(DISTINCT), SUM, AVG, MIN, MAX), including zero-row
//!   and all-NULL filters, GROUP BY combination and multiple aggregates.
//! * `AVG(integer)` returning an exact NUMERIC (never integer-truncated).
//! * Empty-input / all-NULL aggregate NULL semantics (SUM/AVG/MIN/MAX
//!   → NULL, COUNT → 0) as previously fixed.
//!
//! These tests fail against the old implementation (FILTER ignored by
//! SUM/AVG; AVG(integer) truncated with `Value::Int8(sum / count)`).

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage = std::env::temp_dir().join(format!("plomid-agg-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!("plomid-agg-{tag}-wal-{}", std::process::id()));
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

fn text_rows(rows: &[Vec<Value>]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|value| match value {
                    Value::Null => "NULL".to_string(),
                    other => other.to_sql_text(),
                })
                .collect()
        })
        .collect()
}

/// Builds the canonical sales table used by both FILTER and AVG checks.
fn create_sales<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>) {
    exec_ok(
        executor,
        "CREATE TABLE sales (
            id INTEGER,
            category TEXT,
            region TEXT,
            amount INTEGER,
            quantity INTEGER,
            note TEXT
        );",
    );
    exec_ok(
        executor,
        "INSERT INTO sales VALUES
        (1, 'A', 'US', 100, 2, 'one'),
        (2, 'A', 'US', 100, 3, 'two'),
        (3, 'A', 'IN', 200, 4, 'three'),
        (4, 'B', 'IN', 300, 5, 'four'),
        (5, 'B', 'IN', 300, 6, 'five'),
        (6, 'B', 'US', NULL, 7, NULL),
        (7, 'C', 'US', 500, 8, 'seven'),
        (8, 'C', NULL, 500, NULL, 'eight'),
        (9, NULL, 'US', 600, 10, 'nine'),
        (10, NULL, NULL, NULL, NULL, NULL);",
    );
}

// ---------------------------------------------------------------------------
// FILTER
// ---------------------------------------------------------------------------

#[test]
fn filter_count_and_count_distinct() {
    let (storage, wal) = unique_engine("filter-count");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    create_sales(&mut executor);

    // COUNT(*) FILTER
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(*) FILTER (WHERE amount IS NOT NULL) FROM sales;"
        )),
        vec![vec!["8"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(*) FILTER (WHERE amount > 250) FROM sales;"
        )),
        vec![vec!["5"]]
    );
    // FILTER returning zero rows.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(*) FILTER (WHERE FALSE) FROM sales;"
        )),
        vec![vec!["0"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(*) FILTER (WHERE amount > 1000) FROM sales;"
        )),
        vec![vec!["0"]]
    );
    // FILTER matching only NULL-valued rows still counts the rows.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(*) FILTER (WHERE amount IS NULL) FROM sales;"
        )),
        vec![vec!["2"]]
    );

    // COUNT(DISTINCT ...) FILTER
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(DISTINCT category) FILTER (WHERE region = 'US') FROM sales;"
        )),
        vec![vec!["3"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(DISTINCT region) FILTER (WHERE amount IS NOT NULL) FROM sales;"
        )),
        vec![vec!["2"]]
    );
}

#[test]
fn filter_sum_avg_min_max() {
    let (storage, wal) = unique_engine("filter-numeric");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    create_sales(&mut executor);

    // US rows: 100, 100, NULL, 500, 600 -> SUM = 1300
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT SUM(amount) FILTER (WHERE region = 'US') FROM sales;"
        )),
        vec![vec!["1300"]]
    );
    // IN rows: 200, 300, 300 -> SUM = 800
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT SUM(amount) FILTER (WHERE region = 'IN') FROM sales;"
        )),
        vec![vec!["800"]]
    );
    // No matching rows: SUM must be NULL, not 0.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT SUM(amount) FILTER (WHERE amount > 1000) FROM sales;"
        )),
        vec![vec!["NULL"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT SUM(amount) FILTER (WHERE FALSE) FROM sales;"
        )),
        vec![vec!["NULL"]]
    );
    // AVG FILTER: US = (100+100+500+600)/4 = 325
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT AVG(amount) FILTER (WHERE region = 'US') FROM sales;"
        )),
        vec![vec!["325"]]
    );
    // AVG FILTER with no matching rows -> NULL.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT AVG(amount) FILTER (WHERE amount > 1000) FROM sales;"
        )),
        vec![vec!["NULL"]]
    );
    // MIN/MAX FILTER: IN region -> MIN 200, MAX 300.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT MIN(amount) FILTER (WHERE region = 'IN'), MAX(amount) FILTER (WHERE region = 'IN') FROM sales;"
        )),
        vec![vec!["200", "300"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT MIN(amount) FILTER (WHERE FALSE), MAX(amount) FILTER (WHERE FALSE) FROM sales;"
        )),
        vec![vec!["NULL", "NULL"]]
    );
}

#[test]
fn filter_multiple_mixed_grouped_null() {
    let (storage, wal) = unique_engine("filter-multi");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    create_sales(&mut executor);

    // 2 filtered aggregates + unfiltered combined.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(*) FILTER (WHERE region = 'US'), COUNT(*) FILTER (WHERE region = 'IN'), COUNT(*) FROM sales;"
        )),
        vec![vec!["5", "3", "10"]]
    );
    // 3 filtered aggregates, distinct functions/filters.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(*) FILTER (WHERE region = 'US'), SUM(amount) FILTER (WHERE region = 'IN'), AVG(amount) FILTER (WHERE region = 'US') FROM sales;"
        )),
        vec![vec!["5", "800", "325"]]
    );
    // FILTER + GROUP BY: per-region counts. NULL-region group has one row
    // with amount > 250; PG default ORDER BY ASC sorts NULLS LAST.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT region, COUNT(*) FILTER (WHERE amount > 250) FROM sales GROUP BY region ORDER BY region;"
        )),
        vec![vec!["IN", "2"], vec!["US", "2"], vec!["NULL", "1"]]
    );
    // FILTER with non-trivial predicate + NULL semantics.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT COUNT(*) FILTER (WHERE region = 'US' AND (amount > 150 OR amount IS NULL)) FROM sales;"
        )),
        vec![vec!["3"]]
    );
}

// ---------------------------------------------------------------------------
// AVG(integer) → exact NUMERIC (never integer-truncated)
// ---------------------------------------------------------------------------

#[test]
fn avg_integer_is_exact_numeric() {
    let (storage, wal) = unique_engine("avg-numeric");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    create_sales(&mut executor);

    // Full table: (100+100+200+300+300+500+500+600)/8 = 325 (NULL rows skipped)
    assert_eq!(
        text_rows(&exec(&mut executor, "SELECT AVG(amount) FROM sales;")),
        vec![vec!["325"]]
    );
    // Non-divisible case: IN rows (200+300+300)/3 = 266.66666... NUMERIC.
    let avg_in = text_rows(&exec(
        &mut executor,
        "SELECT AVG(amount) FILTER (WHERE region = 'IN') FROM sales;",
    ));
    let avg_in_value = &avg_in[0][0];
    // Must NOT be the integer-truncated 266: NUMERIC keeps fractional digits.
    assert!(
        avg_in_value.contains('.'),
        "AVG(200+300+300)/3 = 266.66666 should be NUMERIC with a fraction, got {avg_in_value}"
    );
    // All-NULL input: AVG → NULL.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT AVG(amount) FILTER (WHERE amount IS NULL) FROM sales;"
        )),
        vec![vec!["NULL"]]
    );
}
