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
//! Focused regressions for the `json_object_agg` / `jsonb_object_agg` empty
//! input semantics and the TRUNCATE / aggregate state-lifecycle interaction.
//!
//! PostgreSQL semantics asserted here:
//!   * non-empty rows   -> a real JSON/JSONB object
//!   * zero input rows  -> SQL NULL (NOT an empty `{}`)
//!   * NULL values keep  -> `"key": null` members (not dropped, not NULL)
//!   * TRUNCATE empties a table, so a following aggregate returns NULL

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn exec<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} => {e}"))
    {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("{sql} => {other:?}"),
    }
}

/// Executes a query and returns the single first-column value of its first row.
fn single<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Value {
    let rows = exec(executor, sql);
    assert_eq!(
        rows.len(),
        1,
        "{sql} should produce exactly one row, got {rows:?}"
    );
    rows[0][0].clone()
}

#[test]
fn empty_object_aggregates_return_null() {
    let storage =
        std::env::temp_dir().join(format!("plomid-json-agg-empty-{}", std::process::id()));
    let wal =
        std::env::temp_dir().join(format!("plomid-json-agg-empty-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Zero input rows (WHERE FALSE) => no transition => SQL NULL.
    let empty_json = single(
        &mut executor,
        "SELECT json_object_agg(key, value) FROM (SELECT 'name' AS key, 'PLOMID' AS value WHERE FALSE) x;",
    );
    assert_eq!(
        empty_json,
        Value::Null,
        "empty json_object_agg must be NULL, not {{}}; got {empty_json:?}"
    );

    let empty_jsonb = single(
        &mut executor,
        "SELECT jsonb_object_agg(key, value) FROM (SELECT 'name' AS key, 'PLOMID' AS value WHERE FALSE) x;",
    );
    assert_eq!(
        empty_jsonb,
        Value::Null,
        "empty jsonb_object_agg must be NULL; got {empty_jsonb:?}"
    );

    // Non-empty input still aggregates to a real object (not NULL).
    let non_empty = single(
        &mut executor,
        "SELECT json_object_agg(key, value) FROM (SELECT 'name' AS key, 'PLOMID' AS value UNION ALL SELECT 'version', '1') x;",
    );
    assert_ne!(
        non_empty,
        Value::Null,
        "non-empty json_object_agg must be an object"
    );
    assert!(
        non_empty.to_sql_text().contains("\"name\"")
            && non_empty.to_sql_text().contains("\"version\""),
        "unexpected object: {non_empty:?}",
    );

    // A NULL value is kept as `"key": null`, not dropped and not NULL.
    let null_value = single(
        &mut executor,
        "SELECT json_object_agg(key, value) FROM (SELECT 'missing' AS key, NULL::text AS value) x;",
    );
    assert_ne!(
        null_value,
        Value::Null,
        "aggregate with a NULL value must still emit an object"
    );
    assert!(
        null_value.to_sql_text().contains("missing"),
        "NULL value must produce a member, got {null_value:?}",
    );
}

#[test]
fn aggregate_after_truncate_returns_null() {
    let storage =
        std::env::temp_dir().join(format!("plomid-json-agg-trunc-{}", std::process::id()));
    let wal =
        std::env::temp_dir().join(format!("plomid-json-agg-trunc-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    executor
        .execute("CREATE TABLE json_agg_state_test (id INTEGER, name TEXT);")
        .unwrap();
    executor
        .execute("INSERT INTO json_agg_state_test VALUES (1, 'PLOMID'), (2, 'TEST');")
        .unwrap();

    // Before TRUNCATE the aggregate produces a real object.
    let before = single(
        &mut executor,
        "SELECT json_object_agg(id::text, name) FROM json_agg_state_test;",
    );
    assert_ne!(
        before,
        Value::Null,
        "before TRUNCATE the object aggregate must be non-NULL"
    );
    assert!(
        before.to_sql_text().contains("PLOMID"),
        "unexpected object before TRUNCATE: {before:?}"
    );

    let before_jsonb = single(
        &mut executor,
        "SELECT jsonb_object_agg(id::text, name) FROM json_agg_state_test;",
    );
    assert_ne!(
        before_jsonb,
        Value::Null,
        "before TRUNCATE jsonb_object_agg must be non-NULL"
    );

    executor.execute("TRUNCATE json_agg_state_test;").unwrap();

    // The table now has zero rows, so each aggregate must finalize to NULL.
    let after = single(
        &mut executor,
        "SELECT json_object_agg(id::text, name) FROM json_agg_state_test;",
    );
    assert_eq!(
        after,
        Value::Null,
        "json_object_agg over an emptied table must be NULL; got {after:?}"
    );

    let after_jsonb = single(
        &mut executor,
        "SELECT jsonb_object_agg(id::text, name) FROM json_agg_state_test;",
    );
    assert_eq!(
        after_jsonb,
        Value::Null,
        "jsonb_object_agg over an emptied table must be NULL; got {after_jsonb:?}"
    );

    // Plain scans also see the emptied table.
    let rows = exec(&mut executor, "SELECT * FROM json_agg_state_test;");
    assert!(
        rows.is_empty(),
        "TRUNCATE must empty the table, got {rows:?}"
    );

    // A plain count sees zero rows too.
    let count = single(&mut executor, "SELECT count(*) FROM json_agg_state_test;");
    assert_eq!(
        count,
        Value::Int8(0),
        "count(*) over an emptied table must be 0; got {count:?}"
    );
}
