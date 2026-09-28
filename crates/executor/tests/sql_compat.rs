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
//! Regression tests for SQL compatibility features exercised through the same
//! Executor path used by the PostgreSQL wire server.
//!
//! `Executor::execute` drives lexer → parser → binder/planner → executor, which
//! is exactly what the network handler calls for every `Query` message, so
//! these tests guard the server's SQL behaviour without requiring a socket.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-sqlcompat-{tag}-{}", std::process::id()));
    let wal =
        std::env::temp_dir().join(format!("plomid-sqlcompat-{tag}-wal-{}", std::process::id()));
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

fn exec_err<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> String {
    match executor.execute(sql) {
        Ok(QueryResult::Rows { rows, .. }) => panic!("{sql} should fail, got rows={rows:?}"),
        Ok(other) => panic!("{sql} should fail, got {other:?}"),
        Err(e) => e.to_string(),
    }
}

/// Executes and returns the full project shape (column names, column types and
/// row values) of a `Rows` result. Intended for `... RETURNING ...` assertions
/// where the wire protocol requires `columns.len() == column_types.len() ==`
/// every row's width.
fn rows_of<E: plomid_txn::StorageEngine>(
    executor: &mut Executor<E>,
    sql: &str,
) -> (
    Vec<String>,
    Vec<Option<plomid_sql::ColumnType>>,
    Vec<Vec<Value>>,
) {
    match executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should execute: {e}"))
    {
        QueryResult::Rows {
            columns,
            column_types,
            rows,
        } => (columns, column_types, rows),
        other => panic!("{sql} should return rows, got {other:?}"),
    }
}

/// Asserts that every projected RETURNING row has exactly the same width as the
/// column metadata, and that the column name/type vectors agree on the width.
fn assert_returning_widths<E: plomid_txn::StorageEngine>(
    executor: &mut Executor<E>,
    sql: &str,
    expected_columns: Vec<&str>,
) -> (
    Vec<String>,
    Vec<Option<plomid_sql::ColumnType>>,
    Vec<Vec<Value>>,
) {
    let (columns, column_types, rows) = rows_of(executor, sql);
    assert_eq!(
        columns.len(),
        expected_columns.len(),
        "column name count for: {sql}"
    );
    for (i, name) in columns.iter().enumerate() {
        assert_eq!(
            *name, expected_columns[i],
            "column name at index {i} for: {sql}"
        );
    }
    assert_eq!(
        column_types.len(),
        expected_columns.len(),
        "column type count for: {sql}"
    );
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(
            row.len(),
            expected_columns.len(),
            "row {index} width for: {sql}"
        );
    }
    (columns, column_types, rows)
}

