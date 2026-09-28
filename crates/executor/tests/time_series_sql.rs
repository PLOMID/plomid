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
//! SQL time-series and statistics release coverage.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::{PlomidStorageEngine, StorageEngine};

fn paths(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir();
    let storage = base.join(format!("plomid-timeseries-{tag}-{}", std::process::id()));
    let wal = base.join(format!(
        "plomid-timeseries-{tag}-wal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&storage);
    let _ = std::fs::remove_dir_all(&wal);
    (storage, wal)
}

fn exec<E: StorageEngine>(session: &mut Executor<E>, sql: &str) -> Vec<Vec<Value>> {
    match session
        .execute(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
    {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows for {sql}, got {other:?}"),
    }
}

#[test]
fn vacuum_publishes_a_pruned_time_series_read_path_without_stale_results() {
    let (storage, wal) = paths("columnar");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut session = Executor::new(engine).unwrap();
    session
        .execute(
            "CREATE TABLE readings (id INTEGER, observed_at TIMESTAMPTZ, device TEXT, value INTEGER);",
        )
        .unwrap();
    session
        .execute(
            "INSERT INTO readings VALUES
             (1, '2024-01-01 00:00:00+00', 'a', 10),
             (2, '2024-01-02 00:00:00+00', 'a', 20),
             (3, '2024-01-03 00:00:00+00', 'b', 30),
             (4, '2024-01-04 00:00:00+00', 'b', 40);",
        )
        .unwrap();
    session.execute("VACUUM readings;").unwrap();

    let rows = exec(
        &mut session,
        "SELECT id, value FROM readings
         WHERE observed_at BETWEEN TIMESTAMPTZ '2024-01-02 00:00:00+00' AND TIMESTAMPTZ '2024-01-03 00:00:00+00'
         ORDER BY observed_at DESC;",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Int4(3));
    assert_eq!(rows[1][0], Value::Int4(2));

    // A write invalidates the immutable read accelerator. The result must
    // come from the current Hot Row Store state until the next VACUUM.
    session
        .execute("UPDATE readings SET value = 99 WHERE id = 2;")
        .unwrap();
    let rows = exec(&mut session, "SELECT value FROM readings WHERE id = 2;");
    assert_eq!(rows, vec![vec![Value::Int4(99)]]);
}

#[test]
fn analyze_collects_and_reloads_reproducible_statistics() {
    let (storage, wal) = paths("analyze");
    let engine = PlomidStorageEngine::create(&storage, &wal, 32).unwrap();
    let mut session = Executor::new(engine).unwrap();
    session
        .execute("CREATE TABLE measurements (id INTEGER, observed_at TIMESTAMP, note TEXT);")
        .unwrap();
    session
        .execute(
            "INSERT INTO measurements VALUES
             (1, '2025-01-01 00:00:00', NULL),
             (2, '2025-01-02 00:00:00', 'ok'),
             (3, '2025-01-03 00:00:00', 'ok');",
        )
        .unwrap();
    session.execute("ANALYZE measurements;").unwrap();
    let stats = session.analyzed_statistics("measurements").unwrap();
    assert_eq!(stats.0, 3);
    assert_eq!(stats.1[0].1, 3);
    assert_eq!(stats.1[2].2, 1);
    assert_eq!(stats.1[2].3, 1);
    assert_eq!(stats.1[1].4.as_deref(), Some("2025-01-01 00:00:00"));
    assert_eq!(stats.1[1].5.as_deref(), Some("2025-01-03 00:00:00"));

    drop(session);
    let engine = PlomidStorageEngine::open(&storage, &wal, 32).unwrap();
    let reopened = Executor::new(engine).unwrap();
    assert_eq!(reopened.analyzed_statistics("measurements").unwrap().0, 3);
}
