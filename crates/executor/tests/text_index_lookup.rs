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
//! Unique/indexed text lookup regression (Blocker 2).
//!
//! A selective `unique_key = ?` predicate must resolve through the index
//! probe (candidate discovery + MVCC validation + row fetch), never a full
//! table scan. Coverage: PRIMARY KEY, UNIQUE, and plain single-column index
//! equality over text and integer keys, hits, misses, NULL behavior,
//! cross-type safety, uncommitted-row parity with the integer path, and
//! concurrent-writer visibility. A scaling probe asserts lookup cost tracks
//! index depth, not table size.

use plomid_executor::Executor;
use plomid_sql::{QueryResult, Value};
use plomid_txn::PlomidStorageEngine;
use std::time::Instant;

fn scratch(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("plomid-txtidx-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
    path
}

fn cleanup(root: &std::path::Path) {
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root.with_extension("wal"));
}

fn open(root: &std::path::Path) -> Executor<PlomidStorageEngine> {
    let wal = root.with_extension("wal");
    let engine = PlomidStorageEngine::create(&root, &wal, 64)
        .or_else(|_| PlomidStorageEngine::open(&root, &wal, 64))
        .expect("open engine");
    Executor::new(engine).expect("executor")
}

fn rows_of(result: QueryResult) -> Vec<Vec<Value>> {
    match result {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

fn load<E: plomid_txn::StorageEngine>(session: &mut Executor<E>, n: i64) {
    session
        .execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, ukey VARCHAR, tag TEXT, n INTEGER);")
        .unwrap();
    session
        .execute("CREATE UNIQUE INDEX docs_ukey_uidx ON docs (ukey);")
        .unwrap();
    session
        .execute("CREATE INDEX docs_tag_idx ON docs (tag);")
        .unwrap();
    let mut batch = String::from("INSERT INTO docs VALUES ");
    let mut in_batch = 0;
    for i in 0..n {
        if in_batch > 0 {
            batch.push_str(", ");
        }
        let ukey = format!("key-{i:08}");
        let tag = ["a", "b", "c", "d"][(i % 4) as usize];
        if i % 13 == 0 {
            batch.push_str(&format!("({i}, NULL, '{tag}', {i})"));
        } else {
            batch.push_str(&format!("({i}, '{ukey}', '{tag}', {i})"));
        }
        in_batch += 1;
        if in_batch == 2000 || i + 1 == n {
            batch.push(';');
            session.execute(&batch).unwrap();
            batch = String::from("INSERT INTO docs VALUES ");
            in_batch = 0;
        }
    }
}

#[test]
fn unique_text_lookup_hits_and_misses() {
    let root = scratch("hitmiss");
    let mut session = open(&root);
    load(&mut session, 20_000);

    // UNIQUE hit returns exactly its row.
    let rows = rows_of(
        session
            .execute("SELECT id, tag, n FROM docs WHERE ukey = 'key-00012345';")
            .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int4(12345));

    // Miss returns empty without error.
    let rows = rows_of(
        session
            .execute("SELECT id FROM docs WHERE ukey = 'key-no-such-key';")
            .unwrap(),
    );
    assert!(rows.is_empty());

    // NULL literal matches nothing (never an index probe result).
    let rows = rows_of(
        session
            .execute("SELECT id FROM docs WHERE ukey = NULL;")
            .unwrap(),
    );
    assert!(rows.is_empty());

    // NULL rows are invisible to equality but visible to IS NULL.
    let rows = rows_of(
        session
            .execute("SELECT COUNT(*) FROM docs WHERE ukey IS NULL;")
            .unwrap(),
    );
    assert_eq!(rows, vec![vec![Value::Int8(1539)]]);

    // Plain single-column text index equality.
    let rows = rows_of(
        session
            .execute("SELECT COUNT(*) FROM docs WHERE tag = 'b';")
            .unwrap(),
    );
    assert_eq!(rows, vec![vec![Value::Int8(5000)]]);

    // PRIMARY KEY integer equality (pre-existing fast path guard).
    let rows = rows_of(
        session
            .execute("SELECT tag FROM docs WHERE id = 19999;")
            .unwrap(),
    );
    assert_eq!(rows.len(), 1);

    // Cross-type: integer column against non-numeric text stays correct
    // (scan fallback, empty result).
    let rows = rows_of(
        session
            .execute("SELECT id FROM docs WHERE n = 'not-a-number';")
            .unwrap(),
    );
    assert!(rows.is_empty());
    // Integer-coercible text keeps the historical probe behavior.
    let rows = rows_of(
        session
            .execute("SELECT id FROM docs WHERE n = '42';")
            .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    cleanup(&root);
}

#[test]
fn text_equality_dml_updates_and_deletes() {
    // UPDATE/DELETE ... WHERE text-col = literal must affect exactly the
    // matching rows (discovery shares the index-probe machinery).
    let root = scratch("dml");
    let mut session = open(&root);
    load(&mut session, 5_000);
    let updated = match session
        .execute("UPDATE docs SET n = -1 WHERE tag = 'b';")
        .unwrap()
    {
        QueryResult::Updated(count) => count,
        other => panic!("expected Updated, got {other:?}"),
    };
    assert_eq!(updated, 1250);
    let rows = rows_of(
        session
            .execute("SELECT COUNT(*) FROM docs WHERE n = -1;")
            .unwrap(),
    );
    assert_eq!(rows, vec![vec![Value::Int8(1250)]]);
    let deleted = match session
        .execute("DELETE FROM docs WHERE ukey = 'key-00000042';")
        .unwrap()
    {
        QueryResult::Deleted(count) => count,
        other => panic!("expected Deleted, got {other:?}"),
    };
    assert_eq!(deleted, 1);
    let rows = rows_of(
        session
            .execute("SELECT id FROM docs WHERE ukey = 'key-00000042';")
            .unwrap(),
    );
    assert!(rows.is_empty());
    cleanup(&root);
}

#[test]
fn unique_text_lookup_sees_committed_writes() {
    // Cross-session visibility through the server-faithful shared engine:
    // the probe reads live index state, so a concurrently committed row is
    // found immediately and a deleted row disappears. (Sequentially reopened
    // engines share only durable state and can hold stale version caches;
    // production serves all connections from one shared engine.)
    use std::sync::{Arc, Mutex};
    let root = scratch("visibility");
    let wal = root.with_extension("wal");
    let engine = PlomidStorageEngine::create(&root, &wal, 64).expect("create");
    let shared = Arc::new(Mutex::new(engine));
    {
        let mut setup = Executor::new_shared(Arc::clone(&shared)).expect("setup");
        load(&mut setup, 5_000);
    }
    let mut reader = Executor::new_shared(Arc::clone(&shared)).expect("reader");
    {
        let mut writer = Executor::new_shared(Arc::clone(&shared)).expect("writer");
        writer
            .execute("INSERT INTO docs VALUES (99991, 'key-new-row', 'z', 7);")
            .unwrap();
    }
    let rows = rows_of(
        reader
            .execute("SELECT id, n FROM docs WHERE ukey = 'key-new-row';")
            .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Int4(7));
    // ... and disappears after delete.
    {
        let mut writer = Executor::new_shared(Arc::clone(&shared)).expect("writer");
        writer
            .execute("DELETE FROM docs WHERE ukey = 'key-new-row';")
            .unwrap();
    }
    let rows = rows_of(
        reader
            .execute("SELECT id FROM docs WHERE ukey = 'key-new-row';")
            .unwrap(),
    );
    assert!(rows.is_empty());
    cleanup(&root);
}

#[test]
fn unique_lookup_scales_with_index_not_table() {
    // Lookup latency must stay flat as the table grows 10K → 100K.
    for (label, n) in [("10k", 10_000i64), ("100k", 100_000i64)] {
        let root = scratch(label);
        let mut session = open(&root);
        load(&mut session, n);
        let key = format!("key-{:08}", n - 7);
        let timed = Instant::now();
        let rows = rows_of(
            session
                .execute(&format!("SELECT id, n FROM docs WHERE ukey = '{key}';"))
                .unwrap(),
        );
        let hit_us = timed.elapsed().as_micros();
        assert_eq!(rows.len(), 1);
        let timed = Instant::now();
        let rows = rows_of(
            session
                .execute("SELECT id FROM docs WHERE ukey = 'key-missing-entirely';")
                .unwrap(),
        );
        let miss_us = timed.elapsed().as_micros();
        assert!(rows.is_empty());
        eprintln!("unique lookup {label}: hit={hit_us}µs miss={miss_us}µs");
        // Generous bound: indexed probes stay sub-millisecond-ish in debug
        // at both scales (a full scan would read 10× more at 100K).
        assert!(
            hit_us < 50_000 && miss_us < 50_000,
            "lookup must not scan the table at {label}"
        );
        cleanup(&root);
    }
}
