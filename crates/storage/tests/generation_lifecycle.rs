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
//! Catalog and generation lifecycle tests.
//!
//! These tests exercise the durable metadata lifecycle end to end against real
//! filesystem state: versioned catalog publication, immutable generations,
//! reader consistency across a concurrent publication, retention, garbage
//! collection, failure injection at every durability boundary, corruption
//! rejection, and recovery that selects the newest safe durable state.
use plomid_core::{
    BlockId, CatalogVersion, ColumnId, DatabaseId, ErrorKind, GenerationId, Lsn, ObjectId, PackId,
    PageId, PlomidError, RowId, SchemaId, SegmentId,
};
use plomid_storage::{
    catalog_path, catalog_staged_path, create_checkpoint, discover_generation_ids,
    generation_path_in_schema, load_catalog, load_generation, load_publication_pointer,
    publication_pointer_path, GcFailPoint, GenerationManager, ObjectChange, PhysicalReference,
    PhysicalStructure, PublicationFailPoint, PublicationPointerSource, PublicationRequest,
    SchemaColumn, SchemaMetadata, StorageManager,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Scratch-seed identities. Storage-level tests create isolated roots and seed the
/// default database/schema so that hierarchy-aware paths resolve deterministically.
const SEED_DB: DatabaseId = DatabaseId::new(1);
#[allow(dead_code)]
const SEED_SCHEMA: SchemaId = SchemaId::new(1);

/// Schema identity used by the shared `schema()` helper, matching the
/// `TableIdentity` derived by `ObjectChange::new` (database 1, schema 7).
const PUB_SCHEMA: SchemaId = SchemaId::new(7);

/// Resolves the hierarchy-aware published path of a generation for reads in
/// these tests. Publication writes generation bytes inside the logical
/// `Database → Schema → Table` tree, so the flat reader path no longer applies.
fn gen_path(dir: &Path, table: u64, generation: GenerationId) -> PathBuf {
    generation_path_in_schema(
        dir,
        SEED_DB,
        PUB_SCHEMA,
        plomid_core::TableId::new(table),
        generation,
    )
}

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-generation-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

fn open_manager(root: &Path) -> Arc<GenerationManager> {
    Arc::new(GenerationManager::open(root).expect("manager"))
}

fn structure(page: u64) -> PhysicalStructure {
    segment_structure(1, page)
}

/// Builds a physical structure inside one storage segment.
fn segment_structure(segment: u64, page: u64) -> PhysicalStructure {
    PhysicalStructure::new(
        SegmentId::new(segment),
        PackId::new(1),
        BlockId::new(1),
        PageId::new(page),
        Some(RowId::new(page)),
    )
}

fn reference(object: u64, generation: u64, page: u64) -> PhysicalReference {
    PhysicalReference::new(
        ObjectId::new(object),
        GenerationId::new(generation),
        structure(page),
    )
}

/// Builds a physical reference naming a specific storage segment.
fn segment_reference(object: u64, generation: u64, segment: u64) -> PhysicalReference {
    PhysicalReference::new(
        ObjectId::new(object),
        GenerationId::new(generation),
        segment_structure(segment, 1),
    )
}

fn schema(version: u64) -> SchemaMetadata {
    SchemaMetadata::new(
        SchemaId::new(7),
        CatalogVersion::new(version),
        vec![
            SchemaColumn {
                column_id: ColumnId::new(1),
                type_code: 23,
            },
            SchemaColumn {
                column_id: ColumnId::new(2),
                type_code: 25,
            },
        ],
    )
    .expect("schema")
}

fn change(object: u64, generation: u64) -> ObjectChange {
    ObjectChange::new(
        ObjectId::new(object),
        schema(1),
        GenerationId::new(generation),
        vec![reference(object, generation, 1)],
    )
    .expect("change")
}

fn write(storage: u64, lsn: u64, changes: Vec<ObjectChange>) -> PublicationRequest {
    PublicationRequest::write(GenerationId::new(storage), Lsn::new(lsn), changes).expect("request")
}

#[test]
fn publishes_recovers_and_repeats_deterministically() {
    let dir = scratch("lifecycle");
    let result = (|| {
        let manager = open_manager(&dir);
        let published = manager.publish(write(1, 5, vec![change(1, 5), change(2, 7)]))?;
        assert_eq!(published.pointer.catalog_version, CatalogVersion::new(1));
        assert_eq!(published.pointer.catalog_generation, GenerationId::new(1));
        assert_eq!(published.generations.len(), 2);
        assert_eq!(published.superseded_catalog, None);
        manager.validate()?;
        assert_eq!(
            manager.pointer()?,
            load_publication_pointer(&dir)?,
            "the durable pointer must agree with the authoritative record"
        );

        let recovered = GenerationManager::recover(&dir)?;
        assert_eq!(recovered.source, PublicationPointerSource::Pointer);
        assert_eq!(recovered.pointer, published.pointer);
        assert_eq!(recovered.catalog, published.catalog);
        assert_eq!(recovered.generations, published.generations);

        // Recovery is read-only and repeatable: a second pass observes exactly
        // the same durable state and mutates nothing.
        let after = discover_generation_ids(&dir)?;
        let again = GenerationManager::recover(&dir)?;
        assert_eq!(again, recovered);
        assert_eq!(
            discover_generation_ids(&dir)?,
            after,
            "recovery must not create or remove metadata"
        );
        let layout = plomid_storage::DatabaseLayout::new(&dir);
        assert_eq!(publication_pointer_path(&dir), layout.current_path());
        assert!(layout.current_path().is_file());
        assert!(!dir.join("catalog/CURRENT").exists());
        assert!(!dir.join("generations").exists());
        assert!(!dir.join("generation").exists());
        for (table, generation) in [(1, 5), (2, 7)] {
            let table = plomid_core::TableId::new(table);
            let generation = GenerationId::new(generation);
            assert!(
                gen_path(&dir, table.get(), generation).is_file(),
                "generation metadata is published inside the logical hierarchy"
            );
            assert_eq!(
                load_generation(&dir, generation)?.object_id.get(),
                table.get()
            );
        }

        let reopened = open_manager(&dir);
        assert_eq!(reopened.pointer()?, published.pointer);
        assert_eq!(reopened.load()?, published.catalog);
        assert_eq!(
            reopened.load()?.generation_of(ObjectId::new(1)),
            Some(GenerationId::new(5))
        );
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn retains_superseded_generations_until_they_are_released() {
    let dir = scratch("retention");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(1, 1, vec![change(1, 5)]))?;
        let published = manager.publish(write(1, 2, vec![change(1, 6)]))?;
        let record = published
            .catalog
            .object(ObjectId::new(1))
            .expect("object record");
        assert_eq!(record.current_generation, GenerationId::new(5 + 1));
        assert_eq!(record.retained_generations, vec![GenerationId::new(5)]);
        assert!(published.catalog.owns_generation(GenerationId::new(5)));
        assert_eq!(published.superseded_catalog, Some(GenerationId::new(1)));

        // The retained generation stays durable and keeps its own identity.
        let retained = load_generation(&dir, GenerationId::new(5))?;
        assert_eq!(retained.object_id, ObjectId::new(1));
        assert_eq!(retained.catalog_version, CatalogVersion::new(1));
        let current = load_generation(&dir, GenerationId::new(6))?;
        assert_eq!(current.previous_generation, Some(GenerationId::new(5)));
        assert_eq!(current.catalog_version, CatalogVersion::new(2));
        assert_eq!(current.publication_generation, GenerationId::new(2));

        // Releasing retention is an explicit durable publication of its own.
        let released = manager.release(GenerationId::new(5))?;
        assert_eq!(released.pointer.catalog_version, CatalogVersion::new(3));
        assert!(released.catalog.object(ObjectId::new(1)).is_some());
        assert!(!released
            .catalog
            .generation_ids()
            .contains(&GenerationId::new(5)));
        // The superseded catalog state still references it, which is what makes
        // recovery from an older boundary possible until that state is gone.
        let superseded = plomid_storage::load_catalog_for_generation(&dir, GenerationId::new(2))?
            .expect("superseded catalog state");
        assert!(superseded.owns_generation(GenerationId::new(5)));

        // A generation its object still owns cannot be released.
        assert!(manager.release(GenerationId::new(6)).is_err());
        assert!(manager.release(GenerationId::new(40)).is_err());
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn published_metadata_is_never_modified_in_place() {
    let dir = scratch("immutable");
    let result = (|| {
        let manager = open_manager(&dir);
        let first = manager.publish(write(1, 1, vec![change(1, 5)]))?;
        let catalog_bytes = std::fs::read(catalog_path(&dir, GenerationId::new(1)))?;
        let generation_bytes = std::fs::read(gen_path(&dir, 1, GenerationId::new(5)))?;
        let pointer_bytes = std::fs::read(publication_pointer_path(&dir))?;

        manager.publish(write(1, 2, vec![change(1, 6)]))?;
        assert_eq!(
            std::fs::read(catalog_path(&dir, GenerationId::new(1)))?,
            catalog_bytes,
            "a superseded catalog state is retained byte-for-byte"
        );
        assert_eq!(
            std::fs::read(gen_path(&dir, 1, GenerationId::new(5)))?,
            generation_bytes,
            "a published generation is retained byte-for-byte"
        );
        assert_ne!(
            std::fs::read(publication_pointer_path(&dir))?,
            pointer_bytes,
            "publication replaces the pointer rather than trusting the old one"
        );
        assert_eq!(
            load_catalog(&catalog_path(&dir, GenerationId::new(1)))?,
            first.catalog,
            "the superseded state still decodes to what was published"
        );
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn readers_never_observe_a_partial_publication() {
    let dir = scratch("reader-publish");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(1, 1, vec![change(1, 5), change(2, 7)]))?;
        let before = manager.reader()?;
        assert!(before.is_complete());
        assert_eq!(before.catalog_generation(), GenerationId::new(1));
        assert_eq!(
            before
                .object(ObjectId::new(1))
                .expect("record")
                .current_generation,
            GenerationId::new(5)
        );

        // A publication advances one object while the reader is live.
        manager.publish(write(1, 2, vec![change(1, 6)]))?;

        // The reader keeps observing the complete state it began reading.
        assert!(before.is_complete());
        assert_eq!(before.catalog_generation(), GenerationId::new(1));
        assert_eq!(
            before
                .object(ObjectId::new(1))
                .expect("record")
                .current_generation,
            GenerationId::new(5)
        );
        assert_eq!(
            before
                .object(ObjectId::new(2))
                .expect("record")
                .current_generation,
            GenerationId::new(7)
        );
        let snapshot_generation = before.generation(GenerationId::new(5)).expect("generation");
        assert_eq!(snapshot_generation.generation_id, GenerationId::new(5));
        assert_eq!(
            snapshot_generation.publication_generation,
            GenerationId::new(1)
        );

        // A reader that starts after the publication observes the new state.
        let after = manager.reader()?;
        assert!(after.is_complete());
        assert_eq!(after.catalog_generation(), GenerationId::new(2));
        let record = after.object(ObjectId::new(1)).expect("record");
        assert_eq!(record.current_generation, GenerationId::new(6));
        assert_eq!(record.retained_generations, vec![GenerationId::new(5)]);
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn gc_keeps_generations_a_live_reader_still_requires() {
    let dir = scratch("gc-reader");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(1, 1, vec![change(1, 5)]))?;
        let reader = manager.reader()?;
        manager.publish(write(1, 2, vec![change(1, 6)]))?;
        manager.release(GenerationId::new(5))?;

        // The generation is no longer referenced by published metadata, but the
        // live reader still requires it.
        let guarded = manager.gc()?;
        assert!(guarded.reclaimed.is_empty(), "{guarded:?}");
        assert!(guarded.retained.contains(&GenerationId::new(5)));
        assert!(gen_path(&dir, 1, GenerationId::new(5)).exists());
        assert!(reader.generation(GenerationId::new(5)).is_some());

        // Once the reader releases its reference and no durable reference needs
        // the generation, collection may reclaim it.
        drop(reader);
        let collected = manager.gc()?;
        assert!(
            collected.reclaimed.contains(&GenerationId::new(5)),
            "{collected:?}"
        );
        assert!(!gen_path(&dir, 1, GenerationId::new(5)).exists());
        manager.validate()?;
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn gc_reclaims_only_released_and_unreferenced_generations() {
    let dir = scratch("gc-reclaim");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(1, 1, vec![change(1, 5)]))?;
        manager.publish(write(1, 2, vec![change(1, 6)]))?;

        // While the superseded generation is retained, collection keeps it.
        let retained = manager.gc()?;
        assert!(retained.reclaimed.is_empty(), "{retained:?}");
        assert!(retained.retained.contains(&GenerationId::new(5)));
        assert!(retained.retained.contains(&GenerationId::new(6)));
        assert!(gen_path(&dir, 1, GenerationId::new(5)).exists());

        // After retention is released the superseded generation and the catalog
        // states that no longer describe the current boundary are collectable.
        manager.release(GenerationId::new(5))?;
        let collected = manager.gc()?;
        assert!(
            collected.reclaimed.contains(&GenerationId::new(5)),
            "{collected:?}"
        );
        assert!(collected.retained.contains(&GenerationId::new(6)));
        assert!(!gen_path(&dir, 1, GenerationId::new(5)).exists());
        assert!(gen_path(&dir, 1, GenerationId::new(6)).exists());

        // The current state remains complete and recoverable.
        manager.validate()?;
        let recovered = GenerationManager::recover(&dir)?;
        assert_eq!(recovered.generations, vec![GenerationId::new(6)]);
        assert_eq!(recovered.pointer, manager.pointer()?);

        // Collection is idempotent.
        let again = manager.gc()?;
        assert!(again.reclaimed.is_empty(), "{again:?}");
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn gc_never_reclaims_the_current_state() {
    let dir = scratch("gc-current");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(1, 1, vec![change(1, 5), change(2, 6)]))?;
        manager.publish(write(1, 2, vec![change(1, 7)]))?;
        let outcome = manager.gc()?;
        let pointer = manager.pointer()?;
        // The superseded generation, the current generation, and the generation
        // of the object this publication did not touch are all retained.
        assert!(outcome.retained.contains(&GenerationId::new(5)));
        assert!(outcome.retained.contains(&GenerationId::new(6)));
        assert!(outcome.retained.contains(&GenerationId::new(7)));
        assert!(!outcome.reclaimed.contains(&GenerationId::new(7)));
        assert!(catalog_path(&dir, pointer.catalog_generation).exists());
        assert!(publication_pointer_path(&dir).exists());
        assert_eq!(GenerationManager::recover(&dir)?.pointer, pointer);
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn interrupted_reclamation_leaves_a_recoverable_state() {
    let dir = scratch("gc-interrupt");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(1, 1, vec![change(1, 5), change(2, 6)]))?;
        manager.publish(write(1, 2, vec![change(1, 7), change(2, 8)]))?;
        manager.publish(
            write(1, 3, vec![change(1, 9), change(2, 10)]).with_releases(vec![
                GenerationId::new(5),
                GenerationId::new(6),
                GenerationId::new(7),
                GenerationId::new(8),
            ])?,
        )?;

        // A reclamation interrupted after its first removal must leave the
        // authoritative state untouched and fully recoverable.
        let interrupted = manager.gc_with_fail_point(GcFailPoint::DuringReclaim);
        assert!(interrupted.is_err());
        assert_eq!(interrupted.expect_err("failure").kind(), ErrorKind::Io);
        let recovered = GenerationManager::recover(&dir)?;
        assert_eq!(
            recovered.generations,
            vec![GenerationId::new(9), GenerationId::new(10)]
        );
        assert_eq!(recovered.pointer, manager.pointer()?);
        let reopened = open_manager(&dir);
        reopened.validate()?;
        assert_eq!(reopened.pointer()?, manager.pointer()?);

        // A crash before any removal is equally safe.
        assert!(manager
            .gc_with_fail_point(GcFailPoint::BeforeReclaim)
            .is_err());
        reopened.validate()?;

        // Re-running collection completes the pass.
        let completed = manager.gc()?;
        assert!(completed.reclaimed.contains(&GenerationId::new(6)));
        assert!(completed.reclaimed.contains(&GenerationId::new(7)));
        assert!(completed.reclaimed.contains(&GenerationId::new(8)));
        manager.validate()?;

        // A crash after the pass completed leaves the same durable state.
        assert!(manager
            .gc_with_fail_point(GcFailPoint::AfterReclaim)
            .is_err());
        assert_eq!(
            GenerationManager::recover(&dir)?.generations,
            vec![GenerationId::new(9), GenerationId::new(10)]
        );
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn failure_at_every_publication_boundary_leaves_the_previous_state_current() {
    let boundaries = [
        PublicationFailPoint::BeforeBuild,
        PublicationFailPoint::AfterBuild,
        PublicationFailPoint::AfterFlush,
        PublicationFailPoint::DuringVerify,
        PublicationFailPoint::BeforeSync,
        PublicationFailPoint::AfterSync,
        PublicationFailPoint::DuringPublish,
    ];
    for boundary in boundaries {
        let dir = scratch("publish-failure");
        let result = (|| {
            let manager = open_manager(&dir);
            let published = manager.publish(write(1, 1, vec![change(1, 5)]))?;
            let failed = manager.publish_with_fail_point(write(1, 2, vec![change(1, 6)]), boundary);
            assert!(failed.is_err(), "{boundary:?} must fail");

            // The previous state remains authoritative: it is still the pointer
            // the manager exposes and still what the durable record names.
            assert_eq!(manager.pointer()?, published.pointer);
            assert_eq!(manager.load()?, published.catalog);
            assert_eq!(load_publication_pointer(&dir)?, published.pointer);

            // A restart observes the previous complete generation.
            let reopened = open_manager(&dir);
            assert_eq!(reopened.pointer()?, published.pointer);
            assert_eq!(
                reopened.load()?.generation_of(ObjectId::new(1)),
                Some(GenerationId::new(5))
            );
            reopened.validate()?;
            let recovered = GenerationManager::recover(&dir)?;
            assert_eq!(recovered.generations, vec![GenerationId::new(5)]);
            assert_eq!(recovered.source, PublicationPointerSource::Pointer);

            // The superseded generation is untouched and still readable.
            assert_eq!(
                load_generation(&dir, GenerationId::new(5))?.generation_id,
                GenerationId::new(5)
            );
            Ok::<(), PlomidError>(())
        })();
        cleanup(&dir);
        assert!(result.is_ok(), "{boundary:?}: {result:?}");
    }
}

#[test]
fn failure_immediately_after_pointer_replacement_is_durable() {
    let dir = scratch("publish-after-pointer");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(1, 1, vec![change(1, 5)]))?;
        let failed = manager.publish_with_fail_point(
            write(1, 2, vec![change(1, 6)]),
            PublicationFailPoint::AfterPublish,
        );
        assert!(failed.is_err());

        // The pointer swap already happened, so the new state is the durable
        // one and recovery selects it deterministically.
        let recovered = GenerationManager::recover(&dir)?;
        assert_eq!(recovered.generations.len(), 2);
        assert_eq!(
            recovered.catalog.generation_of(ObjectId::new(1)),
            Some(GenerationId::new(6))
        );
        let reopened = open_manager(&dir);
        assert_eq!(
            reopened.load()?.generation_of(ObjectId::new(1)),
            Some(GenerationId::new(6))
        );
        reopened.validate()?;
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn incomplete_publication_leaves_the_previous_generation_current() {
    let dir = scratch("incomplete");
    let result = (|| {
        let manager = open_manager(&dir);
        let published = manager.publish(write(1, 1, vec![change(1, 5)]))?;

        // Build and flush a complete new generation, then never publish it.
        let mut publication = manager.begin_publication(write(1, 2, vec![change(1, 6)]))?;
        publication.flush()?;
        assert!(!publication.is_published());
        assert_eq!(manager.pointer()?, published.pointer);
        assert_eq!(
            manager.load()?.generation_of(ObjectId::new(1)),
            Some(GenerationId::new(5))
        );
        drop(publication);

        // The staged artifacts exist but are not discoverable as metadata.
        assert!(plomid_storage::generation_staged_path_in_schema(
            &dir,
            SEED_DB,
            PUB_SCHEMA,
            plomid_core::TableId::new(1),
            GenerationId::new(6)
        )
        .exists());
        assert!(!discover_generation_ids(&dir)?.contains(&GenerationId::new(6)));
        assert_eq!(discover_generation_ids(&dir)?, vec![GenerationId::new(5)]);

        // Reopening selects the previous complete state.
        let reopened = open_manager(&dir);
        assert_eq!(reopened.pointer()?, published.pointer);
        assert_eq!(
            reopened.published_generations()?,
            vec![GenerationId::new(5)]
        );
        reopened.validate()?;
        assert_eq!(GenerationManager::recover(&dir)?.pointer, published.pointer);
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_publication_that_fails_verification_is_never_published() {
    let dir = scratch("verify-failure");
    let result = (|| {
        let manager = open_manager(&dir);
        let published = manager.publish(write(1, 1, vec![change(1, 5)]))?;

        // A complete publication is built and flushed, then the staged metadata
        // is damaged before verification reads it back.
        let mut publication = manager.begin_publication(write(1, 2, vec![change(1, 6)]))?;
        publication.flush()?;
        corrupt(&catalog_staged_path(&dir, GenerationId::new(2)), 8)?;
        let error = publication
            .verify()
            .expect_err("damaged staged metadata must not verify");
        assert_eq!(error.kind(), ErrorKind::Corruption);
        assert!(!publication.is_published());
        drop(publication);

        // The previous state remains current, durable, and complete.
        assert_eq!(manager.pointer()?, published.pointer);
        assert_eq!(manager.load()?, published.catalog);
        assert_eq!(
            manager
                .reader()?
                .generation(GenerationId::new(5))
                .map(|metadata| metadata.generation_id),
            Some(GenerationId::new(5))
        );
        let reopened = open_manager(&dir);
        assert_eq!(reopened.pointer()?, published.pointer);
        assert_eq!(
            reopened.load()?.generation_of(ObjectId::new(1)),
            Some(GenerationId::new(5))
        );
        reopened.validate()?;
        // The damaged staging artifact is not discoverable as metadata.
        assert_eq!(discover_generation_ids(&dir)?, vec![GenerationId::new(5)]);
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn corrupted_current_metadata_is_rejected_on_reopen() {
    // The authoritative pointer names damaged state, so the store must fail
    // loudly instead of substituting a different state.
    let catalog_dir_test = scratch("corrupt-catalog");
    let result = (|| {
        let manager = open_manager(&catalog_dir_test);
        let published = manager.publish(write(1, 1, vec![change(1, 5)]))?;
        let path = catalog_path(&catalog_dir_test, published.pointer.catalog_generation);
        corrupt(&path, 0)?;
        assert!(load_catalog(&path).is_err());
        let error = GenerationManager::recover(&catalog_dir_test).expect_err("corrupt catalog");
        assert_eq!(error.kind(), ErrorKind::Corruption);
        assert!(GenerationManager::open(&catalog_dir_test).is_err());
        Ok::<(), PlomidError>(())
    })();
    cleanup(&catalog_dir_test);
    assert!(result.is_ok(), "{result:?}");

    // A corruption of the checked header fields and of the checksum is rejected
    // in every case, and a corrupted generation makes the state unusable.
    for offset in [0_usize, 4, 8, 12, 40, 56] {
        let dir = scratch("corrupt-fields");
        let result = (|| {
            let manager = open_manager(&dir);
            let published = manager.publish(write(1, 1, vec![change(1, 5)]))?;
            let path = catalog_path(&dir, published.pointer.catalog_generation);
            corrupt(&path, offset)?;
            assert!(GenerationManager::recover(&dir).is_err());
            Ok::<(), PlomidError>(())
        })();
        cleanup(&dir);
        assert!(result.is_ok(), "catalog offset {offset}: {result:?}");
    }

    let dir = scratch("corrupt-generation");
    let result = (|| {
        let manager = open_manager(&dir);
        let published = manager.publish(write(1, 1, vec![change(1, 5)]))?;
        corrupt(&gen_path(&dir, 1, GenerationId::new(5)), 8)?;
        assert!(load_generation(&dir, GenerationId::new(5)).is_err());
        let error = GenerationManager::recover(&dir).expect_err("corrupt generation");
        assert_eq!(error.kind(), ErrorKind::Corruption);
        // The catalog itself is intact, so its own decode still succeeds.
        assert!(load_catalog(&catalog_path(&dir, published.pointer.catalog_generation)).is_ok());
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn corrupt_publication_pointer_falls_back_to_the_newest_valid_state() {
    let dir = scratch("corrupt-pointer");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(1, 1, vec![change(1, 5)]))?;
        let published = manager.publish(write(1, 2, vec![change(1, 6)]))?;

        // Policy 1: the pointer is unusable, so the newest published catalog
        // state is selected deterministically.
        corrupt(&publication_pointer_path(&dir), 0)?;
        let recovered = GenerationManager::recover(&dir)?;
        assert_eq!(recovered.source, PublicationPointerSource::Catalog);
        assert_eq!(recovered.pointer, published.pointer);
        assert_eq!(recovered.catalog, published.catalog);

        // Policy 2: with a checkpoint naming the catalog version, the checkpoint
        // chain selects the same durable state.
        std::fs::remove_file(publication_pointer_path(&dir))?;
        create_checkpoint(&dir, manager.checkpoint_request()?)?;
        let recovered = GenerationManager::recover(&dir)?;
        assert_eq!(recovered.source, PublicationPointerSource::Checkpoint);
        assert_eq!(recovered.pointer, published.pointer);
        let reopened = open_manager(&dir);
        assert_eq!(reopened.pointer()?, published.pointer);
        reopened.validate()?;
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn stale_durable_boundaries_are_rejected() {
    let dir = scratch("stale");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(5, 9, vec![change(1, 5)]))?;
        let error = manager
            .publish(write(4, 10, vec![change(1, 6)]))
            .expect_err("a backwards storage generation must be rejected");
        assert_eq!(error.kind(), ErrorKind::Conflict);
        let error = manager
            .publish(write(5, 8, vec![change(1, 6)]))
            .expect_err("a backwards WAL boundary must be rejected");
        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(
            manager.load()?.generation_of(ObjectId::new(1)),
            Some(GenerationId::new(5))
        );
        // Advancing, rather than repeating, the durable boundary is accepted.
        let advanced = manager.publish(write(6, 12, vec![change(1, 6)]))?;
        assert_eq!(advanced.pointer.storage_generation, GenerationId::new(6));
        assert_eq!(advanced.pointer.checkpoint_lsn, Lsn::new(12));
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

/// Flips one byte of a persisted metadata file.
fn corrupt(path: &Path, offset: usize) -> Result<(), PlomidError> {
    let mut bytes = std::fs::read(path)?;
    assert!(offset < bytes.len(), "offset {offset} is beyond the image");
    bytes[offset] ^= 0xFF;
    std::fs::write(path, bytes)?;
    Ok(())
}

#[test]
fn fresh_root_reports_absence_and_publishes_its_first_state() {
    let dir = scratch("fresh");
    let result = (|| {
        assert!(GenerationManager::open(&dir).is_ok());
        assert_eq!(GenerationManager::recover_if_present(&dir)?, None);
        let error = GenerationManager::recover(&dir).expect_err("no published state");
        assert_eq!(error.kind(), ErrorKind::NotFound);
        let manager = open_manager(&dir);
        assert_eq!(
            manager.pointer().expect_err("no pointer").kind(),
            ErrorKind::NotFound
        );
        let error = manager.reader().err().expect("no reader");
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(manager.gc().is_err());

        // The first publication establishes generation one deterministically.
        let published = manager.publish(write(1, 3, vec![change(1, 5)]))?;
        assert_eq!(published.pointer.catalog_generation, GenerationId::new(1));
        assert_eq!(published.pointer.catalog_version, CatalogVersion::new(1));
        assert_eq!(
            GenerationManager::recover_if_present(&dir)?.map(|outcome| outcome.pointer),
            Some(published.pointer)
        );
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn historical_catalog_versions_keep_the_schema_they_were_published_with() {
    let dir = scratch("schema-version");
    let result = (|| {
        let manager = open_manager(&dir);
        let first = manager.publish(write(1, 1, vec![change(1, 5)]))?;
        // A schema change publishes a new catalog version; it never rewrites the
        // schema state an older generation was published against.
        let redefined = ObjectChange::new(
            ObjectId::new(1),
            SchemaMetadata::new(
                SchemaId::new(7),
                CatalogVersion::new(2),
                vec![SchemaColumn {
                    column_id: ColumnId::new(9),
                    type_code: 25,
                }],
            )?,
            GenerationId::new(6),
            vec![reference(1, 6, 2)],
        )?;
        let second = manager.publish(write(1, 2, vec![redefined]))?;

        let record = second.catalog.object(ObjectId::new(1)).expect("record");
        assert_eq!(record.schema.schema_version, CatalogVersion::new(2));
        assert_eq!(record.schema.columns.len(), 1);
        assert_eq!(
            first
                .catalog
                .object(ObjectId::new(1))
                .expect("record")
                .schema
                .schema_version,
            CatalogVersion::new(1)
        );
        // The superseded catalog state on disk still carries the old schema.
        let superseded = plomid_storage::load_catalog_for_generation(&dir, GenerationId::new(1))?
            .expect("superseded catalog state");
        assert_eq!(
            superseded
                .object(ObjectId::new(1))
                .expect("record")
                .schema
                .columns
                .len(),
            2
        );
        // The retained generation still names the schema it was published with.
        let retained = load_generation(&dir, GenerationId::new(5))?;
        assert_eq!(retained.catalog_version, CatalogVersion::new(1));
        manager.validate()?;
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_checkpoint_pins_the_catalog_state_a_released_generation_requires() {
    let dir = scratch("checkpoint-pin");
    let result = (|| {
        let manager = open_manager(&dir);
        let first = manager.publish(write(1, 1, vec![change(1, 5)]))?;

        // The checkpoint names the catalog version the recovery chain resolves.
        let checkpoint = create_checkpoint(&dir, manager.checkpoint_request()?)?;
        assert_eq!(
            checkpoint.catalog_generation, first.pointer.catalog_version,
            "the checkpoint must name the published catalog version"
        );
        assert_eq!(
            checkpoint.storage_generation,
            first.pointer.storage_generation
        );
        assert_eq!(checkpoint.checkpoint_lsn, first.pointer.checkpoint_lsn);

        manager.publish(write(1, 2, vec![change(1, 6)]))?;
        manager.release(GenerationId::new(5))?;

        // The generation is no longer referenced by the current catalog state,
        // but the checkpoint still resolves to the state that requires it.
        let outcome = manager.gc()?;
        assert!(outcome.reclaimed.is_empty(), "{outcome:?}");
        assert!(outcome.retained.contains(&GenerationId::new(5)));
        assert!(catalog_path(&dir, GenerationId::new(1)).exists());
        manager.validate()?;
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_published_generation_is_never_replaced() {
    let dir = scratch("immutable-generation");
    let result = (|| {
        let manager = open_manager(&dir);
        let published = manager.publish(write(1, 1, vec![change(1, 5)]))?;
        let bytes = std::fs::read(gen_path(&dir, 1, GenerationId::new(5)))?;

        // Reusing the identity of a durable generation would replace an image a
        // reader may still be observing, so the request is rejected.
        let error = manager
            .publish(write(1, 2, vec![change(1, 5)]))
            .expect_err("an existing generation must not be replaced");
        assert_eq!(error.kind(), ErrorKind::Conflict);
        // The same identity is refused for a different object as well.
        let error = manager
            .publish(write(1, 2, vec![change(2, 5)]))
            .expect_err("an existing generation must not be shared");
        assert_eq!(error.kind(), ErrorKind::Conflict);
        // A generation older than the object's current generation moves durable
        // state backwards.
        let error = manager
            .publish(write(1, 2, vec![change(1, 4)]))
            .expect_err("generations of an object must increase");
        assert_eq!(error.kind(), ErrorKind::Conflict);

        // Nothing was published, and the durable image is byte-for-byte intact.
        assert_eq!(manager.pointer()?, published.pointer);
        assert_eq!(
            std::fs::read(gen_path(&dir, 1, GenerationId::new(5)))?,
            bytes
        );
        assert_eq!(
            manager.load()?.generation_of(ObjectId::new(1)),
            Some(GenerationId::new(5))
        );
        manager.validate()?;
        // A newer generation of the same object is accepted.
        let advanced = manager.publish(write(1, 2, vec![change(1, 6)]))?;
        assert_eq!(advanced.pointer.catalog_generation, GenerationId::new(2));
        assert_eq!(
            advanced
                .catalog
                .object(ObjectId::new(1))
                .expect("record")
                .retained_generations,
            vec![GenerationId::new(5)]
        );
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn physical_references_are_validated_against_the_storage_image() {
    let dir = scratch("physical");
    let result = (|| {
        let storage = StorageManager::create(&dir, 32, 1)?;
        assert_eq!(storage.storage_generation(), GenerationId::new(1));
        let manager = open_manager(&dir);
        manager.publish(PublicationRequest::write(
            storage.storage_generation(),
            Lsn::new(1),
            vec![ObjectChange::new(
                ObjectId::new(1),
                schema(1),
                GenerationId::new(5),
                vec![segment_reference(1, 5, 1)],
            )?],
        )?)?;
        // The reference names a segment the durable image contains.
        manager.validate_physical()?;

        // A reference to a segment the durable image does not contain is
        // rejected, while the metadata relationships stay consistent.
        manager.publish(PublicationRequest::write(
            storage.storage_generation(),
            Lsn::new(2),
            vec![ObjectChange::new(
                ObjectId::new(1),
                schema(1),
                GenerationId::new(6),
                vec![segment_reference(1, 6, 9)],
            )?],
        )?)?;
        let error = manager
            .validate_physical()
            .expect_err("a reference to a missing segment must be rejected");
        assert_eq!(error.kind(), ErrorKind::Corruption);
        // The metadata relationships themselves stay consistent, and recovery
        // still resolves the published state: only the physical reference is
        // unresolvable.
        manager.validate()?;
        let recovered = GenerationManager::recover(&dir)?;
        assert_eq!(recovered.pointer, manager.pointer()?);
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn gc_reclaims_crash_orphaned_generation_files() {
    let dir = scratch("gc-orphan");
    let result = (|| {
        let manager = open_manager(&dir);
        manager.publish(write(1, 1, vec![change(1, 5)]))?;
        assert!(gen_path(&dir, 1, GenerationId::new(5)).exists());
        // Simulate the kill window: generation 9's files landed on disk
        // without any catalog publication referencing them.
        let orphan = gen_path(&dir, 1, GenerationId::new(9));
        std::fs::create_dir_all(orphan.parent().expect("orphan parent"))?;
        std::fs::copy(gen_path(&dir, 1, GenerationId::new(5)), &orphan)?;
        assert!(orphan.exists());

        // The sweep removes exactly the unreferenced identity; the published
        // generation and its catalog state survive untouched.
        let swept = manager.gc()?;
        assert!(swept.reclaimed.contains(&GenerationId::new(9)), "{swept:?}");
        assert!(!orphan.exists(), "orphan file must be gone");
        assert!(gen_path(&dir, 1, GenerationId::new(5)).exists());
        assert_eq!(discover_generation_ids(&dir)?, vec![GenerationId::new(5)]);

        // Idempotent: a second pass finds nothing and changes nothing.
        let again = manager.gc()?;
        assert!(again.reclaimed.is_empty(), "{again:?}");
        assert!(gen_path(&dir, 1, GenerationId::new(5)).exists());
        Ok::<(), PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}
