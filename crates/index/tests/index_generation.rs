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
//! Index generation lifecycle tests.
//!
//! These cover the properties the generation engine must guarantee: one
//! lifecycle behind both triggers, provenance and source-data compatibility
//! metadata, generation-identity safety across restarts, isolation between
//! indexes and tables, retention before reclamation, and integrity of the
//! durable record.

use plomid_core::{DatabaseId, GenerationId, IndexId, RowId, SchemaId, TableId, TableIdentity};
use plomid_index::{
    ArtIndex, AutomaticOutcome, IndexBuildFailPoint, IndexGenerationStore, IndexKind,
};
use plomid_storage::{IndexGenerationState, IndexGenerationTrigger};

fn scratch(label: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "plomid-index-gen-{label}-{}-{id}",
        std::process::id()
    ))
}

fn cleanup(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn identity(table: u64) -> TableIdentity {
    TableIdentity::new(DatabaseId::new(1), SchemaId::new(1), TableId::new(table))
}

/// Builds a small deterministic key set for an index.
fn rows(pairs: &[(u64, u64)]) -> Vec<(Vec<u8>, RowId)> {
    pairs
        .iter()
        .map(|(key, row)| (key.to_be_bytes().to_vec(), RowId::new(*row)))
        .collect()
}

#[test]
fn automatic_generation_creates_one_published_generation() {
    let dir = scratch("auto-create");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);

        let outcome = store.request_automatic_generation(
            table,
            index,
            GenerationId::new(1),
            &rows(&[(1, 10), (2, 20)]),
        )?;
        let AutomaticOutcome::Published(generation) = outcome else {
            panic!("expected a published generation, got {outcome:?}");
        };

        let current = store
            .current_generation(table, index)?
            .expect("a current generation exists");
        assert_eq!(current.generation_id, generation);
        assert_eq!(current.source_data_generation_id, GenerationId::new(1));
        assert_eq!(current.trigger, IndexGenerationTrigger::Automatic);
        assert_eq!(current.state, IndexGenerationState::Current);
        assert_eq!(current.index_id, index);
        assert_eq!(current.table_id, TableId::new(1));
        assert_eq!(current.database_id, DatabaseId::new(1));
        assert_eq!(current.schema_id, SchemaId::new(1));

        // Exactly one generation exists: nothing was churned.
        assert_eq!(store.generations(table, index)?.len(), 1);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("automatic generation");
}

#[test]
fn automatic_generation_is_idempotent_for_the_same_source_state() {
    let dir = scratch("auto-idempotent");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);
        let source = GenerationId::new(7);
        let data = rows(&[(1, 10), (2, 20)]);

        let first = store.request_automatic_generation(table, index, source, &data)?;
        let first_generation = match first {
            AutomaticOutcome::Published(generation) => generation,
            other => panic!("expected published, got {other:?}"),
        };

        // Repeated automatic triggers over the same source state must not
        // create duplicate generations.
        for _ in 0..3 {
            let again = store.request_automatic_generation(table, index, source, &data)?;
            assert_eq!(again, AutomaticOutcome::AlreadyCurrent(first_generation));
        }
        assert_eq!(store.generations(table, index)?.len(), 1);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("idempotent automatic generation");
}

#[test]
fn automatic_generation_creates_a_new_generation_for_a_new_source_state() {
    let dir = scratch("auto-new-source");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);

        let first = store.request_automatic_generation(
            table,
            index,
            GenerationId::new(1),
            &rows(&[(1, 10)]),
        )?;
        let AutomaticOutcome::Published(first_generation) = first else {
            panic!("expected published, got {first:?}");
        };

        let second = store.request_automatic_generation(
            table,
            index,
            GenerationId::new(2),
            &rows(&[(1, 10), (2, 20)]),
        )?;
        let AutomaticOutcome::Published(second_generation) = second else {
            panic!("expected published, got {second:?}");
        };

        assert_ne!(first_generation, second_generation);
        let current = store
            .current_generation(table, index)?
            .expect("current generation");
        assert_eq!(current.generation_id, second_generation);
        assert_eq!(current.source_data_generation_id, GenerationId::new(2));

        // The superseded generation is retained, not replaced or removed.
        let all = store.generations(table, index)?;
        assert_eq!(all.len(), 2);
        let previous = all
            .iter()
            .find(|record| record.generation_id == first_generation)
            .expect("previous generation retained");
        assert_eq!(previous.state, IndexGenerationState::Retained);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("new source state");
}

