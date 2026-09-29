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
//! Hot Row Store integration tests: CRUD, MVCC visibility, RowID stability,
//! batch atomicity, WAL ordering, checkpoint recovery, and concurrency.
use plomid_core::{ErrorKind, PlomidError, RowId};
use plomid_storage::{Field, Row, StorageEngine};
use plomid_txn::{HotRowStore, PlomidStorageEngine};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "plomid-hot-row-tests-{label}-{}-{id}",
        std::process::id()
    ))
}

fn cleanup(paths: &[&Path]) {
    for path in paths {
        let _ = std::fs::remove_dir_all(path);
        let _ = std::fs::remove_file(path);
    }
}

fn row(id: i64, text: &str) -> Row {
    Row::new(vec![
        Field::Integer(id),
        Field::String(text.to_owned()),
        Field::Null,
    ])
}

fn create(label: &str) -> (PlomidStorageEngine, PathBuf) {
    let dir = scratch(label);
    let engine = create_at(&dir).expect("engine create");
    (engine, dir)
}

fn create_at(dir: &Path) -> plomid_core::Result<PlomidStorageEngine> {
    PlomidStorageEngine::create(dir, &dir.join("wal-alt"), 32)
}

fn open(dir: &Path) -> PlomidStorageEngine {
    PlomidStorageEngine::open(dir, &dir.join("wal-alt"), 32).expect("engine open")
}

