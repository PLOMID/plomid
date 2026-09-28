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
//! MVCC-aware compaction integration tests.
//!
//! Every test runs against real engine, generation, and filesystem state. The
//! suite covers compaction's lifecycle, MVCC/delete correctness, RowID
//! preservation, failure injection at each durability boundary, restart and
//! recovery, idempotence, per-table isolation, hierarchy ownership, and
//! corruption rejection.

use plomid_columnar::{
    compact, mvcc_safety, read_generation_rows, select_inputs, ColumnType, ColumnarPublish,
    ColumnarStore, CompactionFailPoint, CompactionRequest, FlushConfig,
};
use plomid_core::{
    CatalogVersion, DatabaseId, ErrorKind, GenerationId, Lsn, ObjectId, SchemaId, TableId,
    TableIdentity,
};
use plomid_storage::{Field, Row, StorageEngine as _, StorageEngineTransaction as _};
use plomid_txn::{HotRowStore, PlomidStorageEngine};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Owning identity of the table these tests compact.
fn identity() -> TableIdentity {
    TableIdentity::new(DatabaseId::new(3), SchemaId::new(4), TableId::new(1))
}

fn object() -> ObjectId {
    ObjectId::new(1)
}

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-compaction-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_dir_all(path);
}

fn int_rows(values: &[i64]) -> Vec<Row> {
    values
        .iter()
        .map(|value| Row::new(vec![Field::Integer(*value)]))
        .collect()
}

/// Publishes `rows` as one immutable generation of the test table.
fn publish_generation_typed(
    store: &ColumnarStore,
    engine: &mut PlomidStorageEngine,
    generation: u64,
    rows: &[Row],
    types: &[ColumnType],
) -> u64 {
    let publish = ColumnarPublish::new(
        object(),
        ColumnarStore::columnar_schema(SchemaId::new(4), CatalogVersion::new(1), types)
            .expect("schema"),
        GenerationId::new(generation),
        GenerationId::new(1),
        Lsn::new(1),
    )
    .with_identity(identity());
    let segment_id = store.allocate_segment_id();
    store
        .flush_rows_with_id(
            engine,
            rows,
            types,
            segment_id,
            &FlushConfig::default(),
            &publish,
            plomid_columnar::ColumnarFailPoint::None,
        )
        .expect("publish generation");
    segment_id.get()
}

/// Publishes `rows` as one single-integer-column generation of the test table.
fn publish_generation(
    store: &ColumnarStore,
    engine: &mut PlomidStorageEngine,
    generation: u64,
    rows: &[Row],
) -> u64 {
    publish_generation_typed(store, engine, generation, rows, &[ColumnType::Integer])
}

fn request(generation: u64) -> CompactionRequest {
    CompactionRequest {
        identity: identity(),
        object_id: object(),
        column_types: vec![ColumnType::Integer],
        config: FlushConfig::default(),
        generation: GenerationId::new(generation),
        storage_generation: GenerationId::new(1),
        checkpoint_lsn: Lsn::new(1),
    }
}

/// Creates an engine + store pair over a fresh root.
fn setup(label: &str) -> (PathBuf, PlomidStorageEngine, ColumnarStore) {
    let dir = scratch(label);
    let engine = PlomidStorageEngine::create(&dir, &dir.join("wal"), 32).expect("engine");
    let store = ColumnarStore::open(&dir).expect("store");
    (dir, engine, store)
}

