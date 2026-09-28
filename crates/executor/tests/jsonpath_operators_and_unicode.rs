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
//! Regression tests for PostgreSQL JSONPath operators (`@?`, `@@`), GIN
//! operator classes (`jsonb_path_ops`), and Unicode JSON escapes.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-jsonpath-{tag}-{}", std::process::id()));
    let wal =
        std::env::temp_dir().join(format!("plomid-jsonpath-{tag}-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn rows<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor.execute(sql).expect("sql should execute") {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

fn setup_json_table<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>) {
    executor
        .execute("CREATE TABLE jtest (id INTEGER PRIMARY KEY, data JSONB);")
        .expect("create table");
    executor
        .execute(
            "INSERT INTO jtest (id, data) VALUES
                (1, '{\"a\": 1, \"b\": {\"c\": 2}}'),
                (2, '{\"a\": 10, \"items\": [1, 2, 3]}'),
                (3, '{\"nested\": {\"deep\": {\"value\": 42}}}');",
        )
        .expect("insert");
}

#[test]
fn jsonpath_exists_operator_positive_match() {
    let (storage, wal) = unique_engine("exists-pos");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_json_table(&mut executor);
    let result = rows(
        &mut executor,
        "SELECT data @? '$.a' FROM jtest WHERE id = 1;",
    );
    assert_eq!(result[0][0], Value::Bool(true));
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn jsonpath_exists_operator_negative_match() {
    let (storage, wal) = unique_engine("exists-neg");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_json_table(&mut executor);
    let result = rows(
        &mut executor,
        "SELECT data @? '$.nonexistent' FROM jtest WHERE id = 1;",
    );
    assert_eq!(result[0][0], Value::Bool(false));
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn jsonpath_exists_operator_nested() {
    let (storage, wal) = unique_engine("exists-nested");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_json_table(&mut executor);
    let result = rows(
        &mut executor,
        "SELECT data @? '$.nested.deep.value' FROM jtest WHERE id = 3;",
    );
    assert_eq!(result[0][0], Value::Bool(true));
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn jsonpath_match_operator_true_predicate() {
    let (storage, wal) = unique_engine("match-true");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_json_table(&mut executor);
    let result = rows(
        &mut executor,
        "SELECT data @@ '$.a == 1' FROM jtest WHERE id = 1;",
    );
    assert_eq!(result[0][0], Value::Bool(true));
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn jsonpath_match_operator_false_predicate() {
    let (storage, wal) = unique_engine("match-false");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_json_table(&mut executor);
    let result = rows(
        &mut executor,
        "SELECT data @@ '$.a == 999' FROM jtest WHERE id = 1;",
    );
    assert_eq!(result[0][0], Value::Bool(false));
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn jsonpath_operators_in_where_clause() {
    let (storage, wal) = unique_engine("where-clause");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_json_table(&mut executor);
    let result = rows(&mut executor, "SELECT id FROM jtest WHERE data @? '$.b.c';");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0][0], Value::Int4(1));
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn gin_operator_class_jsonb_path_ops() {
    let (storage, wal) = unique_engine("gin-opclass");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_json_table(&mut executor);
    executor
        .execute("CREATE INDEX jtest_data_gin ON jtest USING GIN (data jsonb_path_ops);")
        .expect("create GIN index with jsonb_path_ops");
    let result = rows(&mut executor, "SELECT COUNT(*) FROM jtest;");
    assert_eq!(result[0][0], Value::Int8(3));
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn gin_operator_class_survives_reload() {
    let (storage, wal) = unique_engine("gin-reload");
    {
        let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
        let mut executor = Executor::new(engine).unwrap();
        setup_json_table(&mut executor);
        executor
            .execute("CREATE INDEX jtest_data_gin ON jtest USING GIN (data jsonb_path_ops);")
            .expect("create GIN index");
        drop(executor);
    }
    {
        let engine = PlomidStorageEngine::open(&storage, &wal, 32).unwrap();
        let mut executor = Executor::new(engine).unwrap();
        executor
            .execute("INSERT INTO jtest (id, data) VALUES (4, '{\"x\": 1}');")
            .expect("insert after reload");
        let result = rows(&mut executor, "SELECT COUNT(*) FROM jtest;");
        assert_eq!(result[0][0], Value::Int8(4));
        drop(executor);
    }
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn unicode_escape_ascii() {
    let (storage, wal) = unique_engine("unicode-ascii");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    // The JSONB value is stored in a binary format. We extract the text
    // and check it contains the expected Unicode character.
    let result = rows(
        &mut executor,
        "SELECT E'{\"text\":\"\\u0041\"}'::jsonb::text;",
    );
    let text = format!("{:?}", result[0][0]);
    // The Text variant shows escaped quotes, so we look for the pattern
    // that represents "A" in the debug output.
    assert!(
        text.contains("A") && !text.contains("\\u"),
        "Expected literal A, got {}",
        text
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn unicode_escape_latin1() {
    let (storage, wal) = unique_engine("unicode-latin1");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT E'{\"text\":\"\\u00E9\"}'::jsonb::text;",
    );
    let text = format!("{:?}", result[0][0]);
    assert!(text.contains("é"), "Expected é, got {}", text);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn unicode_escape_chinese() {
    let (storage, wal) = unique_engine("unicode-chinese");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT E'{\"text\":\"\\u4F60\\u597D\"}'::jsonb::text;",
    );
    let text = format!("{:?}", result[0][0]);
    assert!(text.contains("你好"), "Expected 你好, got {}", text);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn unicode_escape_surrogate_pair() {
    let (storage, wal) = unique_engine("unicode-surrogate");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT E'{\"text\":\"\\uD83D\\uDE00\"}'::jsonb::text;",
    );
    let text = format!("{:?}", result[0][0]);
    assert!(text.contains("😀"), "Expected 😀, got {}", text);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn normal_utf8_still_works() {
    let (storage, wal) = unique_engine("utf8-normal");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT '{\"text\": \"Hello 你好 😀\"}'::jsonb::text;",
    );
    let text = format!("{:?}", result[0][0]);
    assert!(
        text.contains("Hello 你好 😀"),
        "Expected Hello 你好 😀, got {}",
        text
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
// --- P0: TIMESTAMPTZ typed literals --------------------------------------
// PostgreSQL accepts TIMESTAMPTZ as shorthand for TIMESTAMP WITH TIME ZONE.
// The exact JSON_ALL failure was JSON_SCALAR(TIMESTAMPTZ '...'), which must
// resolve through the normal type/expression layer.
#[test]
fn timestamptz_literal_in_json_scalar() {
    let (storage, wal) = unique_engine("p0-timestamptz");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT JSON_SCALAR(TIMESTAMPTZ '2026-01-01 12:30:45+05:30');",
    );
    match &result[0][0] {
        Value::Json(text) => assert!(text.contains("2026-01-01")),
        other => panic!("expected JSON scalar, got {other:?}"),
    }
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn timestamp_with_time_zone_spelling_matches_timestamptz() {
    let (storage, wal) = unique_engine("p0-tstz-spelling");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let short = rows(
        &mut executor,
        "SELECT JSON_SCALAR(TIMESTAMPTZ '2026-01-01 12:30:45+05:30');",
    );
    let long = rows(
        &mut executor,
        "SELECT JSON_SCALAR(TIMESTAMP WITH TIME ZONE '2026-01-01 12:30:45+05:30');",
    );
    assert_eq!(short, long);
    let plain = rows(
        &mut executor,
        "SELECT JSON_SCALAR(TIMESTAMP '2026-01-01 12:30:45');",
    );
    match &plain[0][0] {
        Value::Json(text) => assert!(text.contains("2026-01-01")),
        other => panic!("expected JSON scalar, got {other:?}"),
    }
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// --- P0: bare flag suffix consumes trailing input -------------------------
#[test]
fn jsonpath_bare_flag_consumes_trailing_input() {
    let (storage, wal) = unique_engine("p0-flag");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT jsonb_path_query('{\"name\":\"PLOMID\"}'::jsonb, '$.name flag \"i\"');",
    );
    assert_eq!(result.len(), 1);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn jsonpath_like_regex_with_flag_still_works() {
    let (storage, wal) = unique_engine("p0-like-regex-flag");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let ok = rows(
        &mut executor,
        "SELECT jsonb_path_exists('{\"name\":\"plomid\"}'::jsonb, '$.name like_regex \"^PLO.*\" flag \"i\"');",
    );
    assert_eq!(ok[0][0], Value::Bool(true));
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn jsonpath_genuinely_invalid_path_still_rejected() {
    let (storage, wal) = unique_engine("p0-invalid-path");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let err = executor
        .execute("SELECT jsonb_path_query('{\"a\":1}'::jsonb, '$.a 123');")
        .expect_err("invalid JSONPath must still fail");
    let text = format!("{err:?}");
    assert!(text.contains("trailing") || text.contains("JSONPath"));
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// --- P0: aggregate nested inside a scalar call ----------------------------
#[test]
fn jsonb_agg_nested_in_scalar_aggregate_context() {
    let (storage, wal) = unique_engine("p0-agg-ctx");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let plain = rows(
        &mut executor,
        "SELECT jsonb_agg(value) FROM (VALUES (1),(2),(3)) s(value);",
    );
    assert_eq!(plain.len(), 1);
    let nested = rows(
        &mut executor,
        "SELECT jsonb_path_query_array(jsonb_agg(payload), '$[*] ? (@.active == true).customer') FROM (SELECT jsonb_build_object('id', id, 'active', id % 2 = 0, 'customer', jsonb_build_object('id', id + 1000)) AS payload FROM generate_series(1,10) id) s;",
    );
    assert_eq!(nested.len(), 1);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// --- JSON_TABLE / json_to_record row production ---------------------------
//
// These cover the document modality's row-production path. The algorithm lives
// in `plomid-json` (`table.rs`) and reaches back into the executor only through
// the `ExpressionEval` adapter in `join.rs`, so these tests are the end-to-end
// guard on that boundary.

#[test]
fn json_table_projects_rows_from_a_path() {
    let (storage, wal) = unique_engine("jt-rows");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT * FROM JSON_TABLE('[1,2,3]', '$[*]' COLUMNS (v INTEGER PATH '$')) AS t;",
    );
    assert_eq!(
        result,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)]
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn json_table_for_ordinality_numbers_rows() {
    let (storage, wal) = unique_engine("jt-ord");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT * FROM JSON_TABLE('[1,2]', '$[*]' COLUMNS (n FOR ORDINALITY, v INTEGER PATH '$')) AS t;",
    );
    assert_eq!(
        result,
        vec![
            vec![Value::Int8(1), Value::Int4(1)],
            vec![Value::Int8(2), Value::Int4(2)]
        ]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn json_table_nested_path_multiplies_rows() {
    let (storage, wal) = unique_engine("jt-nested");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT * FROM JSON_TABLE('{\"items\":[1,2]}', '$' COLUMNS (NESTED PATH '$.items[*]' COLUMNS (v INTEGER PATH '$'))) AS t;",
    );
    assert_eq!(result, vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn json_table_exists_column_reports_path_presence() {
    let (storage, wal) = unique_engine("jt-exists");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT * FROM JSON_TABLE('{\"a\":1}', '$' COLUMNS (has BOOLEAN EXISTS PATH '$.a', missing BOOLEAN EXISTS PATH '$.zz')) AS t;",
    );
    assert_eq!(result, vec![vec![Value::Bool(true), Value::Bool(false)]]);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn json_table_default_on_empty_fills_a_missing_path() {
    let (storage, wal) = unique_engine("jt-default");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT * FROM JSON_TABLE('{}', '$' COLUMNS (v INTEGER PATH '$.missing' DEFAULT '5' ON EMPTY)) AS t;",
    );
    assert_eq!(result, vec![vec![Value::Int4(5)]]);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn json_to_record_expands_an_object_into_declared_columns() {
    let (storage, wal) = unique_engine("jt-record");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT * FROM json_to_record('{\"a\":1,\"b\":\"x\"}'::json) AS t(a INTEGER, b TEXT);",
    );
    assert_eq!(result, vec![vec![Value::Int4(1), Value::Text("x".into())]]);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn jsonb_to_recordset_expands_an_array_of_objects() {
    let (storage, wal) = unique_engine("jt-recordset");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT * FROM jsonb_to_recordset('[{\"a\":1},{\"a\":2}]'::jsonb) AS t(a INTEGER);",
    );
    assert_eq!(result, vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn json_to_record_missing_column_is_null() {
    let (storage, wal) = unique_engine("jt-null");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let result = rows(
        &mut executor,
        "SELECT * FROM json_to_record('{\"a\":1}'::json) AS t(a INTEGER, missing TEXT);",
    );
    assert_eq!(result, vec![vec![Value::Int4(1), Value::Null]]);
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
