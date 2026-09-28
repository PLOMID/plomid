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
//! Generation-layout tests: table-local generation directories, generation
//! `META.dat`, and the prohibition on any database-level generation tree.
use plomid_core::{CatalogVersion, GenerationId, Lsn, ObjectId, TableId};
use plomid_storage::layout::{
    generation_dir_name, generation_from_dir_name, validate_generation_flat, DatabaseLayout,
    META_FILE_NAME,
};
use plomid_storage::{GenerationMetadata, PublicationState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-layout-generations-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// Builds a minimal valid generation record for layout-level tests. Cross-record
/// relationships are the generation manager's concern, not the layout's.
fn generation_metadata(generation: GenerationId, object: ObjectId) -> GenerationMetadata {
    GenerationMetadata::new(
        generation,
        object,
        CatalogVersion::new(1),
        GenerationId::new(1),
        GenerationId::new(1),
        Lsn::new(0),
        None,
        PublicationState::Published,
        Vec::new(),
    )
    .expect("generation metadata")
}

#[test]
fn generation_paths_are_table_local() {
    let root = scratch("paths");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(1);
    let generation = GenerationId::new(1);

    assert_eq!(generation_dir_name(generation), "GEN-00000000000000000001");
    assert_eq!(
        generation_from_dir_name("GEN-00000000000000000001"),
        Some(generation)
    );

    assert_eq!(
        layout.generation_dir_flat(table_id, generation),
        layout
            .table_generations_dir(table_id)
            .join("GEN-00000000000000000001")
    );
    assert_eq!(
        layout.generation_meta_path_flat(table_id, generation),
        layout
            .generation_dir_flat(table_id, generation)
            .join(META_FILE_NAME)
    );
    assert_eq!(
        layout.generation_segments_dir_flat(table_id, generation),
        layout
            .generation_dir_flat(table_id, generation)
            .join("segments")
    );

    // The generation lives *inside* the owning table directory, which is what
    // lets one table own many generations and lets a table span devices.
    assert!(layout
        .generation_dir_flat(table_id, generation)
        .starts_with(layout.table_dir(table_id)));

    cleanup(&root);
}

#[test]
fn no_database_level_generation_tree_exists() {
    let root = scratch("no-root-generations");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    for table in 1..=2_u64 {
        layout
            .ensure_table(TableId::new(table))
            .expect("ensure table");
        for generation in 1..=3_u64 {
            layout
                .ensure_generation_dir_flat(TableId::new(table), GenerationId::new(generation))
                .expect("ensure generation");
        }
    }

    // There must be exactly one generation hierarchy, and it is table-local.
    assert!(
        !root.join("generation").exists(),
        "no singular database-level generation directory may exist"
    );
    assert!(
        !root.join("generations").exists(),
        "no database-level generations directory may exist"
    );
    for table in 1..=2_u64 {
        assert!(layout.table_generations_dir(TableId::new(table)).is_dir());
    }

    cleanup(&root);
}

#[test]
fn creating_a_generation_materializes_its_directory_and_segments() {
    let root = scratch("create");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(4);
    let generation = GenerationId::new(2);

    layout
        .ensure_generation_dir_flat(table_id, generation)
        .expect("ensure generation");

    assert!(layout.generation_dir_flat(table_id, generation).is_dir());
    assert!(layout
        .generation_segments_dir_flat(table_id, generation)
        .is_dir());
    // Ensuring a generation never invents metadata for it: the generation
    // manager publishes the record.
    assert!(!layout
        .generation_meta_path_flat(table_id, generation)
        .exists());
    // The owning table was materialized first.
    assert!(layout.table_dir(table_id).is_dir());

    cleanup(&root);
}

#[test]
fn zero_generation_identity_is_rejected() {
    let root = scratch("zero");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    assert!(layout
        .ensure_generation_dir_flat(TableId::new(1), GenerationId::new(0))
        .is_err());
    cleanup(&root);
}
#[test]
fn generation_metadata_is_read_from_the_table_local_path() {
    let root = scratch("meta");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(9);
    let generation = GenerationId::new(5);
    layout
        .ensure_generation_dir_flat(table_id, generation)
        .expect("ensure generation");

    let meta = generation_metadata(generation, ObjectId::new(9));
    std::fs::write(
        layout.generation_meta_path_flat(table_id, generation),
        meta.encode().expect("encode"),
    )
    .expect("write");

    validate_generation_flat(&layout, table_id, generation).expect("validate");
    assert_eq!(
        GenerationMetadata::decode(
            &std::fs::read(layout.generation_meta_path_flat(table_id, generation)).expect("read")
        )
        .expect("decode"),
        meta
    );

    cleanup(&root);
}

#[test]
fn generation_metadata_from_another_identity_is_rejected() {
    let root = scratch("mismatch");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(1);
    let generation = GenerationId::new(1);
    layout
        .ensure_generation_dir_flat(table_id, generation)
        .expect("ensure generation");

    // A record for a different generation must not validate at this path.
    let wrong = generation_metadata(GenerationId::new(2), ObjectId::new(1));
    std::fs::write(
        layout.generation_meta_path_flat(table_id, generation),
        wrong.encode().expect("encode"),
    )
    .expect("write");

    assert!(validate_generation_flat(&layout, table_id, generation).is_err());
    cleanup(&root);
}

#[test]
fn generation_discovery_spans_tables_in_identity_order() {
    let root = scratch("discover");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    for (table, generations) in [(1_u64, vec![2_u64, 1]), (2, vec![1])] {
        for generation in generations {
            let table_id = TableId::new(table);
            let generation_id = GenerationId::new(generation);
            layout
                .ensure_generation_dir_flat(table_id, generation_id)
                .expect("ensure generation");
            let meta = generation_metadata(generation_id, ObjectId::new(table));
            std::fs::write(
                layout.generation_meta_path_flat(table_id, generation_id),
                meta.encode().expect("encode"),
            )
            .expect("write");
        }
    }

    let found = layout.discover_generation_files().expect("discover");
    let mut ids: Vec<u64> = found.iter().map(|(id, _)| id.get()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 1, 2]);
    // Every discovered metadata file is inside a table-local generations tree.
    for (_, path) in &found {
        assert!(path.starts_with(layout.tables_dir()));
    }

    cleanup(&root);
}

#[test]
fn a_generation_survives_a_restart() {
    let root = scratch("restart");
    let table_id = TableId::new(11);
    let generation = GenerationId::new(1);
    let meta = generation_metadata(generation, ObjectId::new(11));
    {
        let layout = DatabaseLayout::new(&root);
        layout.initialize().expect("initialize");
        layout
            .ensure_generation_dir_flat(table_id, generation)
            .expect("ensure generation");
        std::fs::write(
            layout.generation_meta_path_flat(table_id, generation),
            meta.encode().expect("encode"),
        )
        .expect("write");
    }

    let reopened = DatabaseLayout::new(&root);
    validate_generation_flat(&reopened, table_id, generation).expect("validate after restart");
    assert_eq!(
        reopened
            .discover_generation_files()
            .expect("discover")
            .len(),
        1
    );

    cleanup(&root);
}