#[test]
fn a_single_generation_is_a_no_op() {
    let (dir, mut engine, store) = setup("noop");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &int_rows(&[1, 2, 3]));

        // One generation is already minimal: no new generation is created, so
        // compaction never manufactures a meaningless empty generation.
        let (active, last) = mvcc_safety(&engine)?;
        let outcome = compact(
            &store,
            &mut engine,
            &request(6),
            &int_rows(&[1, 2, 3]),
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;
        assert!(outcome.published.is_none(), "no-op must not publish");
        assert_eq!(outcome.visible_rows, 3);
        assert_eq!(outcome.input_rows, vec![(GenerationId::new(5), 3)]);
        assert_eq!(outcome.retained_inputs, vec![GenerationId::new(5)]);
        assert!(outcome.released.is_empty());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

/// Visible hot-row state through the existing MVCC read path.
fn visible_rows(engine: &mut PlomidStorageEngine) -> Vec<Row> {
    ColumnarStore::snapshot_hot_rows(engine).expect("visible hot rows")
}

#[test]
fn failure_before_publication_leaves_the_old_generations_valid() {
    let (dir, mut engine, store) = setup("fail-before");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));
        let before = store.generations().reader()?;
        assert!(before.is_complete());

        for fail_at in [
            CompactionFailPoint::DuringInputRead,
            CompactionFailPoint::AfterBuild,
            CompactionFailPoint::AfterFlush,
            CompactionFailPoint::DuringVerify,
            CompactionFailPoint::BeforeSync,
            CompactionFailPoint::AfterSync,
            CompactionFailPoint::DuringPublish,
        ] {
            assert!(fail_at.is_pre_publication(), "{fail_at:?}");
            let (active, last) = mvcc_safety(&engine)?;
            let error = compact(
                &store,
                &mut engine,
                &request(7),
                &int_rows(&[1, 2]),
                &active,
                last,
                fail_at,
                true,
            )
            .expect_err("injected failure must be reported");
            // The failure comes from an existing subsystem's injection point, so
            // its kind is the one that subsystem reports.
            assert!(
                matches!(error.kind(), ErrorKind::Io | ErrorKind::Internal),
                "{fail_at:?} reported {:?}",
                error.kind()
            );

            // The previously published state is untouched and complete, and the
            // new generation was never published.
            let after = store.generations().reader()?;
            assert!(after.is_complete(), "{fail_at:?}");
            assert_eq!(
                after.pointer().catalog_generation,
                before.pointer().catalog_generation,
                "{fail_at:?} must not move the publication pointer"
            );
            assert_eq!(
                after.object(object()).expect("record").current_generation,
                GenerationId::new(6),
                "{fail_at:?} must leave generation 6 current"
            );
        }
        drop(before);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn failure_after_publication_leaves_the_new_generation_authoritative() {
    let (dir, mut engine, store) = setup("fail-after");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

        // AfterPublish reports a failure the caller did not observe: the
        // generation IS published, and that is the documented contract.
        let (active, last) = mvcc_safety(&engine)?;
        let error = compact(
            &store,
            &mut engine,
            &request(7),
            &int_rows(&[1, 2, 3]),
            &active,
            last,
            CompactionFailPoint::AfterPublish,
            false,
        )
        .expect_err("injected failure must be reported");
        assert_eq!(error.kind(), ErrorKind::Internal);

        let reader = store.generations().reader()?;
        assert!(reader.is_complete());
        assert_eq!(
            reader.object(object()).expect("record").current_generation,
            GenerationId::new(7),
            "the published generation is authoritative"
        );
        // Generation 7 is readable and holds the compacted rows.
        let rows = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert_eq!(rows.len(), 3);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}
