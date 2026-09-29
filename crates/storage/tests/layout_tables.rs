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
//! Table-layout tests: deterministic logical table paths, table `META.dat`,
//! and restart of a materialized table inside the database/schema hierarchy.
use plomid_core::{DatabaseId, ErrorKind, ObjectId, SchemaId, TableId};
use plomid_storage::layout::{
    table_dir_name, table_from_dir_name, validate_table, DatabaseLayout, TableMetadata,
    META_FILE_NAME,
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
        "plomid-layout-tables-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn table_paths_are_deterministic_and_table_local() {
    let root = scratch("paths");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(1);

    assert_eq!(table_dir_name(table_id), "T-00000000000000000001");
    assert_eq!(
        table_from_dir_name("T-00000000000000000001"),
        Some(table_id)
    );
    assert_eq!(table_from_dir_name("T-1"), None);

    let tables_dir = layout.schema_tables_dir(MY_DB, MY_SCHEMA);
    assert_eq!(
        layout.table_dir_in_schema(MY_DB, MY_SCHEMA, table_id),
        tables_dir.join("T-00000000000000000001")
    );
    assert_eq!(
        layout.table_meta_path_in_schema(MY_DB, MY_SCHEMA, table_id),
        layout
            .table_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
            .join(META_FILE_NAME)
    );
    assert_eq!(
        layout.table_hot_dir_in_schema(MY_DB, MY_SCHEMA, table_id),
        layout
            .table_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
            .join("hot")
    );
    assert_eq!(
        layout.table_generations_dir_in_schema(MY_DB, MY_SCHEMA, table_id),
        layout
            .table_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
            .join("generations")
    );
    assert_eq!(
        layout.table_indexes_dir_in_schema(MY_DB, MY_SCHEMA, table_id),
        layout
            .table_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
            .join("indexes")
    );

    cleanup(&root);
}

#[test]
fn creating_a_table_materializes_its_directory_structure() {
    let root = scratch("create");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(7);

    layout
        .ensure_table_in_schema(MY_DB, MY_SCHEMA, table_id)
        .expect("ensure table");

    assert!(layout
        .table_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
        .is_dir());
    assert!(layout
        .table_hot_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
        .is_dir());
    assert!(layout
        .table_generations_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
        .is_dir());
    assert!(layout
        .table_indexes_dir_in_schema(MY_DB, MY_SCHEMA, table_id)
        .is_dir());
    // The table publishes its own metadata as part of creation, so the
    // directory is never left materialized without an identity record.
    assert_eq!(
        layout
            .read_table_meta_in_schema(MY_DB, MY_SCHEMA, table_id)
            .expect("read"),
        TableMetadata::new(table_id, ObjectId::new(7)).expect("metadata")
    );

    cleanup(&root);
}

#[test]
fn table_metadata_round_trips_and_is_validated() {
    let root = scratch("meta");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(3);
    layout
        .ensure_table_in_schema(MY_DB, MY_SCHEMA, table_id)
        .expect("ensure table");

    let meta = TableMetadata::new(table_id, ObjectId::new(3)).expect("metadata");
    meta.publish(&layout.table_meta_path_in_schema(MY_DB, MY_SCHEMA, table_id))
        .expect("publish");

    validate_table(&layout, MY_DB, MY_SCHEMA, table_id).expect("validate");
    assert_eq!(
        layout
            .read_table_meta_in_schema(MY_DB, MY_SCHEMA, table_id)
            .expect("read"),
        meta
    );

    // The staging artifact of a completed publication never survives.
    assert!(!layout
        .table_meta_path_in_schema(MY_DB, MY_SCHEMA, table_id)
        .with_extension("dat.tmp")
        .exists());

    cleanup(&root);
}

#[test]
fn table_metadata_requires_matching_object_identity() {
    let table_id = TableId::new(5);
    let error = TableMetadata::new(table_id, ObjectId::new(6)).expect_err("mismatch");
    assert_eq!(error.kind(), ErrorKind::Corruption);
}

#[test]
fn table_discovery_is_ordered_and_ignores_unrelated_names() {
    let root = scratch("discover");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    for id in [3_u64, 1, 2] {
        layout
            .ensure_table_in_schema(MY_DB, MY_SCHEMA, TableId::new(id))
            .expect("ensure table");
    }
    // Unrelated entries are never mistaken for tables.
    let tables_dir = layout.schema_tables_dir(MY_DB, MY_SCHEMA);
    std::fs::create_dir_all(tables_dir.join("not-a-table")).expect("dir");
    std::fs::create_dir_all(tables_dir.join("T-1")).expect("dir");
    std::fs::write(tables_dir.join("T-00000000000000000009"), b"file").expect("file");

    assert_eq!(
        layout
            .discover_table_ids_in_schema(MY_DB, MY_SCHEMA)
            .expect("discover"),
        vec![TableId::new(1), TableId::new(2), TableId::new(3)]
    );

    cleanup(&root);
}

#[test]
fn a_table_survives_a_restart() {
    let root = scratch("restart");
    let table_id = TableId::new(42);
    let meta;
    {
        let layout = DatabaseLayout::new(&root);
        layout.initialize().expect("initialize");
        layout
            .ensure_table_in_schema(MY_DB, MY_SCHEMA, table_id)
            .expect("ensure table");
        meta = TableMetadata::new(table_id, ObjectId::new(42)).expect("metadata");
        meta.publish(&layout.table_meta_path_in_schema(MY_DB, MY_SCHEMA, table_id))
            .expect("publish");
    }

    // A fresh process-level view observes the same table at the same path.
    let reopened = DatabaseLayout::new(&root);
    reopened.initialize().expect("reopen initialize");
    validate_table(&reopened, MY_DB, MY_SCHEMA, table_id).expect("validate");
    assert_eq!(
        reopened
            .read_table_meta_in_schema(MY_DB, MY_SCHEMA, table_id)
            .expect("read"),
        meta
    );
    assert_eq!(
        reopened
            .discover_table_ids_in_schema(MY_DB, MY_SCHEMA)
            .expect("discover"),
        vec![table_id]
    );

    cleanup(&root);
}

#[test]
fn a_missing_table_directory_is_reported() {
    let root = scratch("missing");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let error =
        validate_table(&layout, MY_DB, MY_SCHEMA, TableId::new(1)).expect_err("missing table");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    cleanup(&root);
}