#[test]
fn qualified_keyword_catalog_function_is_queryable() {
    let (storage, wal) = unique_engine("keywords");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(
        &mut executor,
        "SELECT word FROM pg_catalog.pg_get_keywords() LIMIT 1;",
    );
    assert_eq!(rows, vec![vec![Value::Name("select".to_string())]]);
    let filtered = exec(
        &mut executor,
        "SELECT string_agg(word, ',') FROM pg_catalog.pg_get_keywords() WHERE word <> ALL ('{select}'::text[]);",
    );
    assert_eq!(filtered.len(), 1);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// Expression operators + three-valued NULL logic
// ---------------------------------------------------------------------------

#[test]
fn comparison_operators_and_three_valued_logic() {
    let (storage, wal) = unique_engine("expr");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    assert_eq!(
        exec(&mut executor, "SELECT 1 = 1;"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 1 <> 2;"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 1 != 1;"),
        vec![vec![Value::Bool(false)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 2 > 1;"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 2 >= 2;"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 1 < 2;"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 1 <= 0;"),
        vec![vec![Value::Bool(false)]]
    );

    // SQL NULL comparison yields NULL, never TRUE.
    assert_eq!(
        exec(&mut executor, "SELECT NULL = NULL;"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 1 = NULL;"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 1 <> NULL;"),
        vec![vec![Value::Null]]
    );

    // Arithmetic (integer arithmetic is promoted to bigint by the executor).
    assert_eq!(
        exec(&mut executor, "SELECT 2 + 3;"),
        vec![vec![Value::Int8(5)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 2 - 3;"),
        vec![vec![Value::Int8(-1)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 2 * 3;"),
        vec![vec![Value::Int8(6)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 7 / 2;"),
        vec![vec![Value::Int8(3)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 7 % 3;"),
        vec![vec![Value::Int8(1)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 0.95;"),
        vec![vec![Value::Float8(0.95)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 0.95 + 1;"),
        vec![vec![Value::Float8(1.95)]]
    );
    let div_zero = exec_err(&mut executor, "SELECT 1 / 0;");
    assert!(div_zero.contains("division by zero"), "{div_zero}");

    // Three-valued boolean logic.
    assert_eq!(
        exec(&mut executor, "SELECT NULL AND FALSE;"),
        vec![vec![Value::Bool(false)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NULL AND TRUE;"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NULL OR TRUE;"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NULL OR FALSE;"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NOT NULL;"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NOT TRUE;"),
        vec![vec![Value::Bool(false)]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
fn text_rows(rows: &[Vec<Value>]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| row.iter().map(Value::to_sql_text).collect())
        .collect()
}

// ---------------------------------------------------------------------------
// Predicates: IN, BETWEEN, LIKE
// ---------------------------------------------------------------------------

#[test]
fn in_between_like_predicates() {
    let (storage, wal) = unique_engine("predicates");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    assert_eq!(
        exec(&mut executor, "SELECT 2 IN (1, 2, 3);"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 4 IN (1, 2, 3);"),
        vec![vec![Value::Bool(false)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 4 NOT IN (1, 2, 3);"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NULL IN (1, 2);"),
        vec![vec![Value::Null]]
    );
    // IN (list) with a NULL list member behaves like SQL: no match -> NULL.
    assert_eq!(
        exec(&mut executor, "SELECT 3 IN (1, NULL);"),
        vec![vec![Value::Null]]
    );

    assert_eq!(
        exec(&mut executor, "SELECT 5 BETWEEN 1 AND 10;"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 5 NOT BETWEEN 1 AND 4;"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 5 BETWEEN NULL AND 10;"),
        vec![vec![Value::Null]]
    );

    assert_eq!(
        exec(&mut executor, "SELECT 'abc' LIKE 'a%';"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 'abc' LIKE 'b%';"),
        vec![vec![Value::Bool(false)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 'abc' NOT LIKE 'b%';"),
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 'abc' LIKE 'a_c';"),
        vec![vec![Value::Bool(true)]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
// ---------------------------------------------------------------------------
// Expressions: CASE, COALESCE, NULLIF
// ---------------------------------------------------------------------------

#[test]
fn conditional_expressions() {
    let (storage, wal) = unique_engine("conditional");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(
        &mut executor,
        "SELECT CASE WHEN 1 > 0 THEN 'yes' ELSE 'no' END;",
    );
    assert_eq!(rows, vec![vec![Value::Text("yes".into())]]);

    let rows = exec(
        &mut executor,
        "SELECT CASE 2 WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END;",
    );
    assert_eq!(rows, vec![vec![Value::Text("two".into())]]);

    let rows = exec(
        &mut executor,
        "SELECT CASE WHEN NULL THEN 'bad' ELSE 'good' END;",
    );
    assert_eq!(rows, vec![vec![Value::Text("good".into())]]);

    assert_eq!(
        exec(&mut executor, "SELECT COALESCE(NULL, 'x');"),
        vec![vec![Value::Text("x".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT COALESCE(NULL, NULL, 3);"),
        vec![vec![Value::Int4(3)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NULLIF(1, 1);"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NULLIF(1, 2);"),
        vec![vec![Value::Int4(1)]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn string_concatenation_and_dml_counts() {
    let (storage, wal) = unique_engine("concat-dml-counts");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    assert_eq!(
        exec(&mut executor, "SELECT 'hello' || ' world';"),
        vec![vec![Value::Text("hello world".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 'a' || 'b' || 'c';"),
        vec![vec![Value::Text("abc".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 'hello' || NULL;"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NULL || 'hello';"),
        vec![vec![Value::Null]]
    );

    exec_ok(
        &mut executor,
        "CREATE TABLE count_users (id INTEGER PRIMARY KEY, salary INTEGER);",
    );
    exec_ok(&mut executor, "INSERT INTO count_users VALUES (1, 55000);");
    assert_eq!(
        executor
            .execute("UPDATE count_users SET salary = salary + 1000 WHERE id = 1;")
            .unwrap(),
        QueryResult::Updated(1)
    );
    assert_eq!(
        executor
            .execute("UPDATE count_users SET salary = salary + 1000 WHERE id = 999;")
            .unwrap(),
        QueryResult::Updated(0)
    );
    assert_eq!(
        executor
            .execute("INSERT INTO count_users VALUES (2, 60000), (3, 61000);")
            .unwrap(),
        QueryResult::Inserted(2)
    );
    assert_eq!(
        executor
            .preview_dml_count("UPDATE count_users SET salary = salary + 1000 WHERE id = 1;")
            .unwrap(),
        1
    );
    assert_eq!(
        executor
            .preview_dml_count("UPDATE count_users SET salary = salary + 1000 WHERE id = 999;")
            .unwrap(),
        0
    );
    assert_eq!(
        executor
            .preview_dml_count("INSERT INTO count_users VALUES (2, 60000), (3, 61000);")
            .unwrap(),
        2
    );
    assert_eq!(
        executor
            .preview_dml_count("DELETE FROM count_users WHERE id = 999;")
            .unwrap(),
        0
    );
    assert_eq!(
        executor
            .execute("DELETE FROM count_users WHERE id = 3;")
            .unwrap(),
        QueryResult::Deleted(1)
    );

    assert_eq!(
        executor
            .execute("CREATE INDEX count_users_salary_idx ON count_users(salary);")
            .unwrap(),
        QueryResult::Created("CREATE INDEX".into())
    );
    assert!(executor
        .execute("CREATE INDEX count_users_salary_idx ON count_users(salary);")
        .is_err());
    assert_eq!(
        executor
            .execute("DROP INDEX count_users_salary_idx;")
            .unwrap(),
        QueryResult::Created("DROP INDEX".into())
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// String + numeric functions
// ---------------------------------------------------------------------------

#[test]
fn scalar_functions() {
    let (storage, wal) = unique_engine("scalar");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    assert_eq!(
        exec(&mut executor, "SELECT LOWER('AbC');"),
        vec![vec![Value::Text("abc".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT UPPER('AbC');"),
        vec![vec![Value::Text("ABC".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT LENGTH('hello');"),
        vec![vec![Value::Int4(5)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT TRIM('  x  ');"),
        vec![vec![Value::Text("x".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT LTRIM('xxhixx', 'x');"),
        vec![vec![Value::Text("hixx".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT RTRIM('xxhixx', 'x');"),
        vec![vec![Value::Text("xxhi".into())]]
    );
    let rows = exec(&mut executor, "SELECT SUBSTRING('hello' FROM 2 FOR 3);");
    assert_eq!(rows, vec![vec![Value::Text("ell".into())]]);
    let rows = exec(&mut executor, "SELECT SUBSTRING('hello' FROM 3);");
    assert_eq!(rows, vec![vec![Value::Text("llo".into())]]);
    assert_eq!(
        exec(&mut executor, "SELECT CONCAT('a', 'b', 'c');"),
        vec![vec![Value::Text("abc".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT CONCAT('a', NULL, 'c');"),
        vec![vec![Value::Text("ac".into())]]
    );

    assert_eq!(
        exec(&mut executor, "SELECT ABS(-3);"),
        vec![vec![Value::Int4(3)]]
    );
    let rows = exec(&mut executor, "SELECT ROUND(3.7);");
    assert_eq!(rows[0][0].to_sql_text(), "4");
    let rows = exec(&mut executor, "SELECT ROUND(3.14159, 2);");
    assert_eq!(rows[0][0].to_sql_text(), "3.14");
    assert_eq!(
        exec(&mut executor, "SELECT FLOOR(3.7);"),
        vec![vec![Value::Float8(3.0)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT CEIL(3.2);"),
        vec![vec![Value::Float8(4.0)]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
// ---------------------------------------------------------------------------
// POSITION(substring IN string) function
// ---------------------------------------------------------------------------

#[test]
fn position_function_is_supported() {
    let (storage, wal) = unique_engine("position");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // 1. Standard PostgreSQL syntax: POSITION(substring IN string).
    //    1-based result; 0 when absent; 1 for an empty substring.
    assert_eq!(
        exec(&mut executor, "SELECT POSITION('a' IN 'banana');"),
        vec![vec![Value::Int4(2)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT POSITION('n' IN 'banana');"),
        vec![vec![Value::Int4(3)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT POSITION('z' IN 'banana');"),
        vec![vec![Value::Int4(0)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT POSITION('' IN 'banana');"),
        vec![vec![Value::Int4(1)]]
    );

    // 2. NULL semantics: NULL when either argument is NULL.
    assert_eq!(
        exec(&mut executor, "SELECT POSITION(NULL IN 'banana');"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT POSITION('a' IN NULL);"),
        vec![vec![Value::Null]]
    );

    // 3. POSITION is an ordinary scalar expression: usable in arithmetic,
    //    CASE and COALESCE. (The engine's integer `+` promotes to Int8.)
    let rows = text_rows(&exec(
        &mut executor,
        "SELECT POSITION('a' IN 'banana') + 10;",
    ));
    assert_eq!(rows, vec![vec!["12".to_string()]]);
    // COALESCE falls through only when POSITION yields NULL.
    assert_eq!(
        exec(&mut executor, "SELECT COALESCE(POSITION('x' IN NULL), 99);"),
        vec![vec![Value::Int4(99)]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT CASE WHEN POSITION('a' IN 'banana') > 0 THEN 'yes' ELSE 'no' END;",
        ),
        vec![vec![Value::Text("yes".into())]]
    );
}

// ---------------------------------------------------------------------------
// POSITION(substring IN string) against table columns
// ---------------------------------------------------------------------------

#[test]
fn position_with_columns_and_contexts() {
    let (storage, wal) = unique_engine("position-cols");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE position_test (id INTEGER, value TEXT);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO position_test VALUES (1, 'banana');",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO position_test VALUES (2, 'apple');",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO position_test VALUES (3, 'orange');",
    );
    exec_ok(&mut executor, "INSERT INTO position_test VALUES (4, NULL);");

    // SELECT with column arguments.
    assert_eq!(
        exec(
            &mut executor,
            "SELECT id, POSITION('a' IN value) FROM position_test ORDER BY id;",
        ),
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Int4(1)],
            vec![Value::Int4(3), Value::Int4(3)],
            vec![Value::Int4(4), Value::Null],
        ]
    );

    // WHERE predicate using POSITION.
    assert_eq!(
        exec(
            &mut executor,
            "SELECT id, POSITION('a' IN value) FROM position_test \
             WHERE POSITION('a' IN value) > 0 ORDER BY id;",
        ),
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Int4(1)],
            vec![Value::Int4(3), Value::Int4(3)],
        ]
    );

    // GROUP BY over the POSITION expression. Grouped results are not
    // implicitly ordered, so sort before comparing.
    let mut rows = text_rows(&exec(
        &mut executor,
        "SELECT POSITION('a' IN value), COUNT(*) FROM position_test \
         WHERE value IS NOT NULL GROUP BY POSITION('a' IN value);",
    ));
    rows.sort();
    assert_eq!(
        rows,
        vec![
            vec!["1".to_string(), "1".to_string()],
            vec!["2".to_string(), "1".to_string()],
            vec!["3".to_string(), "1".to_string()],
        ]
    );

    // CTE referencing POSITION.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "WITH located AS (SELECT id, POSITION('a' IN value) AS p \
             FROM position_test WHERE value IS NOT NULL) \
             SELECT id, p FROM located WHERE p = 3 ORDER BY id;",
        )),
        vec![vec!["3".to_string(), "3".to_string()]]
    );

    // Subquery referencing POSITION.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT id, value FROM position_test \
             WHERE id IN (SELECT id FROM position_test WHERE POSITION('a' IN value) > 0) \
             ORDER BY id;",
        )),
        vec![
            vec!["1".to_string(), "banana".to_string()],
            vec!["2".to_string(), "apple".to_string()],
            vec!["3".to_string(), "orange".to_string()],
        ]
    );

    // JOIN condition using POSITION.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT a.id, a.value FROM position_test a \
             JOIN position_test b ON a.id = b.id \
             WHERE POSITION('a' IN a.value) > 0 AND a.id < 4 \
             ORDER BY a.id;",
        )),
        vec![
            vec!["1".to_string(), "banana".to_string()],
            vec!["2".to_string(), "apple".to_string()],
            vec!["3".to_string(), "orange".to_string()],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// POSITION(substring IN string) inside DML (INSERT VALUES, UPDATE SET)
// ---------------------------------------------------------------------------

#[test]
fn position_in_dml_statements() {
    let (storage, wal) = unique_engine("position-dml");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE position_test (id INTEGER, value TEXT);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO position_test VALUES (1, 'banana');",
    );
    exec_ok(&mut executor, "INSERT INTO position_test VALUES (4, NULL);");

    // INSERT ... VALUES accepts a POSITION expression.
    exec_ok(
        &mut executor,
        "INSERT INTO position_test (id, value) \
         VALUES (POSITION('b' IN 'banana') + 100, 'inserted');",
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT id FROM position_test WHERE value = 'inserted';",
        ),
        vec![vec![Value::Int4(101)]]
    );

    // UPDATE ... SET can assign a POSITION expression (cast to the TEXT col).
    exec_ok(
        &mut executor,
        "UPDATE position_test SET value = POSITION('n' IN 'banana')::TEXT WHERE id = 4;",
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT value FROM position_test WHERE id = 4;"
        ),
        vec![vec![Value::Text("3".into())]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// INSERT with expression VALUES + constraint enforcement
// ---------------------------------------------------------------------------

#[test]
fn insert_expression_values_and_constraints() {
    let (storage, wal) = unique_engine("insert");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE items (id INTEGER PRIMARY KEY, price NUMERIC(10,2) NOT NULL CHECK (price >= 0), name TEXT UNIQUE);");
    // Numeric literal 0.95 must insert cleanly.
    exec_ok(
        &mut executor,
        "INSERT INTO items (id, price, name) VALUES (1, 0.95, 'a');",
    );
    let err = exec_err(&mut executor, "INSERT INTO items VALUES (2, -0.95, 'b');");
    assert!(err.contains("check"), "{err}");
    exec_ok(&mut executor, "INSERT INTO items VALUES (3, 5.5, 'c');");

    let rows = exec(&mut executor, "SELECT price FROM items WHERE id = 1;");
    assert_eq!(rows[0][0].to_sql_text(), "0.95");

    // PRIMARY KEY duplicate
    let err = exec_err(&mut executor, "INSERT INTO items VALUES (1, 1.0, 'dup');");
    assert!(err.contains("duplicate"), "{err}");

    // NOT NULL violation
    let err = exec_err(
        &mut executor,
        "INSERT INTO items (id, name) VALUES (4, 'd');",
    );
    assert!(err.contains("null"), "{err}");

    // CHECK violation
    let err = exec_err(&mut executor, "INSERT INTO items VALUES (4, -1.0, 'd');");
    assert!(err.contains("check"), "{err}");

    // UNIQUE violation
    exec_ok(&mut executor, "INSERT INTO items VALUES (4, 1.0, 'b');");
    let err = exec_err(&mut executor, "INSERT INTO items VALUES (5, 1.0, 'b');");
    assert!(err.contains("duplicate"), "{err}");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// Aggregation, GROUP BY, HAVING
// ---------------------------------------------------------------------------

#[test]
fn aggregation_group_by_having() {
    let (storage, wal) = unique_engine("agg");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE sales (id INTEGER, region TEXT, amount INTEGER);",
    );
    for (id, region, amount) in [
        (1, "east", 10),
        (2, "east", 20),
        (3, "west", 30),
        (4, "west", 5),
        (5, "north", 40),
    ] {
        exec_ok(
            &mut executor,
            &format!("INSERT INTO sales VALUES ({id}, '{region}', {amount});"),
        );
    }

    let rows = exec(
        &mut executor,
        "SELECT COUNT(*), SUM(amount), AVG(amount), MIN(amount), MAX(amount) FROM sales;",
    );
    assert_eq!(text_rows(&rows), vec![vec!["5", "105", "21", "5", "40"]]);

    let rows = exec(
        &mut executor,
        "SELECT region, COUNT(*), SUM(amount) FROM sales GROUP BY region ORDER BY region;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["east", "2", "30"],
            vec!["north", "1", "40"],
            vec!["west", "2", "35"],
        ]
    );

    // HAVING filters groups.
    let rows = exec(
        &mut executor,
        "SELECT region, SUM(amount) FROM sales GROUP BY region HAVING SUM(amount) > 30 ORDER BY region;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![vec!["north", "40"], vec!["west", "35"]]
    );

    // Aggregate without GROUP BY over a single group.
    let rows = exec(
        &mut executor,
        "SELECT COUNT(*) FROM sales HAVING COUNT(*) > 3;",
    );
    assert_eq!(rows, vec![vec![Value::Int8(5)]]);
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT region, COUNT(*) FILTER (WHERE amount > 10), COUNT(DISTINCT amount) FROM sales GROUP BY region HAVING COUNT(*) FILTER (WHERE amount > 10) > 0 ORDER BY region;"
        )),
        vec![
            vec!["east", "1", "2"],
            vec!["north", "1", "1"],
            vec!["west", "1", "2"]
        ]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT region, STRING_AGG(amount::text, '|') FROM sales GROUP BY region ORDER BY region;"
        )),
        vec![
            vec!["east", "10|20"],
            vec!["north", "40"],
            vec!["west", "30|5"]
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// Set operations, CTEs, subqueries, EXPLAIN
// ---------------------------------------------------------------------------

#[test]
fn set_operations_ctes_subqueries() {
    let (storage, wal) = unique_engine("setops");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Set operations.
    let rows = exec(&mut executor, "SELECT 1 UNION SELECT 2;");
    assert_eq!(rows.len(), 2);
    let rows = exec(&mut executor, "SELECT 1 UNION ALL SELECT 1;");
    assert_eq!(rows.len(), 2);
    let rows = exec(&mut executor, "SELECT 1 INTERSECT SELECT 1;");
    assert_eq!(rows, vec![vec![Value::Int4(1)]]);
    let rows = exec(&mut executor, "SELECT 1 EXCEPT SELECT 2;");
    assert_eq!(rows, vec![vec![Value::Int4(1)]]);
    let rows = exec(&mut executor, "SELECT 1 EXCEPT SELECT 1;");
    assert_eq!(rows.len(), 0);

    // CTEs.
    let rows = exec(&mut executor, "WITH t AS (SELECT 1 AS x) SELECT x FROM t;");
    assert_eq!(rows, vec![vec![Value::Int4(1)]]);
    let rows = exec(
        &mut executor,
        "WITH a AS (SELECT 1 AS x), b AS (SELECT x + 1 AS y FROM a) SELECT y FROM b;",
    );
    assert_eq!(rows, vec![vec![Value::Int8(2)]]);
    // CTE used inside a UNION.
    let rows = exec(
        &mut executor,
        "WITH t AS (SELECT 1 AS x) SELECT x FROM t UNION SELECT 2;",
    );
    assert_eq!(rows.len(), 2);

    // Scalar subquery.
    let rows = exec(&mut executor, "SELECT (SELECT 42);");
    assert_eq!(rows, vec![vec![Value::Int4(42)]]);
    // EXISTS subquery.
    let rows = exec(&mut executor, "SELECT EXISTS (SELECT 1);");
    assert_eq!(rows, vec![vec![Value::Bool(true)]]);
    let rows = exec(&mut executor, "SELECT EXISTS (SELECT 1 WHERE 1 = 0);");
    assert_eq!(rows, vec![vec![Value::Bool(false)]]);
    // IN (SELECT ...).
    let rows = exec(&mut executor, "SELECT 2 IN (SELECT 2);");
    assert_eq!(rows, vec![vec![Value::Bool(true)]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn explain_returns_plan_rows() {
    let (storage, wal) = unique_engine("explain");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(&mut executor, "EXPLAIN SELECT 1;");
    assert_eq!(
        rows[0][0],
        Value::Text("Seq Scan (cost=0.00..0.00 rows=0 width=0)".into())
    );
    let rows = exec(&mut executor, "EXPLAIN ANALYZE SELECT 1;");
    assert_eq!(rows.len(), 2);
    assert!(rows[1][0].to_sql_text().contains("Execution Time"));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// JOINs, aliases, qualified/ambiguous columns, window functions
// ---------------------------------------------------------------------------

#[test]
fn joins_aliases_and_qualified_columns() {
    let (storage, wal) = unique_engine("joins");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE users (id INTEGER, name TEXT);");
    exec_ok(
        &mut executor,
        "CREATE TABLE orders (id INTEGER, user_id INTEGER, total INTEGER);",
    );
    for (id, name) in [(1, "alice"), (2, "bob"), (3, "carol")] {
        exec_ok(
            &mut executor,
            &format!("INSERT INTO users VALUES ({id}, '{name}');"),
        );
    }
    for (id, user_id, total) in [(10, 1, 100), (11, 2, 50), (12, 2, 75)] {
        exec_ok(
            &mut executor,
            &format!("INSERT INTO orders VALUES ({id}, {user_id}, {total});"),
        );
    }

    // INNER JOIN with aliases + qualified columns.
    let rows = exec(
        &mut executor,
        "SELECT u.name, o.total FROM users u INNER JOIN orders o ON u.id = o.user_id ORDER BY o.total;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![vec!["bob", "50"], vec!["bob", "75"], vec!["alice", "100"],]
    );

    // LEFT JOIN.
    let rows = exec(
        &mut executor,
        "SELECT u.name, o.total FROM users u LEFT JOIN orders o ON u.id = o.user_id AND o.total > 60 ORDER BY u.id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["alice", "100"],
            vec!["bob", "75"],
            vec!["carol", "NULL"],
        ]
    );

    // RIGHT JOIN.
    let rows = exec(
        &mut executor,
        "SELECT u.name, o.total FROM users u RIGHT JOIN orders o ON u.id = o.user_id WHERE o.total = 50;",
    );
    assert_eq!(text_rows(&rows), vec![vec!["bob", "50"]]);

    // CROSS JOIN.
    let rows = exec(
        &mut executor,
        "SELECT count(*) FROM users u CROSS JOIN orders o;",
    );
    assert_eq!(rows[0][0], Value::Int8(9));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
#[test]
fn ambiguous_columns_are_rejected() {
    let (storage, wal) = unique_engine("ambiguous");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE a (id INTEGER, shared TEXT);");
    exec_ok(&mut executor, "CREATE TABLE b (id INTEGER, shared TEXT);");
    exec_ok(&mut executor, "INSERT INTO a VALUES (1, 'a');");
    exec_ok(&mut executor, "INSERT INTO b VALUES (1, 'b');");

    // Bare `shared` is ambiguous across both tables.
    let err = exec_err(&mut executor, "SELECT shared FROM a JOIN b ON a.id = b.id;");
    assert!(err.contains("ambiguous"), "{err}");
    // Qualified references resolve.
    let rows = exec(
        &mut executor,
        "SELECT a.shared, b.shared FROM a JOIN b ON a.id = b.id;",
    );
    assert_eq!(text_rows(&rows), vec![vec!["a", "b"]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn window_functions_over_tables() {
    let (storage, wal) = unique_engine("window");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE scores (id INTEGER, region TEXT, points INTEGER);",
    );
    for (id, region, points) in [
        (1, "east", 10),
        (2, "east", 20),
        (3, "west", 5),
        (4, "west", 30),
    ] {
        exec_ok(
            &mut executor,
            &format!("INSERT INTO scores VALUES ({id}, '{region}', {points});"),
        );
    }

    // ROW_NUMBER over the whole set ordered by points.
    let rows = exec(
        &mut executor,
        "SELECT id, ROW_NUMBER() OVER (ORDER BY points) FROM scores ORDER BY id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "2"],
            vec!["2", "3"],
            vec!["3", "1"],
            vec!["4", "4"],
        ]
    );

    // RANK with ties and a PARTITION BY.
    let rows = exec(
        &mut executor,
        "SELECT id, RANK() OVER (PARTITION BY region ORDER BY points DESC) FROM scores ORDER BY id;",
    );
    assert_eq!(
        text_rows(&rows),
        vec![
            vec!["1", "2"],
            vec!["2", "1"],
            vec!["3", "2"],
            vec!["4", "1"],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
// ---------------------------------------------------------------------------
// JSON/JSONB, arrays, casts, information_schema, views
// ---------------------------------------------------------------------------

#[test]
fn json_arrays_and_casts() {
    let (storage, wal) = unique_engine("json");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // JSON accessors (-> and ->>) with nesting.
    let rows = exec(
        &mut executor,
        "SELECT '{\"a\": {\"b\": 1}}'::jsonb -> 'a' ->> 'b';",
    );
    assert_eq!(rows, vec![vec![Value::Text("1".into())]]);
    let rows = exec(
        &mut executor,
        "SELECT '{\"a\": [10, 20]}'::jsonb -> 'a' ->> 1;",
    );
    assert_eq!(rows, vec![vec![Value::Text("20".into())]]);
    let rows = exec(&mut executor, "SELECT '{\"x\": 5}'::jsonb -> 'missing';");
    assert_eq!(rows, vec![vec![Value::Null]]);

    // Arrays: literal, indexing.
    let rows = exec(&mut executor, "SELECT ARRAY[1,2,3];");
    assert!(matches!(rows[0][0], Value::Array { .. }));
    let rows = exec(&mut executor, "SELECT ARRAY[10,20,30][2];");
    assert_eq!(rows, vec![vec![Value::Int4(20)]]);
    let rows = exec(&mut executor, "SELECT ARRAY[10,20,30][5];");
    assert_eq!(rows, vec![vec![Value::Null]]);

    // Type casts.
    let rows = exec(&mut executor, "SELECT '12'::integer;");
    assert_eq!(rows, vec![vec![Value::Int4(12)]]);
    let rows = exec(&mut executor, "SELECT 1::boolean;");
    assert!(matches!(rows[0][0], Value::Bool(_)));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn extended_arrays_and_json_functions() {
    let (storage, wal) = unique_engine("extended-json-arrays");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    assert_eq!(
        exec(
            &mut executor,
            "SELECT CONCAT_WS('-', 'a', NULL, 'b'), CONCAT_WS(NULL, 'a', 'b');"
        ),
        vec![vec![Value::Text("a-b".into()), Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT CARDINALITY(ARRAY[1,2,3]), ARRAY_LENGTH(ARRAY[1,2,3], 1), ARRAY_POSITION(ARRAY[1,2,3], 2);"),
        vec![vec![Value::Int4(3), Value::Int4(3), Value::Int4(2)]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT ARRAY_TO_STRING(ARRAY[1, NULL, 3], ','), STRING_TO_ARRAY('a,,b', ',');"
        )),
        vec![vec!["1,3", "{a,\"\",b}"]]
    );

    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT '{\"a\": {\"b\": [10,20]}}'::jsonb #>> ARRAY['a','b','1'];"
        )),
        vec![vec!["20"]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT '{\"a\":1,\"b\":2}'::jsonb @> '{\"a\":1}'::jsonb, '{\"a\":1}'::jsonb ? 'a';"
        ),
        vec![vec![Value::Bool(true), Value::Bool(true)]]
    );
    assert_eq!(
        text_rows(&exec(&mut executor, "SELECT JSONB_TYPEOF('{\"a\":1}'::jsonb), JSONB_BUILD_OBJECT('a', 1), JSONB_EXTRACT_PATH_TEXT('{\"a\": {\"b\": 2}}'::jsonb, 'a', 'b');")),
        vec![vec!["object", "{\"a\":1}", "2"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT JSONB_BUILD_OBJECT('text', 'value');"
        )),
        vec![vec!["{\"text\":\"value\"}"]]
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE array_values (id INTEGER PRIMARY KEY, vals INTEGER[]);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO array_values VALUES (1, ARRAY[10,20,30]);",
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT vals[2], ARRAY_LENGTH(vals, 1) FROM array_values;"
        ),
        vec![vec![Value::Int4(20), Value::Int4(3)]]
    );
    let array_types = text_rows(&exec(
        &mut executor,
        "SELECT data_type FROM information_schema.columns WHERE table_name = 'array_values' AND column_name = 'vals';"
    ));
    assert!(array_types.iter().any(|row| row == &vec!["int4[]"]));
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT ARRAY_TO_STRING(ARRAY_APPEND(ARRAY[1,2], 3), ','), ARRAY_TO_STRING(ARRAY_PREPEND(0, ARRAY[1,2]), ',');"
        )),
        vec![vec!["1,2,3", "0,1,2"]]
    );
    assert_eq!(
        text_rows(&exec(&mut executor, "SELECT unnest(ARRAY[4,5,6]);")),
        vec![vec!["4"], vec!["5"], vec!["6"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT unnest(vals) FROM array_values;"
        )),
        vec![vec!["10"], vec!["20"], vec!["30"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT jsonb_array_elements('[1,2,3]'::jsonb);"
        )),
        vec![vec!["1"], vec!["2"], vec!["3"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT JSONB_EXTRACT_PATH_TEXT('{\"a\": {\"b\": [10,20]}}'::jsonb, 'a', 'b', '1'), JSONB_EXTRACT_PATH('{\"a\": {\"b\": 2}}'::jsonb, 'a', 'b');"
        )),
        vec![vec!["20", "2"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT JSONB_SET('{\"a\": {\"b\": 1}}'::jsonb, ARRAY['a','b'], '2'::jsonb), JSONB_SET('{}'::jsonb, ARRAY['a','b'], '3'::jsonb);"
        )),
        vec![vec!["{\"a\":{\"b\":2}}", "{\"a\":{\"b\":3}}"]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT '[1,2]'::jsonb @> '[1]'::jsonb, '[1]'::jsonb <@ '[1,2]'::jsonb, '{\"a\":1,\"b\":2}'::jsonb ?| ARRAY['x','b'], '{\"a\":1,\"b\":2}'::jsonb ?& ARRAY['a','b'];"
        ),
        vec![vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true)
        ]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT PG_TYPEOF(ARRAY[1,2]);"),
        vec![vec![Value::Text("int4[]".into())]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT 2 = ANY(ARRAY[1,2,3]), 5 = ANY(ARRAY[1,2,3]), 1 <> ALL(ARRAY[2,3,4]);"
        ),
        vec![vec![
            Value::Bool(true),
            Value::Bool(false),
            Value::Bool(true)
        ]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
#[test]
fn json_set_lax_null_treatments_and_table_functions() {
    let (storage, wal) = unique_engine("json-set-lax");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // jsonb_set_lax: a SQL NULL replacement honours null_value_treatment.
    // `use_json_null` (the default) writes JSON null, `delete_key` removes the
    // key, `return_target` leaves the document unchanged, and an unknown
    // treatment falls back to `use_json_null` (PostgreSQL semantics).
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT jsonb_set_lax('{\"a\":1}'::jsonb, '{a}', NULL, true, 'use_json_null'), \
jsonb_set_lax('{\"a\":1}'::jsonb, '{a}', NULL, true, 'delete_key'), \
jsonb_set_lax('{\"a\":1}'::jsonb, '{a}', NULL, true, 'return_target');",
        )),
        vec![vec!["{\"a\":null}", "{}", "{\"a\":1}"]]
    );

    // raise_exception rejects a NULL new value even though target is non-NULL.
    let raise_err = exec_err(
        &mut executor,
        "SELECT jsonb_set_lax('{\"a\":1}'::jsonb, '{a}', NULL, true, 'raise_exception');",
    );
    assert!(
        raise_err.contains("raise_exception") && raise_err.contains("NULL"),
        "{raise_err}"
    );

    // Valid JSONB input: plain strings, escaped quotes, and Unicode all round
    // trip; malformed JSON is rejected with a single clean message.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT '\"hello world\"'::jsonb, '\"quote: \\\"hello\\\"\"'::jsonb, '\"🎉\"'::jsonb;",
        )),
        vec![vec![
            "\"hello world\"",
            "\"quote: \\\"hello\\\"\"",
            "\"🎉\""
        ]]
    );
    let malformed = exec_err(&mut executor, "SELECT '{\"a\": bad}'::jsonb;");
    // A single clean rejection: the underlying JSON parse failure must not be
    // double-wrapped by the cast layer.
    assert!(
        malformed.contains("invalid input syntax for type jsonb")
            && !malformed.contains(
                "invalid input syntax for type jsonb: invalid input syntax for type jsonb"
            ),
        "{malformed}"
    );

    // A relation alias and a never-lateral table function, plus the
    // LATERAL form referencing a column of an earlier FROM item.
    exec_ok(
        &mut executor,
        "CREATE TABLE json_tags (id INTEGER, payload JSONB);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO json_tags VALUES \
(1, '{\"tags\":[\"a\",\"b\"]}'::jsonb), \
(2, '{\"tags\":[\"c\"]}'::jsonb), \
(3, '{\"tags\":[]}'::jsonb);",
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT d.id, tag FROM json_tags d \
CROSS JOIN LATERAL jsonb_array_elements_text(d.payload -> 'tags') AS tag \
ORDER BY d.id, tag;",
        )),
        vec![vec!["1", "a"], vec!["1", "b"], vec!["2", "c"]]
    );
    // jsonb_array_elements on an empty array yields zero rows (also verified
    // against a scalar-subquery argument below).
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT d.id, element FROM json_tags d \
CROSS JOIN LATERAL jsonb_array_elements(d.payload -> 'tags') AS element \
ORDER BY d.id;",
        )),
        vec![vec!["1", "\"a\""], vec!["1", "\"b\""], vec!["2", "\"c\""]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT count(*) FROM jsonb_array_elements('[]'::jsonb);"
        )),
        vec![vec!["0"]]
    );
    // SET-returning function in FROM; SQL NULL input yields zero rows.
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT count(*) FROM jsonb_array_elements((SELECT jsonb_agg(i) FROM \
generate_series(1,3) g(i)));",
        )),
        vec![vec!["3"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT count(*) FROM jsonb_array_elements(NULL::jsonb);",
        )),
        vec![vec!["0"]]
    );

    exec_ok(&mut executor, "DROP TABLE json_tags;");
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn regular_expression_operators_and_functions() {
    let (storage, wal) = unique_engine("regex");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    assert_eq!(
        exec(
            &mut executor,
            "SELECT 'Abc' ~ '^A', 'Abc' ~* '^a', 'Abc' !~ '^z', 'Abc' !~* '^z';"
        ),
        vec![vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true)
        ]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT 'abc' SIMILAR TO 'a%', 'abc' NOT SIMILAR TO 'x%';"
        ),
        vec![vec![Value::Bool(true), Value::Bool(true)]]
    );
    assert_eq!(
        text_rows(&exec(&mut executor, "SELECT REGEXP_REPLACE('abc123abc', '[0-9]+', 'X', 'g'), REGEXP_SUBSTR('abc123', '[0-9]+'), REGEXP_MATCH('abc123', '([a-z]+)([0-9]+)');")),
        vec![vec!["abcXabc", "123", "{abc,123}"]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT NULL ~ 'x';"),
        vec![vec![Value::Null]]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn views_and_information_schema() {
    let (storage, wal) = unique_engine("views");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE base (id INTEGER, val TEXT);");
    exec_ok(&mut executor, "INSERT INTO base VALUES (1, 'one');");
    exec_ok(&mut executor, "CREATE VIEW v AS SELECT id, val FROM base;");
    let rows = exec(&mut executor, "SELECT val FROM v;");
    assert_eq!(rows, vec![vec![Value::Text("one".into())]]);

    // information_schema.tables and .columns
    let rows = exec(
        &mut executor,
        "SELECT table_name FROM information_schema.tables WHERE table_name = 'base';",
    );
    assert!(!rows.is_empty());
    assert_eq!(rows[0][0], Value::Text("base".into()));
    let rows = exec(
        &mut executor,
        "SELECT column_name, is_nullable FROM information_schema.columns WHERE table_name = 'base' ORDER BY ordinal_position;",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("id".into()));

    exec_ok(&mut executor, "DROP VIEW v;");
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// ALTER TABLE
// ---------------------------------------------------------------------------

#[test]
fn alter_table_operations() {
    let (storage, wal) = unique_engine("alter");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER, name TEXT);");
    exec_ok(&mut executor, "INSERT INTO t VALUES (1, 'one');");

    // ADD COLUMN
    exec_ok(&mut executor, "ALTER TABLE t ADD COLUMN extra INTEGER;");
    let rows = exec(&mut executor, "SELECT * FROM t;");
    assert_eq!(text_rows(&rows), vec![vec!["1", "one", "NULL"]]);

    // RENAME COLUMN
    exec_ok(&mut executor, "ALTER TABLE t RENAME COLUMN name TO label;");
    let rows = exec(&mut executor, "SELECT label FROM t;");
    assert_eq!(rows, vec![vec![Value::Text("one".into())]]);

    // RENAME TO
    exec_ok(&mut executor, "ALTER TABLE t RENAME TO t2;");
    let rows = exec(&mut executor, "SELECT * FROM t2;");
    assert_eq!(text_rows(&rows), vec![vec!["1", "one", "NULL"]]);

    // DROP COLUMN
    exec_ok(&mut executor, "ALTER TABLE t2 DROP COLUMN extra;");
    let rows = exec(&mut executor, "SELECT * FROM t2;");
    assert_eq!(text_rows(&rows), vec![vec!["1", "one"]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn postgresql_compatibility_regressions() {
    let (storage, wal) = unique_engine("postgresql-regressions");
    let engine = PlomidStorageEngine::create(&storage, &wal, 64).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(&mut executor, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT UNIQUE, age INTEGER, active BOOLEAN DEFAULT TRUE, salary NUMERIC(10,2), created_at TIMESTAMP);");
    exec_ok(&mut executor, "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, amount NUMERIC(10,2), status TEXT, created_at TIMESTAMP);");
    exec_ok(&mut executor, "INSERT INTO users (id, name, email, age, active, salary, created_at) VALUES (1, 'Alice', 'alice@example.com', 30, TRUE, 50000.00, '2025-01-01 10:00:00'), (2, 'Bob', 'bob@example.com', 25, TRUE, 45000.00, '2025-02-01 10:00:00'), (3, 'Charlie', 'charlie@example.com', 40, FALSE, 70000.00, '2025-03-01 10:00:00'), (4, 'David', 'david@example.com', 35, TRUE, 60000.00, '2025-04-01 10:00:00');");
    exec_ok(&mut executor, "INSERT INTO orders (id, user_id, amount, status, created_at) VALUES (101, 1, 100.50, 'paid', '2025-05-01'), (102, 1, 250.00, 'paid', '2025-05-02'), (103, 2, 75.25, 'pending', '2025-05-03'), (104, 3, 500.00, 'paid', '2025-05-04'), (105, 4, 150.00, 'cancelled', '2025-05-05');");

    assert_eq!(
        exec(&mut executor, "SELECT 2 ^ 3;"),
        vec![vec![Value::Float8(8.0)]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT REPLACE('hello world', 'world', 'postgres');"
        ),
        vec![vec![Value::Text("hello postgres".into())]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT LEFT('hello', 2), RIGHT('hello', 2), REVERSE('hello');"
        ),
        vec![vec![
            Value::Text("he".into()),
            Value::Text("lo".into()),
            Value::Text("olleh".into())
        ]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT POWER(2, 3), SQRT(16), MOD(10, 3);"),
        vec![vec![
            Value::Float8(8.0),
            Value::Float8(4.0),
            Value::Float8(1.0)
        ]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT NULL + 1, NULL * 5, 10 + NULL, NULL = NULL, NULL IS NULL;"
        ),
        vec![vec![
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Bool(true)
        ]]
    );
    exec_ok(
        &mut executor,
        "UPDATE users SET salary = salary + 5000 WHERE id = 1;",
    );
    assert_eq!(
        exec(&mut executor, "SELECT salary FROM users WHERE id = 1;"),
        vec![vec![Value::Numeric(
            plomid_types::Numeric::parse("55000.00").unwrap()
        )]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT COUNT(DISTINCT active) FROM users;"),
        vec![vec![Value::Int8(2)]]
    );

    let groups = exec(&mut executor, "SELECT CASE WHEN age < 30 THEN 'young' WHEN age < 40 THEN 'middle' ELSE 'senior' END AS age_group, COUNT(*) FROM users GROUP BY CASE WHEN age < 30 THEN 'young' WHEN age < 40 THEN 'middle' ELSE 'senior' END ORDER BY age_group;");
    assert_eq!(
        text_rows(&groups),
        vec![vec!["middle", "2"], vec!["senior", "1"], vec!["young", "1"]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT COUNT(*) FILTER (WHERE active = TRUE) FROM users;"
        ),
        vec![vec![Value::Int8(3)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT STRING_AGG(name, ', ') FROM users;"),
        vec![vec![Value::Text("Alice, Bob, Charlie, David".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT ARRAY_LENGTH(ARRAY[1,2,3], 1);"),
        vec![vec![Value::Int4(3)]]
    );
    // PG catalog introspection: current_schemas(bool) returns name[] and
    // works inside ANY(...) against pg_namespace (JDBC / psql probing).
    assert_eq!(
        exec(&mut executor, "SELECT current_schemas(true);"),
        vec![vec![Value::Array {
            element_oid: plomid_types::TypeOid::NAME,
            elements: vec![Value::Name("public".into())],
        }]]
    );
    let catalog_probe = exec(
        &mut executor,
        "SELECT n.nspname = ANY(current_schemas(true)), n.nspname, t.typname FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON t.typnamespace = n.oid WHERE t.oid = 22;",
    );
    assert_eq!(catalog_probe.len(), 1);
    assert_eq!(
        catalog_probe,
        vec![vec![
            Value::Bool(false),
            Value::Text("pg_catalog".into()),
            Value::Text("int2vector".into()),
        ]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT ARRAY_AGG(name) FROM users;"),
        vec![vec![Value::Array {
            element_oid: plomid_types::TypeOid::TEXT,
            elements: vec![
                Value::Text("Alice".into()),
                Value::Text("Bob".into()),
                Value::Text("Charlie".into()),
                Value::Text("David".into())
            ]
        }]]
    );

    assert_eq!(
        exec(
            &mut executor,
            "SELECT CAST('123' AS INTEGER), '123'::INTEGER, CAST(123 AS TEXT), 123::TEXT;"
        ),
        vec![vec![
            Value::Int4(123),
            Value::Int4(123),
            Value::Text("123".into()),
            Value::Text("123".into())
        ]]
    );
    let _ = exec(&mut executor, "SELECT CURRENT_DATE, CURRENT_TIME, CURRENT_TIMESTAMP, DATE '2025-01-01', TIMESTAMP '2025-01-01 12:30:00', EXTRACT(YEAR FROM TIMESTAMP '2025-05-10 12:30:00');");
    assert_eq!(
        exec(
            &mut executor,
            "SELECT GREATEST(10, 20, 5), LEAST(10, 20, 5);"
        ),
        vec![vec![Value::Int4(20), Value::Int4(5)]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT EXTRACT(YEAR FROM TIMESTAMP '2025-05-10 12:30:00'), DATE_PART('hour', TIMESTAMP '2025-05-10 12:30:00'), DATE_TRUNC('day', TIMESTAMP '2025-05-10 12:30:00'), DATE '2025-01-01' + INTERVAL '1 day', TIMESTAMP '2025-01-01' - INTERVAL '2 hours', MAKE_DATE(2025, 5, 10), MAKE_TIMESTAMP(2025, 5, 10, 12, 30, 0);"
        )),
        vec![vec![
            "2025",
            "12",
            "2025-05-10 00:00:00",
            "2025-01-02 00:00:00",
            "2024-12-31 22:00:00",
            "2025-05-10",
            "2025-05-10 12:30:00"
        ]]
    );

    for join_kind in ["INNER", "LEFT", "RIGHT", "FULL"] {
        let rows = exec(&mut executor, &format!("SELECT u.name, o.amount FROM users u {join_kind} JOIN orders o ON u.id = o.user_id;"));
        assert!(!rows.is_empty(), "{join_kind} join returned no rows");
    }
    assert_eq!(
        exec(
            &mut executor,
            "SELECT u.name, o.status FROM users u CROSS JOIN orders o LIMIT 10;"
        )
        .len(),
        10
    );
    let cte = exec(&mut executor, "WITH user_totals AS (SELECT user_id, SUM(amount) AS total FROM orders GROUP BY user_id), rich_users AS (SELECT id, name FROM users WHERE salary > 50000) SELECT r.name, u.total FROM rich_users r JOIN user_totals u ON r.id = u.user_id ORDER BY r.name;");
    assert_eq!(
        text_rows(&cte),
        vec![
            vec!["Alice", "350.5"],
            vec!["Charlie", "500"],
            vec!["David", "150"]
        ]
    );
    // DBeaver catalog probes: pg_policies view, pg_language row shape, and
    // the pg_depend dependency probe (all must parse + resolve columns, and
    // return zero rows rather than "does not exist").
    let policies = exec(
        &mut executor,
        "select * from pg_catalog.pg_policies where schemaname='public' and tablename='load_test_users';",
    );
    assert!(policies.is_empty());
    let languages = exec(
        &mut executor,
        "SELECT lanname FROM pg_language WHERE lanname = 'plpgsql';",
    );
    assert!(languages.is_empty());
    let dependencies_mini = exec(
        &mut executor,
        "SELECT DISTINCT dep.deptype, dep.classid, dep.objid, cl.relkind, attr.attname, pg_get_expr(ad.adbin, ad.adrelid) adefval FROM pg_depend dep LEFT JOIN pg_class cl ON dep.objid=cl.oid LEFT JOIN pg_namespace nsc ON cl.relnamespace=nsc.oid LEFT JOIN pg_language la ON dep.objid=la.oid LEFT JOIN pg_namespace ns ON dep.objid=ns.oid LEFT JOIN pg_attrdef ad ON ad.oid=dep.objid LEFT JOIN pg_attribute attr ON attr.attrelid=ad.adrelid and attr.attnum=ad.adnum WHERE dep.refobjid=5 ORDER BY dep.deptype;",
    );
    assert!(dependencies_mini.is_empty());
    // Full DBeaver pg_depend probe (all joins incl. pg_proc / pg_trigger /
    // pg_type / pg_constraint / pg_rewrite / pg_namespace aliases).
    let dependencies_full = exec(
        &mut executor,
        "SELECT DISTINCT dep.deptype, dep.classid, dep.objid, cl.relkind, attr.attname, pg_get_expr(ad.adbin, ad.adrelid) adefval, CASE WHEN cl.relkind IS NOT NULL THEN cl.relkind WHEN tg.oid IS NOT NULL THEN 'T' WHEN ty.oid IS NOT NULL THEN 'y' WHEN ns.oid IS NOT NULL THEN 'n' WHEN la.oid IS NOT NULL THEN 'l' WHEN rw.oid IS NOT NULL THEN 'R' WHEN co.oid IS NOT NULL THEN 'C' WHEN ad.oid IS NOT NULL THEN 'A' ELSE '' END AS type, COALESCE(coc.relname, clrw.relname, tgr.relname) AS ownertable, COALESCE(cl.relname, co.conname, tg.tgname, ty.typname, la.lanname, rw.rulename, ns.nspname) AS refname, COALESCE(nsc.nspname, nso.nspname, nsp.nspname, nst.nspname, nsrw.nspname, tgrn.nspname) AS nspname FROM pg_depend dep LEFT JOIN pg_class cl ON dep.objid=cl.oid LEFT JOIN pg_attribute att ON dep.objid=att.attrelid AND dep.objsubid=att.attnum LEFT JOIN pg_namespace nsc ON cl.relnamespace=nsc.oid LEFT JOIN pg_proc pr ON dep.objid=pr.oid LEFT JOIN pg_namespace nsp ON pr.pronamespace=nsp.oid LEFT JOIN pg_trigger tg ON dep.objid=tg.oid LEFT JOIN pg_class tgr ON tg.tgrelid=tgr.oid LEFT JOIN pg_namespace tgrn ON tgr.relnamespace=tgrn.oid LEFT JOIN pg_type ty ON dep.objid=ty.oid LEFT JOIN pg_namespace nst ON ty.typnamespace=nst.oid LEFT JOIN pg_constraint co ON dep.objid=co.oid LEFT JOIN pg_class coc ON co.conrelid=coc.oid LEFT JOIN pg_namespace nso ON co.connamespace=nso.oid LEFT JOIN pg_rewrite rw ON dep.objid=rw.oid LEFT JOIN pg_class clrw ON clrw.oid=rw.ev_class LEFT JOIN pg_namespace nsrw ON clrw.relnamespace=nsrw.oid LEFT JOIN pg_language la ON dep.objid=la.oid LEFT JOIN pg_namespace ns ON dep.objid=ns.oid LEFT JOIN pg_attrdef ad ON ad.oid=dep.objid LEFT JOIN pg_attribute attr ON attr.attrelid=ad.adrelid and attr.attnum=ad.adnum WHERE dep.refobjid=5 ORDER BY type;",
    );
    assert!(dependencies_full.is_empty());
    // regproc text input: DBeaver probes typinput='pg_catalog.array_in'::regproc
    // to detect array types; the cast must not error.
    let regproc_probe = exec(
        &mut executor,
        "SELECT typinput='pg_catalog.array_in'::regproc as is_array, typtype, typname, pg_type.oid FROM pg_catalog.pg_type WHERE pg_type.oid = 22;",
    );
    assert_eq!(regproc_probe.len(), 1);
    // search-path join probe over generate_series + current_schemas(false).
    let search_path_probe = exec(
        &mut executor,
        "select ns.oid as nspoid, ns.nspname from pg_namespace as ns join ( select s.r, (current_schemas(false))[s.r] as nspname from generate_series(1, array_upper(current_schemas(false), 1)) as s(r) ) as r using ( nspname );",
    );
    assert!(!search_path_probe.is_empty());
    // Full DBeaver search-path probe: regproc cast + generate_series +
    // array_upper(current_schemas(false), 1) + USING-join + ORDER BY.
    let full_probe = exec(
        &mut executor,
        "SELECT typinput='pg_catalog.array_in'::regproc as is_array, typtype, typname, pg_type.oid FROM pg_catalog.pg_type LEFT JOIN (select ns.oid as nspoid, ns.nspname, r.r from pg_namespace as ns join ( select s.r, (current_schemas(false))[s.r] as nspname from generate_series(1, array_upper(current_schemas(false), 1)) as s(r) ) as r using ( nspname ) ) as sp ON sp.nspoid = typnamespace WHERE pg_type.oid = 22 ORDER BY sp.r, pg_type.oid DESC;",
    );
    assert_eq!(full_probe.len(), 1);
    let windows = exec(&mut executor, "SELECT id, amount, LAG(amount) OVER (ORDER BY id), LEAD(amount) OVER (ORDER BY id), SUM(amount) OVER (PARTITION BY user_id) FROM orders ORDER BY id;");
    assert_eq!(windows.len(), 5);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn nulls_errors_and_transaction_regressions() {
    let (storage, wal) = unique_engine("edge-regressions");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE nullable (id INTEGER PRIMARY KEY, value INTEGER);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO nullable VALUES (1, NULL), (2, 1), (3, 2);",
    );
    let first = exec(
        &mut executor,
        "SELECT id FROM nullable ORDER BY value NULLS FIRST;",
    );
    let last = exec(
        &mut executor,
        "SELECT id FROM nullable ORDER BY value NULLS LAST;",
    );
    assert_eq!(
        first,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)]
        ]
    );
    assert_eq!(
        last,
        vec![
            vec![Value::Int4(2)],
            vec![Value::Int4(3)],
            vec![Value::Int4(1)]
        ]
    );
    assert!(
        exec_err(&mut executor, "SELECT CAST('not-an-int' AS INTEGER);").contains("invalid input")
    );
    assert!(exec_err(&mut executor, "SELECT 1 / 0;").contains("division by zero"));
    assert!(exec_err(&mut executor, "SELECT SQRT(-1);").contains("square root"));
    assert!(exec_err(&mut executor, "SELECT DATE '2025-02-30';").contains("invalid"));
    assert!(!exec_err(&mut executor, "SELECT definitely_invalid(").is_empty());
    exec_ok(
        &mut executor,
        "BEGIN; INSERT INTO nullable VALUES (4, 4); ROLLBACK;",
    );
    assert_eq!(
        exec(&mut executor, "SELECT COUNT(*) FROM nullable;"),
        vec![vec![Value::Int8(3)]]
    );
    exec_ok(
        &mut executor,
        "BEGIN; INSERT INTO nullable VALUES (4, 4); COMMIT;",
    );
    assert_eq!(
        exec(&mut executor, "SELECT COUNT(*) FROM nullable;"),
        vec![vec![Value::Int8(4)]]
    );
    let transaction_error = exec_err(
        &mut executor,
        "BEGIN; INSERT INTO nullable VALUES (5, 5); UPDATE nullable SET value = CAST('bad' AS INTEGER) WHERE id = 1; COMMIT;"
    );
    assert!(
        transaction_error.contains("invalid input"),
        "{transaction_error}"
    );
    assert_eq!(
        exec(&mut executor, "SELECT COUNT(*) FROM nullable;"),
        vec![vec![Value::Int8(4)]]
    );
    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn correlated_subqueries_temporal_arithmetic_and_extract() {
    let (storage, wal) = unique_engine("correlation-temporal");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob');",
    );
    exec_ok(&mut executor, "INSERT INTO orders VALUES (10, 1), (11, 1);");

    assert_eq!(
        exec(&mut executor, "SELECT COUNT(*), NULL = NULL FROM users;"),
        vec![vec![Value::Int8(2), Value::Null]]
    );

    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT name, (SELECT COUNT(*) FROM orders o WHERE o.user_id = users.id) AS order_count FROM users ORDER BY id;",
        )),
        vec![vec!["Alice", "2"], vec!["Bob", "0"]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT name FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id) ORDER BY id;",
        )),
        vec![vec!["Alice"]]
    );

    assert!(matches!(
        executor
            .execute("BEGIN; UPDATE users SET name = 'RolledBack' WHERE id = 1; ROLLBACK;")
            .unwrap(),
        QueryResult::RolledBack
    ));
    assert_eq!(
        exec(&mut executor, "SELECT name FROM users WHERE id = 1;"),
        vec![vec![Value::Text("Alice".into())]]
    );
    assert!(matches!(
        executor
            .execute("BEGIN; UPDATE users SET name = 'Committed' WHERE id = 1; COMMIT;")
            .unwrap(),
        QueryResult::Committed
    ));
    assert_eq!(
        exec(&mut executor, "SELECT name FROM users WHERE id = 1;"),
        vec![vec![Value::Text("Committed".into())]]
    );

    assert_eq!(
        exec(
            &mut executor,
            "SELECT TIMESTAMP '2025-01-02' - TIMESTAMP '2025-01-01';",
        ),
        vec![vec![Value::Interval(plomid_types::datetime::Interval {
            micros: plomid_types::datetime::USECS_PER_DAY,
            ..Default::default()
        })]]
    );
    assert_eq!(
        text_rows(&exec(
            &mut executor,
            "SELECT EXTRACT(HOUR FROM TIMESTAMP '2025-05-10 12:30:00'), EXTRACT(MINUTE FROM TIMESTAMP '2025-05-10 12:30:00'), EXTRACT(SECOND FROM TIMESTAMP '2025-05-10 12:30:00');",
        )),
        vec![vec!["12", "30", "0"]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// Bitwise operators (&, |, #, <<, >>)
// ---------------------------------------------------------------------------

#[test]
fn bitwise_operators() {
    let (storage, wal) = unique_engine("bitwise");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(
        &mut executor,
        "SELECT 12 & 10, 12 | 10, 12 # 10, 1 << 4, 256 >> 3;",
    );
    assert_eq!(
        rows[0],
        vec![
            Value::Int4(8),
            Value::Int4(14),
            Value::Int4(6),
            Value::Int4(16),
            Value::Int4(32),
        ]
    );

    // Precedence: additive binds tighter than bitwise.
    let rows = exec(&mut executor, "SELECT 1 + 2 & 3, 2 * 3 | 4;");
    assert_eq!(rows[0][0], Value::Int8(3));
    assert_eq!(rows[0][1], Value::Int8(6));

    // NULL propagation.
    let rows = exec(&mut executor, "SELECT NULL::integer & 5;");
    assert_eq!(rows[0][0], Value::Null);

    // Works against table columns too.
    exec_ok(&mut executor, "CREATE TABLE b (a INT, c INT);");
    exec_ok(&mut executor, "INSERT INTO b VALUES (12, 10);");
    let rows = exec(&mut executor, "SELECT a & c, a << 1 FROM b;");
    assert_eq!(rows[0][0], Value::Int4(8));
    assert_eq!(rows[0][1], Value::Int4(24));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// SELECT DISTINCT and VALUES derived tables
// ---------------------------------------------------------------------------

#[test]
fn distinct_over_values_derived_table() {
    let (storage, wal) = unique_engine("distinct-values");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(
        &mut executor,
        "SELECT DISTINCT value FROM (VALUES (1), (1), (2), (2), (3)) AS x(value) ORDER BY value;",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)]
        ]
    );

    // Bare VALUES query.
    let rows = exec(&mut executor, "VALUES (1), (2);");
    assert_eq!(rows, vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]);

    // Plain DISTINCT over a table.
    exec_ok(&mut executor, "CREATE TABLE d (id INT PRIMARY KEY, v INT);");
    exec_ok(
        &mut executor,
        "INSERT INTO d VALUES (1, 5), (2, 5), (3, 6);",
    );
    let rows = exec(&mut executor, "SELECT DISTINCT v FROM d ORDER BY v;");
    assert_eq!(rows, vec![vec![Value::Int4(5)], vec![Value::Int4(6)]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// SELECT DISTINCT ON
// ---------------------------------------------------------------------------

fn distinct_on_fixture(executor: &mut Executor<PlomidStorageEngine>) {
    exec_ok(executor, "DROP SCHEMA IF EXISTS distinct_test CASCADE;");
    exec_ok(executor, "CREATE SCHEMA distinct_test;");
    exec_ok(
        executor,
        "CREATE TABLE distinct_test.customers (id INTEGER PRIMARY KEY, first_name TEXT, last_name TEXT, country TEXT, credit_limit INTEGER);",
    );
    exec_ok(
        executor,
        "INSERT INTO distinct_test.customers (id, first_name, last_name, country, credit_limit) VALUES (1, 'Alice', 'Smith', 'India', 5000), (2, 'Bob', 'Jones', 'USA', 10000), (3, 'Charlie', 'Brown', 'UK', 2000), (4, 'David', 'Wilson', 'India', 7500), (5, 'Emma', 'Taylor', 'Germany', 9000), (6, 'Frank', 'Miller', 'USA', 12000), (7, 'Grace', 'Lee', 'India', 6000);",
    );
}

#[test]
fn distinct_on_selects_first_row_per_group_by_id() {
    let (storage, wal) = unique_engine("distinct-on-id");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    distinct_on_fixture(&mut executor);

    let rows = exec(
        &mut executor,
        "SELECT DISTINCT ON (country) id, first_name, country, credit_limit FROM distinct_test.customers ORDER BY country, id;",
    );
    assert_eq!(
        rows,
        vec![
            vec![
                Value::Int4(5),
                Value::Text("Emma".into()),
                Value::Text("Germany".into()),
                Value::Int4(9000),
            ],
            vec![
                Value::Int4(1),
                Value::Text("Alice".into()),
                Value::Text("India".into()),
                Value::Int4(5000),
            ],
            vec![
                Value::Int4(3),
                Value::Text("Charlie".into()),
                Value::Text("UK".into()),
                Value::Int4(2000),
            ],
            vec![
                Value::Int4(2),
                Value::Text("Bob".into()),
                Value::Text("USA".into()),
                Value::Int4(10000),
            ],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn distinct_on_selects_first_row_per_group_by_credit_limit_desc() {
    let (storage, wal) = unique_engine("distinct-on-credit");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    distinct_on_fixture(&mut executor);

    let rows = exec(
        &mut executor,
        "SELECT DISTINCT ON (country) id, first_name, country, credit_limit FROM distinct_test.customers ORDER BY country, credit_limit DESC;",
    );
    assert_eq!(
        rows,
        vec![
            vec![
                Value::Int4(5),
                Value::Text("Emma".into()),
                Value::Text("Germany".into()),
                Value::Int4(9000),
            ],
            vec![
                Value::Int4(4),
                Value::Text("David".into()),
                Value::Text("India".into()),
                Value::Int4(7500),
            ],
            vec![
                Value::Int4(3),
                Value::Text("Charlie".into()),
                Value::Text("UK".into()),
                Value::Int4(2000),
            ],
            vec![
                Value::Int4(6),
                Value::Text("Frank".into()),
                Value::Text("USA".into()),
                Value::Int4(12000),
            ],
        ]
    );

    // Ordinary DISTINCT and plain SELECT behaviour must not regress.
    let rows = exec(
        &mut executor,
        "SELECT DISTINCT country FROM distinct_test.customers ORDER BY country;",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Text("Germany".into())],
            vec![Value::Text("India".into())],
            vec![Value::Text("UK".into())],
            vec![Value::Text("USA".into())],
        ]
    );
    let rows = exec(
        &mut executor,
        "SELECT id, first_name, country FROM distinct_test.customers ORDER BY id;",
    );
    assert_eq!(rows.len(), 7);
    assert_eq!(rows[0][0], Value::Int4(1));
    let rows = exec(
        &mut executor,
        "SELECT DISTINCT country, credit_limit FROM distinct_test.customers ORDER BY country, credit_limit;",
    );
    assert_eq!(rows.len(), 7);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// SERIAL types
// ---------------------------------------------------------------------------

#[test]
fn serial_columns() {
    let (storage, wal) = unique_engine("serial");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE s (id SERIAL, name TEXT);");
    // The column accepts integer row identity; `DEFAULT` triggers the
    // sequence-provided value through the column default.
    let rows = exec(&mut executor, "SELECT nextval('s_id_seq');");
    assert_eq!(rows[0][0], Value::Int8(1));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

// ---------------------------------------------------------------------------
// Multi-row INSERT semantics
// ---------------------------------------------------------------------------

#[test]
fn multi_row_insert_shapes_and_returning() {
    let (storage, wal) = unique_engine("multirow");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE m (id INTEGER, name TEXT, score INTEGER);",
    );

    // Three-row VALUES list inserts every row.
    assert_eq!(
        executor
            .execute("INSERT INTO m VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30);")
            .unwrap(),
        QueryResult::Inserted(3)
    );
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM m;"),
        vec![vec![Value::Int8(3)]]
    );

    // Single-row list still reports one inserted row.
    assert_eq!(
        executor
            .execute("INSERT INTO m VALUES (4, 'd', 40);")
            .unwrap(),
        QueryResult::Inserted(1)
    );

    // Explicit column lists with a multi-row VALUES source.
    assert_eq!(
        executor
            .execute("INSERT INTO m (id, name) VALUES (5, 'e'), (6, 'f');")
            .unwrap(),
        QueryResult::Inserted(2)
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT id, score FROM m WHERE id >= 5 ORDER BY id;"
        ),
        vec![
            vec![Value::Int4(5), Value::Null],
            vec![Value::Int4(6), Value::Null]
        ]
    );

    // Commas inside string literals must not split rows.
    exec_ok(
        &mut executor,
        "INSERT INTO m VALUES (7, 'x, y', 1), (8, 'a,b,c', 2);",
    );
    assert_eq!(
        exec(&mut executor, "SELECT name FROM m WHERE id = 7;"),
        vec![vec![Value::Text("x, y".into())]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT name FROM m WHERE id = 8;"),
        vec![vec![Value::Text("a,b,c".into())]]
    );

    // DEFAULT and NULL mixing within a multi-row list.
    exec_ok(
        &mut executor,
        "INSERT INTO m VALUES (9, DEFAULT, DEFAULT), (10, NULL, NULL);",
    );
    assert_eq!(
        exec(&mut executor, "SELECT name, score FROM m WHERE id = 9;"),
        vec![vec![Value::Null, Value::Null]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT name, score FROM m WHERE id = 10;"),
        vec![vec![Value::Null, Value::Null]]
    );

    // Multi-row list with distinct literal types per row.
    exec_ok(
        &mut executor,
        "INSERT INTO m VALUES (11, 'expr', 10), (12, 'expr2', 6);",
    );
    assert_eq!(
        exec(&mut executor, "SELECT score FROM m WHERE id = 11;"),
        vec![vec![Value::Int4(10)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT score FROM m WHERE id = 12;"),
        vec![vec![Value::Int4(6)]]
    );

    // RETURNING with a multi-row insert returns one row per source row.
    let rows = exec(
        &mut executor,
        "INSERT INTO m (id, name, score) VALUES (13, 'r1', 100), (14, 'r2', 200) RETURNING id, name;",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int4(13), Value::Text("r1".into())],
            vec![Value::Int4(14), Value::Text("r2".into())]
        ]
    );

    // Atomicity: a failing row must roll back the whole multi-row insert.
    exec_ok(
        &mut executor,
        "CREATE TABLE mm (id INTEGER PRIMARY KEY, v TEXT);",
    );
    exec_ok(&mut executor, "INSERT INTO mm VALUES (1, 'one');");
    assert!(executor
        .execute("INSERT INTO mm VALUES (2, 'two'), (1, 'dup'), (3, 'three');")
        .is_err());
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM mm;"),
        vec![vec![Value::Int8(1)]]
    );

    // Duplicate row identity anywhere in the list fails atomically.
    assert!(executor
        .execute("INSERT INTO mm VALUES (4, 'ok'), (4, 'dup id');")
        .is_err());
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM mm WHERE id = 4;"),
        vec![vec![Value::Int8(0)]]
    );

    // Count mismatches are rejected per row.
    assert!(executor
        .execute("INSERT INTO mm VALUES (5), (6, 'bad');")
        .is_err());
    assert!(executor
        .execute("INSERT INTO mm VALUES (5, 'a', 1);")
        .is_err());

    // Persistence across restart.
    drop(executor);
    let engine = PlomidStorageEngine::open(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM m;"),
        vec![vec![Value::Int8(14)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM mm;"),
        vec![vec![Value::Int8(1)]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn multi_row_insert_unique_constraints_and_indexes() {
    let (storage, wal) = unique_engine("multirow-unique");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Column-level UNIQUE across a multi-row insert.
    exec_ok(
        &mut executor,
        "CREATE TABLE u (id INTEGER, email TEXT UNIQUE);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO u VALUES (1, 'a@x.com'), (2, 'b@x.com'), (3, 'c@x.com');",
    );
    assert!(executor
        .execute("INSERT INTO u VALUES (4, 'd@x.com'), (5, 'a@x.com');")
        .is_err());
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM u;"),
        vec![vec![Value::Int8(3)]]
    );
    // Duplicate within the same statement is also rejected.
    assert!(executor
        .execute("INSERT INTO u VALUES (4, 'same@x.com'), (5, 'same@x.com');")
        .is_err());
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM u;"),
        vec![vec![Value::Int8(3)]]
    );

    // UNIQUE INDEX created before the insert is enforced per row.
    exec_ok(&mut executor, "CREATE TABLE ui (id INTEGER, tag TEXT);");
    exec_ok(&mut executor, "CREATE UNIQUE INDEX ui_tag_idx ON ui(tag);");
    exec_ok(&mut executor, "INSERT INTO ui VALUES (1, 't1'), (2, 't2');");
    assert!(executor
        .execute("INSERT INTO ui VALUES (3, 't3'), (4, 't1');")
        .is_err());
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM ui;"),
        vec![vec![Value::Int8(2)]]
    );

    // UNIQUE INDEX created after rows already exist blocks new duplicates.
    exec_ok(&mut executor, "INSERT INTO ui VALUES (5, 't5');");
    exec_ok(&mut executor, "CREATE UNIQUE INDEX ui_id_idx ON ui(id);");
    assert!(executor
        .execute("INSERT INTO ui VALUES (5, 't5b');")
        .is_err());
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM ui WHERE id = 5;"),
        vec![vec![Value::Int8(1)]]
    );

    // PRIMARY KEY duplicates inside a multi-row insert.
    exec_ok(
        &mut executor,
        "CREATE TABLE pk (id INTEGER PRIMARY KEY, v TEXT);",
    );
    exec_ok(&mut executor, "INSERT INTO pk VALUES (1, 'a'), (2, 'b');");
    assert!(executor
        .execute("INSERT INTO pk VALUES (3, 'c'), (2, 'dup');")
        .is_err());
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM pk;"),
        vec![vec![Value::Int8(2)]]
    );

    // NULLs are permitted by UNIQUE indexes and don't collide.
    exec_ok(&mut executor, "INSERT INTO ui VALUES (6, NULL), (7, NULL);");
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM ui WHERE tag IS NULL;"),
        vec![vec![Value::Int8(2)]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn multi_row_insert_within_explicit_transactions() {
    let (storage, wal) = unique_engine("multirow-txn");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER, v TEXT);");

    // Multi-row insert inside a transaction commits with it.
    exec_ok(
        &mut executor,
        "BEGIN; INSERT INTO t VALUES (1, 'a'), (2, 'b'); COMMIT;",
    );
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM t;"),
        vec![vec![Value::Int8(2)]]
    );

    // Rolling back discards the whole multi-row insert.
    exec_ok(
        &mut executor,
        "BEGIN; INSERT INTO t VALUES (3, 'c'), (4, 'd'); ROLLBACK;",
    );
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM t;"),
        vec![vec![Value::Int8(2)]]
    );

    // Multiple multi-row inserts in one batch all commit.
    exec_ok(
        &mut executor,
        "BEGIN; INSERT INTO t VALUES (5, 'e'), (6, 'f'); INSERT INTO t VALUES (7, 'g'); COMMIT;",
    );
    assert_eq!(
        exec(&mut executor, "SELECT count(*) FROM t;"),
        vec![vec![Value::Int8(5)]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
#[test]
// 3.14 here is SQL test data under test (the string being parsed), not a
// stand-in for PI.
#[allow(clippy::approx_constant)]
fn multiword_type_casts_execute() {
    let (storage, wal) = unique_engine("multiword-cast");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // `::` postfix cast with multi-word type names must parse the full name
    // (not just the leading word) and execute through the cast registry.
    assert_eq!(
        exec(&mut executor, "SELECT '3.14'::double precision;"),
        vec![vec![Value::Float8(3.14)]]
    );
    assert_eq!(
        exec(&mut executor, "SELECT 'hi'::character varying;"),
        vec![vec![Value::VarChar("hi".to_string())]]
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT '2024-01-01 12:00:00'::timestamp with time zone;"
        ),
        vec![vec![Value::Timestamptz(757425600000000)]]
    );

    // The SQL-standard CAST(... AS ...) form with a multi-word type name.
    assert_eq!(
        exec(&mut executor, "SELECT CAST('3.14' AS double precision);"),
        vec![vec![Value::Float8(3.14)]]
    );

    // A multi-word cast with a type modifier.
    assert_eq!(
        exec(&mut executor, "SELECT 'abc'::character varying(3);"),
        vec![vec![Value::VarChar("abc".to_string())]]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
// ---------------------------------------------------------------------------
// DROP SCHEMA ... CASCADE / RESTRICT
// ---------------------------------------------------------------------------

/// Relation names (tables, indexes, views) in a schema via pg_class.
fn schema_relation_names(schema: &str) -> String {
    format!(
        "SELECT c.relname FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{schema}';"
    )
}

fn rows_col0(rows: Vec<Vec<Value>>) -> Vec<String> {
    rows.into_iter()
        .map(|mut row| match row.remove(0) {
            Value::Name(s) | Value::Text(s) | Value::VarChar(s) => s,
            other => panic!("unexpected label value: {other:?}"),
        })
        .collect()
}
#[test]
fn drop_schema_cascade_removes_all_objects_and_allows_recreate() {
    let (storage, wal) = unique_engine("drop-schema-cascade");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE SCHEMA sched;");
    exec_ok(
        &mut executor,
        "CREATE TABLE sched.customers (\
             id BIGSERIAL PRIMARY KEY,\
             email TEXT NOT NULL UNIQUE,\
             country VARCHAR(40),\
             age INTEGER,\
             CONSTRAINT customers_age_check CHECK (age >= 0)\
         );",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE sched.orders (\
             id BIGSERIAL PRIMARY KEY,\
             customer_id INTEGER NOT NULL,\
             amount NUMERIC(12,2),\
             CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES sched.customers(id)\
         );",
    );
    exec_ok(
        &mut executor,
        "CREATE INDEX customers_country_idx ON sched.customers(country);",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW sched.customers_view AS SELECT id, email FROM sched.customers;",
    );
    exec_ok(&mut executor, "CREATE SEQUENCE sched.ordinal_seq;");
    exec_ok(
        &mut executor,
        "INSERT INTO sched.customers (email, country, age) VALUES ('a@x.com', 'IN', 30);",
    );

    // Everything is present before the drop.
    let before = rows_col0(exec(&mut executor, &schema_relation_names("sched")));
    for expected in [
        "customers",
        "orders",
        "customers_country_idx",
        "customers_view",
    ] {
        assert!(
            before.iter().any(|name| name == expected),
            "missing {expected}: {before:?}"
        );
    }

    // CASCADE must recursively drop every contained object.
    exec_ok(&mut executor, "DROP SCHEMA sched CASCADE;");

    // Schema and all of its objects are gone.
    assert!(rows_col0(exec(&mut executor, &schema_relation_names("sched"))).is_empty());
    assert!(exec(
        &mut executor,
        "SELECT * FROM information_schema.tables WHERE table_schema = 'sched';",
    )
    .is_empty());
    assert!(exec(
        &mut executor,
        "SELECT * FROM information_schema.views WHERE table_schema = 'sched';",
    )
    .is_empty());
    // The explicit sequence was removed as well.
    exec_ok(&mut executor, "CREATE SCHEMA sched;");
    exec_ok(&mut executor, "CREATE SEQUENCE sched.ordinal_seq;");

    // Tables can be recreated with the same names: the key regression.
    exec_ok(
        &mut executor,
        "CREATE TABLE sched.customers (\
             id BIGSERIAL PRIMARY KEY,\
             email TEXT NOT NULL UNIQUE,\
             country VARCHAR(40)\
         );",
    );
    // A shadow index name must also be reusable.
    exec_ok(
        &mut executor,
        "CREATE INDEX customers_country_idx ON sched.customers(country);",
    );
    exec_ok(&mut executor, "DROP SCHEMA sched CASCADE;");

    // Repeated DROP/CREATE cycle.
    exec_ok(&mut executor, "CREATE SCHEMA sched;");
    exec_ok(
        &mut executor,
        "CREATE TABLE sched.customers (id BIGINT PRIMARY KEY, country TEXT);",
    );
    exec_ok(
        &mut executor,
        "CREATE INDEX customers_country_idx ON sched.customers(country);",
    );
    exec_ok(&mut executor, "DROP SCHEMA sched CASCADE;");
    exec_ok(&mut executor, "CREATE SCHEMA sched;");
    exec_ok(
        &mut executor,
        "CREATE TABLE sched.customers (id BIGINT PRIMARY KEY, country TEXT);",
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn drop_schema_restrict_refuses_when_objects_exist() {
    let (storage, wal) = unique_engine("drop-schema-restrict");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE SCHEMA den;");
    exec_ok(&mut executor, "CREATE TABLE den.t (id BIGINT PRIMARY KEY);");

    // Explicit RESTRICT fails because the schema contains an object.
    let err = exec_err(&mut executor, "DROP SCHEMA den RESTRICT;");
    assert!(
        err.contains("den") && err.contains("depend"),
        "unexpected error: {err}"
    );

    // Plain DROP SCHEMA (no CASCADE) defaults to RESTRICT and must also fail.
    let err = exec_err(&mut executor, "DROP SCHEMA den;");
    assert!(err.contains("depend"), "unexpected error: {err}");

    // The object is untouched because the DROP was rejected.
    assert!(!exec(
        &mut executor,
        "SELECT * FROM information_schema.tables WHERE table_schema = 'den';",
    )
    .is_empty());

    // CASCADE is the only form that succeeds.
    exec_ok(&mut executor, "DROP SCHEMA den CASCADE;");
    assert!(exec(
        &mut executor,
        "SELECT * FROM information_schema.tables WHERE table_schema = 'den';",
    )
    .is_empty());

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn drop_schema_if_exists_handles_missing_schema() {
    let (storage, wal) = unique_engine("drop-schema-if-exists");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // IF EXISTS + missing schema is a no-op success, with or without CASCADE.
    exec_ok(
        &mut executor,
        "DROP SCHEMA IF EXISTS missing_schema CASCADE;",
    );
    exec_ok(&mut executor, "DROP SCHEMA IF EXISTS missing_schema;");

    // Without IF EXISTS a missing schema is an error.
    let err = exec_err(&mut executor, "DROP SCHEMA missing_schema;");
    assert!(err.contains("does not exist"), "unexpected error: {err}");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn drop_schema_cascade_cleanup_survives_restart() {
    let (storage, wal) = unique_engine("drop-schema-restart");
    {
        let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
        let mut executor = Executor::new(engine).unwrap();

        exec_ok(&mut executor, "CREATE SCHEMA persist;");
        exec_ok(
            &mut executor,
            "CREATE TABLE persist.customers (id BIGSERIAL PRIMARY KEY, country TEXT);",
        );
        exec_ok(
            &mut executor,
            "CREATE INDEX persist_customers_country_idx ON persist.customers(country);",
        );
        exec_ok(&mut executor, "DROP SCHEMA persist CASCADE;");

        // In-memory state must already be clean before restart.
        assert!(exec(
            &mut executor,
            "SELECT * FROM information_schema.tables WHERE table_schema = 'persist';",
        )
        .is_empty());
        drop(executor);
    }

    // Reopen the persisted storage/catalog: the dropped objects must not come
    // back, and re-creating them must succeed.
    let engine = PlomidStorageEngine::open(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    assert!(exec(
        &mut executor,
        "SELECT * FROM information_schema.tables WHERE table_schema = 'persist';",
    )
    .is_empty());

    exec_ok(&mut executor, "CREATE SCHEMA persist;");
    exec_ok(
        &mut executor,
        "CREATE TABLE persist.customers (id BIGINT PRIMARY KEY, country TEXT);",
    );
    exec_ok(
        &mut executor,
        "CREATE INDEX persist_customers_country_idx ON persist.customers(country);",
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn current_timestamp_statement_context_regression() {
    let (storage, wal) = unique_engine("current-timestamp");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // A. SELECT CURRENT_TIMESTAMP works.
    let rows = exec(&mut executor, "SELECT CURRENT_TIMESTAMP;");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(rows[0][0], Value::Timestamp(_)),
        "expected TIMESTAMP, got {:?}",
        rows[0][0]
    );

    // B. Both references in one statement share the statement timestamp.
    let rows = exec(
        &mut executor,
        "SELECT CURRENT_TIMESTAMP, CURRENT_TIMESTAMP;",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], rows[0][1], "statement timestamp must be stable");

    // C. Table for INSERT/UPDATE timestamp tests.
    exec_ok(
        &mut executor,
        "CREATE TABLE timestamp_test (id INTEGER PRIMARY KEY, created_at TIMESTAMP NOT NULL);",
    );

    // D. INSERT ... VALUES (CURRENT_TIMESTAMP) must not require SELECT context.
    exec_ok(
        &mut executor,
        "INSERT INTO timestamp_test VALUES (1, CURRENT_TIMESTAMP);",
    );

    // E. Row is readable and typed.
    let rows = exec(&mut executor, "SELECT * FROM timestamp_test;");
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0][1], Value::Timestamp(_)));

    // F. Second INSERT also succeeds.
    exec_ok(
        &mut executor,
        "INSERT INTO timestamp_test VALUES (2, CURRENT_TIMESTAMP);",
    );

    // Both columns from one VALUES row share the statement timestamp.
    exec_ok(
        &mut executor,
        "CREATE TABLE two_ts (a TIMESTAMP NOT NULL, b TIMESTAMP NOT NULL);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO two_ts VALUES (CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);",
    );
    let rows = exec(&mut executor, "SELECT * FROM two_ts;");
    assert_eq!(rows[0][0], rows[0][1]);

    // G. UPDATE ... SET col = CURRENT_TIMESTAMP.
    exec_ok(
        &mut executor,
        "UPDATE timestamp_test SET created_at = CURRENT_TIMESTAMP WHERE id = 1;",
    );

    // H. Another plain INSERT.
    exec_ok(
        &mut executor,
        "INSERT INTO timestamp_test VALUES (3, CURRENT_TIMESTAMP);",
    );

    // I. CAST(CURRENT_TIMESTAMP AS TIMESTAMP).
    exec_ok(
        &mut executor,
        "INSERT INTO timestamp_test VALUES (4, CAST(CURRENT_TIMESTAMP AS TIMESTAMP));",
    );

    // J. CURRENT_TIMESTAMP::TIMESTAMP.
    exec_ok(
        &mut executor,
        "INSERT INTO timestamp_test VALUES (5, CURRENT_TIMESTAMP::TIMESTAMP);",
    );

    // Parenthesized form supported by the parser.
    let rows = exec(&mut executor, "SELECT CURRENT_TIMESTAMP();");
    assert!(matches!(rows[0][0], Value::Timestamp(_)));

    // NOW() is the PostgreSQL function spelling of the same statement clock.
    let rows = exec(&mut executor, "SELECT NOW();");
    assert!(
        matches!(rows[0][0], Value::Timestamp(_)),
        "NOW() should produce a timestamp, got {:?}",
        rows[0][0]
    );
    let rows = exec(&mut executor, "SELECT CURRENT_TIMESTAMP, NOW();");
    assert_eq!(
        rows[0][0], rows[0][1],
        "NOW() must match statement timestamp"
    );

    // Timestamp +/- interval arithmetic (torture test section 9).
    let rows = exec(
        &mut executor,
        "SELECT TIMESTAMP '2026-09-10 12:30:00' + INTERVAL '1 day';",
    );
    assert!(matches!(rows[0][0], Value::Timestamp(_)));
    let rows = exec(
        &mut executor,
        "SELECT TIMESTAMP '2026-09-10 12:30:00' - INTERVAL '1 hour';",
    );
    assert!(matches!(rows[0][0], Value::Timestamp(_)));

    // WHERE / CASE / COALESCE / CAST contexts.
    let rows = exec(
        &mut executor,
        "SELECT * FROM timestamp_test WHERE created_at <= CURRENT_TIMESTAMP;",
    );
    assert!(!rows.is_empty());
    let rows = exec(
        &mut executor,
        "SELECT CASE WHEN created_at <= CURRENT_TIMESTAMP THEN 1 ELSE 0 END FROM timestamp_test WHERE id = 1;",
    );
    assert_eq!(rows[0][0], Value::Int4(1));
    let rows = exec(&mut executor, "SELECT COALESCE(NULL, CURRENT_TIMESTAMP);");
    assert!(matches!(rows[0][0], Value::Timestamp(_)));

    // INSERT ... SELECT CURRENT_TIMESTAMP.
    exec_ok(
        &mut executor,
        "INSERT INTO timestamp_test SELECT 6, CURRENT_TIMESTAMP;",
    );

    // DEFAULT CURRENT_TIMESTAMP.
    exec_ok(
        &mut executor,
        "CREATE TABLE ts_default (id INTEGER PRIMARY KEY, created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP);",
    );
    exec_ok(&mut executor, "INSERT INTO ts_default (id) VALUES (1);");
    let rows = exec(
        &mut executor,
        "SELECT created_at FROM ts_default WHERE id = 1;",
    );
    assert!(
        matches!(rows[0][0], Value::Timestamp(_)),
        "DEFAULT CURRENT_TIMESTAMP should produce a timestamp, got {:?}",
        rows[0][0]
    );

    // Literal timestamps keep working.
    exec_ok(
        &mut executor,
        "INSERT INTO timestamp_test VALUES (1000005, '2026-09-10 13:05:00');",
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
// ===========================================================================
// Search-path driven schema resolution & per-schema table identity.
// ===========================================================================

#[test]
fn search_path_schema_resolution_coexists_with_public() {
    let (storage, wal) = unique_engine("search-path");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Setup against the default (public) search path.
    exec_ok(&mut executor, "CREATE SCHEMA plomid_test;");
    exec_ok(
        &mut executor,
        "CREATE TABLE public.customers_test (id BIGINT PRIMARY KEY);",
    );

    // Simulate the wire session publishing `SET search_path TO plomid_test, public`.
    let search_path = vec!["plomid_test".to_string(), "public".to_string()];

    // current_schema() must reflect the first existing schema on the path.
    let schema_rows = match executor
        .execute_all_with_search_path("SELECT current_schema();", &search_path)
        .unwrap()
        .0
        .into_iter()
        .next()
        .unwrap()
    {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    };
    assert_eq!(schema_rows[0][0], Value::Text("plomid_test".into()));

    // An unqualified CREATE TABLE resolves into plomid_test and must NOT be
    // rejected merely because public.customers_test already exists.
    executor
        .execute_all_with_search_path(
            "CREATE TABLE customers_test (id BIGINT PRIMARY KEY);",
            &search_path,
        )
        .unwrap();

    // Both rows must exist in information_schema.tables.
    let both_rows = match executor
        .execute_all_with_search_path(
            "SELECT table_schema, table_name FROM information_schema.tables \
             WHERE table_name = 'customers_test' ORDER BY table_schema;",
            &search_path,
        )
        .unwrap()
        .0
        .into_iter()
        .next()
        .unwrap()
    {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    };
    let schemas: Vec<String> = both_rows
        .iter()
        .map(|r| match &r[0] {
            Value::Text(s) => s.clone(),
            other => panic!("unexpected schema value {other:?}"),
        })
        .collect();
    assert_eq!(
        schemas,
        vec!["plomid_test".to_string(), "public".to_string()]
    );

    // Insert one row into each schema-qualified table and confirm the
    // unqualified name resolves through the search_path to plomid_test.
    executor
        .execute_all_with_search_path("INSERT INTO customers_test VALUES (1);", &search_path)
        .unwrap();
    exec_ok(
        &mut executor,
        "INSERT INTO public.customers_test VALUES (2);",
    );
    let rows = match executor
        .execute_all_with_search_path("SELECT id FROM customers_test;", &search_path)
        .unwrap()
        .0
        .into_iter()
        .next()
        .unwrap()
    {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    };
    assert_eq!(
        rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(),
        vec![Value::Int8(1)]
    );
    // Explicitly qualified public table still sees its own rows.
    let public_rows = exec(&mut executor, "SELECT id FROM public.customers_test;");
    assert_eq!(public_rows[0][0], Value::Int8(2));

    executor
        .execute("DROP SCHEMA plomid_test CASCADE;")
        .unwrap();
    exec_ok(&mut executor, "DROP TABLE public.customers_test;");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
// ---------------------------------------------------------------------------
// Recursive CTEs (WITH RECURSIVE)
// ---------------------------------------------------------------------------

/// Sets up the `recursive_test.employees` hierarchy table used by the JOIN
/// regression test. Mirrors the relationship described in the spec:
/// managers have `manager_id = NULL`, everyone else points at their manager.
fn setup_employees<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>) {
    exec_ok(executor, "CREATE SCHEMA recursive_test;");
    exec_ok(
        executor,
        "CREATE TABLE recursive_test.employees (
            id INTEGER,
            name TEXT,
            manager_id INTEGER
        );",
    );
    exec_ok(
        executor,
        "INSERT INTO recursive_test.employees VALUES (1, 'Alice', NULL);",
    );
    exec_ok(
        executor,
        "INSERT INTO recursive_test.employees VALUES (6, 'Frank', NULL);",
    );
    exec_ok(
        executor,
        "INSERT INTO recursive_test.employees VALUES (2, 'Bob', 1);",
    );
    exec_ok(
        executor,
        "INSERT INTO recursive_test.employees VALUES (3, 'Charlie', 1);",
    );
    exec_ok(
        executor,
        "INSERT INTO recursive_test.employees VALUES (7, 'Grace', 6);",
    );
    exec_ok(
        executor,
        "INSERT INTO recursive_test.employees VALUES (4, 'David', 2);",
    );
    exec_ok(
        executor,
        "INSERT INTO recursive_test.employees VALUES (5, 'Emma', 2);",
    );
    exec_ok(
        executor,
        "INSERT INTO recursive_test.employees VALUES (8, 'Henry', 3);",
    );
}

#[test]
fn recursive_cte_basic_counter() {
    let (storage, wal) = unique_engine("recursive-basic");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(
        &mut executor,
        "WITH RECURSIVE nums(n) AS (
            SELECT 1
            UNION ALL
            SELECT n + 1
            FROM nums
            WHERE n < 5
        )
        SELECT n FROM nums ORDER BY n;",
    );
    // The anchor literal is Int4 (1); the `n + 1` expression promotes to Int8,
    // matching the engine's normal integer-arithmetic widening.
    assert_eq!(
        rows,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int8(2)],
            vec![Value::Int8(3)],
            vec![Value::Int8(4)],
            vec![Value::Int8(5)],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn recursive_cte_aggregate() {
    let (storage, wal) = unique_engine("recursive-agg");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(
        &mut executor,
        "WITH RECURSIVE nums(n) AS (
            SELECT 1
            UNION ALL
            SELECT n + 1
            FROM nums
            WHERE n < 100
        )
        SELECT COUNT(*), SUM(n), MIN(n), MAX(n) FROM nums;",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int8(100));
    assert_eq!(rows[0][1], Value::Int8(5050));
    // MIN picks up the Int4 anchor value; MAX is the widened Int8 from the
    // recursive `n + 1` term.
    assert_eq!(rows[0][2], Value::Int4(1));
    assert_eq!(rows[0][3], Value::Int8(100));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
#[test]
fn recursive_cte_join_hierarchy() {
    let (storage, wal) = unique_engine("recursive-join");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_employees(&mut executor);

    let rows = exec(
        &mut executor,
        "WITH RECURSIVE hierarchy AS (
            SELECT id, name, manager_id, 0 AS level
            FROM recursive_test.employees
            WHERE manager_id IS NULL
            UNION ALL
            SELECT e.id, e.name, e.manager_id, h.level + 1
            FROM recursive_test.employees e
            JOIN hierarchy h ON e.manager_id = h.id
        )
        SELECT id, name, level
        FROM hierarchy
        ORDER BY level, id;",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int4(1), Value::Text("Alice".into()), Value::Int4(0)],
            vec![Value::Int4(6), Value::Text("Frank".into()), Value::Int4(0)],
            // `h.level + 1` widens to Int8, matching the engine's normal
            // integer-arithmetic promotion.
            vec![Value::Int4(2), Value::Text("Bob".into()), Value::Int8(1)],
            vec![
                Value::Int4(3),
                Value::Text("Charlie".into()),
                Value::Int8(1)
            ],
            vec![Value::Int4(7), Value::Text("Grace".into()), Value::Int8(1)],
            vec![Value::Int4(4), Value::Text("David".into()), Value::Int8(2)],
            vec![Value::Int4(5), Value::Text("Emma".into()), Value::Int8(2)],
            vec![Value::Int4(8), Value::Text("Henry".into()), Value::Int8(2)],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn recursive_cte_where_order_limit() {
    let (storage, wal) = unique_engine("recursive-where");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(
        &mut executor,
        "WITH RECURSIVE nums(n) AS (
            SELECT 1
            UNION ALL
            SELECT n + 1
            FROM nums
            WHERE n < 10
        )
        SELECT n
        FROM nums
        WHERE n >= 5
        ORDER BY n DESC
        LIMIT 3;",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int8(10)],
            vec![Value::Int8(9)],
            vec![Value::Int8(8)]
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
#[test]
fn recursive_cte_union_dedup() {
    let (storage, wal) = unique_engine("recursive-union");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Duplicate-eliminating UNION (no ALL). Each iteration's new rows are
    // filtered against everything already emitted before becoming the next
    // working set.
    let rows = exec(
        &mut executor,
        "WITH RECURSIVE nums(n) AS (
            SELECT 1
            UNION
            SELECT n + 1
            FROM nums
            WHERE n < 5
        )
        SELECT n FROM nums ORDER BY n;",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int8(2)],
            vec![Value::Int8(3)],
            vec![Value::Int8(4)],
            vec![Value::Int8(5)],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn recursive_cte_multiple_ctes() {
    let (storage, wal) = unique_engine("recursive-multi");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    setup_employees(&mut executor);

    // `base` is a plain CTE, `hierarchy` is recursive and references it.
    let rows = exec(
        &mut executor,
        "WITH RECURSIVE
            base AS (
                SELECT * FROM recursive_test.employees
            ),
            hierarchy AS (
                SELECT id, name, manager_id, 0 AS level
                FROM base
                WHERE manager_id IS NULL
                UNION ALL
                SELECT e.id, e.name, e.manager_id, h.level + 1
                FROM base e
                JOIN hierarchy h ON e.manager_id = h.id
            )
        SELECT id, name, level
        FROM hierarchy
        ORDER BY level, id;",
    );
    assert_eq!(rows.len(), 8);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn normal_cte_still_works() {
    let (storage, wal) = unique_engine("normal-cte");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(
        &mut executor,
        "WITH x AS (
            SELECT 1 AS n
        )
        SELECT n FROM x;",
    );
    assert_eq!(rows, vec![vec![Value::Int4(1)]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn recursive_cte_composed_with_all_outer_features() {
    let (storage, wal) = unique_engine("recursive-compose");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Composition: WHERE + GROUP BY + HAVING + aggregates + CASE + DISTINCT
    // over a recursive CTE flushed through the normal engine. (The engine
    // requires the GROUP BY expression to be spelled out rather than referenced
    // by its column alias, so the CASE is repeated in GROUP BY.)
    let rows = exec(
        &mut executor,
        "WITH RECURSIVE nums(n) AS (
            SELECT 1
            UNION ALL
            SELECT n + 1
            FROM nums
            WHERE n < 10
        )
        SELECT DISTINCT CASE WHEN n % 2 = 0 THEN 'even' ELSE 'odd' END AS parity,
               COUNT(*),
               SUM(n)
        FROM nums
        WHERE n >= 1
        GROUP BY CASE WHEN n % 2 = 0 THEN 'even' ELSE 'odd' END
        HAVING COUNT(*) >= 3
        ORDER BY parity;",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Text("even".into()), Value::Int8(5), Value::Int8(30)],
            vec![Value::Text("odd".into()), Value::Int8(5), Value::Int8(25)],
        ]
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn generate_series_scalar_regression() {
    let (storage, wal) = unique_engine("generate-series-scalar");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Bare SRF in the target list.
    assert_eq!(
        exec(&mut executor, "SELECT generate_series(1, 3);"),
        vec![
            vec![Value::Int8(1)],
            vec![Value::Int8(2)],
            vec![Value::Int8(3)]
        ],
    );

    // SRF inside an operator expression (SRF-in-targetlist expansion).
    assert_eq!(
        exec(&mut executor, "SELECT 1000 - generate_series(1, 3);"),
        vec![
            vec![Value::Int8(999)],
            vec![Value::Int8(998)],
            vec![Value::Int8(997)]
        ],
    );
    assert_eq!(
        exec(&mut executor, "SELECT generate_series(1, 3) - 2;"),
        vec![
            vec![Value::Int8(-1)],
            vec![Value::Int8(0)],
            vec![Value::Int8(1)]
        ],
    );
    assert_eq!(
        exec(&mut executor, "SELECT generate_series(1, 3) + 10;"),
        vec![
            vec![Value::Int8(11)],
            vec![Value::Int8(12)],
            vec![Value::Int8(13)]
        ],
    );

    // A bare alias of a single-column SRF relation resolves as the scalar
    // column value, not a whole-row composite.
    assert_eq!(
        exec(&mut executor, "SELECT gs FROM generate_series(1, 3) gs;"),
        vec![
            vec![Value::Int8(1)],
            vec![Value::Int8(2)],
            vec![Value::Int8(3)]
        ],
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT gs % 2 FROM generate_series(1, 3) gs;"
        ),
        vec![
            vec![Value::Int8(1)],
            vec![Value::Int8(0)],
            vec![Value::Int8(1)]
        ],
    );

    // Normal FROM / set-returning behaviour must remain intact.
    assert_eq!(
        exec(&mut executor, "SELECT * FROM generate_series(1, 3);"),
        vec![
            vec![Value::Int8(1)],
            vec![Value::Int8(2)],
            vec![Value::Int8(3)]
        ],
    );
    assert_eq!(
        exec(
            &mut executor,
            "SELECT v FROM generate_series(1, 3) AS g(v);"
        ),
        vec![
            vec![Value::Int8(1)],
            vec![Value::Int8(2)],
            vec![Value::Int8(3)]
        ],
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
// ---------------------------------------------------------------------------
// DML RETURNING metadata width
// ---------------------------------------------------------------------------
//
// Regression: `UPDATE/DELETE ... RETURNING <unaliased-column>` produced a Rows
// result whose `columns` metadata was derived from a filter that only kept
// explicitly-aliased targets. `RETURNING id`, `RETURNING credit_limit`, and
// bare-expression targets were silently dropped, so `columns.len()` was
// shorter than the returned row width. The network layer then rejected the
// result ("result column metadata does not match row width") and closed the
// connection. These tests assert columns.len() == column_types.len() == every
// row's width, which is exactly what the wire protocol requires.

#[test]
fn update_returning_metadata_width_and_values() {
    let (storage, wal) = unique_engine("update-returning");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE customers (\n\
            id BIGINT PRIMARY KEY,\n\
            credit_limit NUMERIC(12,2) NOT NULL DEFAULT 1000.00\n\
        );",
    );
    exec_ok(&mut executor, "INSERT INTO customers (id) VALUES (100);");

    // Plain UPDATE (no RETURNING) keeps the row-count result.
    assert_eq!(
        executor
            .execute("UPDATE customers SET credit_limit = credit_limit - 100 WHERE id = 100;")
            .unwrap(),
        QueryResult::updated(1)
    );
    // The NUMERIC default 1000.00 is reduced by 100 by the plain UPDATE.
    assert_eq!(
        exec(
            &mut executor,
            "SELECT credit_limit FROM customers WHERE id = 100;"
        )[0][0]
            .to_sql_text(),
        "900"
    );

    // RETURNING id: exactly 1 metadata column, 1 value, new row.
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "UPDATE customers SET credit_limit = credit_limit + 100 WHERE id = 100 RETURNING id;",
        vec!["id"],
    );
    assert_eq!(rows, vec![vec![Value::Int8(100)]]);

    // RETURNING credit_limit: exactly 1 column (the updated numeric value).
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "UPDATE customers SET credit_limit = credit_limit + 100 WHERE id = 100 RETURNING credit_limit;",
        vec!["credit_limit"],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert!(rows[0][0].to_sql_text().starts_with("1100"));

    // RETURNING id, credit_limit: 2 metadata columns, 2 values (the bug case).
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "UPDATE customers SET credit_limit = credit_limit + 100 WHERE id = 100 RETURNING id, credit_limit;",
        vec!["id", "credit_limit"],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int8(100));
    assert!(rows[0][1].to_sql_text().starts_with("1200"));

    // RETURNING *: all visible columns.
    assert_returning_widths(
        &mut executor,
        "UPDATE customers SET credit_limit = credit_limit + 100 WHERE id = 100 RETURNING *;",
        vec!["id", "credit_limit"],
    );

    // RETURNING qualified names.
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "UPDATE customers SET credit_limit = credit_limit + 100 WHERE id = 100 RETURNING customers.id, customers.credit_limit;",
        vec!["id", "credit_limit"],
    );
    assert_eq!(rows[0][0], Value::Int8(100));
    assert!(rows[0][1].to_sql_text().starts_with("1400"));
    // RETURNING an expression: still one metadata column per value.
    let (columns, _, rows) = assert_returning_widths(
        &mut executor,
        "UPDATE customers SET credit_limit = credit_limit + 100 WHERE id = 100 RETURNING id, credit_limit + 50;",
        vec!["id", "?column?"],
    );
    assert_eq!(rows[0][0], Value::Int8(100));
    assert!(rows[0][1].to_sql_text().starts_with("1550"));
    let _ = columns;

    // Multiple matching rows.
    exec_ok(
        &mut executor,
        "INSERT INTO customers (id, credit_limit) VALUES (101, 1000), (102, 2000);",
    );
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "UPDATE customers SET credit_limit = credit_limit + 100 WHERE id IN (101, 102) RETURNING id, credit_limit;",
        vec!["id", "credit_limit"],
    );
    assert_eq!(rows.len(), 2);
    let text: Vec<String> = rows
        .iter()
        .map(|r| format!("{}|{}", r[0].to_sql_text(), r[1].to_sql_text()))
        .collect();
    assert!(text
        .iter()
        .all(|t| t.starts_with("101|11") || t.starts_with("102|21")));

    // Zero matching rows: 0 rows, metadata still matches, no crash.
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "UPDATE customers SET credit_limit = credit_limit + 100 WHERE id = 999 RETURNING id, credit_limit;",
        vec!["id", "credit_limit"],
    );
    assert_eq!(rows.len(), 0);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn delete_returning_metadata_width() {
    let (storage, wal) = unique_engine("delete-returning");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE customers (id BIGINT PRIMARY KEY, credit_limit NUMERIC(12,2) NOT NULL DEFAULT 1000.00);");
    exec_ok(
        &mut executor,
        "INSERT INTO customers (id, credit_limit) VALUES (101, 1000), (102, 2000);",
    );

    // DELETE RETURNING id: single column.
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "DELETE FROM customers WHERE id = 101 RETURNING id;",
        vec!["id"],
    );
    assert_eq!(rows, vec![vec![Value::Int8(101)]]);

    // DELETE RETURNING id, credit_limit: two columns.
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "DELETE FROM customers WHERE id = 102 RETURNING id, credit_limit;",
        vec!["id", "credit_limit"],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int8(102));
    assert!(rows[0][1].to_sql_text().starts_with("2000"));

    // DELETE RETURNING a nonexistent id: 0 rows, no crash.
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "DELETE FROM customers WHERE id = 999 RETURNING id, credit_limit;",
        vec!["id", "credit_limit"],
    );
    assert_eq!(rows.len(), 0);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn insert_returning_star_regression() {
    let (storage, wal) = unique_engine("insert-returning");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE customers (id BIGINT PRIMARY KEY, credit_limit NUMERIC(12,2) NOT NULL DEFAULT 1000.00);");

    // Default coercion: `INSERT ... RETURNING *` must expose the coerced
    // default (200 | 1000), with metadata matching the row width.
    let (columns, _, rows) = assert_returning_widths(
        &mut executor,
        "INSERT INTO customers (id) VALUES (200) RETURNING *;",
        vec!["id", "credit_limit"],
    );
    assert_eq!(columns, vec!["id", "credit_limit"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int8(200));
    assert!(rows[0][1].to_sql_text().starts_with("1000"));

    // Unaliased explicit column RETURNING on INSERT.
    let (_, _, rows) = assert_returning_widths(
        &mut executor,
        "INSERT INTO customers (id, credit_limit) VALUES (201, 55) RETURNING id, credit_limit;",
        vec!["id", "credit_limit"],
    );
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[0][0], Value::Int8(201));
    assert_eq!(rows[0][1].to_sql_text(), "55");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
