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
    let storage =
        std::env::temp_dir().join(format!("plomid-is-nullable-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!(
        "plomid-is-nullable-{tag}-wal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn exec<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor.execute(sql).expect("sql should execute") {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

#[test]
fn test_plomid_test_users_is_nullable() {
    let (storage, wal) = unique_engine("plomid_test_users");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create the test table
    executor
        .execute(
            "CREATE TABLE plomid_test_users (
            id INT PRIMARY KEY,
            name TEXT NOT NULL,
            age INT,
            active BOOLEAN,
            score NUMERIC
        );",
        )
        .unwrap();

    // Test information_schema.columns for is_nullable
    let rows = exec(&mut executor, "SELECT column_name, is_nullable FROM information_schema.columns WHERE table_name = 'plomid_test_users' ORDER BY ordinal_position;");

    println!("Rows: {:?}", rows);

    assert_eq!(rows.len(), 5);

    // Check each row
    assert_eq!(rows[0][0], Value::Text("id".to_string()));
    assert_eq!(rows[0][1], Value::Text("NO".to_string()));

    assert_eq!(rows[1][0], Value::Text("name".to_string()));
    assert_eq!(rows[1][1], Value::Text("NO".to_string()));

    assert_eq!(rows[2][0], Value::Text("age".to_string()));
    assert_eq!(rows[2][1], Value::Text("YES".to_string()));

    assert_eq!(rows[3][0], Value::Text("active".to_string()));
    assert_eq!(rows[3][1], Value::Text("YES".to_string()));

    assert_eq!(rows[4][0], Value::Text("score".to_string()));
    assert_eq!(rows[4][1], Value::Text("YES".to_string()));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn test_nullable_test_table() {
    let (storage, wal) = unique_engine("nullable_test");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create the test table
    executor
        .execute(
            "CREATE TABLE nullable_test (
            id INT PRIMARY KEY,
            required_col TEXT NOT NULL,
            normal_col TEXT,
            unique_col TEXT UNIQUE
        );",
        )
        .unwrap();

    // Test information_schema.columns for is_nullable
    let rows = exec(&mut executor, "SELECT column_name, is_nullable FROM information_schema.columns WHERE table_name = 'nullable_test' ORDER BY ordinal_position;");

    println!("Rows: {:?}", rows);

    assert_eq!(rows.len(), 4);

    // Check each row
    assert_eq!(rows[0][0], Value::Text("id".to_string()));
    assert_eq!(rows[0][1], Value::Text("NO".to_string()));

    assert_eq!(rows[1][0], Value::Text("required_col".to_string()));
    assert_eq!(rows[1][1], Value::Text("NO".to_string()));

    assert_eq!(rows[2][0], Value::Text("normal_col".to_string()));
    assert_eq!(rows[2][1], Value::Text("YES".to_string()));

    assert_eq!(rows[3][0], Value::Text("unique_col".to_string()));
    assert_eq!(rows[3][1], Value::Text("YES".to_string()));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn test_all_nullable_table() {
    let (storage, wal) = unique_engine("all_nullable");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create the test table
    executor
        .execute(
            "CREATE TABLE all_nullable (
            a INT,
            b TEXT,
            c BOOLEAN
        );",
        )
        .unwrap();

    // Test information_schema.columns for is_nullable
    let rows = exec(&mut executor, "SELECT column_name, is_nullable FROM information_schema.columns WHERE table_name = 'all_nullable' ORDER BY ordinal_position;");

    println!("Rows: {:?}", rows);

    assert_eq!(rows.len(), 3);

    // Check each row
    assert_eq!(rows[0][0], Value::Text("a".to_string()));
    assert_eq!(rows[0][1], Value::Text("YES".to_string()));

    assert_eq!(rows[1][0], Value::Text("b".to_string()));
    assert_eq!(rows[1][1], Value::Text("YES".to_string()));

    assert_eq!(rows[2][0], Value::Text("c".to_string()));
    assert_eq!(rows[2][1], Value::Text("YES".to_string()));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn test_alter_table_add_column() {
    let (storage, wal) = unique_engine("alter_test");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create the test table
    executor
        .execute(
            "CREATE TABLE nullable_test (
            id INT PRIMARY KEY,
            required_col TEXT NOT NULL,
            normal_col TEXT,
            unique_col TEXT UNIQUE
        );",
        )
        .unwrap();

    // Add a column
    executor
        .execute("ALTER TABLE nullable_test ADD COLUMN added_col INT;")
        .unwrap();

    // Add a NOT NULL column
    executor
        .execute("ALTER TABLE nullable_test ADD COLUMN not_null_col INT NOT NULL;")
        .unwrap();

    // Test information_schema.columns for is_nullable
    let rows = exec(&mut executor, "SELECT column_name, is_nullable FROM information_schema.columns WHERE table_name = 'nullable_test' ORDER BY ordinal_position;");

    println!("Rows: {:?}", rows);

    assert_eq!(rows.len(), 6);

    // Check each row
    assert_eq!(rows[0][0], Value::Text("id".to_string()));
    assert_eq!(rows[0][1], Value::Text("NO".to_string()));

    assert_eq!(rows[1][0], Value::Text("required_col".to_string()));
    assert_eq!(rows[1][1], Value::Text("NO".to_string()));

    assert_eq!(rows[2][0], Value::Text("normal_col".to_string()));
    assert_eq!(rows[2][1], Value::Text("YES".to_string()));

    assert_eq!(rows[3][0], Value::Text("unique_col".to_string()));
    assert_eq!(rows[3][1], Value::Text("YES".to_string()));

    assert_eq!(rows[4][0], Value::Text("added_col".to_string()));
    assert_eq!(rows[4][1], Value::Text("YES".to_string()));

    assert_eq!(rows[5][0], Value::Text("not_null_col".to_string()));
    assert_eq!(rows[5][1], Value::Text("NO".to_string()));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn test_check_constraint_does_not_affect_nullable() {
    let (storage, wal) = unique_engine("check_test");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create a table with CHECK constraint
    executor
        .execute(
            "CREATE TABLE check_test (
            id INT PRIMARY KEY,
            age INT CHECK (age >= 0),
            score TEXT
        );",
        )
        .unwrap();

    // Test information_schema.columns for is_nullable
    let rows = exec(&mut executor, "SELECT column_name, is_nullable FROM information_schema.columns WHERE table_name = 'check_test' ORDER BY ordinal_position;");

    println!("Rows: {:?}", rows);

    assert_eq!(rows.len(), 3);

    // Check each row
    assert_eq!(rows[0][0], Value::Text("id".to_string()));
    assert_eq!(rows[0][1], Value::Text("NO".to_string())); // PRIMARY KEY

    assert_eq!(rows[1][0], Value::Text("age".to_string()));
    assert_eq!(rows[1][1], Value::Text("YES".to_string())); // CHECK constraint should NOT make it NO

    assert_eq!(rows[2][0], Value::Text("score".to_string()));
    assert_eq!(rows[2][1], Value::Text("YES".to_string()));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
