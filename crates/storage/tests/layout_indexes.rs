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
//! Index-layout tests: schema-local index directories, index `META.dat`, and
//! per-generation index metadata file paths inside the logical hierarchy.
use plomid_core::{DatabaseId, ErrorKind, GenerationId, IndexId, SchemaId, TableId};
use plomid_storage::layout::{
    index_dir_name, index_from_dir_name, index_generation_file_name,
    index_generation_from_file_name, validate_index, DatabaseLayout, IndexMetadata, META_FILE_NAME,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Seeded identities: every scratch root materializes the default database and
/// schema, so hierarchy-aware paths resolve deterministically.
const MY_DB: DatabaseId = DatabaseId::new(1);
const MY_SCHEMA: SchemaId = SchemaId::new(1);

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-layout-indexes-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn index_paths_are_table_local() {
    let root = scratch("paths");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(1);
    let index_id = IndexId::new(1);

    assert_eq!(index_dir_name(index_id), "I-00000000000000000001");
    assert_eq!(
        index_from_dir_name("I-00000000000000000001"),
        Some(index_id)
    );
    assert_eq!(index_from_dir_name("I-1"), None);

    assert_eq!(
        layout.index_dir_in_schema(MY_DB, MY_SCHEMA, table_id, index_id),
        layout
            .table_indexes_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
            .join("I-00000000000000000001")
    );
    assert_eq!(
        layout.index_meta_path_in_schema(MY_DB, MY_SCHEMA, table_id, index_id),
        layout
            .index_dir_in_schema(MY_DB, MY_SCHEMA, table_id, index_id)
            .join(META_FILE_NAME)
    );
    assert!(layout
        .index_dir_in_schema(MY_DB, MY_SCHEMA, table_id, index_id)
        .starts_with(layout.table_dir_in_schema(MY_DB, MY_SCHEMA, table_id)));

    cleanup(&root);
}

#[test]
fn index_generation_file_names_are_deterministic() {
    let root = scratch("names");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(2);
    let index_id = IndexId::new(3);
    let generation = GenerationId::new(4);

    assert_eq!(
        index_generation_file_name(generation),
        "GEN-00000000000000000004.dat"
    );
    assert_eq!(
        index_generation_from_file_name("GEN-00000000000000000004.dat"),
        Some(generation)
    );
    assert_eq!(index_generation_from_file_name("GEN-4.dat"), None);
    assert_eq!(
        index_generation_from_file_name("META.dat"),
        None,
        "the index metadata record is not a generation file"
    );

    // One metadata file per generation, all inside the index directory.
    assert_eq!(
        layout.index_generation_path_in_schema(MY_DB, MY_SCHEMA, table_id, index_id, generation),
        layout
            .index_dir_in_schema(MY_DB, MY_SCHEMA, table_id, index_id)
            .join("GEN-00000000000000000004.dat")
    );
    assert_eq!(
        layout.index_generation_staged_path_in_schema(
            MY_DB, MY_SCHEMA, table_id, index_id, generation
        ),
        layout
            .index_dir_in_schema(MY_DB, MY_SCHEMA, table_id, index_id)
            .join("GEN-00000000000000000004.dat.tmp")
    );

    cleanup(&root);
}

#[test]
fn creating_an_index_materializes_and_publishes_its_metadata() {
    let root = scratch("create");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(6);
    let index_id = IndexId::new(2);

    layout
        .ensure_index_in_schema(MY_DB, MY_SCHEMA, table_id, index_id)
        .expect("ensure index");

    assert!(layout
        .index_dir_in_schema(MY_DB, MY_SCHEMA, table_id, index_id)
        .is_dir());
    // The index owns its identity record, created together with the directory.
    assert_eq!(
        layout
            .read_index_meta_in_schema(MY_DB, MY_SCHEMA, table_id, index_id)
            .expect("read"),
        IndexMetadata::new(index_id, table_id).expect("metadata")
    );
    validate_index(&layout, MY_DB, MY_SCHEMA, table_id, index_id).expect("validate");
    // The owning table was materialized first.
    assert!(layout
        .table_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
        .is_dir());

    cleanup(&root);
}

#[test]
fn index_discovery_is_ordered_and_table_scoped() {
    let root = scratch("discover");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    for index in [3_u64, 1, 2] {
        layout
            .ensure_index_in_schema(MY_DB, MY_SCHEMA, TableId::new(1), IndexId::new(index))
            .expect("ensure index");
    }
    layout
        .ensure_index_in_schema(MY_DB, MY_SCHEMA, TableId::new(2), IndexId::new(9))
        .expect("ensure index");

    assert_eq!(
        layout
            .discover_index_ids_in_schema(MY_DB, MY_SCHEMA, TableId::new(1))
            .expect("discover"),
        vec![IndexId::new(1), IndexId::new(2), IndexId::new(3)]
    );
    assert_eq!(
        layout
            .discover_index_ids_in_schema(MY_DB, MY_SCHEMA, TableId::new(2))
            .expect("discover"),
        vec![IndexId::new(9)]
    );

    cleanup(&root);
}

#[test]
fn index_metadata_from_another_index_is_rejected() {
    let root = scratch("mismatch");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    layout
        .ensure_index_in_schema(MY_DB, MY_SCHEMA, TableId::new(1), IndexId::new(1))
        .expect("ensure index");

    // A record describing a different index must not validate here.
    let wrong = IndexMetadata::new(IndexId::new(2), TableId::new(1)).expect("metadata");
    wrong
        .publish(&layout.index_meta_path_in_schema(
            MY_DB,
            MY_SCHEMA,
            TableId::new(1),
            IndexId::new(1),
        ))
        .expect("publish");

    assert!(validate_index(&layout, MY_DB, MY_SCHEMA, TableId::new(1), IndexId::new(1)).is_err());
    cleanup(&root);
}

#[test]
fn an_index_survives_a_restart() {
    let root = scratch("restart");
    let table_id = TableId::new(12);
    let index_id = IndexId::new(1);
    {
        let layout = DatabaseLayout::new(&root);
        layout.initialize().expect("initialize");
        layout
            .ensure_index_in_schema(MY_DB, MY_SCHEMA, table_id, index_id)
            .expect("ensure index");
    }

    let reopened = DatabaseLayout::new(&root);
    validate_index(&reopened, MY_DB, MY_SCHEMA, table_id, index_id)
        .expect("validate after restart");
    assert_eq!(
        reopened
            .discover_index_ids_in_schema(MY_DB, MY_SCHEMA, table_id)
            .expect("discover"),
        vec![index_id]
    );

    cleanup(&root);
}

#[test]
fn a_missing_index_directory_is_reported() {
    let root = scratch("missing");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    layout
        .ensure_table_in_schema(MY_DB, MY_SCHEMA, TableId::new(1))
        .expect("table");
    let error = validate_index(&layout, MY_DB, MY_SCHEMA, TableId::new(1), IndexId::new(1))
        .expect_err("missing");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    cleanup(&root);
}
