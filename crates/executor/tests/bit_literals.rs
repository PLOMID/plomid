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

fn exec<E: plomid_txn::StorageEngine>(executor: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match executor.execute(sql).unwrap() {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("{sql} => {other:?}"),
    }
}

#[test]
fn bit_literal_basic() {
    let storage = std::env::temp_dir().join("plomid-bit-basic");
    let wal = std::env::temp_dir().join("plomid-bit-wal");
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Basic bit literal
    let rows = exec(&mut executor, "SELECT B'101010';");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert!(matches!(rows[0][0], Value::Bit { .. }));

    // Bit literal with explicit cast
    let rows = exec(&mut executor, "SELECT B'101010'::bit(6);");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert!(matches!(rows[0][0], Value::Bit { .. }));

    // Bit literal with varbit cast
    let rows = exec(&mut executor, "SELECT B'101010'::varbit;");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert!(matches!(rows[0][0], Value::Bit { .. }));

    // Bit literal with lowercase b
    let rows = exec(&mut executor, "SELECT b'101010';");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert!(matches!(rows[0][0], Value::Bit { .. }));

    // Single bit
    let rows = exec(&mut executor, "SELECT B'1';");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert!(matches!(rows[0][0], Value::Bit { .. }));

    // All zeros
    let rows = exec(&mut executor, "SELECT B'000000';");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert!(matches!(rows[0][0], Value::Bit { .. }));

    // Longer bit string
    let rows = exec(&mut executor, "SELECT B'1010101010';");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert!(matches!(rows[0][0], Value::Bit { .. }));
}

#[test]
fn bit_literal_to_json() {
    let storage = std::env::temp_dir().join("plomid-bit-json");
    let wal = std::env::temp_dir().join("plomid-bit-wal-json");
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // to_json with bit literal
    let rows = exec(&mut executor, "SELECT to_json(B'101010'::bit(6));");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    // to_json should return a JSON text representation
    assert!(matches!(rows[0][0], Value::Text(_) | Value::Json(_)));

    // to_jsonb with bit literal
    let rows = exec(&mut executor, "SELECT to_jsonb(B'101010'::bit(6));");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    // to_jsonb should return a JSONB binary representation
    assert!(matches!(rows[0][0], Value::Jsonb(_) | Value::Text(_)));
}

#[test]
fn bit_literal_with_leading_zeros() {
    let storage = std::env::temp_dir().join("plomid-bit-leading-zeros");
    let wal = std::env::temp_dir().join("plomid-bit-wal-lz");
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Bit literal with leading zeros - should preserve all 6 bits
    let rows = exec(&mut executor, "SELECT B'001010';");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    if let Value::Bit { len, bytes } = &rows[0][0] {
        assert_eq!(*len, 6); // Must preserve length of 6
        assert_eq!(bytes.len(), 1); // 6 bits fits in 1 byte
    } else {
        panic!("Expected Bit value, got {:?}", rows[0][0]);
    }
}

#[test]
fn bit_literal_invalid_characters() {
    let storage = std::env::temp_dir().join("plomid-bit-invalid");
    let wal = std::env::temp_dir().join("plomid-bit-wal-inv");
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut executor = Executor::new(engine).unwrap();

    // Bit literal with invalid character should fail
    let result = executor.execute("SELECT B'102010';");
    assert!(
        result.is_err(),
        "Should reject invalid bit literal with character '2'"
    );
}
