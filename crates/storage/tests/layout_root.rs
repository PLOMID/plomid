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
//! Root-layout tests: database initialization, the required directory tree,
//! idempotency, restart, and rejection of a structurally invalid root.
use plomid_core::ErrorKind;
use plomid_storage::layout::{required_dirs, validate_root, DatabaseLayout};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Returns an isolated scratch directory for one test.
fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-layout-root-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn fresh_initialization_creates_the_required_tree() {
    let root = scratch("fresh");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    // The root directory names are the authoritative top-level structure.
    assert!(layout.catalog_dir().is_dir());
    assert!(layout.wal_dir().is_dir());
    assert!(layout.checkpoints_dir().is_dir());
    assert!(layout.objects_dir().is_dir());
    assert!(layout.devices_dir().is_dir());
    assert!(layout.shared_dir().is_dir());
    assert!(layout.temp_dir().is_dir());
    assert!(layout.lost_found_dir().is_dir());
    // The shared structure is part of the required tree.
    assert!(layout.shared_tx_dir().is_dir());
    assert!(layout.shared_rowid_dir().is_dir());
    assert!(layout.shared_statistics_dir().is_dir());
    assert!(layout.shared_filters_dir().is_dir());
    assert!(layout.shared_dictionaries_dir().is_dir());
    // Fresh initialization creates the logical object containers; no database,
    // schema, or table object exists yet, so only `objects/` subtree roots are
    // expected.
    assert!(layout.tables_dir().is_dir());
    assert!(layout.databases_dir().is_dir());
    // `CURRENT` may be absent until the first publication; it is never a
    // directory and never created empty by initialization.
    assert!(!layout.current_exists());

    cleanup(&root);
}

#[test]
fn initialization_is_idempotent_and_restart_safe() {
    let root = scratch("idempotent");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("first initialize");

    // Publish a table record, then re-initialize: durable state must survive.
    let table_id = plomid_core::TableId::new(1);
    layout.ensure_table(table_id).expect("table");
    let meta = plomid_storage::layout::TableMetadata::new(table_id, plomid_core::ObjectId::new(1))
        .expect("metadata");
    meta.publish(&layout.table_meta_path(table_id))
        .expect("publish");

    layout.initialize().expect("second initialize");

    // Re-initialization neither destroyed nor replaced the table record.
    assert_eq!(
        layout.read_table_meta(table_id).expect("read"),
        meta,
        "re-initialization must not overwrite durable metadata"
    );

    // A restart observes the same structure.
    let reopened = DatabaseLayout::new(&root);
    reopened.initialize().expect("restart initialize");
    validate_root(&reopened).expect("restart validation");
    assert_eq!(reopened.read_table_meta(table_id).expect("read"), meta);

    cleanup(&root);
}

#[test]
fn validation_lists_the_required_directories_deterministically() {
    let root = scratch("required");
    let layout = DatabaseLayout::new(&root);
    assert_eq!(
        required_dirs(&layout),
        vec![
            layout.catalog_dir(),
            layout.wal_dir(),
            layout.checkpoints_dir(),
            layout.objects_dir(),
            layout.tables_dir(),
            layout.databases_dir(),
            layout.devices_dir(),
            layout.shared_dir(),
            layout.shared_tx_dir(),
            layout.shared_rowid_dir(),
            layout.shared_statistics_dir(),
            layout.shared_filters_dir(),
            layout.shared_dictionaries_dir(),
            layout.temp_dir(),
            layout.lost_found_dir(),
        ]
    );
    cleanup(&root);
}

#[test]
fn a_missing_required_directory_is_reported() {
    let root = scratch("missing-dir");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    std::fs::remove_dir(layout.wal_dir()).expect("remove wal");

    let error = layout.validate().expect_err("missing directory");
    assert_eq!(error.kind(), ErrorKind::NotFound);

    cleanup(&root);
}

#[test]
fn a_missing_root_is_reported() {
    let root = scratch("missing-root").join("does-not-exist");
    let layout = DatabaseLayout::new(&root);
    let error = layout.validate().expect_err("missing root");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    cleanup(root.parent().expect("parent"));
}

#[test]
fn a_root_that_is_a_file_is_rejected() {
    let root = scratch("root-is-file");
    let file = root.join("database-root");
    std::fs::write(&file, b"not a directory").expect("write");

    let layout = DatabaseLayout::new(&file);
    let error = layout.validate().expect_err("file root");
    assert_eq!(error.kind(), ErrorKind::Corruption);
    // Initialization must refuse it too rather than replacing it.
    assert!(layout.initialize().is_err());
    assert!(file.is_file(), "the file must be left untouched");

    cleanup(&root);
}

#[test]
fn root_paths_are_pure_join_operations() {
    let layout = DatabaseLayout::new("/tmp/example-root");
    assert_eq!(layout.objects_dir(), Path::new("/tmp/example-root/objects"));
    assert_eq!(
        layout.tables_dir(),
        Path::new("/tmp/example-root/objects/tables")
    );
    assert_eq!(layout.devices_dir(), Path::new("/tmp/example-root/devices"));
    assert_eq!(
        layout.current_path(),
        Path::new("/tmp/example-root/CURRENT")
    );
}
