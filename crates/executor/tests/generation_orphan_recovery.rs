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
//! Kill-orphan generation recovery regression.
//!
//! A kill between a generation's files landing and the catalog pointer
//! advancing orphans that identity on disk. The publish guard refuses to
//! replace a durable generation, so an allocator sizing from the published
//! catalog alone would re-offer the orphaned identity and fail every later
//! VACUUM permanently. Sizing from max(published, on-disk) moves forward
//! past the orphan instead.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;
use std::sync::{Arc, Mutex};

fn scratch(label: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("plomid-gen-orphan-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
    path
}

fn copy_dir_all(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target_path = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &target_path);
        } else {
            std::fs::copy(entry.path(), target_path).unwrap();
        }
    }
}

fn count(session: &mut Executor<plomid_txn::ConcurrentPlomidStorageEngine>) -> i64 {
    match session.execute("SELECT COUNT(*) FROM t;").unwrap() {
        QueryResult::Rows { rows, .. } => match rows[0][0] {
            Value::Int8(n) => n,
            Value::Int4(n) => n as i64,
            ref other => panic!("count shape: {other:?}"),
        },
        other => panic!("count shape: {other:?}"),
    }
}

#[test]
fn vacuum_survives_orphaned_generation_files() {
    let dir = scratch("vacuum");
    let wal = dir.with_extension("wal");
    let engine = PlomidStorageEngine::create(&dir, &wal, 64).expect("create");
    let mut session = Executor::new_shared(Arc::new(Mutex::new(engine))).expect("executor");
    session
        .execute("CREATE TABLE t (id BIGINT PRIMARY KEY, v INTEGER);")
        .unwrap();
    session
        .execute("INSERT INTO t SELECT i, i FROM generate_series(1, 100) i;")
        .unwrap();
    session.execute("VACUUM t;").unwrap();
    assert_eq!(count(&mut session), 100);

    // Simulate the kill orphan: generation files for the next identity land
    // on disk without any catalog publication (planted as a copy of the
    // published generation's files; content is irrelevant — only the
    // identity collides with what a catalog-sized allocator would offer).
    let generations = dir
        .join("objects/databases/DB-00000000000000000001/schemas/S-00000000000000000001/tables/T-00000000000000000001/generations");
    let published: Vec<_> = std::fs::read_dir(&generations)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        published.len(),
        1,
        "one published generation, got {published:?}"
    );
    let orphan = generations.join("GEN-00000000000000000002");
    copy_dir_all(&published[0], &orphan);

    // A new write forces a fresh generation: without the fix the allocator
    // re-offers identity 2 and the publish guard rejects it permanently.
    session.execute("INSERT INTO t VALUES (101, 101);").unwrap();
    session
        .execute("VACUUM t;")
        .expect("vacuum moves past the orphan");
    assert_eq!(count(&mut session), 101);
    session
        .execute("VACUUM t;")
        .expect("repeat vacuum stays stable");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(wal);
}