#[test]
fn automatic_generation_does_not_retire_a_previous_generation() {
    let dir = scratch("auto-retain");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);
        store.request_automatic_generation(
            table,
            index,
            GenerationId::new(1),
            &rows(&[(1, 10)]),
        )?;
        store.request_automatic_generation(
            table,
            index,
            GenerationId::new(2),
            &rows(&[(1, 10)]),
        )?;

        // Reclamation is explicit: a completed build never retires, so every
        // generation is still retained.
        let outcome = store.gc(table, index)?;
        assert!(outcome.reclaimed.is_empty());
        assert_eq!(outcome.retained.len(), 2);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("retention");
}

#[test]
fn automatic_generation_over_an_empty_table_creates_nothing() {
    let dir = scratch("auto-empty");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);

        let outcome =
            store.request_automatic_generation(table, index, GenerationId::new(1), &[])?;
        assert_eq!(outcome, AutomaticOutcome::Empty);
        assert!(store.current_generation(table, index)?.is_none());
        assert!(store.generations(table, index)?.is_empty());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("empty table");
}

#[test]
fn manual_rebuild_always_creates_a_new_generation() {
    let dir = scratch("manual-rebuild");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);
        let source = GenerationId::new(5);
        let data = rows(&[(1, 10)]);

        let first = store.rebuild_generation_manually(table, index, source, &data)?;
        assert_eq!(first.trigger, IndexGenerationTrigger::Manual);

        // A manual rebuild is explicit: the same source state still produces a
        // new generation, unlike the automatic path.
        let second = store.rebuild_generation_manually(table, index, source, &data)?;
        assert_ne!(first.generation_id, second.generation_id);
        assert_eq!(second.source_data_generation_id, source);

        let current = store
            .current_generation(table, index)?
            .expect("current generation");
        assert_eq!(current.generation_id, second.generation_id);

        let all = store.generations(table, index)?;
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].state, IndexGenerationState::Retained);
        assert_eq!(all[1].state, IndexGenerationState::Current);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("manual rebuild");
}

