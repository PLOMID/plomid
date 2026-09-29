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
//! Validation tests: structural validation of the root and of a dynamic set of
//! registered devices, plus metadata validation for materialized objects.
use plomid_core::{
    DatabaseId, DeviceId, ErrorKind, GenerationId, IndexId, ObjectId, SchemaId, TableId,
};
use plomid_storage::layout::{
    required_dirs, validate_devices, validate_generation, validate_index, validate_root,
    validate_table, DatabaseLayout,
};
use plomid_storage::{DeviceRecord, TableMetadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const TEST_DB: DatabaseId = DatabaseId::new(1);
const TEST_SCHEMA: SchemaId = SchemaId::new(1);

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-validation-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

fn register_device(layout: &DatabaseLayout, id: u64) {
    let device_id = DeviceId::new(id);
    layout.ensure_device_dirs(device_id).expect("device dirs");
    DeviceRecord::new(device_id, 1u64 << 40)
        .expect("record")
        .publish(&layout.device_meta_path(device_id))
        .expect("publish");
}

#[test]
fn a_fresh_database_validates() {
    let root = scratch("fresh");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    validate_root(&layout).expect("root");
    assert!(validate_devices(&layout).expect("devices").is_empty());
    cleanup(&root);
}

#[test]
fn root_validation_reports_the_first_missing_directory() {
    let root = scratch("missing");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    // Remove a late directory: the reported path is the concrete missing one.
    std::fs::remove_dir(layout.lost_found_dir()).expect("remove");
    let error = validate_root(&layout).expect_err("missing directory");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    assert_eq!(
        required_dirs(&layout).last(),
        Some(&layout.lost_found_dir())
    );

    cleanup(&root);
}

#[test]
fn device_validation_covers_every_registered_device() {
    let root = scratch("devices");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    // Validation never assumes a single device: register several and remove
    // the structural directory of a non-first device.
    for id in 1..=3_u64 {
        register_device(&layout, id);
    }
    assert_eq!(
        validate_devices(&layout).expect("validate"),
        vec![DeviceId::new(1), DeviceId::new(2), DeviceId::new(3)]
    );

    std::fs::remove_dir(layout.device_free_dir(DeviceId::new(3))).expect("remove free");
    let error = validate_devices(&layout).expect_err("device 3 invalid");
    assert_eq!(error.kind(), ErrorKind::Corruption);

    cleanup(&root);
}

#[test]
fn table_generation_and_index_metadata_are_validated() {
    let root = scratch("objects");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(1);

    layout
        .ensure_table_in_schema(TEST_DB, TEST_SCHEMA, table_id)
        .expect("table");
    layout
        .ensure_index_in_schema(TEST_DB, TEST_SCHEMA, table_id, IndexId::new(1))
        .expect("index");
    layout
        .ensure_generation_dir(TEST_DB, TEST_SCHEMA, table_id, GenerationId::new(1))
        .expect("generation");

    validate_table(&layout, TEST_DB, TEST_SCHEMA, table_id).expect("table");
    validate_index(&layout, TEST_DB, TEST_SCHEMA, table_id, IndexId::new(1)).expect("index");

    // A generation without its metadata record is not yet a valid generation.
    assert!(validate_generation(
        &layout,
        TEST_DB,
        TEST_SCHEMA,
        table_id,
        GenerationId::new(1)
    )
    .is_err());

    cleanup(&root);
}

#[test]
fn corrupt_table_metadata_is_detected() {
    let root = scratch("corrupt-table");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(1);
    layout
        .ensure_table_in_schema(TEST_DB, TEST_SCHEMA, table_id)
        .expect("table");

    // Overwrite the published record with a corrupt image.
    let mut bytes = TableMetadata::new(table_id, ObjectId::new(1))
        .expect("metadata")
        .encode()
        .expect("encode");
    bytes[20] ^= 0xFF;
    std::fs::write(
        layout.table_meta_path_in_schema(TEST_DB, TEST_SCHEMA, table_id),
        &bytes,
    )
    .expect("write");

    let error =
        validate_table(&layout, TEST_DB, TEST_SCHEMA, table_id).expect_err("corrupt metadata");
    assert_eq!(error.kind(), ErrorKind::Corruption);
    assert_eq!(
        layout
            .read_table_meta_in_schema(TEST_DB, TEST_SCHEMA, table_id)
            .expect_err("read")
            .kind(),
        ErrorKind::Corruption
    );

    cleanup(&root);
}

#[test]
fn corrupt_device_metadata_is_detected() {
    let root = scratch("corrupt-device");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    register_device(&layout, 1);

    let mut bytes = DeviceRecord::new(DeviceId::new(1), 1u64 << 40)
        .expect("record")
        .encode()
        .expect("encode");
    bytes[0] ^= 0xFF;
    std::fs::write(layout.device_meta_path(DeviceId::new(1)), &bytes).expect("write");

    let error = plomid_storage::DeviceRegistry::discover(&layout).expect_err("corrupt device");
    assert_eq!(error.kind(), ErrorKind::Corruption);

    cleanup(&root);
}

#[test]
fn validation_does_not_read_physical_content() {
    let root = scratch("cheap");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    register_device(&layout, 1);

    // A large, unrelated file in the device pack directory does not affect
    // structural validation: it never scans packs, blocks, or pages.
    let payload = layout
        .device_packs_dir(DeviceId::new(1))
        .join("unrelated.bin");
    std::fs::write(&payload, vec![0xAB_u8; 1024 * 1024]).expect("write payload");

    validate_root(&layout).expect("root");
    validate_devices(&layout).expect("devices");

    cleanup(&root);
}
