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
//! Bulk-write reopen regression: large single-commit batches through the
//! shared engine must reopen with all committed data intact, without any
//! checkpoint. (Eviction used to flush parents ahead of children, leaving
//! dangling references that neither replay nor a fresh mount could traverse.)
use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::{ConcurrentPlomidStorageEngine, PlomidStorageEngine};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn scratch(label: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("plomid-bulk-reopen-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn count(session: &mut Executor<ConcurrentPlomidStorageEngine>) -> i64 {
    match session.execute("SELECT COUNT(*) FROM t").expect("count") {
        QueryResult::Rows { rows, .. } => match rows[0][0] {
            Value::Int8(n) => n,
            Value::Int4(n) => n as i64,
            ref other => panic!("unexpected {other:?}"),
        },
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn large_pk_batch_reopens_with_all_rows() {
    let dir = scratch("pk");
    let wal = dir.with_extension("wal");
    {
        let engine = PlomidStorageEngine::create(&dir, &wal, 64).expect("create");
        let mut ex = Executor::new_shared(Arc::new(Mutex::new(engine))).expect("exec");
        ex.execute("CREATE TABLE t (id BIGINT PRIMARY KEY, v INTEGER)")
            .unwrap();
        let vals: String = (0..5000)
            .map(|i| format!("({i},{i})"))
            .collect::<Vec<_>>()
            .join(",");
        ex.execute(&format!("INSERT INTO t VALUES {vals}")).unwrap();
    }
    let engine2 = PlomidStorageEngine::open(&dir, &wal, 64).expect("reopen");
    let mut ex2 = Executor::new_shared(Arc::new(Mutex::new(engine2))).expect("exec2");
    assert_eq!(count(&mut ex2), 5000);
    match ex2
        .execute("SELECT v FROM t WHERE id = 4242")
        .expect("point")
    {
        QueryResult::Rows { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert!(matches!(rows[0][0], Value::Int4(4242) | Value::Int8(4242)));
        }
        other => panic!("unexpected {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&wal);
}
