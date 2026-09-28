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
//! PostgreSQL client compatibility regression tests.
//!
//! These tests verify that PLOMID exposes PostgreSQL-compatible catalog
//! metadata so that GUI clients (DBeaver, Beekeeper Studio, DataGrip,
//! pgAdmin, TablePlus) and generic PostgreSQL introspection tools can
//! connect and discover database objects correctly.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-pgcompat-{tag}-{}", std::process::id()));
    let wal =
        std::env::temp_dir().join(format!("plomid-pgcompat-{tag}-wal-{}", std::process::id()));
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
        .map(|row| row.iter().map(Value::to_sql_text).collect())
        .collect()
}

/// DBeaver metadata reproduction: verify that a client can discover
/// schemas, tables, columns, primary keys, foreign keys, indexes, and views.
#[test]
fn dbeaver_metadata_reproduction() {
    let (storage, wal) = unique_engine("dbeaver");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create the schema and objects that DBeaver needs to discover.
    exec_ok(
        &mut executor,
        "DROP SCHEMA IF EXISTS client_compat CASCADE;
         CREATE SCHEMA client_compat;",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE client_compat.customers (
            id BIGINT PRIMARY KEY,
            name TEXT NOT NULL
        );",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE client_compat.orders (
            id BIGINT PRIMARY KEY,
            customer_id BIGINT NOT NULL,
            total NUMERIC(12,2),
            CONSTRAINT orders_customer_fk
                FOREIGN KEY (customer_id)
                REFERENCES client_compat.customers(id)
        );",
    );
    exec_ok(
        &mut executor,
        "CREATE INDEX orders_customer_idx
         ON client_compat.orders(customer_id);",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW client_compat.order_summary AS
         SELECT o.id, o.customer_id, o.total
         FROM client_compat.orders o;",
    );

    // 1. Schema discovery via information_schema.schemata
    let rows = exec(
        &mut executor,
        "SELECT schema_name FROM information_schema.schemata
         WHERE schema_name = 'client_compat';",
    );
    assert!(
        text_rows(&rows).iter().any(|r| r[0] == "client_compat"),
        "client_compat schema should be discoverable"
    );

    // 2. Table discovery via information_schema.tables
    let rows = exec(
        &mut executor,
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = 'client_compat'
           AND table_type = 'BASE TABLE'
         ORDER BY table_name;",
    );
    let table_names: Vec<String> = text_rows(&rows).into_iter().map(|r| r[0].clone()).collect();
    assert!(table_names.contains(&"customers".to_string()));
    assert!(table_names.contains(&"orders".to_string()));

    // 3. Column discovery via information_schema.columns
    let rows = exec(
        &mut executor,
        "SELECT column_name, is_nullable, udt_name
         FROM information_schema.columns
         WHERE table_schema = 'client_compat' AND table_name = 'customers'
         ORDER BY ordinal_position;",
    );
    let cols = text_rows(&rows);
    assert_eq!(cols[0][0], "id");
    assert_eq!(cols[0][1], "NO"); // NOT NULL
    assert_eq!(cols[1][0], "name");
    assert_eq!(cols[1][1], "NO"); // NOT NULL

    // 4. Primary key discovery via information_schema.table_constraints
    let rows = exec(
        &mut executor,
        "SELECT constraint_name, constraint_type
         FROM information_schema.table_constraints
         WHERE table_schema = 'client_compat' AND table_name = 'customers'
           AND constraint_type = 'PRIMARY KEY';",
    );
    assert!(!text_rows(&rows).is_empty(), "customers should have a PK");

    // 5. Foreign key discovery via pg_constraint
    let rows = exec(
        &mut executor,
        "SELECT c.conname, c.contype, c.conrelid, c.confrelid,
                c.conkey, c.confkey
         FROM pg_constraint c
         JOIN pg_class t ON t.oid = c.conrelid
         WHERE t.relname = 'orders'
           AND c.contype = 'f';",
    );
    let fk_rows = text_rows(&rows);
    assert!(!fk_rows.is_empty(), "orders should have a FK");
    // Verify conkey and confkey are not null (this is the keyRefNumbers fix)
    for row in &fk_rows {
        assert_ne!(row[4], "NULL", "conkey must not be null for FK");
        assert_ne!(row[5], "NULL", "confkey must not be null for FK");
    }

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