#[test]
fn manual_rebuild_records_manual_provenance_and_automatic_records_automatic() {
    let dir = scratch("provenance");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);

        store.request_automatic_generation(table, index, GenerationId::new(1), &rows(&[(1, 1)]))?;
        let automatic = store.current_generation(table, index)?.expect("current");
        assert_eq!(automatic.trigger, IndexGenerationTrigger::Automatic);

        store.rebuild_generation_manually(table, index, GenerationId::new(1), &rows(&[(1, 1)]))?;
        let manual = store.current_generation(table, index)?.expect("current");
        assert_eq!(manual.trigger, IndexGenerationTrigger::Manual);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("provenance");
}
#[test]
fn generation_identity_is_never_reused_across_a_restart() {
    let dir = scratch("id-collision");
    let result = (|| {
        let index = IndexId::new(1);
        let table = identity(1);

        let first = {
            let store = IndexGenerationStore::new(&dir);
            store.rebuild_generation_manually(
                table,
                index,
                GenerationId::new(1),
                &rows(&[(1, 1)]),
            )?
        };

        // A reopened process must continue the sequence, never restart it.
        let (second, observed_first) = {
            let store = IndexGenerationStore::new(&dir);
            let second = store.rebuild_generation_manually(
                table,
                index,
                GenerationId::new(1),
                &rows(&[(1, 1)]),
            )?;
            let observed = store
                .generations(table, index)?
                .into_iter()
                .find(|record| record.generation_id == first.generation_id)
                .expect("first generation survives the restart");
            (second, observed)
        };

        assert_ne!(first.generation_id, second.generation_id);
        assert!(
            second.generation_id > first.generation_id,
            "identities advance monotonically"
        );
        // The first generation is unchanged: same source, same trigger.
        assert_eq!(
            observed_first.source_data_generation_id,
            GenerationId::new(1)
        );
        assert_eq!(observed_first.trigger, IndexGenerationTrigger::Manual);
        assert_eq!(observed_first.state, IndexGenerationState::Retained);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("identity safety");
}

#[test]
fn automatic_generation_after_a_restart_stays_idempotent() {
    let dir = scratch("restart-auto");
    let result = (|| {
        let index = IndexId::new(2);
        let table = identity(2);
        let source = GenerationId::new(3);
        let data = rows(&[(1, 1), (2, 2)]);

        let first = {
            let store = IndexGenerationStore::new(&dir);
            match store.request_automatic_generation(table, index, source, &data)? {
                AutomaticOutcome::Published(generation) => generation,
                other => panic!("expected published, got {other:?}"),
            }
        };

        let store = IndexGenerationStore::new(&dir);
        // The published state survived: the same source is still current.
        assert!(store.is_current_for(table, index, source)?);
        assert_eq!(
            store.request_automatic_generation(table, index, source, &data)?,
            AutomaticOutcome::AlreadyCurrent(first)
        );
        assert_eq!(store.generations(table, index)?.len(), 1);

        // A different source after restart advances the generation identity.
        let next = store.request_automatic_generation(table, index, GenerationId::new(4), &data)?;
        let AutomaticOutcome::Published(next_generation) = next else {
            panic!("expected published, got {next:?}");
        };
        assert!(next_generation > first);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("restart idempotence");
}

#[test]
fn multiple_indexes_keep_independent_generations() {
    let dir = scratch("multi-index");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let table = identity(3);
        let (i1, i2) = (IndexId::new(1), IndexId::new(2));

        store.request_automatic_generation(table, i1, GenerationId::new(1), &rows(&[(1, 1)]))?;
        store.request_automatic_generation(table, i1, GenerationId::new(2), &rows(&[(1, 1)]))?;
        store.request_automatic_generation(table, i2, GenerationId::new(1), &rows(&[(9, 9)]))?;

        assert_eq!(store.generations(table, i1)?.len(), 2);
        assert_eq!(store.generations(table, i2)?.len(), 1);

        // Reclaiming one index cannot reclaim another index's generation.
        let outcome = store.gc(table, i1)?;
        assert!(outcome.reclaimed.is_empty());
        assert_eq!(store.generations(table, i2)?.len(), 1);

        let first = store
            .generations(table, i1)?
            .first()
            .expect("i1 has a generation")
            .generation_id;
        assert_eq!(first, GenerationId::new(1));
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("multi-index isolation");
}

#[test]
fn indexes_of_different_tables_never_share_generations() {
    let dir = scratch("multi-table");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let (a, b) = (identity(1), identity(2));

        store.request_automatic_generation(a, index, GenerationId::new(1), &rows(&[(1, 1)]))?;
        store.request_automatic_generation(a, index, GenerationId::new(2), &rows(&[(1, 1)]))?;
        store.request_automatic_generation(b, index, GenerationId::new(1), &rows(&[(2, 2)]))?;

        assert_eq!(store.generations(a, index)?.len(), 2);
        assert_eq!(store.generations(b, index)?.len(), 1);

        let a_records = store.generations(a, index)?;
        let b_records = store.generations(b, index)?;
        assert!(a_records
            .iter()
            .all(|record| record.table_id == TableId::new(1)));
        assert!(b_records
            .iter()
            .all(|record| record.table_id == TableId::new(2)));
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("table isolation");
}
#[test]
fn each_generation_records_the_source_data_generation_it_was_built_from() {
    let dir = scratch("source-tracking");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);

        for source in 1..=3u64 {
            store.request_automatic_generation(
                table,
                index,
                GenerationId::new(source),
                &rows(&[(1, 1)]),
            )?;
        }

        let records = store.generations(table, index)?;
        assert_eq!(records.len(), 3);
        for (offset, record) in records.iter().enumerate() {
            assert_eq!(
                record.source_data_generation_id,
                GenerationId::new(offset as u64 + 1)
            );
        }
        // Only what the source state matches is compatible.
        assert!(store.is_current_for(table, index, GenerationId::new(3))?);
        assert!(!store.is_current_for(table, index, GenerationId::new(2))?);
        assert!(!store.is_current_for(table, index, GenerationId::new(99))?);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("source tracking");
}

#[test]
fn gc_reclaims_only_explicitly_retired_generations() {
    let dir = scratch("gc");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);

        let first = store.rebuild_generation_manually(
            table,
            index,
            GenerationId::new(1),
            &rows(&[(1, 1)]),
        )?;
        let second = store.rebuild_generation_manually(
            table,
            index,
            GenerationId::new(2),
            &rows(&[(1, 1)]),
        )?;

        let index_dir = store.layout().index_dir_in_schema(
            DatabaseId::new(1),
            SchemaId::new(1),
            TableId::new(1),
            index,
        );
        let names = |dir: &std::path::Path| {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .expect("index dir")
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        };
        let marker = |generation: GenerationId| format!("{:020}", generation.get());
        let before = names(&index_dir);
        assert!(before
            .iter()
            .any(|name| name.contains(&marker(first.generation_id))));

        // With nothing retired, reclamation is a no-op.
        let noop = store.gc(table, index)?;
        assert!(noop.reclaimed.is_empty());
        assert_eq!(names(&index_dir), before);

        // Retiring the superseded generation makes exactly it reclaimable.
        store.retire_generation(table, index, first.generation_id)?;
        let outcome = store.gc(table, index)?;
        assert_eq!(outcome.reclaimed, vec![first.generation_id]);
        assert_eq!(outcome.retained, vec![second.generation_id]);

        let after = names(&index_dir);
        assert!(!after
            .iter()
            .any(|name| name.contains(&marker(first.generation_id))));
        assert!(after
            .iter()
            .any(|name| name.contains(&marker(second.generation_id))));

        // The current generation stays authoritative and usable.
        let current = store
            .current_generation(table, index)?
            .expect("current generation");
        assert_eq!(current.generation_id, second.generation_id);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("gc");
}

#[test]
fn a_corrupt_generation_record_is_rejected() {
    let dir = scratch("corrupt");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);
        let published = store.rebuild_generation_manually(
            table,
            index,
            GenerationId::new(1),
            &rows(&[(1, 1)]),
        )?;

        let meta_path = store.layout().index_generation_path_in_schema(
            DatabaseId::new(1),
            SchemaId::new(1),
            TableId::new(1),
            index,
            published.generation_id,
        );

        // A single flipped byte inside the checksummed region must be rejected.
        for offset in [12usize, 20, 44, 61] {
            let good = std::fs::read(&meta_path).expect("read record");
            let mut bad = good.clone();
            bad[offset] ^= 0x01;
            std::fs::write(&meta_path, &bad).expect("write corrupt record");
            assert!(
                store.generations(table, index).is_err(),
                "corruption at offset {offset} must be rejected"
            );
            std::fs::write(&meta_path, &good).expect("restore record");
        }

        // A truncated record is rejected too.
        let good = std::fs::read(&meta_path).expect("read record");
        std::fs::write(&meta_path, &good[..20]).expect("write truncated record");
        assert!(store.generations(table, index).is_err());
        std::fs::write(&meta_path, &good).expect("restore record");

        // With the record restored, the generation is authoritative again.
        assert!(store.current_generation(table, index)?.is_some());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("corruption handling");
}
#[test]
fn the_generation_lands_under_the_table_hierarchy() {
    let dir = scratch("hierarchy");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);
        let published = store.rebuild_generation_manually(
            table,
            index,
            GenerationId::new(1),
            &rows(&[(1, 1)]),
        )?;

        let index_dir = store.layout().index_dir_in_schema(
            DatabaseId::new(1),
            SchemaId::new(1),
            TableId::new(1),
            index,
        );
        let expected = dir
            .join("objects")
            .join("databases")
            .join("DB-00000000000000000001")
            .join("schemas")
            .join("S-00000000000000000001")
            .join("tables")
            .join("T-00000000000000000001")
            .join("indexes")
            .join("I-00000000000000000001");
        assert_eq!(
            index_dir.canonicalize().expect("index dir exists"),
            expected.canonicalize().expect("expected dir exists")
        );

        // Both artifacts are present and named by generation identity.
        let generation = published.generation_id.get();
        let meta = index_dir.join(format!("GEN-{generation:020}.dat"));
        let payload = index_dir.join(format!("IDX-{generation:020}.dat"));
        assert!(meta.is_file(), "generation metadata exists");
        assert!(payload.is_file(), "generation payload exists");

        // No legacy flat table path was written.
        assert!(!dir.join("objects").join("tables").exists());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("hierarchy");
}

