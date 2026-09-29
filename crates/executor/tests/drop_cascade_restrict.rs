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
use plomid_sql::Value;
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-drop-cascade-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!(
        "plomid-drop-cascade-{tag}-wal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn exec_ok<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) {
    executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"));
}

fn exec_err<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> String {
    match executor.execute(sql) {
        Ok(plomid_sql::QueryResult::Rows { rows, .. }) => {
            panic!("{sql} should fail, got rows={rows:?}")
        }
        Ok(other) => panic!("{sql} should fail, got {other:?}"),
        Err(e) => e.to_string(),
    }
}

fn exec<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor.execute(sql) {
        Ok(plomid_sql::QueryResult::Rows { rows, .. }) => rows,
        Ok(other) => panic!("{sql} expected rows, got {other:?}"),
        Err(e) => panic!("{sql} should succeed: {e}"),
    }
}

#[test]
fn drop_table_cascade_removes_dependent_views() {
    let (storage, wal) = unique_engine("drop-table-cascade");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob');",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW user_names AS SELECT id, name FROM users;",
    );

    let err = exec_err(&mut executor, "DROP TABLE users RESTRICT;");
    assert!(err.contains("depend"), "unexpected error: {err}");

    exec_ok(&mut executor, "DROP TABLE users CASCADE;");

    let err = exec_err(&mut executor, "SELECT * FROM user_names;");
    assert!(
        err.contains("does not exist") || err.contains("not found"),
        "view should be dropped: {err}"
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn drop_table_restrict_succeeds_without_dependencies() {
    let (storage, wal) = unique_engine("drop-table-restrict");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE standalone (id INTEGER PRIMARY KEY);",
    );
    exec_ok(&mut executor, "DROP TABLE standalone RESTRICT;");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn drop_view_cascade_and_restrict_both_work() {
    let (storage, wal) = unique_engine("drop-view-cascade");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TABLE t (id INTEGER PRIMARY KEY);");
    exec_ok(&mut executor, "CREATE VIEW v AS SELECT * FROM t;");
    exec_ok(&mut executor, "DROP VIEW v RESTRICT;");

    exec_ok(&mut executor, "CREATE VIEW v2 AS SELECT * FROM t;");
    exec_ok(&mut executor, "DROP VIEW v2 CASCADE;");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn create_view_in_custom_schema_with_public_view_of_same_name() {
    let (storage, wal) = unique_engine("view-search-path");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create a view in the public schema with a specific name.
    exec_ok(
        &mut executor,
        "CREATE VIEW public.customer_summary AS SELECT 1 AS id;",
    );

    // Switch the search_path to a custom schema first.
    exec_ok(&mut executor, "CREATE SCHEMA custom;");
    exec_ok(&mut executor, "SET search_path TO custom, public;");

    // Creating a view with the same unqualified name should succeed
    // because it is placed in the custom schema, not public.
    exec_ok(
        &mut executor,
        "CREATE VIEW customer_summary AS SELECT 2 AS id;",
    );

    // Both views should be queryable via schema qualification.
    let rows = exec(&mut executor, "SELECT * FROM custom.customer_summary;");
    assert_eq!(rows, vec![vec![Value::Int4(2)]]);

    let rows = exec(&mut executor, "SELECT * FROM public.customer_summary;");
    assert_eq!(rows, vec![vec![Value::Int4(1)]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn create_view_rejected_when_same_relation_exists_in_same_schema() {
    let (storage, wal) = unique_engine("view-same-schema-conflict");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create a view first.
    exec_ok(&mut executor, "CREATE VIEW v AS SELECT 1;");

    // Trying to create another view with the same name should fail.
    let err = exec_err(&mut executor, "CREATE VIEW v AS SELECT 2;");
    assert!(
        err.contains("already exists"),
        "expected 'already exists', got: {err}"
    );

    // Drop and recreate should work.
    exec_ok(&mut executor, "DROP VIEW v;");
    exec_ok(&mut executor, "CREATE VIEW v AS SELECT 2;");

    let rows = exec(&mut executor, "SELECT * FROM v;");
    assert_eq!(rows, vec![vec![Value::Int4(2)]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn create_schema_qualified_view() {
    let (storage, wal) = unique_engine("view-schema-qualified");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE SCHEMA analytics;");

    // Create a schema-qualified view.
    exec_ok(
        &mut executor,
        "CREATE VIEW analytics.revenue_summary AS SELECT 100 AS total;",
    );

    // Query it via schema qualification.
    let rows = exec(&mut executor, "SELECT * FROM analytics.revenue_summary;");
    assert_eq!(rows, vec![vec![Value::Int4(100)]]);

    // Verify it appears in pg_views with the correct schema.
    let rows = exec(
        &mut executor,
        "SELECT schemaname, viewname FROM pg_views WHERE viewname = 'revenue_summary';",
    );
    assert_eq!(
        rows,
        vec![vec![
            Value::Text("analytics".into()),
            Value::Text("revenue_summary".into())
        ]]
    );

    // Creating an unqualified view with the same name in a different schema should succeed.
    exec_ok(&mut executor, "CREATE SCHEMA reporting;");
    exec_ok(&mut executor, "SET search_path TO reporting;");
    exec_ok(
        &mut executor,
        "CREATE VIEW revenue_summary AS SELECT 200 AS total;",
    );

    let rows = exec(&mut executor, "SELECT * FROM reporting.revenue_summary;");
    assert_eq!(rows, vec![vec![Value::Int4(200)]]);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn create_and_drop_type_enum() {
    let (storage, wal) = unique_engine("create-type-enum");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TYPE status AS ENUM ('active', 'inactive');",
    );
    exec_ok(&mut executor, "DROP TYPE status RESTRICT;");

    exec_ok(
        &mut executor,
        "CREATE TYPE priority AS ENUM ('low', 'medium', 'high');",
    );
    exec_ok(&mut executor, "DROP TYPE priority CASCADE;");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn drop_type_restrict_refuses_when_used() {
    let (storage, wal) = unique_engine("drop-type-restrict");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TYPE status AS ENUM ('active', 'inactive');",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE tasks (id INTEGER PRIMARY KEY, status status);",
    );

    let err = exec_err(&mut executor, "DROP TYPE status RESTRICT;");
    assert!(err.contains("depend"), "unexpected error: {err}");

    exec_ok(&mut executor, "DROP TYPE status CASCADE;");

    exec_ok(&mut executor, "INSERT INTO tasks (id) VALUES (1);");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn create_and_drop_domain() {
    let (storage, wal) = unique_engine("create-domain");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE DOMAIN email_addr AS TEXT;");
    exec_ok(&mut executor, "DROP DOMAIN email_addr RESTRICT;");

    exec_ok(
        &mut executor,
        "CREATE DOMAIN email_addr AS TEXT CHECK (POSITION('@' IN VALUE) > 1);",
    );
    exec_ok(&mut executor, "DROP DOMAIN email_addr CASCADE;");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn drop_domain_restrict_refuses_when_used() {
    let (storage, wal) = unique_engine("drop-domain-restrict");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE DOMAIN email_addr AS TEXT;");
    exec_ok(
        &mut executor,
        "CREATE TABLE contacts (id INTEGER PRIMARY KEY, email email_addr);",
    );

    let err = exec_err(&mut executor, "DROP DOMAIN email_addr RESTRICT;");
    assert!(err.contains("depend"), "unexpected error: {err}");

    exec_ok(&mut executor, "DROP DOMAIN email_addr CASCADE;");

    exec_ok(&mut executor, "INSERT INTO contacts (id) VALUES (1);");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn create_execute_and_drop_function() {
    let (storage, wal) = unique_engine("create-function");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE FUNCTION calculate_total(integer) RETURNS integer LANGUAGE SQL AS $$ SELECT $1 * 2; $$;",
    );

    let result = executor
        .execute("SELECT calculate_total(21);")
        .unwrap_or_else(|e| panic!("function call should succeed: {e}"));
    let plomid_sql::QueryResult::Rows { rows, .. } = result else {
        panic!("expected rows from function call");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].to_sql_text(), "42", "calculate_total(21) = 42");

    exec_ok(
        &mut executor,
        "DROP FUNCTION calculate_total(integer) RESTRICT;",
    );
    exec_ok(
        &mut executor,
        "CREATE FUNCTION calculate_total(integer) RETURNS integer LANGUAGE SQL AS $$ SELECT $1 * 2; $$;",
    );
    exec_ok(
        &mut executor,
        "DROP FUNCTION calculate_total(integer) CASCADE;",
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn schema_qualified_type_and_function() {
    let (storage, wal) = unique_engine("schema-qualified");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE SCHEMA test_db;");
    exec_ok(
        &mut executor,
        "CREATE TYPE test_db.status AS ENUM ('active', 'inactive');",
    );
    exec_ok(&mut executor, "DROP TYPE test_db.status RESTRICT;");
    exec_ok(
        &mut executor,
        "CREATE TYPE test_db.status AS ENUM ('active', 'inactive');",
    );
    exec_ok(&mut executor, "DROP TYPE test_db.status CASCADE;");

    exec_ok(
        &mut executor,
        "CREATE FUNCTION test_db.calculate_total(integer) RETURNS integer LANGUAGE SQL AS $$ SELECT $1 * 2; $$;",
    );
    exec_ok(
        &mut executor,
        "DROP FUNCTION test_db.calculate_total(integer) CASCADE;",
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn alter_table_drop_column_cascade_restrict_with_view() {
    let (storage, wal) = unique_engine("alter-drop-column");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT, price INTEGER);",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW item_labels AS SELECT id, label FROM items;",
    );

    let err = exec_err(
        &mut executor,
        "ALTER TABLE items DROP COLUMN label RESTRICT;",
    );
    assert!(err.contains("depend"), "unexpected error: {err}");

    exec_ok(
        &mut executor,
        "ALTER TABLE items DROP COLUMN label CASCADE;",
    );

    let err = exec_err(&mut executor, "SELECT * FROM item_labels;");
    assert!(
        err.contains("does not exist") || err.contains("not found"),
        "dependent view should be dropped: {err}"
    );

    exec_ok(
        &mut executor,
        "ALTER TABLE items DROP COLUMN price RESTRICT;",
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn create_view_same_name_as_table_rejected() {
    let (storage, wal) = unique_engine("view-table-conflict");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE conflict (id INTEGER PRIMARY KEY);",
    );
    let err = exec_err(&mut executor, "CREATE VIEW conflict AS SELECT 1;");
    assert!(
        err.contains("already exists"),
        "expected relation already exists, got: {err}"
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn drop_type_cascade_succeeds_without_dependencies() {
    let (storage, wal) = unique_engine("drop-type-cascade-nodeps");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE TYPE mood AS ENUM ('happy', 'sad');");
    exec_ok(&mut executor, "DROP TYPE mood CASCADE;");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn drop_domain_cascade_succeeds_without_dependencies() {
    let (storage, wal) = unique_engine("drop-domain-cascade-nodeps");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE DOMAIN us_postal_code AS TEXT;");
    exec_ok(&mut executor, "DROP DOMAIN us_postal_code CASCADE;");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn schema_qualified_custom_type_in_table_definition() {
    let (storage, wal) = unique_engine("schema-qualified-type-col");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(&mut executor, "CREATE SCHEMA hr;");
    exec_ok(
        &mut executor,
        "CREATE TYPE hr.employment_type AS ENUM ('fulltime', 'parttime');",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE hr.employees (id INTEGER PRIMARY KEY, etype hr.employment_type);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO hr.employees VALUES (1, 'fulltime');",
    );

    let err = exec_err(&mut executor, "DROP TYPE hr.employment_type RESTRICT;");
    assert!(err.contains("depend"), "unexpected error: {err}");

    exec_ok(&mut executor, "DROP TYPE hr.employment_type CASCADE;");
    exec_ok(&mut executor, "INSERT INTO hr.employees (id) VALUES (2);");

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
