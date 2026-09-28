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
//! Device tests: registering devices, durable identity/capacity records,
//! lifecycle state, directory validation, and restart discovery.
use plomid_core::{DeviceId, ErrorKind};
use plomid_storage::layout::{validate_devices, DatabaseLayout};
use plomid_storage::{
    read_device_record, read_device_state, DeviceRecord, DeviceRegistry, DeviceStateRecord,
    Lifecycle,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-devices-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// Registers a device the way database creation does: directories, then the
/// durable identity record.
fn register(layout: &DatabaseLayout, id: u64, capacity: u64) -> DeviceRecord {
    let device_id = DeviceId::new(id);
    layout.ensure_device_dirs(device_id).expect("device dirs");
    let record = DeviceRecord::new(device_id, capacity).expect("device record");
    record
        .publish(&layout.device_meta_path(device_id))
        .expect("publish device record");
    record
}

#[test]
fn registering_devices_creates_the_required_directories() {
    let root = scratch("register");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    for id in 1..=3_u64 {
        register(&layout, id, 1u64 << 40);
    }

    for id in 1..=3_u64 {
        let device_id = DeviceId::new(id);
        assert!(layout.device_dir(device_id).is_dir());
        assert!(layout.device_packs_dir(device_id).is_dir());
        assert!(layout.device_free_dir(device_id).is_dir());
        assert!(layout.device_state_dir(device_id).is_dir());
        assert!(layout.device_meta_path(device_id).is_file());
    }

    // Identity order is deterministic and no device is special.
    assert_eq!(
        validate_devices(&layout).expect("validate devices"),
        vec![DeviceId::new(1), DeviceId::new(2), DeviceId::new(3)]
    );

    cleanup(&root);
}

#[test]
fn device_metadata_records_identity_and_capacity() {
    let root = scratch("metadata");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    let published = register(&layout, 2, 1u64 << 41);
    let loaded = read_device_record(&layout.device_meta_path(DeviceId::new(2))).expect("read");
    assert_eq!(loaded, published);
    assert_eq!(loaded.device_id, DeviceId::new(2));
    assert_eq!(loaded.capacity, 1u64 << 41);

    cleanup(&root);
}

#[test]
fn device_state_is_durable_and_governs_usability() {
    let root = scratch("state");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let device_id = DeviceId::new(1);
    register(&layout, 1, 1u64 << 40);

    // With no state record the physical header stays authoritative and the
    // device counts as usable.
    let registry = DeviceRegistry::discover(&layout).expect("discover");
    assert!(registry.get(device_id).expect("entry").is_usable());

    // A recorded offline state makes the device ineligible for placement.
    let offline = DeviceStateRecord::new(Lifecycle::Offline);
    offline
        .publish(&layout.device_state_path(device_id))
        .expect("publish state");
    assert_eq!(
        read_device_state(&layout.device_state_path(device_id)).expect("read state"),
        offline
    );
    let registry = DeviceRegistry::discover(&layout).expect("discover");
    assert!(!registry.get(device_id).expect("entry").is_usable());

    // Returning online restores eligibility without touching the identity.
    DeviceStateRecord::new(Lifecycle::Online)
        .publish(&layout.device_state_path(device_id))
        .expect("publish state");
    let registry = DeviceRegistry::discover(&layout).expect("discover");
    assert!(registry.get(device_id).expect("entry").is_usable());

    cleanup(&root);
}

#[test]
fn logical_metadata_and_physical_container_are_distinct_paths() {
    let root = scratch("distinct");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let device_id = DeviceId::new(1);
    register(&layout, 1, 1u64 << 40);

    let meta = layout.device_meta_path(device_id);
    let physical = layout.device_physical_path(device_id);
    let state = layout.device_state_path(device_id);

    // Three different files with three different responsibilities.
    assert_ne!(meta, physical);
    assert_ne!(meta, state);
    assert_ne!(physical, state);
    // Physical storage lives under the device's pack directory, never under a
    // logical object directory.
    assert!(physical.starts_with(layout.device_packs_dir(device_id)));
    assert!(meta.starts_with(layout.device_dir(device_id)));
    assert!(state.starts_with(layout.device_state_dir(device_id)));
    assert!(!physical.starts_with(layout.objects_dir()));

    cleanup(&root);
}
#[test]
fn a_device_directory_without_its_pack_directory_is_invalid() {
    let root = scratch("invalid-dir");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let device_id = DeviceId::new(1);
    register(&layout, 1, 1u64 << 40);

    std::fs::remove_dir(layout.device_packs_dir(device_id)).expect("remove packs");

    let error = validate_devices(&layout).expect_err("invalid device");
    assert_eq!(error.kind(), ErrorKind::Corruption);

    cleanup(&root);
}

#[test]
fn a_device_directory_without_its_record_is_reported() {
    let root = scratch("no-record");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let device_id = DeviceId::new(1);
    layout.ensure_device_dirs(device_id).expect("dirs");

    let error = DeviceRegistry::discover(&layout).expect_err("missing record");
    assert_eq!(error.kind(), ErrorKind::Corruption);

    cleanup(&root);
}

#[test]
fn device_identity_must_match_its_directory() {
    let root = scratch("identity");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let device_id = DeviceId::new(1);
    layout.ensure_device_dirs(device_id).expect("dirs");

    // A record for a different device must never be adopted for this one.
    let wrong = DeviceRecord::new(DeviceId::new(2), 1u64 << 40).expect("record");
    wrong
        .publish(&layout.device_meta_path(device_id))
        .expect("publish");

    let error = DeviceRegistry::discover(&layout).expect_err("identity mismatch");
    assert_eq!(error.kind(), ErrorKind::Corruption);

    cleanup(&root);
}

#[test]
fn devices_survive_a_restart() {
    let root = scratch("restart");
    {
        let layout = DatabaseLayout::new(&root);
        layout.initialize().expect("initialize");
        register(&layout, 1, 1u64 << 40);
        register(&layout, 2, 1u64 << 41);
        DeviceStateRecord::new(Lifecycle::Online)
            .publish(&layout.device_state_path(DeviceId::new(2)))
            .expect("publish state");
    }

    let reopened = DatabaseLayout::new(&root);
    reopened.initialize().expect("reopen initialize");
    let registry = DeviceRegistry::discover(&reopened).expect("discover");

    assert_eq!(registry.len(), 2);
    assert!(registry.contains(DeviceId::new(1)));
    assert!(registry.contains(DeviceId::new(2)));
    assert_eq!(registry.total_capacity(), (1u64 << 40) + (1u64 << 41));
    assert_eq!(
        registry.usable_capacity(),
        (1u64 << 40) + (1u64 << 41),
        "no state record means usable; a recorded online state is usable too"
    );
    assert_eq!(
        registry
            .get(DeviceId::new(2))
            .expect("entry")
            .record
            .capacity,
        1u64 << 41
    );

    cleanup(&root);
}

#[test]
fn an_empty_device_set_is_not_an_error() {
    let root = scratch("empty");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    assert!(validate_devices(&layout).expect("validate").is_empty());
    assert!(DeviceRegistry::discover(&layout)
        .expect("discover")
        .is_empty());
    cleanup(&root);
}