#[test]
fn the_payload_is_readable_and_holds_the_built_keys() {
    let dir = scratch("payload");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index = IndexId::new(1);
        let table = identity(1);
        let data = rows(&[(3, 30), (1, 10), (2, 20)]);
        let published =
            store.rebuild_generation_manually(table, index, GenerationId::new(1), &data)?;
        assert_eq!(published.entry_count, 3);

        let payload = store.layout().index_payload_path_in_schema(
            DatabaseId::new(1),
            SchemaId::new(1),
            TableId::new(1),
            index,
            published.generation_id,
        );
        let mut reopened =
            plomid_index::btree::BTreeIndex::open(&payload, 64).expect("open payload");
        assert_eq!(reopened.index_id(), index);
        assert_eq!(reopened.generation(), published.generation_id);
        assert_eq!(reopened.entry_count(), 3);
        // The ordered structure returns exactly the keys that were inserted.
        let mut found: Vec<u64> = reopened
            .scan_all()
            .expect("scan")
            .iter()
            .map(|entry| {
                let bytes: [u8; 8] = entry.key.as_slice().try_into().expect("key width");
                u64::from_be_bytes(bytes)
            })
            .collect();
        found.sort_unstable();
        assert_eq!(found, vec![1, 2, 3]);
        reopened.close()?;
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("payload");
}