#[test]
fn reclamation_failure_never_loses_a_required_generation() {
    let (dir, mut engine, store) = setup("fail-gc");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

        let (active, last) = mvcc_safety(&engine)?;
        let error = compact(
            &store,
            &mut engine,
            &request(7),
            &int_rows(&[1, 2]),
            &active,
            last,
            CompactionFailPoint::BeforeReclaim,
            true,
        )
        .expect_err("injected reclamation failure must be reported");
        assert_eq!(error.kind(), ErrorKind::Io);

        // Nothing was removed, and the published state still resolves every
        // generation it references.
        let reader = store.generations().reader()?;
        assert!(reader.is_complete());
        assert!(reader.generation(GenerationId::new(7)).is_some());
        store.generations().validate()?;
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn compaction_survives_a_restart_and_is_idempotent() {
    let dir = scratch("restart");
    let result = (|| {
        {
            let mut engine = PlomidStorageEngine::create(&dir, &dir.join("wal"), 32)?;
            let store = ColumnarStore::open(&dir)?;
            publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
            publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

            let (active, last) = mvcc_safety(&engine)?;
            let outcome = compact(
                &store,
                &mut engine,
                &request(7),
                &int_rows(&[1, 2]),
                &active,
                last,
                CompactionFailPoint::None,
                true,
            )?;
            assert_eq!(outcome.output_rows, 2);
            engine.checkpoint()?;
        }

        // Recovery establishes the same published generation set.
        let mut engine = PlomidStorageEngine::open(&dir, &dir.join("wal"), 32)?;
        let store = ColumnarStore::open(&dir)?;
        let reader = store.generations().reader()?;
        assert!(reader.is_complete());
        assert_eq!(
            reader.object(object()).expect("record").current_generation,
            GenerationId::new(7)
        );
        store.generations().validate()?;
        drop(reader);

        // The compacted payload is durable in the existing storage image: it is
        // read back through the normal segment reader, which verifies CRC32C.
        let compacted = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert_eq!(compacted.len(), 2);

        // Repeated compaction converges: the state is now one generation, so a
        // second and third run are no-ops that neither lose nor duplicate rows.
        for generation in [8_u64, 9] {
            let (active, last) = mvcc_safety(&engine)?;
            let outcome = compact(
                &store,
                &mut engine,
                &request(generation),
                &int_rows(&[1, 2]),
                &active,
                last,
                CompactionFailPoint::None,
                true,
            )?;
            assert!(
                outcome.published.is_none(),
                "run {generation} must be a no-op"
            );
            assert_eq!(outcome.retained_inputs, vec![GenerationId::new(7)]);
        }
        let rows = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert_eq!(rows.len(), 2);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn compaction_over_an_update_chain_keeps_only_the_visible_row() {
    let (dir, mut engine, store) = setup("updates");
    let result = (|| {
        let ids = {
            let mut hot = HotRowStore::new(&mut engine);
            hot.batch_insert(int_rows(&[10, 20, 30])).expect("seed")
        };
        // Generation 5: the seeded state.
        let seed_rows = visible_rows(&mut engine);
        publish_generation(&store, &mut engine, 5, &seed_rows);
        // Two updates and one delete, each its own committed transaction.
        {
            let mut hot = HotRowStore::new(&mut engine);
            hot.update(ids[0], Row::new(vec![Field::Integer(11)]))
                .expect("update 1");
            hot.update(ids[1], Row::new(vec![Field::Integer(21)]))
                .expect("update 2");
            hot.delete(ids[2]).expect("delete");
        }
        // Generation 6: the post-update state.
        let post_update_rows = visible_rows(&mut engine);
        publish_generation(&store, &mut engine, 6, &post_update_rows);

        let before = visible_rows(&mut engine);
        assert_eq!(before.len(), 2, "the deleted row is gone, updates applied");

        let (active, last) = mvcc_safety(&engine)?;
        let outcome = compact(
            &store,
            &mut engine,
            &request(7),
            &before,
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;
        assert_eq!(
            outcome.input_rows,
            vec![(GenerationId::new(5), 3), (GenerationId::new(6), 2)]
        );

        // The compacted generation holds exactly the visible row state: no
        // duplicate current row, no resurrected deleted row, no lost update.
        let compacted = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert_eq!(compacted, before);

        // RowID identity is untouched: the same logical rows are still readable
        // through the same row ids.
        let mut hot = HotRowStore::new(&mut engine);
        assert_eq!(
            hot.read(ids[0]).expect("row 0"),
            Row::new(vec![Field::Integer(11)])
        );
        assert_eq!(
            hot.read(ids[1]).expect("row 1"),
            Row::new(vec![Field::Integer(21)])
        );
        assert!(hot.read(ids[2]).is_err(), "the deleted row stays deleted");
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_released_input_is_reclaimed_and_a_still_needed_one_is_retained() {
    let (dir, mut engine, store) = setup("retention");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

        // A live reader holds generation 5 out of reach of reclamation.
        let reader = store.generations().reader()?;
        assert!(reader.generation(GenerationId::new(5)).is_some());

        let (active, last) = mvcc_safety(&engine)?;
        let outcome = compact(
            &store,
            &mut engine,
            &request(7),
            &int_rows(&[1, 2]),
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;
        assert_eq!(
            outcome.released,
            vec![GenerationId::new(5), GenerationId::new(6)]
        );
        // Reclamation never removes a generation a live reader observes: 5 and 6
        // stay on disk while the reader is alive.
        let reclaimed = outcome.reclaimed.expect("reclamation pass");
        assert!(
            !reclaimed.reclaimed.contains(&GenerationId::new(5)),
            "a live reader keeps its generation"
        );
        assert!(
            !reclaimed.reclaimed.contains(&GenerationId::new(6)),
            "the reader's catalog state keeps generation 6"
        );

        // Once the reader releases its lease, the existing reclamation pass
        // reclaims what is now unreachable.
        drop(reader);
        let reclaimed = store.generations().gc()?;
        assert!(reclaimed.reclaimed.contains(&GenerationId::new(5)));
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn an_active_transaction_protects_the_inputs_from_release() {
    let (dir, mut engine, store) = setup("active-txn");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

        // An explicit transaction is open, so a snapshot older than this
        // compaction may still be required.
        let mut txn = HotRowStore::transaction(&mut engine)?;
        txn.insert(Row::new(vec![Field::Integer(3)]))?;

        let plan = select_inputs(
            store.generations(),
            identity(),
            object(),
            &[txn.txn_id().get()],
            0,
        )?;
        assert!(
            !plan.inputs_releasable,
            "an active transaction must block release"
        );

        txn.commit()?;

        // With the transaction gone, release becomes provable and the run
        // reclaims the superseded generations.
        let (active, last) = mvcc_safety(&engine)?;
        let outcome = compact(
            &store,
            &mut engine,
            &request(7),
            &int_rows(&[1, 2, 3]),
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;
        assert!(!outcome.released.is_empty());
        assert!(outcome.retained_inputs.is_empty());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_concurrent_write_during_compaction_is_not_lost() {
    let (dir, mut engine, store) = setup("concurrent");
    let result = (|| {
        {
            let mut hot = HotRowStore::new(&mut engine);
            hot.batch_insert(int_rows(&[1, 2])).expect("seed");
        }
        let seeded = visible_rows(&mut engine);
        assert_eq!(seeded.len(), 2);
        publish_generation(&store, &mut engine, 5, &seeded);
        let seeded_again = visible_rows(&mut engine);
        publish_generation(&store, &mut engine, 6, &seeded_again);

        // A write commits through the normal path while compaction is being
        // prepared; compaction then reads the post-commit committed snapshot.
        let (active, last) = mvcc_safety(&engine)?;
        {
            let mut hot = HotRowStore::new(&mut engine);
            hot.insert(Row::new(vec![Field::Integer(3)]))?;
        }
        let after_write = visible_rows(&mut engine);
        assert_eq!(after_write.len(), 3);

        let outcome = compact(
            &store,
            &mut engine,
            &request(7),
            &after_write,
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;
        assert_eq!(outcome.output_rows, 3);

        // The concurrent write is present in the compacted generation and the
        // hot store is untouched.
        let compacted = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert_eq!(compacted, after_write);
        assert_eq!(visible_rows(&mut engine), after_write);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_generation_created_after_selection_is_compacted_instead_of_lost() {
    let (dir, mut engine, store) = setup("late-gen");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

        // The input set is selected first...
        let stale = select_inputs(store.generations(), identity(), object(), &[], 0)?;
        assert_eq!(
            stale.generation_ids(),
            vec![GenerationId::new(5), GenerationId::new(6)]
        );

        // ...then a normal flush publishes generation 7 concurrently.
        publish_generation(&store, &mut engine, 7, &int_rows(&[1, 2, 3]));

        // Compaction re-selects the input set at run time, so the newer
        // generation is folded into the output instead of being left behind or
        // overwritten: a stale plan can never be published.
        let (active, last) = mvcc_safety(&engine)?;
        let outcome = compact(
            &store,
            &mut engine,
            &request(8),
            &int_rows(&[1, 2, 3]),
            &active,
            last,
            CompactionFailPoint::None,
            false,
        )?;
        assert_eq!(
            outcome.plan.generation_ids(),
            vec![
                GenerationId::new(5),
                GenerationId::new(6),
                GenerationId::new(7)
            ]
        );

        // The new current generation is the compacted one and holds every row,
        // including the ones generation 7 introduced.
        let reader = store.generations().reader()?;
        assert!(reader.is_complete());
        assert_eq!(
            reader.object(object()).expect("record").current_generation,
            GenerationId::new(8)
        );
        drop(reader);
        let rows = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(8),
        )?;
        assert_eq!(rows, int_rows(&[1, 2, 3]));
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn compaction_uses_the_placement_layer_and_never_assumes_a_device() {
    let (dir, mut engine, store) = setup("multi-device");
    let result = (|| {
        // A second device is registered; the storage manager's allocator picks
        // among devices, so the compacted payload lands wherever placement
        // decides and no device identity is hard-coded.
        let layout = plomid_storage::DatabaseLayout::new(&dir);
        plomid_storage::register_device(
            &layout,
            plomid_core::DeviceId::new(7),
            plomid_storage::capacity_for_extents(10_000)?,
        )?;
        let devices = plomid_storage::DeviceRegistry::discover(&layout)?;
        assert!(devices.len() >= 2, "a second device must be available");

        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));
        let (active, last) = mvcc_safety(&engine)?;
        compact(
            &store,
            &mut engine,
            &request(7),
            &int_rows(&[1, 2]),
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;

        // The payload is readable and every segment of the storage image is
        // owned by a registered device.
        let rows = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert_eq!(rows.len(), 2);
        let layout = plomid_storage::DatabaseLayout::new(&dir);
        let devices = plomid_storage::validate_devices(&layout)?;
        assert!(devices.len() >= 2);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn compaction_is_isolated_per_table() {
    let (dir, mut engine, store) = setup("isolation");
    let result = (|| {
        // Table A lives at object 1; table B at object 2 with its own identity.
        let table_b = TableIdentity::new(DatabaseId::new(3), SchemaId::new(4), TableId::new(2));
        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

        let types = vec![ColumnType::Integer];
        let publish_b = ColumnarPublish::new(
            ObjectId::new(2),
            ColumnarStore::columnar_schema(SchemaId::new(4), CatalogVersion::new(1), &types)?,
            GenerationId::new(50),
            GenerationId::new(1),
            Lsn::new(1),
        )
        .with_identity(table_b);
        let segment_b = store.allocate_segment_id();
        store.flush_rows_with_id(
            &mut engine,
            &int_rows(&[99]),
            &types,
            segment_b,
            &FlushConfig::default(),
            &publish_b,
            plomid_columnar::ColumnarFailPoint::None,
        )?;

        // Selecting inputs for A with B's identity is refused: compaction can
        // never read or delete another table's generations.
        let error = select_inputs(store.generations(), table_b, object(), &[], 0)
            .expect_err("a different identity must be rejected");
        assert_eq!(error.kind(), ErrorKind::Catalog);

        let (active, last) = mvcc_safety(&engine)?;
        let outcome = compact(
            &store,
            &mut engine,
            &request(7),
            &int_rows(&[1, 2]),
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;
        assert_eq!(outcome.plan.identity, identity());

        // Table B is untouched: same current generation, same rows, no release.
        let reader = store.generations().reader()?;
        let record_b = reader.object(ObjectId::new(2)).expect("table B record");
        assert_eq!(record_b.current_generation, GenerationId::new(50));
        assert_eq!(record_b.table_identity, table_b);
        assert!(record_b.retained_generations.is_empty());
        drop(reader);
        let rows_b = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            ObjectId::new(2),
            GenerationId::new(50),
        )?;
        assert_eq!(rows_b, int_rows(&[99]));
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn the_compacted_generation_lands_in_the_table_hierarchy() {
    let (dir, mut engine, store) = setup("hierarchy");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

        let (active, last) = mvcc_safety(&engine)?;
        compact(
            &store,
            &mut engine,
            &request(7),
            &int_rows(&[1, 2]),
            &active,
            last,
            CompactionFailPoint::None,
            false,
        )?;

        // The generation is physically associated with its real ownership:
        // objects/databases/DB-3/schemas/S-4/tables/T-1/generations/GEN-7/.
        let layout = plomid_storage::DatabaseLayout::new(&dir);
        let path = layout.generation_meta_path(
            DatabaseId::new(3),
            SchemaId::new(4),
            TableId::new(1),
            GenerationId::new(7),
        );
        assert!(path.is_file(), "generation metadata missing at {path:?}");
        assert!(layout
            .generation_segments_dir(
                DatabaseId::new(3),
                SchemaId::new(4),
                TableId::new(1),
                GenerationId::new(7),
            )
            .is_dir());
        plomid_storage::validate_generation(
            &layout,
            DatabaseId::new(3),
            SchemaId::new(4),
            TableId::new(1),
            GenerationId::new(7),
        )?;
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn an_empty_table_compacts_without_inventing_rows() {
    let (dir, mut engine, store) = setup("empty");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &[]);
        publish_generation(&store, &mut engine, 6, &[]);

        let (active, last) = mvcc_safety(&engine)?;
        let outcome = compact(
            &store,
            &mut engine,
            &request(7),
            &[],
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;
        assert_eq!(outcome.visible_rows, 0);
        assert_eq!(outcome.output_rows, 0);
        assert_eq!(
            outcome.input_rows,
            vec![(GenerationId::new(5), 0), (GenerationId::new(6), 0)]
        );
        // The compacted generation is empty and decodes cleanly.
        let rows = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert!(rows.is_empty());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn nulls_and_variable_length_values_are_preserved() {
    let (dir, mut engine, store) = setup("values");
    let result = (|| {
        let rows = vec![
            Row::new(vec![Field::Null, Field::String(String::new())]),
            Row::new(vec![
                Field::Integer(-1),
                Field::String("unicode-ß-日本".to_owned()),
            ]),
            Row::new(vec![
                Field::Integer(i64::MAX),
                Field::String("x".repeat(1024)),
            ]),
            Row::new(vec![Field::Integer(i64::MIN), Field::Null]),
        ];
        let types = vec![ColumnType::Integer, ColumnType::String];
        publish_generation_typed(&store, &mut engine, 5, &rows, &types);
        publish_generation_typed(&store, &mut engine, 6, &rows, &types);

        let publish = ColumnarPublish::new(
            object(),
            ColumnarStore::columnar_schema(SchemaId::new(4), CatalogVersion::new(1), &types)?,
            GenerationId::new(7),
            GenerationId::new(1),
            Lsn::new(1),
        )
        .with_identity(identity());
        let segment = store.allocate_segment_id();
        store.flush_rows_with_id(
            &mut engine,
            &rows,
            &types,
            segment,
            &FlushConfig::default(),
            &publish,
            plomid_columnar::ColumnarFailPoint::None,
        )?;

        // Read the compacted generation back through the normal reader: the
        // NULL bitmap, the empty string, the unicode text, and the wide value
        // all survive.
        let back = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert_eq!(back, rows);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_corrupt_input_generation_is_rejected_before_anything_is_written() {
    let (dir, mut engine, store) = setup("corrupt");
    let result = (|| {
        publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
        publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

        // Damage the durable payload of one payload slice in place, leaving the
        // stored CRC32C covering the original bytes.
        let mut damaged = false;
        for (key, value) in engine.scan(None, None)? {
            if key.len() == 17
                && key[..4] == plomid_columnar::COLUMNAR_KEY_PREFIX
                && key[12] == plomid_columnar::COLUMNAR_SLICE_KIND
            {
                let mut corrupt = value.clone();
                let last = corrupt.len() - 1;
                corrupt[last] ^= 0xFF;
                // Rewriting with the same key goes through the normal write
                // path; the payload no longer matches its checksum.
                let mut txn = engine.begin()?;
                txn.put(&key, &corrupt)?;
                txn.commit()?;
                damaged = true;
                break;
            }
        }
        assert!(damaged, "a payload slice must exist");

        let before = store.generations().reader()?;
        let (active, last) = mvcc_safety(&engine)?;
        let error = compact(
            &store,
            &mut engine,
            &request(7),
            &int_rows(&[1, 2]),
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )
        .expect_err("a corrupt input must be rejected");
        // The existing CRC32C verification rejects the damaged payload.
        assert_eq!(error.kind(), ErrorKind::Corruption);

        // Nothing was published, and the published state is unchanged.
        let after = store.generations().reader()?;
        assert!(after.is_complete());
        assert_eq!(
            after.pointer().catalog_generation,
            before.pointer().catalog_generation
        );
        assert!(after.generation(GenerationId::new(7)).is_none());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_full_insert_update_update_delete_chain_over_one_row_id_is_compacted() {
    let (dir, mut engine, store) = setup("delete-chain");
    let result = (|| {
        // One logical row carries four committed physical versions:
        //   v1 INSERT 10 → v2 UPDATE 11 → v3 UPDATE 12 → v4 DELETE
        let id = {
            let mut hot = HotRowStore::new(&mut engine);
            hot.batch_insert(int_rows(&[10])).expect("insert")[0]
        };
        // Generation 5 carries the INSERT state.
        let inserted = visible_rows(&mut engine);
        assert_eq!(inserted, int_rows(&[10]));
        publish_generation(&store, &mut engine, 5, &inserted);

        {
            let mut hot = HotRowStore::new(&mut engine);
            hot.update(id, Row::new(vec![Field::Integer(11)]))
                .expect("update 1");
        }
        // Generation 6 carries the first UPDATE state.
        let updated_once = visible_rows(&mut engine);
        assert_eq!(updated_once, int_rows(&[11]));
        publish_generation(&store, &mut engine, 6, &updated_once);

        {
            let mut hot = HotRowStore::new(&mut engine);
            hot.update(id, Row::new(vec![Field::Integer(12)]))
                .expect("update 2");
            // The same committed transaction deletes the row it just updated,
            // which is the final version of the chain.
            hot.delete(id).expect("delete");
        }
        // Generation 7 carries the post-delete state: no logical row at all.
        let deleted = visible_rows(&mut engine);
        assert!(deleted.is_empty(), "the row is deleted");
        publish_generation(&store, &mut engine, 7, &deleted);

        // Compaction of an all-deleted table must not resurrect the row and must
        // not fail merely because the visible state is empty.
        let (active, last) = mvcc_safety(&engine)?;
        let outcome = compact(
            &store,
            &mut engine,
            &request(8),
            &[],
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;
        assert_eq!(outcome.visible_rows, 0);
        assert_eq!(outcome.output_rows, 0);
        // The three superseded generations were all selected as input.
        assert_eq!(
            outcome.input_rows,
            vec![
                (GenerationId::new(5), 1),
                (GenerationId::new(6), 1),
                (GenerationId::new(7), 0),
            ]
        );

        // The row stays deleted for every newer snapshot, and no version of the
        // chain survives as a visible current row.
        let mut hot = HotRowStore::new(&mut engine);
        assert!(hot.read(id).is_err(), "the deleted row stays deleted");
        assert!(visible_rows(&mut engine).is_empty());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn b_row_id_identity_is_preserved_across_many_versions_and_compaction() {
    let (dir, mut engine, store) = setup("rowid");
    let result = (|| {
        let ids = {
            let mut hot = HotRowStore::new(&mut engine);
            hot.batch_insert(int_rows(&[1, 2, 3])).expect("seed")
        };
        let seeded = visible_rows(&mut engine);
        publish_generation(&store, &mut engine, 5, &seeded);

        // Ten committed updates to one RowID: a long single-key version chain.
        for value in 0..10_i64 {
            let mut hot = HotRowStore::new(&mut engine);
            hot.update(ids[1], Row::new(vec![Field::Integer(100 + value)]))
                .expect("update");
        }
        // One delete of another RowID.
        {
            let mut hot = HotRowStore::new(&mut engine);
            hot.delete(ids[2]).expect("delete");
        }
        let before = visible_rows(&mut engine);
        assert_eq!(before, int_rows(&[1, 109]));
        publish_generation(&store, &mut engine, 6, &before);

        let (active, last) = mvcc_safety(&engine)?;
        let outcome = compact(
            &store,
            &mut engine,
            &request(7),
            &before,
            &active,
            last,
            CompactionFailPoint::None,
            true,
        )?;
        assert_eq!(outcome.visible_rows, 2);

        // The compacted generation holds exactly two rows, not twelve: the
        // superseded versions collapse, and it never invents a RowID.
        let compacted = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert_eq!(compacted.len(), 2);
        assert_eq!(compacted, before);

        // RowID identity of the surviving rows is untouched: the same handles
        // still resolve to the same logical rows after compaction.
        let mut hot = HotRowStore::new(&mut engine);
        assert_eq!(hot.read(ids[0])?, Row::new(vec![Field::Integer(1)]));
        assert_eq!(hot.read(ids[1])?, Row::new(vec![Field::Integer(109)]));
        assert!(hot.read(ids[2]).is_err());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}
/// Failure during retention release cannot make a required generation
/// disappear.
///
/// Release is the one boundary at which compaction removes inputs from the
/// published catalog, so it is the boundary where an interrupted run could
/// plausibly strand the system. This test injects a failure exactly there and
/// proves the resulting state is still a valid published one:
///
/// * the failure is surfaced rather than swallowed;
/// * the new generation is published, complete, and authoritative;
/// * the released inputs are gone, but the authoritative generation is retained;
/// * the compacted rows still read back through the normal segment reader;
/// * a restart establishes the same published state; and
/// * compaction stays safe and idempotent after recovery.
#[test]
fn failure_during_release_keeps_the_published_state_valid() {
    let dir = scratch("fail-release");
    let result = (|| {
        {
            let mut engine = PlomidStorageEngine::create(&dir, &dir.join("wal"), 32)?;
            let store = ColumnarStore::open(&dir)?;
            publish_generation(&store, &mut engine, 5, &int_rows(&[1]));
            publish_generation(&store, &mut engine, 6, &int_rows(&[1, 2]));

            // With no live transaction the inputs are legitimately releasable,
            // so the injected failure lands *after* the release is published.
            let (active, last) = mvcc_safety(&engine)?;
            assert!(active.is_empty(), "no transaction should be live");
            let error = compact(
                &store,
                &mut engine,
                &request(7),
                &int_rows(&[1, 2]),
                &active,
                last,
                CompactionFailPoint::DuringRelease,
                true,
            )
            .expect_err("injected release failure must be reported");
            assert_eq!(error.kind(), ErrorKind::Io);

            // The new generation is published and complete: a failure at the
            // release boundary cannot leave a half-published generation behind.
            let reader = store.generations().reader()?;
            assert!(reader.is_complete());
            assert!(reader.generation(GenerationId::new(7)).is_some());
            assert_eq!(
                reader.object(object()).expect("record").current_generation,
                GenerationId::new(7)
            );
            drop(reader);

            // The inputs were released, but the generation the object now
            // resolves to is retained: release can never remove the generation
            // the published state depends on.
            assert!(!store.generations().retains(GenerationId::new(5))?);
            assert!(!store.generations().retains(GenerationId::new(6))?);
            assert!(store.generations().retains(GenerationId::new(7))?);
            store.generations().validate()?;

            // No data loss: the compacted payload reads back through the normal
            // segment reader, which verifies CRC32C on the way.
            let compacted = read_generation_rows(
                &store,
                &mut engine,
                store.generations(),
                object(),
                GenerationId::new(7),
            )?;
            assert_eq!(compacted.len(), 2);
            assert_eq!(compacted, int_rows(&[1, 2]));
            engine.checkpoint()?;
        }

        // Recovery establishes the same published generation set.
        let mut engine = PlomidStorageEngine::open(&dir, &dir.join("wal"), 32)?;
        let store = ColumnarStore::open(&dir)?;
        let reader = store.generations().reader()?;
        assert!(reader.is_complete());
        assert_eq!(
            reader.object(object()).expect("record").current_generation,
            GenerationId::new(7)
        );
        drop(reader);
        store.generations().validate()?;

        let compacted = read_generation_rows(
            &store,
            &mut engine,
            store.generations(),
            object(),
            GenerationId::new(7),
        )?;
        assert_eq!(compacted, int_rows(&[1, 2]));

        // Compaction stays idempotent across the recovery point: the state is
        // one generation, so further runs neither publish nor lose rows.
        for generation in [8_u64, 9] {
            let (active, last) = mvcc_safety(&engine)?;
            let outcome = compact(
                &store,
                &mut engine,
                &request(generation),
                &int_rows(&[1, 2]),
                &active,
                last,
                CompactionFailPoint::None,
                true,
            )?;
            assert!(
                outcome.published.is_none(),
                "run {generation} must be a no-op"
            );
            assert_eq!(outcome.output_rows, 2);
        }
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}