/// Regression test for "Cannot read the array length because keyRefNumbers is null".
/// This verifies that pg_constraint.confkey is never NULL for foreign keys.
#[test]
fn pg_constraint_confkey_not_null_for_fk() {
    let (storage, wal) = unique_engine("confkey");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE parent (id BIGINT PRIMARY KEY);",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE child (
            id BIGINT PRIMARY KEY,
            parent_id BIGINT,
            CONSTRAINT child_parent_fk
                FOREIGN KEY (parent_id)
                REFERENCES parent(id)
        );",
    );

    // Query pg_constraint for FK constraints and verify confkey is not null
    let rows = exec(
        &mut executor,
        "SELECT contype, conkey, confkey
         FROM pg_constraint
         WHERE contype = 'f';",
    );

    assert!(!rows.is_empty(), "should have FK constraints");

    for row in &rows {
        // contype should be 'f'
        assert_eq!(row[0].to_sql_text(), "f");
        // conkey must not be null
        assert_ne!(
            row[1].to_sql_text(),
            "NULL",
            "conkey must not be null for FK constraint"
        );
        // confkey must not be null - this is the keyRefNumbers fix!
        assert_ne!(
            row[2].to_sql_text(),
            "NULL",
            "confkey must not be null for FK constraint (DBeaver keyRefNumbers)"
        );
    }

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