#[test]
fn index_crash_before_publication_keeps_the_previous_generation_current() {
    let dir = scratch("crash-before-publish");
    let result = (|| {
        let index = IndexId::new(1);
        let table = identity(1);

        // A first, fully published generation is the state a reader must keep.
        let first = {
            let store = IndexGenerationStore::new(&dir);
            store.rebuild_generation_manually(
                table,
                index,
                GenerationId::new(1),
                &rows(&[(1, 1)]),
            )?
        };
        let first_payload = {
            let store = IndexGenerationStore::new(&dir);
            store.layout().index_payload_path_in_schema(
                DatabaseId::new(1),
                SchemaId::new(1),
                TableId::new(1),
                index,
                first.generation_id,
            )
        };
        let first_bytes = std::fs::read(&first_payload).expect("first payload");

        // A crash before publication must surface an error...
        let store = IndexGenerationStore::new(&dir);
        let error = store
            .rebuild_generation_manually_with_failpoint(
                table,
                index,
                GenerationId::new(2),
                &rows(&[(1, 1), (2, 2)]),
                IndexBuildFailPoint::BeforePublish,
            )
            .expect_err("a crash before publication must be reported");
        assert_eq!(error.kind(), plomid_core::ErrorKind::Io);

        // ...and a reopened process must still find the first generation
        // current and complete. No half-built generation may become visible.
        let reopened = IndexGenerationStore::new(&dir);
        let current = reopened
            .current_generation(table, index)?
            .expect("the previous generation stays current");
        assert_eq!(current.generation_id, first.generation_id);
        assert_eq!(current.source_data_generation_id, GenerationId::new(1));
        assert_eq!(current.state, IndexGenerationState::Current);
        // The prior payload is byte-for-byte untouched.
        assert_eq!(
            std::fs::read(&first_payload).expect("first payload after crash"),
            first_bytes
        );
        // Exactly one current generation exists; nothing else is authoritative.
        let all = reopened.generations(table, index)?;
        assert_eq!(
            all.iter()
                .filter(|record| record.state == IndexGenerationState::Current)
                .count(),
            1
        );
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("crash before publication");
}

#[test]
fn index_crash_after_build_never_publishes_a_generation() {
    let dir = scratch("crash-after-build");
    let result = (|| {
        let index = IndexId::new(1);
        let table = identity(1);

        // No previous generation: the build fails right after the payload is
        // written, before it is verified, so nothing may become active.
        let store = IndexGenerationStore::new(&dir);
        let error = store
            .request_automatic_generation_with_failpoint(
                table,
                index,
                GenerationId::new(1),
                &rows(&[(1, 1)]),
                IndexBuildFailPoint::AfterBuild,
            )
            .expect_err("a crash after build must be reported");
        assert_eq!(error.kind(), plomid_core::ErrorKind::Io);

        let reopened = IndexGenerationStore::new(&dir);
        assert!(
            reopened.current_generation(table, index)?.is_none(),
            "an unverified payload must never become the active index"
        );
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("crash after build");
}

#[test]
fn index_crash_after_publication_makes_the_new_generation_current() {
    let dir = scratch("crash-after-publish");
    let result = (|| {
        let index = IndexId::new(1);
        let table = identity(1);

        let first = {
            let store = IndexGenerationStore::new(&dir);
            store.rebuild_generation_manually(
                table,
                index,
                GenerationId::new(1),
                &rows(&[(1, 1)]),
            )?
        };

        // The record is atomically renamed into place before this point fires,
        // so the caller sees an error even though publication fully succeeded.
        let store = IndexGenerationStore::new(&dir);
        let error = store
            .rebuild_generation_manually_with_failpoint(
                table,
                index,
                GenerationId::new(2),
                &rows(&[(1, 1), (2, 2)]),
                IndexBuildFailPoint::AfterPublish,
            )
            .expect_err("the caller did not observe the success");
        assert_eq!(error.kind(), plomid_core::ErrorKind::Io);

        // Recovery must select the fully published new generation, and the
        // generation it superseded must remain retained rather than lost.
        let reopened = IndexGenerationStore::new(&dir);
        let current = reopened
            .current_generation(table, index)?
            .expect("the published generation is current");
        assert_ne!(current.generation_id, first.generation_id);
        assert!(current.generation_id > first.generation_id);
        assert_eq!(current.source_data_generation_id, GenerationId::new(2));
        assert!(reopened.is_current_for(table, index, GenerationId::new(2))?);
        assert!(!reopened.is_current_for(table, index, GenerationId::new(1))?);
        let superseded = reopened
            .generations(table, index)?
            .into_iter()
            .find(|record| record.generation_id == first.generation_id)
            .expect("the superseded generation is retained");
        assert_eq!(superseded.state, IndexGenerationState::Retained);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("crash after publication");
}

#[test]
fn index_crash_before_publication_leaves_no_active_index_when_none_existed() {
    let dir = scratch("crash-no-previous");
    let result = (|| {
        let index = IndexId::new(3);
        let table = identity(3);

        // A manual rebuild with no prior generation, failing before the record
        // is published: there must be no active generation afterwards.
        let store = IndexGenerationStore::new(&dir);
        store
            .rebuild_generation_manually_with_failpoint(
                table,
                index,
                GenerationId::new(1),
                &rows(&[(1, 1), (2, 2)]),
                IndexBuildFailPoint::BeforePublish,
            )
            .expect_err("crash before publication");

        let reopened = IndexGenerationStore::new(&dir);
        assert!(
            reopened.current_generation(table, index)?.is_none(),
            "no invalid index may become active"
        );
        assert!(reopened.generations(table, index)?.is_empty());

        // A later request with no injected failure succeeds normally, so the
        // failed attempt did not poison the index.
        let outcome = reopened.request_automatic_generation(
            table,
            index,
            GenerationId::new(1),
            &rows(&[(1, 1), (2, 2)]),
        )?;
        assert!(matches!(outcome, AutomaticOutcome::Published(_)));
        assert!(reopened.current_generation(table, index)?.is_some());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("crash before publication with no previous generation");
}

#[test]
fn derive_art_reconstructs_lookups_from_the_current_durable_generation() {
    let dir = scratch("derive-art");
    let result = (|| {
        let index = IndexId::new(1);
        let table = identity(1);
        let data = rows(&[(10, 100), (20, 200), (30, 300)]);
        let store = IndexGenerationStore::new(&dir);
        store.rebuild_generation_manually(table, index, GenerationId::new(1), &data)?;

        let art = store
            .derive_art(table, index)?
            .expect("a published generation yields an ART");
        assert_eq!(art.len(), 3);
        art.validate().expect("derived ART satisfies invariants");
        assert_eq!(art.kind(), IndexKind::NonUnique);
        for (key, row) in &data {
            assert_eq!(
                art.lookup(key.as_slice()),
                Some(std::slice::from_ref(row)),
                "every durable row is reachable through the derived ART"
            );
        }
        assert_eq!(art.lookup(&[9u8; 8]), None);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("derive ART from the current generation");
}

#[test]
fn derive_art_tracks_the_newest_generation_after_an_automatic_build() {
    let dir = scratch("derive-art-newest");
    let result = (|| {
        let index = IndexId::new(2);
        let table = identity(2);
        let store = IndexGenerationStore::new(&dir);
        store.rebuild_generation_manually(
            table,
            index,
            GenerationId::new(1),
            &rows(&[(1, 1), (2, 2)]),
        )?;
        store.request_automatic_generation(
            table,
            index,
            GenerationId::new(5),
            &rows(&[(1, 1), (2, 2), (3, 3)]),
        )?;

        let art = store
            .derive_art(table, index)?
            .expect("the newest generation yields an ART");
        assert_eq!(art.len(), 3);
        art.validate().expect("derived ART satisfies invariants");
        assert_eq!(art.lookup(&3u64.to_be_bytes()), Some(&[RowId::new(3)][..]));
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("derived ART tracks the newest generation");
}

#[test]
fn derive_art_without_a_generation_is_an_absence_not_an_error() {
    let dir = scratch("derive-art-empty");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        assert!(
            store.derive_art(identity(9), IndexId::new(9))?.is_none(),
            "an index that was never built has no ART to derive"
        );
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("derive ART absence");
}

#[test]
fn derive_art_is_isolated_between_indexes_and_tables() {
    let dir = scratch("derive-art-isolation");
    let result = (|| {
        let store = IndexGenerationStore::new(&dir);
        let index_a = IndexId::new(3);
        let index_b = IndexId::new(4);
        let table_a = identity(3);
        let table_b = identity(4);
        store.rebuild_generation_manually(
            table_a,
            index_a,
            GenerationId::new(1),
            &rows(&[(7, 70)]),
        )?;
        store.rebuild_generation_manually(
            table_a,
            index_b,
            GenerationId::new(1),
            &rows(&[(8, 80)]),
        )?;
        store.rebuild_generation_manually(
            table_b,
            index_a,
            GenerationId::new(1),
            &rows(&[(9, 90)]),
        )?;

        let art_a = store
            .derive_art(table_a, index_a)?
            .expect("index A of table A");
        let art_b = store
            .derive_art(table_a, index_b)?
            .expect("index B of table A");
        let art_other = store
            .derive_art(table_b, index_a)?
            .expect("index A of table B");
        assert_eq!(
            art_a.lookup(&7u64.to_be_bytes()),
            Some(&[RowId::new(70)][..])
        );
        assert_eq!(art_a.lookup(&8u64.to_be_bytes()), None);
        assert_eq!(art_a.lookup(&9u64.to_be_bytes()), None);
        assert_eq!(
            art_b.lookup(&8u64.to_be_bytes()),
            Some(&[RowId::new(80)][..])
        );
        assert_eq!(art_b.lookup(&7u64.to_be_bytes()), None);
        assert_eq!(
            art_other.lookup(&9u64.to_be_bytes()),
            Some(&[RowId::new(90)][..])
        );
        assert_eq!(art_other.lookup(&7u64.to_be_bytes()), None);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("derived ART isolation");
}

#[test]
fn derive_art_rejects_a_corrupt_durable_source() {
    let dir = scratch("derive-art-corrupt");
    let result = (|| {
        let index = IndexId::new(5);
        let table = identity(5);
        let store = IndexGenerationStore::new(&dir);
        let published = store.rebuild_generation_manually(
            table,
            index,
            GenerationId::new(1),
            &rows(&[(1, 1)]),
        )?;
        assert!(store.derive_art(table, index)?.is_some());

        let payload = store.layout().index_payload_path_in_schema(
            DatabaseId::new(1),
            SchemaId::new(1),
            TableId::new(5),
            index,
            published.generation_id,
        );
        let good = std::fs::read(&payload).expect("read payload");
        let mut bad = good.clone();
        bad[64] ^= 0xFF;
        std::fs::write(&payload, &bad).expect("write corrupt payload");

        assert!(
            store.derive_art(table, index).is_err(),
            "a corrupt payload must never yield an ART"
        );
        std::fs::write(&payload, &good).expect("restore payload");
        assert!(
            store.derive_art(table, index)?.is_some(),
            "the durable source recovers once restored"
        );
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    result.expect("derived ART corruption handling");
}

#[test]
fn rebuild_from_entries_preserves_prefix_binary_and_growth_shape(
) -> Result<(), plomid_index::ArtError> {
    let mut pairs: Vec<(Vec<u8>, RowId)> = vec![
        (b"orders/2024/".to_vec(), RowId::new(1)),
        (b"orders/2024/01".to_vec(), RowId::new(2)),
        (b"orders/2025/".to_vec(), RowId::new(3)),
    ];
    for n in 0u64..64 {
        let mut key = b"user/".to_vec();
        key.push(n as u8);
        key.extend_from_slice(&[0x00, 0xFF, (n >> 8) as u8]);
        pairs.push((key, RowId::new(100 + n)));
    }
    let art = ArtIndex::rebuild_from_entries(IndexKind::NonUnique, pairs.clone())
        .expect("pure rebuild over a mixed dataset");
    art.validate().expect("rebuilt tree satisfies invariants");
    assert_eq!(art.len(), pairs.len());
    for (key, row) in &pairs {
        assert_eq!(
            art.lookup(key.as_slice()),
            Some(std::slice::from_ref(row)),
            "prefix and binary keys remain reachable"
        );
    }
    Ok::<(), plomid_index::ArtError>(())
}
