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
use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage = std::env::temp_dir().join(format!("plomid-torture-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!("plomid-torture-{tag}-{}.wal", std::process::id()));
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

fn create_orders<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>) {
    exec_ok(
        executor,
        "CREATE TABLE orders (id INTEGER, status TEXT, total INTEGER);",
    );
    exec_ok(
        executor,
        "INSERT INTO orders VALUES (1, 'paid', 100), (2, 'paid', 200), (3, 'paid', 300), (4, 'pending', 400), (5, 'shipped', NULL);",
    );
}

#[test]
fn torture_three_filtered_counts() {
    let (storage, wal) = unique_engine("filter3");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    create_orders(&mut executor);
    let rows = exec(
        &mut executor,
        "SELECT COUNT(*) FILTER (WHERE status = 'paid'), COUNT(*) FILTER (WHERE status = 'pending'), COUNT(*) FILTER (WHERE status = 'shipped') FROM orders;",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 3, "must return 3 columns, got {:?}", rows[0]);
    assert_eq!(text_rows(&rows), vec![vec!["3", "1", "1"]]);
}

#[test]
fn torture_jsonb_containment_brand() {
    let (storage, wal) = unique_engine("jsonb");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE products (id INTEGER, metadata JSONB);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO products VALUES (1, '{\"ram\":16,\"brand\":\"PLOMID\"}'), (2, '{\"brand\":\"PLOMID\",\"storage\":256}'), (3, '{\"brand\":\"PLOMID\",\"layout\":\"US\"}'), (4, '{\"brand\":\"PLOMID\",\"wireless\":true}'), (5, '{\"brand\":\"OTHER\"}');",
    );
    let rows = exec(
        &mut executor,
        "SELECT id FROM products WHERE metadata @> '{\"brand\":\"PLOMID\"}' ORDER BY id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![vec!["1"], vec!["2"], vec!["3"], vec!["4"]]
    );
    let rows = exec(
        &mut executor,
        "SELECT metadata ? 'brand', metadata ? 'ram', metadata @> '{\"brand\":\"PLOMID\"}' FROM products WHERE id = 1;",
    );
    assert_eq!(text_rows(&rows), vec![vec!["t", "t", "t"]]);
    let cases = [
        ("SELECT '{\"a\":1,\"b\":2}'::jsonb @> '{\"a\":1}';", "t"),
        ("SELECT '{\"a\":1}'::jsonb @> '{\"a\":2}';", "f"),
        ("SELECT '{\"a\":1,\"b\":2}'::jsonb @> '{\"b\":2}';", "t"),
        (
            "SELECT '{\"a\":{\"b\":1,\"c\":2}}'::jsonb @> '{\"a\":{\"b\":1}}';",
            "t",
        ),
        (
            "SELECT '{\"a\":{\"b\":1}}'::jsonb @> '{\"a\":{\"b\":2}}';",
            "f",
        ),
        ("SELECT '{\"a\":1}'::jsonb @> '{\"a\":\"1\"}';", "f"),
        ("SELECT '{\"a\":1}'::jsonb @> '{}';", "t"),
    ];
    for (sql, expected) in cases {
        let rows = exec(&mut executor, sql);
        assert_eq!(text_rows(&rows), vec![vec![expected.to_string()]], "{sql}");
    }
}

