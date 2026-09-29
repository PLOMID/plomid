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
//! LIMIT/OFFSET early-termination semantics: capped scans must return exactly
//! what a full scan filtered downstream would return.
use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("plomid-limit-{label}-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn setup() -> (Executor<PlomidStorageEngine>, PathBuf) {
    let dir = scratch("sem");
    let wal = dir.with_extension("wal");
    let mut ex = Executor::new(PlomidStorageEngine::create(&dir, &wal, 32).expect("engine"))
        .expect("executor");
    ex.execute("CREATE TABLE t (id BIGINT PRIMARY KEY, v INT)")
        .expect("ddl");
    let vals: Vec<String> = (0..100).map(|i| format!("({i},{})", i % 7)).collect();
    ex.execute(&format!("INSERT INTO t VALUES {}", vals.join(",")))
        .expect("seed");
    (ex, dir)
}

fn ints(ex: &mut Executor<PlomidStorageEngine>, sql: &str) -> Vec<i64> {
    match ex.execute(sql).expect(sql) {
        QueryResult::Rows { rows, .. } => rows
            .into_iter()
            .map(|row| match row[0] {
                Value::Int8(n) => n,
                Value::Int4(n) => n as i64,
                ref other => panic!("unexpected {other:?}"),
            })
            .collect(),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn limit_returns_first_scan_rows() {
    let (mut ex, dir) = setup();
    // Scan order for this table is id order only insofar as the engine
    // returns it; assert self-consistency: LIMIT n == first n of no-limit.
    let all = ints(&mut ex, "SELECT id FROM t");
    assert_eq!(all.len(), 100);
    assert_eq!(ints(&mut ex, "SELECT id FROM t LIMIT 10"), &all[..10]);
    assert_eq!(ints(&mut ex, "SELECT id FROM t LIMIT 0"), &[] as &[i64]);
    assert_eq!(ints(&mut ex, "SELECT id FROM t LIMIT 200"), &all[..]);
    assert_eq!(ints(&mut ex, "SELECT id FROM t OFFSET 90"), &all[90..]);
    assert_eq!(
        ints(&mut ex, "SELECT id FROM t LIMIT 5 OFFSET 90"),
        &all[90..95]
    );
    assert_eq!(
        ints(&mut ex, "SELECT id FROM t LIMIT 50 OFFSET 90"),
        &all[90..]
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.with_extension("wal"));
}

#[test]
fn limit_with_where_returns_first_matches() {
    let (mut ex, dir) = setup();
    let all = ints(&mut ex, "SELECT id FROM t WHERE v = 3");
    assert!(!all.is_empty());
    assert_eq!(
        ints(&mut ex, "SELECT id FROM t WHERE v = 3 LIMIT 2"),
        &all[..2]
    );
    assert_eq!(
        ints(&mut ex, "SELECT id FROM t WHERE v = 3 LIMIT 2 OFFSET 1"),
        &all[1..3]
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.with_extension("wal"));
}

#[test]
fn limit_with_order_and_distinct_stays_correct() {
    let (mut ex, dir) = setup();
    let ordered = ints(&mut ex, "SELECT id FROM t ORDER BY id LIMIT 5");
    assert_eq!(ordered.len(), 5);
    assert!(ordered.windows(2).all(|w| w[0] <= w[1]));
    let distinct = ints(&mut ex, "SELECT DISTINCT v FROM t LIMIT 3");
    assert_eq!(distinct.len(), 3);
    let mut sorted = distinct.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 3, "distinct values must be unique");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.with_extension("wal"));
}
