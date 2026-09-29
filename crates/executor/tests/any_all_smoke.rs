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

fn exec<E: plomid_txn::StorageEngine>(ex: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match ex.execute(sql).unwrap() {
        QueryResult::Rows { rows, .. } => rows,
        o => panic!("{} -> {:?}", sql, o),
    }
}

#[test]
fn any_all_smoke() {
    let s = std::env::temp_dir().join(format!("plomid-anycheck-{}", std::process::id()));
    let w = std::env::temp_dir().join(format!("plomid-anycheck-wal-{}", std::process::id()));
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&w);
    let eng = PlomidStorageEngine::create(&s, &w, 32).unwrap();
    let mut ex = Executor::new(eng).unwrap();
    let cases = [
        ("SELECT 10 > ANY (SELECT 5);", vec![vec![Value::Bool(true)]]),
        (
            "SELECT 10 < ALL (SELECT 20);",
            vec![vec![Value::Bool(true)]],
        ),
        (
            "SELECT 10 > SOME (SELECT 5);",
            vec![vec![Value::Bool(true)]],
        ),
        (
            "SELECT 10 > ANY (SELECT 20);",
            vec![vec![Value::Bool(false)]],
        ),
        ("SELECT 10 > ALL (SELECT 5);", vec![vec![Value::Bool(true)]]),
        (
            "SELECT 10 > ALL (SELECT 20);",
            vec![vec![Value::Bool(false)]],
        ),
        ("SELECT 10 > ANY (SELECT NULL);", vec![vec![Value::Null]]),
        ("SELECT 10 > ALL (SELECT NULL);", vec![vec![Value::Null]]),
        (
            "SELECT 10 > ANY (SELECT 5 WHERE FALSE);",
            vec![vec![Value::Bool(false)]],
        ),
        (
            "SELECT 10 > ALL (SELECT 5 WHERE FALSE);",
            vec![vec![Value::Bool(true)]],
        ),
        (
            "SELECT 10 = ANY (SELECT 5);",
            vec![vec![Value::Bool(false)]],
        ),
        ("SELECT 5 = ANY (SELECT 5);", vec![vec![Value::Bool(true)]]),
        (
            "SELECT 10 <> ALL (SELECT 5);",
            vec![vec![Value::Bool(true)]],
        ),
        (
            "SELECT 10 = ANY (SELECT 20);",
            vec![vec![Value::Bool(false)]],
        ),
        ("SELECT NULL > ANY (SELECT 5);", vec![vec![Value::Null]]),
        ("SELECT NULL > ALL (SELECT 5);", vec![vec![Value::Null]]),
        (
            "SELECT 10 <= ALL (SELECT 20);",
            vec![vec![Value::Bool(true)]],
        ),
        (
            "SELECT 10 >= ANY (SELECT 20);",
            vec![vec![Value::Bool(false)]],
        ),
        (
            "SELECT 10 < SOME (SELECT 20);",
            vec![vec![Value::Bool(true)]],
        ),
        (
            "SELECT 2 = ANY(ARRAY[1,2,3]);",
            vec![vec![Value::Bool(true)]],
        ),
    ];
    for (q, expected) in cases {
        assert_eq!(exec(&mut ex, q), expected, "query: {}", q);
    }
    for q in [
        "DROP SCHEMA IF EXISTS plomid_any_test CASCADE;",
        "CREATE SCHEMA plomid_any_test;",
        "SET search_path TO plomid_any_test, public;",
        "CREATE TABLE products (id INTEGER, price INTEGER);",
        "CREATE TABLE orders (total_amount INTEGER);",
        "INSERT INTO products VALUES (1,1200),(2,800),(3,100),(4,50);",
        "INSERT INTO orders VALUES (1300),(850),(500),(1200),(100);",
    ] {
        ex.execute(q).unwrap();
    }
    let rows = exec(
        &mut ex,
        "SELECT * FROM products WHERE price > ANY (SELECT total_amount FROM orders) ORDER BY id;",
    );
    // PostgreSQL semantics: 1200 > 100 true, 800 > 500 true,
    // 100 > ANY {1300,850,500,1200,100} false (100 > 100 is false),
    // 50 > ANY ... false. So exactly 2 rows qualify.
    assert_eq!(rows.len(), 2, "ANY table rows: {:?}", rows);
    let rows2 = exec(
        &mut ex,
        "SELECT * FROM products WHERE price < ALL (SELECT total_amount FROM orders) ORDER BY id;",
    );
    assert_eq!(rows2.len(), 1, "ALL table rows: {:?}", rows2);
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&w);
}