#[test]
fn repro() {
    let storage = std::env::temp_dir().join("plomid-repro-json");
    let wal = std::env::temp_dir().join("plomid-repro-json-wal");
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    let queries = [
        "SELECT json_build_array(1,2,3);",
        "SELECT json_build_array('PLOMID',1,true,NULL);",
        "SELECT '{\"a\":1,\"b\":2}'::jsonb - 'a';",
        "SELECT '[\"a\",\"b\",\"c\"]'::jsonb - 1;",
        "SELECT '{\"a\":1,\"b\":2}'::jsonb - ARRAY['a','b'];",
        "SELECT '{\"a\":{\"b\":1,\"c\":2}}'::jsonb #- ARRAY['a','b'];",
        "SELECT '{\"a\":1,\"b\":2}'::jsonb || '{\"c\":3}'::jsonb;",
        "SELECT '{\"a\":1,\"b\":2}'::jsonb || '{\"b\":99,\"c\":3}'::jsonb;",
        "SELECT '[\"a\",\"b\"]'::jsonb || '[\"c\",\"d\"]'::jsonb;",
        "SELECT json_array_length('[1,2,3]'::json);",
        "SELECT jsonb_array_length('[1,2,3]'::jsonb);",
        "SELECT json_array_elements_text('[\"a\",\"b\",\"c\"]'::json);",
        "SELECT json_array_elements_text('[1,true,\"x\",null]'::json);",
        "SELECT jsonb_each('{\"a\":1,\"b\":2}'::jsonb);",
        "SELECT jsonb_pretty('{\"name\":\"PLOMID\",\"version\":1}'::jsonb);",
        "SELECT to_json(123), to_json('PLOMID');",
        "SELECT to_jsonb(123), to_jsonb('PLOMID');",
        "SELECT jsonb_insert('{\"a\":1,\"b\":2}'::jsonb, ARRAY['b'], '99'::jsonb);",
        "SELECT jsonb_insert('{\"tags\":[\"sql\",\"postgres\"]}'::jsonb, ARRAY['tags','1'], '\"json\"'::jsonb);",
        "SELECT json_agg(1);",
        "SELECT jsonb_agg(1);",
        "SELECT json_agg(value) FROM (SELECT 1 AS value UNION ALL SELECT 2 UNION ALL SELECT 3) x;",
        "SELECT jsonb_agg(value) FROM (SELECT 1 AS value UNION ALL SELECT 2 UNION ALL SELECT 3) x;",
        "SELECT json_agg(value) FROM (SELECT 1 AS value UNION ALL SELECT NULL UNION ALL SELECT 3) x;",
        "SELECT json_object_agg(key, value) FROM (SELECT 'name' AS key, 'PLOMID' AS value UNION ALL SELECT 'version', '1') x;",
        "SELECT jsonb_object_agg(key, value) FROM (SELECT 'name' AS key, 'PLOMID' AS value UNION ALL SELECT 'version', '1') x;",
        "SELECT 'name' AS key, 'PLOMID' AS value;",
        "SELECT json_each('{\"a\":1,\"b\":2}'::json);",
        "SELECT jsonb_each('{\"a\":1,\"b\":2}'::jsonb);",
        "SELECT json_each_text('{\"name\":\"PLOMID\",\"active\":true,\"version\":1,\"nothing\":null}'::json);",
        "SELECT * FROM json_each_text('{\"a\":1,\"b\":2}'::json);",
        "SELECT jsonb_each_text('{\"a\":1,\"b\":2}'::jsonb);",
        "SELECT json_each('{\"a\":1,\"b\":2}'::json);",
    ];
    for q in queries {
        println!("--- {q}");
        let r = executor.execute(q);
        match r {
            Ok(QueryResult::Rows { rows, .. }) => println!("   rows={rows:?}"),
            Ok(other) => println!("   other={other:?}"),
            Err(e) => println!("   ERR: {e}"),
        }
    }

    // Empty object aggregates must return NULL (PostgreSQL semantics): zero
    // input rows produce no aggregate transition, so the final result is NULL
    // rather than an empty `{}` object.
    let empty_queries = [
        "SELECT json_object_agg(key, value) FROM (SELECT 1 AS dummy WHERE FALSE) x;",
        "SELECT jsonb_object_agg(key, value) FROM (SELECT 1 AS dummy WHERE FALSE) x;",
    ];
    for q in empty_queries {
        println!("--- {q}");
        let r = executor.execute(q);
        match r {
            Ok(QueryResult::Rows { rows, .. }) => println!("   rows={rows:?}"),
            Ok(other) => println!("   other={other:?}"),
            Err(e) => println!("   ERR: {e}"),
        }
    }

    // Parser-level regressions
    let parser_queries = [
        // ROW(...) expression.
        "SELECT row_to_json(ROW(1, 'PLOMID'));",
        // VALUES-derived table with column aliases.
        "SELECT json_agg(value) FROM (VALUES (1), (2), (3)) x(value);",
        "SELECT jsonb_agg(value) FROM (VALUES (1), (2), (3)) x(value);",
        // Object aggregates over VALUES-derived tables with column aliases.
        "SELECT json_object_agg(key, value) FROM (VALUES ('name', 'PLOMID'), ('version', '1')) x(key, value);",
        "SELECT jsonb_object_agg(key, value) FROM (VALUES ('name', 'PLOMID'), ('version', '1')) x(key, value);",
        // SQL-standard spellings JSON_ARRAYAGG / JSON_OBJECTAGG previously fell
        // through as unsupported functions. They must route to the same
        // aggregate machinery as json_agg / json_object_agg.
        "SELECT JSON_ARRAYAGG(x) FROM (VALUES (1), (2), (3)) s(x);",
        "SELECT JSON_ARRAYAGG(x ORDER BY x DESC) FROM (VALUES (1), (2), (3)) s(x);",
        "SELECT JSON_OBJECTAGG(key, value) FROM (VALUES ('a', '1'), ('a', '2')) s(key, value);",
    ];
    for q in parser_queries {
        println!("--- {q}");
        let r = executor.execute(q);
        match r {
            Ok(QueryResult::Rows { rows, .. }) => println!("   rows={rows:?}"),
            Ok(other) => println!("   other={other:?}"),
            Err(e) => println!("   ERR: {e}"),
        }
    }
}
