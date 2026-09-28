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
    let storage = std::env::temp_dir().join(format!("plomid-rel-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!("plomid-rel-{tag}-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn rows<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor.execute(sql).expect("SQL should execute") {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

#[test]
fn order_by_limit_preserves_nulls_and_multiple_keys() {
    let (storage, wal) = unique_engine("order");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    executor
        .execute("CREATE TABLE ranked (id INTEGER, score INTEGER, bucket TEXT);")
        .unwrap();
    executor
        .execute(
            "INSERT INTO ranked VALUES
             (1, 10, 'b'), (2, 10, 'a'), (3, NULL, 'z'),
             (4, 20, 'a'), (5, 20, 'b');",
        )
        .unwrap();

    assert_eq!(
        rows(
            &mut executor,
            "SELECT id FROM ranked ORDER BY score ASC NULLS FIRST LIMIT 1;"
        ),
        vec![vec![Value::Int4(3)]]
    );
    assert_eq!(
        rows(
            &mut executor,
            "SELECT id FROM ranked ORDER BY score DESC NULLS LAST, bucket ASC LIMIT 3;"
        ),
        vec![
            vec![Value::Int4(4)],
            vec![Value::Int4(5)],
            vec![Value::Int4(2)]
        ]
    );
    assert_eq!(
        rows(
            &mut executor,
            "SELECT id FROM ranked ORDER BY score ASC NULLS LAST, bucket DESC LIMIT 2 OFFSET 1;"
        ),
        vec![vec![Value::Int4(2)], vec![Value::Int4(5)]]
    );
}

#[test]
fn aggregation_handles_empty_and_null_heavy_input() {
    let (storage, wal) = unique_engine("aggregate");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    executor
        .execute("CREATE TABLE measures (group_id INTEGER, value INTEGER);")
        .unwrap();

    assert_eq!(
        rows(
            &mut executor,
            "SELECT COUNT(*), SUM(value), AVG(value), MIN(value), MAX(value) FROM measures;"
        ),
        vec![vec![
            Value::Int8(0),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null
        ]]
    );
    executor
        .execute(
            "INSERT INTO measures VALUES
             (1, NULL), (1, 10), (1, 20), (2, NULL), (2, NULL);",
        )
        .unwrap();
    match executor
        .execute("SELECT AVG(value) FROM measures;")
        .unwrap()
    {
        QueryResult::Rows {
            column_types, rows, ..
        } => {
            assert_eq!(
                rows[0][0],
                Value::Numeric(plomid_types::Numeric::new(15, 0))
            );
            assert_eq!(
                column_types[0].map(|ty| ty.type_oid),
                Some(plomid_types::TypeOid::NUMERIC)
            );
        }
        other => panic!("expected aggregate rows, got {other:?}"),
    }
    let grouped = rows(
        &mut executor,
        "SELECT group_id, COUNT(value), SUM(value), AVG(value) FROM measures GROUP BY group_id ORDER BY group_id;",
    );
    assert_eq!(grouped[0][0], Value::Int4(1));
    assert_eq!(grouped[0][1], Value::Int8(2));
    assert_eq!(grouped[0][2], Value::Int8(30));
    assert_eq!(
        grouped[1],
        vec![Value::Int4(2), Value::Int8(0), Value::Null, Value::Null]
    );
}

#[test]
fn inner_equi_join_hash_path_preserves_duplicates_and_null_semantics() {
    let (storage, wal) = unique_engine("join");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();
    executor
        .execute("CREATE TABLE left_rows (id INTEGER, name TEXT);")
        .unwrap();
    executor
        .execute("CREATE TABLE right_rows (id INTEGER, amount INTEGER);")
        .unwrap();
    executor
        .execute("INSERT INTO left_rows VALUES (1, 'a'), (1, 'b'), (NULL, 'n');")
        .unwrap();
    executor
        .execute("INSERT INTO right_rows VALUES (1, 10), (1, 20), (NULL, 99);")
        .unwrap();

    let result = rows(
        &mut executor,
        "SELECT l.name, r.amount FROM left_rows l JOIN right_rows r ON l.id = r.id ORDER BY l.name, r.amount;",
    );
    assert_eq!(
        result,
        vec![
            vec![Value::Text("a".into()), Value::Int4(10)],
            vec![Value::Text("a".into()), Value::Int4(20)],
            vec![Value::Text("b".into()), Value::Int4(10)],
            vec![Value::Text("b".into()), Value::Int4(20)],
        ]
    );
}
