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
//! Regression tests for two root causes behind JSON_ALL failures:
//!
//! 1. Expression indexes (`CREATE INDEX ... ON t((payload ->> 'k'))`) must
//!    preserve the index expression as an AST and evaluate it through the
//!    normal expression evaluator — the expression's Debug/string form must
//!    never be resolved as a column name.
//! 2. `GENERATED ALWAYS AS (expr) STORED` columns must parse, persist in the
//!    catalog, and recompute on INSERT/UPDATE using the normal expression
//!    infrastructure.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage = std::env::temp_dir().join(format!("plomid-expridx-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!("plomid-expridx-{tag}-wal-{}", std::process::id()));
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

fn setup<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>) {
    executor
        .execute("CREATE SCHEMA json_gap_081;")
        .expect("create schema");
    executor
        .execute(
            "CREATE TABLE json_gap_081.operator_test (
                id INTEGER PRIMARY KEY,
                payload JSONB
            );",
        )
        .expect("create table");
    executor
        .execute(
            "INSERT INTO json_gap_081.operator_test (id, payload) VALUES
                (1, '{\"group\":1,\"active\":true}'),
                (2, '{\"group\":3,\"active\":false}');",
        )
        .expect("insert");
}

#[test]
fn expression_index_preserves_expression_ast() {
    let (storage, wal) = unique_engine("expr-index");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor);

    // Casted JSON expression index: must not raise
    // `column "TypeCast { .. }" does not exist`.
    executor
        .execute(
            "CREATE INDEX operator_test_group_idx
             ON json_gap_081.operator_test(((payload ->> 'group')::INTEGER));",
        )
        .expect("create casted expression index");

    // Bare JSON arrow expression index.
    executor
        .execute(
            "CREATE INDEX operator_test_active_idx
             ON json_gap_081.operator_test((payload ->> 'active'));",
        )
        .expect("create json arrow expression index");

    // Rows inserted after the expression indexes exist must maintain them
    // (stage_index_puts evaluates the expression, not a column lookup).
    executor
        .execute(
            "INSERT INTO json_gap_081.operator_test (id, payload)
             VALUES (3, '{\"group\":2,\"active\":true}');",
        )
        .expect("insert after expression index");

    let count = rows(
        &mut executor,
        "SELECT COUNT(*) FROM json_gap_081.operator_test;",
    );
    assert_eq!(count[0][0], Value::Int8(3));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn plain_column_index_resolution_still_works() {
    let (storage, wal) = unique_engine("plain-index");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor);

    executor
        .execute("CREATE INDEX operator_test_id_idx ON json_gap_081.operator_test(id);")
        .expect("create plain column index");

    let found = rows(
        &mut executor,
        "SELECT payload FROM json_gap_081.operator_test WHERE id = 2;",
    );
    assert_eq!(found.len(), 1);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn generated_stored_columns_compute_on_insert_and_update() {
    let (storage, wal) = unique_engine("generated");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup(&mut executor);

    executor
        .execute(
            "CREATE TABLE json_gap_081.generated_workload (
                id INTEGER PRIMARY KEY,
                payload JSONB NOT NULL,
                customer_id INTEGER
                    GENERATED ALWAYS AS ((payload ->> 'customer_id')::INTEGER) STORED,
                active BOOLEAN
                    GENERATED ALWAYS AS ((payload ->> 'active')::BOOLEAN) STORED
            );",
        )
        .expect("create table with generated columns");

    executor
        .execute(
            "INSERT INTO json_gap_081.generated_workload (id, payload)
             VALUES (1, '{\"customer_id\":1001,\"active\":true}');",
        )
        .expect("insert into generated workload");

    let row = rows(
        &mut executor,
        "SELECT customer_id, active FROM json_gap_081.generated_workload WHERE id = 1;",
    );
    assert_eq!(row[0][0], Value::Int4(1001));
    assert_eq!(row[0][1], Value::Bool(true));

    // UPDATE of the source column must recompute the generated values.
    executor
        .execute(
            "UPDATE json_gap_081.generated_workload
             SET payload = '{\"customer_id\":2001,\"active\":false}'
             WHERE id = 1;",
        )
        .expect("update payload");

    let updated = rows(
        &mut executor,
        "SELECT customer_id, active FROM json_gap_081.generated_workload WHERE id = 1;",
    );
    assert_eq!(updated[0][0], Value::Int4(2001));
    assert_eq!(updated[0][1], Value::Bool(false));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn generated_columns_survive_catalog_reload() {
    let (storage, wal) = unique_engine("generated-reload");
    {
        let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
        let mut executor = Executor::new(engine).unwrap();
        executor
            .execute(
                "CREATE TABLE gw (
                    id INTEGER PRIMARY KEY,
                    payload JSONB NOT NULL,
                    n INTEGER GENERATED ALWAYS AS ((payload ->> 'n')::INTEGER) STORED
                );",
            )
            .expect("create generated table");
        executor
            .execute("INSERT INTO gw (id, payload) VALUES (1, '{\"n\":42}');")
            .expect("insert");
        drop(executor);
    }
    // Reopen the engine: the generated expression must persist in the catalog
    // so later inserts still compute the column.
    {
        let engine = PlomidStorageEngine::open(&storage, &wal, 32).unwrap();
        let mut executor = Executor::new(engine).unwrap();
        executor
            .execute("INSERT INTO gw (id, payload) VALUES (2, '{\"n\":43}');")
            .expect("insert after reload");
        let found = rows(&mut executor, "SELECT n FROM gw WHERE id = 2;");
        assert_eq!(found[0][0], Value::Int4(43));
        drop(executor);
    }
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