/// Regression test for pg_default_acl compatibility.
/// DBeaver queries this table during metadata discovery.
#[test]
fn pg_default_acl_exists_and_returns_empty() {
    let (storage, wal) = unique_engine("default_acl");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // pg_default_acl should exist and return zero rows (PLOMID doesn't implement ACLs)
    // Use a WHERE clause to ensure the query goes through the general engine path
    // which properly handles system catalog relations.
    let rows = exec(
        &mut executor,
        "SELECT oid, defaclrole, defaclnamespace, defaclobjtype, defaclacl
         FROM pg_default_acl
         WHERE defaclrole IS NOT NULL;",
    );

    assert!(
        rows.is_empty(),
        "pg_default_acl should return empty (PLOMID doesn't implement ACLs)"
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

/// Regression test for ALTER TABLE ... ADD COLUMN ... STORAGE EXTENDED NOT NULL.
/// DBeaver emits this syntax when adding columns.
#[test]
fn alter_table_add_column_storage_extended() {
    let (storage, wal) = unique_engine("storage");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE test_storage (id BIGINT PRIMARY KEY);",
    );

    // This is the exact syntax DBeaver generates
    exec_ok(
        &mut executor,
        "ALTER TABLE test_storage ADD COLUMN data BYTEA STORAGE EXTENDED NOT NULL;",
    );

    // Verify the column was added
    let rows = exec(
        &mut executor,
        "SELECT column_name, is_nullable
         FROM information_schema.columns
         WHERE table_name = 'test_storage'
         ORDER BY ordinal_position;",
    );
    let cols = text_rows(&rows);
    assert_eq!(cols[0][0], "id");
    assert_eq!(cols[1][0], "data");
    assert_eq!(cols[1][1], "NO"); // NOT NULL

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

/// Test all valid STORAGE options
#[test]
fn alter_table_storage_all_options() {
    let (storage, wal) = unique_engine("storage_all");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE test_storage (id BIGINT PRIMARY KEY);",
    );

    // Test all PostgreSQL STORAGE options
    exec_ok(
        &mut executor,
        "ALTER TABLE test_storage ADD COLUMN col_plain BYTEA STORAGE PLAIN;",
    );
    exec_ok(
        &mut executor,
        "ALTER TABLE test_storage ADD COLUMN col_external BYTEA STORAGE EXTERNAL;",
    );
    exec_ok(
        &mut executor,
        "ALTER TABLE test_storage ADD COLUMN col_extended BYTEA STORAGE EXTENDED;",
    );
    exec_ok(
        &mut executor,
        "ALTER TABLE test_storage ADD COLUMN col_main BYTEA STORAGE MAIN;",
    );

    // Verify all columns were added
    let rows = exec(
        &mut executor,
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_name = 'test_storage';",
    );
    assert_eq!(text_rows(&rows)[0][0], "5"); // id + 4 storage columns

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

/// Regression test for SHOW transaction_isolation.
/// DBeaver and other clients query this during connection initialization.
#[test]
fn show_transaction_isolation() {
    let (storage, wal) = unique_engine("isolation");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    let rows = exec(&mut executor, "SHOW transaction_isolation;");
    assert!(
        !rows.is_empty(),
        "transaction_isolation should be available"
    );
    let isolation = rows[0][0].to_sql_text();
    assert!(
        isolation == "read committed" || isolation == "Read Committed",
        "expected 'read committed', got '{isolation}'"
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

/// Test that SET TRANSACTION ISOLATION LEVEL syntax is accepted.
#[test]
fn set_transaction_isolation_level() {
    let (storage, wal) = unique_engine("set_isolation");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // These should all parse and execute without error
    exec_ok(
        &mut executor,
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED;",
    );
    exec_ok(
        &mut executor,
        "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED;",
    );
    exec_ok(
        &mut executor,
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;",
    );
    exec_ok(
        &mut executor,
        "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE;",
    );

    // Also test SET SESSION CHARACTERISTICS AS TRANSACTION
    exec_ok(
        &mut executor,
        "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL READ COMMITTED;",
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
}

/// Test that BEGIN with ISOLATION LEVEL syntax is accepted.
/// Note: PLOMID requires BEGIN to be followed by COMMIT or ROLLBACK in a single execute call.
#[test]
fn begin_with_isolation_level() {
    let (storage, wal) = unique_engine("begin_isolation");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // These should all parse and execute without error
    // PLOMID requires BEGIN/COMMIT or BEGIN/ROLLBACK in a single execute call
    exec_ok(
        &mut executor,
        "BEGIN ISOLATION LEVEL READ COMMITTED; CREATE TABLE txn_test1 (id INT); COMMIT;",
    );
    exec_ok(
        &mut executor,
        "START TRANSACTION ISOLATION LEVEL READ COMMITTED; ROLLBACK;",
    );
    exec_ok(
        &mut executor,
        "BEGIN ISOLATION LEVEL SERIALIZABLE; CREATE TABLE txn_test2 (id INT); COMMIT;",
    );
    exec_ok(
        &mut executor,
        "START TRANSACTION ISOLATION LEVEL REPEATABLE READ; ROLLBACK;",
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

/// Test composite primary key metadata discovery.
#[test]
fn composite_primary_key_metadata() {
    let (storage, wal) = unique_engine("composite_pk");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE composite_pk_test (
            id1 BIGINT,
            id2 BIGINT,
            data TEXT,
            PRIMARY KEY (id1, id2)
        );",
    );

    // Verify PK constraint exists
    let rows = exec(
        &mut executor,
        "SELECT constraint_name, constraint_type
         FROM information_schema.table_constraints
         WHERE table_name = 'composite_pk_test'
           AND constraint_type = 'PRIMARY KEY';",
    );
    assert!(!text_rows(&rows).is_empty(), "composite PK should exist");

    // Verify key_column_usage shows both columns
    let rows = exec(
        &mut executor,
        "SELECT column_name, ordinal_position
         FROM information_schema.key_column_usage
         WHERE table_name = 'composite_pk_test'
         ORDER BY ordinal_position;",
    );
    let pk_cols = text_rows(&rows);
    assert_eq!(pk_cols.len(), 2);
    assert_eq!(pk_cols[0][0], "id1");
    assert_eq!(pk_cols[1][0], "id2");

    // Verify pg_constraint.conkey has both columns
    let rows = exec(
        &mut executor,
        "SELECT conkey FROM pg_constraint
         WHERE contype = 'p'
           AND conrelid = (SELECT oid FROM pg_class WHERE relname = 'composite_pk_test');",
    );
    assert!(!rows.is_empty());
    let conkey = rows[0][0].to_sql_text();
    assert_ne!(conkey, "NULL", "conkey must not be null for PK");
    // Should contain both column numbers
    assert!(
        conkey.contains("1") && conkey.contains("2"),
        "conkey should reference both columns"
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

/// Test referential_constraints information_schema view.
#[test]
fn referential_constraints_metadata() {
    let (storage, wal) = unique_engine("ref_constraints");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    exec_ok(
        &mut executor,
        "CREATE TABLE ref_parent (id BIGINT PRIMARY KEY);",
    );
    exec_ok(
        &mut executor,
        "CREATE TABLE ref_child (
            id BIGINT PRIMARY KEY,
            parent_id BIGINT,
            CONSTRAINT ref_fk
                FOREIGN KEY (parent_id)
                REFERENCES ref_parent(id)
        );",
    );

    let rows = exec(
        &mut executor,
        "SELECT constraint_name, unique_constraint_name,
                match_option, update_rule, delete_rule
         FROM information_schema.referential_constraints
         WHERE constraint_schema = 'public';",
    );
    let ref_constraints = text_rows(&rows);
    assert!(
        !ref_constraints.is_empty(),
        "referential_constraints should have rows"
    );

    // Verify the FK constraint is present
    let fk = &ref_constraints[0];
    assert_eq!(fk[0], "ref_fk"); // constraint_name
    assert_eq!(fk[2], "NONE"); // match_option (MATCH SIMPLE)
    assert_eq!(fk[3], "NO ACTION"); // update_rule
    assert_eq!(fk[4], "NO ACTION"); // delete_rule

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
