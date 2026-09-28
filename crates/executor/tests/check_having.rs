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
        std::env::temp_dir().join(format!("plomid-check-having-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!(
        "plomid-check-having-{tag}-wal-{}",
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
fn check_constraint_enforces_negative_values() {
    let (storage, wal) = unique_engine("check");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create table with CHECK constraint for non-negative age
    executor
        .execute("CREATE TABLE ages (id INTEGER, age INTEGER CHECK (age >= 0));")
        .unwrap();

    // Insert valid value
    executor
        .execute("INSERT INTO ages VALUES (1, 25);")
        .unwrap();

    // Insert zero (boundary case)
    executor.execute("INSERT INTO ages VALUES (2, 0);").unwrap();

    // Try to insert negative value - should fail
    let result = executor.execute("INSERT INTO ages VALUES (3, -1);");
    assert!(result.is_err(), "Negative CHECK value should be rejected");

    // Try to insert another negative value - should fail
    let result = executor.execute("INSERT INTO ages VALUES (4, -10);");
    assert!(result.is_err(), "Negative CHECK value should be rejected");

    // Verify valid rows were inserted
    let rows = exec(&mut executor, "SELECT * FROM ages ORDER BY id;");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Int4(25));
    assert_eq!(rows[1][1], Value::Int4(0));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn check_constraint_with_strict_inequality() {
    let (storage, wal) = unique_engine("check_strict");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create table with CHECK constraint using strict inequality
    executor
        .execute("CREATE TABLE scores (id INTEGER, score INTEGER CHECK (score > 0));")
        .unwrap();

    // Insert valid positive value
    executor
        .execute("INSERT INTO scores VALUES (1, 1);")
        .unwrap();

    // Try to insert zero - should fail (not > 0)
    let result = executor.execute("INSERT INTO scores VALUES (2, 0);");
    assert!(result.is_err(), "Zero should not pass CHECK (score > 0)");

    // Try to insert negative value - should fail
    let result = executor.execute("INSERT INTO scores VALUES (3, -5);");
    assert!(
        result.is_err(),
        "Negative value should not pass CHECK (score > 0)"
    );

    // Verify only valid row was inserted
    let rows = exec(&mut executor, "SELECT * FROM scores;");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Int4(1));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn having_filters_groups_correctly() {
    let (storage, wal) = unique_engine("having");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create test table
    executor
        .execute("CREATE TABLE sales (id INTEGER, product_id INTEGER, amount INTEGER);")
        .unwrap();

    // Insert test data
    executor
        .execute("INSERT INTO sales VALUES (1, 1, 100);")
        .unwrap();
    executor
        .execute("INSERT INTO sales VALUES (2, 1, 50);")
        .unwrap();
    executor
        .execute("INSERT INTO sales VALUES (3, 2, 30);")
        .unwrap();
    executor
        .execute("INSERT INTO sales VALUES (4, 2, 40);")
        .unwrap();
    executor
        .execute("INSERT INTO sales VALUES (5, 3, 10);")
        .unwrap();

    // Test HAVING with aggregate: only show products with total sales > 100
    let rows = exec(&mut executor, "SELECT product_id, SUM(amount) as total FROM sales GROUP BY product_id HAVING SUM(amount) > 100;");
    assert_eq!(rows.len(), 1);

    // Product 1: 100 + 50 = 150 (> 100) -> included
    // Product 2: 30 + 40 = 70 (<= 100) -> excluded
    // Product 3: 10 (<= 100) -> excluded

    // Find product 1 row
    let product1_row = rows.iter().find(|r| r[0] == Value::Int4(1)).unwrap();
    assert_eq!(product1_row[1], Value::Int8(150));

    // Find product 3 row (should not exist since sum is 10)
    let product3_row = rows.iter().find(|r| r[0] == Value::Int4(3));
    assert!(
        product3_row.is_none(),
        "Product 3 should be filtered out by HAVING"
    );

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn having_with_count_aggregate() {
    let (storage, wal) = unique_engine("having_count");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create test table
    executor
        .execute("CREATE TABLE orders (id INTEGER, customer_id INTEGER, order_id INTEGER);")
        .unwrap();

    // Insert test data
    executor
        .execute("INSERT INTO orders VALUES (1, 1, 101);")
        .unwrap();
    executor
        .execute("INSERT INTO orders VALUES (2, 1, 102);")
        .unwrap();
    executor
        .execute("INSERT INTO orders VALUES (3, 1, 103);")
        .unwrap();
    executor
        .execute("INSERT INTO orders VALUES (4, 2, 201);")
        .unwrap();
    executor
        .execute("INSERT INTO orders VALUES (5, 3, 301);")
        .unwrap();
    executor
        .execute("INSERT INTO orders VALUES (6, 3, 302);")
        .unwrap();

    // Test HAVING COUNT: only show customers with more than 2 orders
    let rows = exec(&mut executor, "SELECT customer_id, COUNT(*) as order_count FROM orders GROUP BY customer_id HAVING COUNT(*) > 2;");
    assert_eq!(rows.len(), 1);

    // Only customer 1 has 3 orders (> 2)
    let row = &rows[0];
    assert_eq!(row[0], Value::Int4(1));
    assert_eq!(row[1], Value::Int8(3));

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}

#[test]
fn having_without_group_by() {
    let (storage, wal) = unique_engine("having_no_group");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Create test table
    executor
        .execute("CREATE TABLE test_values (id INTEGER, val INTEGER);")
        .unwrap();

    // Insert test data
    executor
        .execute("INSERT INTO test_values VALUES (1, 5);")
        .unwrap();
    executor
        .execute("INSERT INTO test_values VALUES (2, 15);")
        .unwrap();
    executor
        .execute("INSERT INTO test_values VALUES (3, 25);")
        .unwrap();

    // Test HAVING without GROUP BY (applies to entire table)
    let rows = exec(
        &mut executor,
        "SELECT COUNT(*) as cnt FROM test_values HAVING COUNT(*) > 2;",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int8(3));

    // Test HAVING without GROUP BY that fails
    let rows = exec(
        &mut executor,
        "SELECT COUNT(*) as cnt FROM test_values HAVING COUNT(*) > 5;",
    );
    assert_eq!(rows.len(), 0);

    drop(executor);
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
}