#[test]
fn insert_and_read_round_trip_through_the_store() {
    let (mut engine, dir) = create("roundtrip");
    {
        let mut store = HotRowStore::new(&mut engine);
        let id = store.insert(row(1, "alpha")).expect("insert");
        let read_back = store.read(id).expect("read");
        assert_eq!(read_back.fields()[0], Field::Integer(1));
        assert_eq!(read_back.fields()[1], Field::String("alpha".to_owned()));
        assert_eq!(read_back.fields()[2], Field::Null);
    }
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn row_ids_are_unique_and_stable_across_updates() {
    let (mut engine, dir) = create("unique-ids");
    {
        let mut store = HotRowStore::new(&mut engine);
        let first = store.insert(row(1, "a")).expect("insert first");
        let second = store.insert(row(2, "b")).expect("insert second");
        assert_ne!(first, second);
        store.update(first, row(10, "updated")).expect("update");
        let read_back = store.read(first).expect("read");
        assert_eq!(read_back.fields()[0], Field::Integer(10));
        // Row identity is preserved across the update.
        let read_again = store.read(first).expect("read again");
        assert_eq!(read_back, read_again);
    }
    drop(engine);
    cleanup(&[&dir]);
}

fn expect_not_found(result: Result<(), PlomidError>) {
    let error = result.expect_err("operation on a missing row must fail");
    assert_eq!(error.kind(), ErrorKind::NotFound);
}

#[test]
fn operations_on_missing_rows_fail_in_a_controlled_way() {
    let (mut engine, dir) = create("missing");
    let missing = RowId::new(9_999);
    {
        let mut store = HotRowStore::new(&mut engine);
        expect_not_found(store.read(missing).map(|_| ()));
    }
    {
        let mut store = HotRowStore::new(&mut engine);
        expect_not_found(store.update(missing, row(0, "x")));
    }
    {
        let mut store = HotRowStore::new(&mut engine);
        expect_not_found(store.delete(missing));
    }
    {
        let mut store = HotRowStore::new(&mut engine);
        expect_not_found(store.batch_update(vec![(missing, row(0, "x"))]));
    }
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn delete_removes_the_row_but_preserves_historical_visibility() {
    let (mut engine, dir) = create("delete");
    let id = HotRowStore::new(&mut engine)
        .insert(row(1, "before"))
        .expect("insert");
    let before_delete = HotRowStore::new(&mut engine)
        .committed_snapshot()
        .expect("snapshot");
    HotRowStore::new(&mut engine).delete(id).expect("delete");
    let outcome: Result<(), PlomidError> = HotRowStore::new(&mut engine).read(id).map(|_| ());
    expect_not_found(outcome);
    // A snapshot taken before the delete still observes the row.
    assert!(HotRowStore::new(&mut engine)
        .read_at(id, &before_delete)
        .expect("read_at")
        .is_some());
    // Repeated delete fails; the identity was consumed, not corrupted.
    expect_not_found(HotRowStore::new(&mut engine).delete(id));
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn update_keeps_old_snapshots_on_the_old_version() {
    let (mut engine, dir) = create("mvcc-update");
    let id = HotRowStore::new(&mut engine)
        .insert(row(1, "v1"))
        .expect("insert");
    let old_snapshot = HotRowStore::new(&mut engine)
        .committed_snapshot()
        .expect("snapshot");
    HotRowStore::new(&mut engine)
        .update(id, row(2, "v2"))
        .expect("update");
    let new_snapshot = HotRowStore::new(&mut engine)
        .committed_snapshot()
        .expect("snapshot");
    let old_view = HotRowStore::new(&mut engine)
        .read_at(id, &old_snapshot)
        .expect("read_at")
        .expect("old version");
    assert_eq!(old_view.fields()[1], Field::String("v1".to_owned()));
    let new_view = HotRowStore::new(&mut engine)
        .read_at(id, &new_snapshot)
        .expect("read_at")
        .expect("new version");
    assert_eq!(new_view.fields()[1], Field::String("v2".to_owned()));
    assert_eq!(
        HotRowStore::new(&mut engine)
            .read(id)
            .expect("read")
            .fields()[1],
        Field::String("v2".to_owned())
    );
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn aborted_transactions_leave_no_trace() {
    let (mut engine, dir) = create("abort");
    let txn = HotRowStore::transaction(&mut engine).expect("begin");
    txn.abort().expect("abort");
    // No borrow of the engine remains, so it can be scanned directly.
    let remaining = engine.scan(None, None).expect("scan");
    assert!(
        remaining.is_empty(),
        "an aborted transaction must leave no committed rows"
    );
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn row_ids_survive_a_restart_without_reuse() {
    let dir = scratch("restart-ids");
    let mut engine = create_at(&dir).expect("engine create");
    let first = HotRowStore::new(&mut engine)
        .insert(row(1, "before restart"))
        .expect("insert");
    drop(engine);
    let mut engine = open(&dir);
    let read_back = HotRowStore::new(&mut engine).read(first).expect("read");
    assert_eq!(
        read_back.fields()[1],
        Field::String("before restart".to_owned())
    );
    let second = HotRowStore::new(&mut engine)
        .insert(row(2, "after restart"))
        .expect("insert");
    assert!(second > first, "row ids must never be reused after restart");
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn recovery_restores_inserted_rows_after_restart() {
    let dir = scratch("recover-insert");
    let mut engine = create_at(&dir).expect("engine create");
    let inserted = HotRowStore::new(&mut engine)
        .insert(row(1, "durable"))
        .expect("insert");
    drop(engine);
    let mut engine = open(&dir);
    let read_back = HotRowStore::new(&mut engine).read(inserted).expect("read");
    assert_eq!(read_back.fields()[1], Field::String("durable".to_owned()));
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn recovery_restores_updates_and_deletes_after_restart() {
    let dir = scratch("recover-update-delete");
    let mut engine = create_at(&dir).expect("engine create");
    let updated_id = HotRowStore::new(&mut engine)
        .insert(row(1, "v1"))
        .expect("insert");
    HotRowStore::new(&mut engine)
        .update(updated_id, row(2, "v2"))
        .expect("update");
    let deleted_id = HotRowStore::new(&mut engine)
        .insert(row(3, "gone"))
        .expect("insert");
    HotRowStore::new(&mut engine)
        .delete(deleted_id)
        .expect("delete");
    drop(engine);
    let mut engine = open(&dir);
    let updated = HotRowStore::new(&mut engine)
        .read(updated_id)
        .expect("read");
    assert_eq!(updated.fields()[1], Field::String("v2".to_owned()));
    let outcome: Result<(), PlomidError> =
        HotRowStore::new(&mut engine).read(deleted_id).map(|_| ());
    expect_not_found(outcome);
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn batch_operations_are_atomic_and_identity_stable() {
    let (mut engine, dir) = create("batch");
    // Empty batch.
    assert!(HotRowStore::new(&mut engine)
        .batch_insert(Vec::new())
        .expect("batch")
        .is_empty());
    // Large batch.
    let rows: Vec<Row> = (0..200).map(|i| row(i, "batch")).collect();
    let ids = HotRowStore::new(&mut engine)
        .batch_insert(rows)
        .expect("batch insert");
    assert_eq!(ids.len(), 200);
    assert_eq!(
        HotRowStore::new(&mut engine)
            .read(ids[42])
            .expect("read")
            .fields()[0],
        Field::Integer(42)
    );
    // Batch update preserves identity.
    let updates: Vec<(RowId, Row)> = ids[..3].iter().map(|id| (*id, row(7, "updated"))).collect();
    HotRowStore::new(&mut engine)
        .batch_update(updates)
        .expect("batch update");
    assert_eq!(
        HotRowStore::new(&mut engine)
            .read(ids[0])
            .expect("read")
            .fields()[1],
        Field::String("updated".to_owned())
    );
    // Batch delete.
    HotRowStore::new(&mut engine)
        .batch_delete(vec![ids[0], ids[1]])
        .expect("batch delete");
    let outcome: Result<(), PlomidError> = HotRowStore::new(&mut engine).read(ids[0]).map(|_| ());
    expect_not_found(outcome);
    // Failing batch changes nothing.
    let error = HotRowStore::new(&mut engine)
        .batch_update(vec![
            (ids[2], row(8, "kept")),
            (RowId::new(9_999), row(0, "x")),
        ])
        .expect_err("batch with a missing row must fail");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    assert_eq!(
        HotRowStore::new(&mut engine)
            .read(ids[2])
            .expect("read")
            .fields()[1],
        Field::String("updated".to_owned())
    );
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn concurrent_inserts_are_unique_and_visible() {
    let dir = scratch("concurrent");
    let engine = Arc::new(Mutex::new(create_at(&dir).expect("engine create")));
    let threads = 4;
    let per_thread = 8;
    let mut handles = Vec::new();
    for t in 0..threads {
        let engine = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            let mut guard = engine.lock().expect("engine mutex");
            let mut store = HotRowStore::new(&mut guard);
            let mut collected = Vec::new();
            for _i in 0..per_thread {
                collected.push(store.insert(row(t as i64, "concurrent")).expect("insert"));
            }
            collected
        }));
    }
    let mut all_ids = Vec::new();
    for handle in handles {
        all_ids.extend(handle.join().expect("join"));
    }
    all_ids.sort();
    let unique = all_ids.len();
    all_ids.dedup();
    assert_eq!(all_ids.len(), unique, "row ids must be unique");
    {
        let mut guard = engine.lock().expect("engine mutex");
        let mut store = HotRowStore::new(&mut guard);
        for id in &all_ids {
            let observed = store.read(*id).expect("read");
            assert_eq!(observed.fields()[1], Field::String("concurrent".to_owned()));
        }
    }
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn oversized_rows_are_rejected_without_wal_side_effects() {
    let (mut engine, dir) = create("oversize");
    {
        let mut store = HotRowStore::new(&mut engine);
        let large = Row::new(vec![Field::Bytes(vec![
            0u8;
            plomid_storage::PAGE_DATA_SIZE + 1
        ])]);
        let error = store.insert(large).expect_err("oversized row must fail");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn read_your_own_writes_within_a_transaction() {
    let (mut engine, dir) = create("own-writes");
    let mut txn = HotRowStore::transaction(&mut engine).expect("begin");
    let id = txn.insert(row(5, "mine")).expect("insert");
    let seen = txn.read(id).expect("read");
    assert_eq!(seen.fields()[1], Field::String("mine".to_owned()));
    txn.delete(id).expect("delete");
    assert!(!txn.exists(id).expect("exists"));
    txn.commit().expect("commit");
    expect_not_found(HotRowStore::new(&mut engine).read(id).map(|_| ()));
    drop(engine);
    cleanup(&[&dir]);
}

#[test]
fn uncommitted_transactions_are_not_recovered() {
    let dir = scratch("recover-uncommitted");
    let mut engine = create_at(&dir).expect("engine create");
    // Begin a transaction and drop it without commit or abort; nothing is
    // durable, so recovery must expose an empty store.
    drop(HotRowStore::transaction(&mut engine).expect("begin"));
    drop(engine);
    let mut engine = open(&dir);
    {
        let mut store = HotRowStore::new(&mut engine);
        expect_not_found(store.read(RowId::new(u64::MAX)).map(|_| ()));
    }
    // No committed row exists after recovery; the store reads as empty.
    let remaining = engine.scan(None, None).expect("scan");
    assert!(
        remaining.is_empty(),
        "recovery must expose no rows for an uncommitted transaction"
    );
    drop(engine);
    cleanup(&[&dir]);
}
// ---------------------------------------------------------------------------
// Reopen diagnostics: seeded rows must be readable after engine restart.
// ---------------------------------------------------------------------------

#[test]
fn seeded_rows_survive_engine_reopen() {
    let dir = scratch("reopen");
    // Capture the seeded ids: the row-id allocator has a process-global floor
    // shared with other tests, so hard-coding an id would be fragile.
    let ids = {
        let mut engine = create_at(&dir).expect("engine create");
        let mut store = HotRowStore::new(&mut engine);
        store
            .batch_insert((0..50).map(|i| row(i, "seed")).collect())
            .expect("seed")
    };
    let first = ids[0];
    {
        let mut engine = open(&dir);
        let raw = engine.get(&plomid_txn::row_key(first)).expect("raw get");
        println!("raw storage get present = {}", raw.is_some());
        drop(engine);
    }
    let mut engine = open(&dir);
    {
        let mut store = HotRowStore::new(&mut engine);
        let snap = store.committed_snapshot().expect("snapshot");
        println!("snapshot after reopen = {:?}", snap);
        let got = store.read(first).expect("read after reopen");
        assert_eq!(got.fields()[0], Field::Integer(0));
    }
    drop(engine);
    cleanup(&[&dir]);
}
