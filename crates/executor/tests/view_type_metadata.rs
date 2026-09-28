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
use plomid_types::TypeOid;

fn unique_engine(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let storage =
        std::env::temp_dir().join(format!("plomid-view-typed-{tag}-{}", std::process::id()));
    let wal = std::env::temp_dir().join(format!(
        "plomid-view-typed-{tag}-wal-{}",
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

fn exec<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"))
    {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("{sql} expected rows, got {other:?}"),
    }
}

fn view_column_types<E: plomid_txn::StorageEngine>(
    executor: &mut Executor<E>,
    schema: &str,
    view: &str,
) -> Vec<(String, String)> {
    let rows = exec(
        executor,
        &format!(
            "SELECT column_name, data_type FROM information_schema.columns \
             WHERE table_schema = '{schema}' AND table_name = '{view}' ORDER BY ordinal_position"
        ),
    );
    rows.into_iter()
        .map(|r| {
            let name = match &r[0] {
                Value::Text(s) => s.clone(),
                _ => format!("{:?}", r[0]),
            };
            let ty = match &r[1] {
                Value::Text(s) => s.clone(),
                _ => format!("{:?}", r[1]),
            };
            (name, ty)
        })
        .collect()
}

fn pg_typeof_types<E: plomid_txn::StorageEngine>(
    executor: &mut Executor<E>,
    sql: &str,
) -> Vec<String> {
    let rows = executor
        .execute(sql)
        .unwrap_or_else(|e| panic!("{sql} should succeed: {e}"));
    let QueryResult::Rows { rows, .. } = rows else {
        panic!("{sql} expected rows")
    };
    rows.into_iter()
        .next()
        .map(|r| {
            r.into_iter()
                .filter_map(|v| match v {
                    Value::Text(s) => Some(s),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pg_type_to_typeoid(name: &str) -> TypeOid {
    match name.trim() {
        "int8" => TypeOid::INT8,
        "int" | "int4" => TypeOid::INT4,
        "smallint" | "int2" => TypeOid::INT2,
        "numeric" | "decimal" => TypeOid::NUMERIC,
        "real" | "float4" => TypeOid::FLOAT4,
        "double precision" | "float8" => TypeOid::FLOAT8,
        "text" => TypeOid::TEXT,
        "varchar" => TypeOid::VARCHAR,
        "boolean" | "bool" => TypeOid::BOOL,
        "date" => TypeOid::DATE,
        "timestamp without time zone" | "timestamp" => TypeOid::TIMESTAMP,
        "timestamp with time zone" | "timestamptz" => TypeOid::TIMESTAMPTZ,
        "json" => TypeOid::JSON,
        "jsonb" => TypeOid::JSONB,
        other => panic!("unexpected pg_typeof value: {other}"),
    }
}

fn catalog_type_to_typeoid(data_type: &str) -> TypeOid {
    match data_type.trim() {
        "int8" => TypeOid::INT8,
        "int4" => TypeOid::INT4,
        "smallint" => TypeOid::INT2,
        "numeric" | "decimal" => TypeOid::NUMERIC,
        "real" => TypeOid::FLOAT4,
        "double precision" => TypeOid::FLOAT8,
        "text" => TypeOid::TEXT,
        "varchar" => TypeOid::VARCHAR,
        "boolean" => TypeOid::BOOL,
        "date" => TypeOid::DATE,
        "timestamp without time zone" | "timestamp" => TypeOid::TIMESTAMP,
        "timestamp with time zone" => TypeOid::TIMESTAMPTZ,
        "json" => TypeOid::JSON,
        "jsonb" => TypeOid::JSONB,
        other => panic!("unexpected data_type value: {other}"),
    }
}

#[test]
fn view_direct_columns_preserve_base_types() {
    let (storage, wal) = unique_engine("direct");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    executor.execute("CREATE SCHEMA view_type_test").unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE view_type_test.t (id BIGINT, name VARCHAR(100), age INTEGER);",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW view_type_test.v AS SELECT id, name, age FROM view_type_test.t;",
    );

    let types = view_column_types(&mut executor, "view_type_test", "v");
    assert_eq!(types.len(), 3);
    assert_eq!(types[0], ("id".to_string(), "int8".to_string()));
    assert_eq!(types[1], ("name".to_string(), "varchar".to_string()));
    assert_eq!(types[2], ("age".to_string(), "int4".to_string()));
}

#[test]
fn view_aggregate_expressions_preserve_types() {
    let (storage, wal) = unique_engine("agg");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    executor.execute("CREATE SCHEMA view_type_test").unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE view_type_test.t (id INTEGER, amount NUMERIC(10,2));",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW view_type_test.v AS \
         SELECT COUNT(id) AS cnt, SUM(amount) AS total, AVG(amount) AS avg_amount \
         FROM view_type_test.t;",
    );

    let types = view_column_types(&mut executor, "view_type_test", "v");
    assert_eq!(types.len(), 3);
    assert_eq!(types[0], ("cnt".to_string(), "int8".to_string()));
    assert_eq!(types[1], ("total".to_string(), "numeric".to_string()));
    assert_eq!(types[2], ("avg_amount".to_string(), "numeric".to_string()));
}

#[test]
fn view_expression_types_resolve_correctly() {
    let (storage, wal) = unique_engine("expr");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    executor.execute("CREATE SCHEMA view_type_test").unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE view_type_test.t (id BIGINT, name VARCHAR(100));",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW view_type_test.v AS \
         SELECT id + 1 AS next_id, CAST(id AS TEXT) AS id_text, \
                COALESCE(name, 'unknown') AS display_name \
         FROM view_type_test.t;",
    );

    let types = view_column_types(&mut executor, "view_type_test", "v");
    assert_eq!(types.len(), 3);
    assert_eq!(types[0], ("next_id".to_string(), "int8".to_string()));
    assert_eq!(types[1], ("id_text".to_string(), "text".to_string()));
    assert_eq!(
        types[2],
        ("display_name".to_string(), "varchar".to_string())
    );
}

#[test]
fn view_customer_summary_matches_runtime_pg_typeof() {
    let (storage, wal) = unique_engine("customer_summary");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    executor.execute("CREATE SCHEMA view_type_test").unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE view_type_test.customers (id BIGINT, email VARCHAR(100), age INTEGER);",
    );
    exec_ok(&mut executor, "CREATE TABLE view_type_test.orders (id BIGINT, customer_id BIGINT, total_amount NUMERIC(10,2));");
    exec_ok(
        &mut executor,
        "INSERT INTO view_type_test.customers VALUES (1, 'a@b.com', 30), (2, 'c@d.com', 40);",
    );
    exec_ok(
        &mut executor,
        "INSERT INTO view_type_test.orders VALUES (1, 1, 10.00), (2, 1, 20.00), (3, 2, 5.00);",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW view_type_test.customer_summary AS \
         SELECT c.id, c.email, \
                COUNT(o.id) AS order_count, \
                SUM(o.total_amount) AS total_spent, \
                AVG(o.total_amount) AS average_spent, \
                c.age \
         FROM view_type_test.customers c \
         LEFT JOIN view_type_test.orders o ON o.customer_id = c.id \
         GROUP BY c.id, c.email, c.age;",
    );

    let types = view_column_types(&mut executor, "view_type_test", "customer_summary");
    assert_eq!(types.len(), 6);
    assert_eq!(types[0], ("id".to_string(), "int8".to_string()));
    assert_eq!(types[1], ("email".to_string(), "varchar".to_string()));
    assert_eq!(types[2], ("order_count".to_string(), "int8".to_string()));
    assert_eq!(types[3], ("total_spent".to_string(), "numeric".to_string()));
    assert_eq!(
        types[4],
        ("average_spent".to_string(), "numeric".to_string())
    );
    assert_eq!(types[5], ("age".to_string(), "int4".to_string()));

    let pg_types = pg_typeof_types(
        &mut executor,
        "SELECT pg_typeof(id), pg_typeof(email), pg_typeof(order_count), \
                pg_typeof(total_spent), pg_typeof(average_spent), pg_typeof(age) \
         FROM view_type_test.customer_summary LIMIT 1",
    );
    assert_eq!(pg_types.len(), 6);
    for (catalog_row, runtime_ty) in types.iter().zip(pg_types.iter()) {
        let (_, data_type) = catalog_row;
        assert_eq!(
            pg_type_to_typeoid(runtime_ty),
            catalog_type_to_typeoid(data_type),
            "mismatch for data_type={data_type} pg_typeof={runtime_ty}"
        );
    }
}

#[test]
fn view_min_max_sum_follow_input_type() {
    let (storage, wal) = unique_engine("minmax");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    executor.execute("CREATE SCHEMA view_type_test").unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE view_type_test.t (a INTEGER, b BIGINT, c FLOAT8);",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW view_type_test.v AS \
         SELECT MIN(a) AS min_a, MAX(b) AS max_b, SUM(a) AS sum_a, AVG(c) AS avg_c \
         FROM view_type_test.t;",
    );

    let types = view_column_types(&mut executor, "view_type_test", "v");
    assert_eq!(types.len(), 4);
    assert_eq!(types[0], ("min_a".to_string(), "int4".to_string()));
    assert_eq!(types[1], ("max_b".to_string(), "int8".to_string()));
    // SUM(int4) widens to int8
    assert_eq!(types[2], ("sum_a".to_string(), "int8".to_string()));
    assert_eq!(types[3], ("avg_c".to_string(), "numeric".to_string()));
}

#[test]
fn view_cast_expression_resolves_to_target_type() {
    let (storage, wal) = unique_engine("cast");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    executor.execute("CREATE SCHEMA view_type_test").unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE view_type_test.t (id INTEGER, price NUMERIC(10,2));",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW view_type_test.v AS \
         SELECT CAST(id AS DATE) AS id_date, CAST(price AS TEXT) AS price_text \
         FROM view_type_test.t;",
    );

    let types = view_column_types(&mut executor, "view_type_test", "v");
    assert_eq!(types.len(), 2);
    assert_eq!(types[0], ("id_date".to_string(), "date".to_string()));
    assert_eq!(types[1], ("price_text".to_string(), "text".to_string()));
}

#[test]
fn view_arithmetic_promotion_rules() {
    let (storage, wal) = unique_engine("arith");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    executor.execute("CREATE SCHEMA view_type_test").unwrap();
    exec_ok(
        &mut executor,
        "CREATE TABLE view_type_test.t (a INTEGER, b BIGINT, c NUMERIC(10,2), d FLOAT8);",
    );
    exec_ok(
        &mut executor,
        "CREATE VIEW view_type_test.v AS \
         SELECT a + b AS int_plus_bigint, a * c AS int_times_numeric, d + 1.0 AS float_plus_lit \
         FROM view_type_test.t;",
    );

    let types = view_column_types(&mut executor, "view_type_test", "v");
    assert_eq!(types.len(), 3);
    assert_eq!(types[0].1, "int8");
    assert_eq!(types[1].1, "numeric");
    assert_eq!(types[2].1, "numeric");
}
