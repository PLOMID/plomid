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
//! Multi-device tests: one logical table spanning several physical devices,
//! device capacity adding up to database capacity, and the separation between
//! logical object paths and physical device bytes.
use plomid_core::{DeviceId, ErrorKind, GenerationId, IndexId, TableId};
use plomid_storage::{
    capacity_for_extents, validate_devices, DatabaseLayout, DeviceAllocator, DeviceRecord,
    DeviceRegistry, StorageDevice, EXTENT_SIZE,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-multi-device-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// Capacity of one allocatable extent: a small simulated device.
fn small_capacity() -> u64 {
    capacity_for_extents(1).expect("capacity")
}

/// Registers a device and creates its physical container.
fn create_device(layout: &DatabaseLayout, id: u64, capacity: u64) -> StorageDevice {
    let device_id = DeviceId::new(id);
    layout.ensure_device_dirs(device_id).expect("device dirs");
    DeviceRecord::new(device_id, capacity)
        .expect("record")
        .publish(&layout.device_meta_path(device_id))
        .expect("publish record");
    StorageDevice::create(&layout.device_physical_path(device_id), device_id, capacity)
        .expect("create device")
}

/// A database with two devices stores, restarts, and recovers across both.
///
/// This is the multi-device restart contract: the devices stay registered, the
/// segments stay placed on the devices that own them, the logical data is still
/// readable, and the logical object tree still holds no physical bytes.
#[test]
fn a_database_spans_two_devices_and_recovers_after_restart() {
    use plomid_storage::{register_device, DeviceRegistry, StorageManager};

    let root = scratch("manager-restart");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    // Two devices of four extents each: the manager reserves one extent per
    // segment, so a small database spreads over both devices instead of one.
    let capacity = capacity_for_extents(4).expect("capacity");
    register_device(&layout, DeviceId::new(1), capacity).expect("register D1");
    register_device(&layout, DeviceId::new(2), capacity).expect("register D2");

    // Two devices of two extents each. Segments are placed capacity-first, so
    // one small database spreads over both devices instead of one.
    let key = |index: u64| format!("key-{index:04}").into_bytes();
    let value = |index: u64| format!("value-{index:04}").into_bytes();
    {
        let mut storage = StorageManager::create(&root, 32, 1).expect("create storage");
        for index in 0..4_u64 {
            storage.insert(&key(index), &value(index)).expect("insert");
        }
        storage.sync().expect("sync");
    }

    // Both devices received segments of the same logical database.
    {
        let reopened = StorageManager::open(&root, 32, 1).expect("reopen");
        let placed: std::collections::BTreeSet<u64> = reopened
            .segments()
            .map(|segment| segment.device_id.get())
            .collect();
        assert_eq!(
            placed.into_iter().collect::<Vec<u64>>(),
            vec![1, 2],
            "one logical database, two physical devices"
        );
    }

    // After a restart every device is still registered and every key is still
    // readable from the segment that owns it.
    let mut reopened = StorageManager::open(&root, 32, 1).expect("reopen");
    let registry = DeviceRegistry::discover(&layout).expect("discover");
    assert_eq!(registry.len(), 2);
    assert!(registry.contains(DeviceId::new(1)));
    assert!(registry.contains(DeviceId::new(2)));
    for index in 0..4_u64 {
        assert_eq!(
            reopened.get(&key(index)).expect("read").as_deref(),
            Some(value(index).as_slice())
        );
    }

    // The logical object tree holds metadata only: physical bytes are owned by
    // the devices.
    for entry in walk(&layout.objects_dir()) {
        assert!(
            !entry.to_string_lossy().contains("SEG-"),
            "no physical segment may live under objects/: {}",
            entry.display()
        );
    }

    cleanup(&root);
}

#[test]
fn a_logical_table_spans_multiple_devices() {
    let root = scratch("span");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    let capacity = small_capacity();
    let d1 = create_device(&layout, 1, capacity);
    let d2 = create_device(&layout, 2, capacity);

    // One logical table with one generation and one index.
    let table_id = TableId::new(1);
    layout.ensure_table(table_id).expect("table");
    layout
        .ensure_generation_dir_flat(table_id, GenerationId::new(1))
        .expect("generation");
    layout
        .ensure_index(table_id, IndexId::new(1))
        .expect("index");

    // Two physical extents of that single table land on two different devices.
    let extent_a = d1.allocate(EXTENT_SIZE).expect("allocate on D1");
    let extent_b = d2.allocate(EXTENT_SIZE).expect("allocate on D2");
    assert_eq!(d1.allocated_extent_count(), 1);
    assert_eq!(d2.allocated_extent_count(), 1);
    // Extent identity is device-local: both devices number their first extent
    // `1`, and the same byte offset is meaningful on both. Logical placement is
    // therefore always the pair (device identity, extent offset) — never the
    // offset alone, which is why nothing may assume a single device.
    assert_eq!(extent_a.extent_id, extent_b.extent_id);
    assert_eq!(extent_a.start, extent_b.start);
    assert_eq!(extent_a.length, EXTENT_SIZE);

    // The table is still exactly one logical table.
    assert_eq!(layout.discover_table_ids().expect("tables"), vec![table_id]);
    assert_eq!(
        validate_devices(&layout).expect("devices"),
        vec![DeviceId::new(1), DeviceId::new(2)]
    );
    // The logical tree holds no physical extent storage.
    assert!(!layout.table_dir(table_id).join("packs").exists());
    assert!(!layout.table_dir(table_id).join("data").exists());
    // Both devices are independently registered and allocated.
    assert!(layout.device_physical_path(DeviceId::new(1)).is_file());
    assert!(layout.device_physical_path(DeviceId::new(2)).is_file());

    cleanup(&root);
}

#[test]
fn device_capacity_adds_up_across_devices() {
    let root = scratch("aggregate");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    // Simulated devices of different sizes: a database is the sum of physical
    // capacity, never the capacity of one chosen device.
    let d1 = capacity_for_extents(1).expect("capacity");
    let d2 = capacity_for_extents(4).expect("capacity");
    let d3 = capacity_for_extents(8).expect("capacity");
    create_device(&layout, 1, d1);
    create_device(&layout, 2, d2);
    create_device(&layout, 3, d3);

    let registry = DeviceRegistry::discover(&layout).expect("discover");
    assert_eq!(registry.len(), 3);
    assert_eq!(registry.total_capacity(), d1 + d2 + d3);
    assert_eq!(
        registry.get(DeviceId::new(2)).expect("d2").capacity(),
        d2,
        "each device reports its own capacity"
    );
    // Usable capacity is the whole device set, not `DeviceId::new(1)`.
    assert_eq!(registry.usable_capacity(), d1 + d2 + d3);

    cleanup(&root);
}

#[test]
fn allocation_continues_on_another_device_when_one_is_full() {
    let root = scratch("capacity");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let capacity = small_capacity();
    create_device(&layout, 1, capacity);
    create_device(&layout, 2, capacity);

    // Fill the first device completely. Devices are writable only while online,
    // which `StorageDevice::create` establishes.
    let d1 = StorageDevice::open(&layout.device_physical_path(DeviceId::new(1))).expect("open d1");
    d1.allocate(EXTENT_SIZE).expect("fill d1");
    assert_eq!(d1.free_bytes(), 0, "D1 is full");

    // The allocator observes all devices and moves the next allocation to the
    // device that still has room. Nothing is bound to device 1.
    let registry = DeviceRegistry::discover(&layout).expect("discover");
    let allocator = DeviceAllocator::new();
    let target = allocator
        .select_device(&registry, EXTENT_SIZE)
        .expect("select");
    assert_eq!(
        target.device_id,
        DeviceId::new(2),
        "D1 is full, so D2 is used"
    );
    target.device.allocate(EXTENT_SIZE).expect("allocate on d2");
    assert_eq!(target.device.free_bytes(), 0);

    // With every device exhausted, the failure is a controlled capacity error.
    let error = allocator
        .select_device(&registry, EXTENT_SIZE)
        .expect_err("exhausted");
    assert_eq!(error.kind(), ErrorKind::Conflict);

    cleanup(&root);
}

#[test]
fn a_device_that_cannot_hold_the_request_is_skipped() {
    let root = scratch("too-small");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    // One-extent device, but a two-extent request.
    create_device(&layout, 1, small_capacity());
    create_device(&layout, 2, capacity_for_extents(4).expect("capacity"));

    let registry = DeviceRegistry::discover(&layout).expect("discover");
    let target = DeviceAllocator::new()
        .select_device(&registry, 2 * EXTENT_SIZE)
        .expect("select");
    // The small device cannot hold two extents, so the larger one is chosen.
    assert_eq!(target.device_id, DeviceId::new(2));

    cleanup(&root);
}

#[test]
fn an_offline_device_is_not_selected() {
    let root = scratch("offline");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let capacity = small_capacity();
    create_device(&layout, 1, capacity);
    create_device(&layout, 2, capacity);

    // Mark D1 offline: it must not receive new storage even though it has room.
    plomid_storage::DeviceStateRecord::new(plomid_storage::Lifecycle::Offline)
        .publish(&layout.device_state_path(DeviceId::new(1)))
        .expect("publish state");

    let registry = DeviceRegistry::discover(&layout).expect("discover");
    let target = DeviceAllocator::new()
        .select_device(&registry, EXTENT_SIZE)
        .expect("select");
    assert_eq!(target.device_id, DeviceId::new(2));

    cleanup(&root);
}

#[test]
fn physical_bytes_belong_to_the_device_not_the_logical_table() {
    let root = scratch("ownership");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let capacity = small_capacity();
    create_device(&layout, 1, capacity);

    let table_id = TableId::new(1);
    layout.ensure_table(table_id).expect("table");
    layout
        .ensure_generation_dir_flat(table_id, GenerationId::new(1))
        .expect("generation");

    // Allocate an extent and write a recognizable payload into the physical
    // data area, exactly where the storage layer owns bytes.
    let device =
        StorageDevice::open(&layout.device_physical_path(DeviceId::new(1))).expect("open device");
    let extent = device.allocate(EXTENT_SIZE).expect("allocate");
    let marker = b"PLOMID_PHYSICAL_PAYLOAD_MARKER";

    // Reserving the extent is logical: the device container still holds only
    // its metadata footprint. The data area grows when bytes are placed in it.
    assert!(device.physical_len() < device.capacity());
    let mutation = (|| -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        device
            .ensure_data_capacity(extent.start + marker.len() as u64)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(layout.device_physical_path(DeviceId::new(1)))?;
        file.seek(SeekFrom::Start(device.data_start() + extent.start))?;
        file.write_all(marker)?;
        file.sync_all()?;
        Ok(())
    })();
    mutation.expect("write payload");
    assert_eq!(
        device.physical_len(),
        device.data_start() + extent.start + marker.len() as u64
    );
    // Reopening a demand-grown container keeps the logical capacity intact.
    let reopened =
        StorageDevice::open(&layout.device_physical_path(DeviceId::new(1))).expect("reopen");
    assert_eq!(reopened.capacity(), device.capacity());
    assert_eq!(reopened.allocated_extent_count(), 1);
    drop(reopened);

    // The payload is physically owned by the device's pack container.
    let physical = std::fs::read(layout.device_physical_path(DeviceId::new(1)))
        .expect("read physical container");
    assert!(
        contains(&physical, marker),
        "the device container owns the physical bytes"
    );

    // The logical table directory holds no copy of that physical data: it
    // describes structure and metadata only.
    for entry in walk(&layout.table_dir(table_id)) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        assert!(
            !contains(&bytes, marker),
            "logical object path must not contain physical data: {}",
            entry.display()
        );
        assert!(
            !entry.to_string_lossy().ends_with(".db"),
            "no `.db` file may appear under the logical tree: {}",
            entry.display()
        );
    }
    // Nor does the physical container appear anywhere under `objects/`.
    assert!(!layout.objects_dir().join("devices").exists());

    cleanup(&root);
}

/// Returns true when `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Collects every file path below `dir`, deterministically ordered.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}
