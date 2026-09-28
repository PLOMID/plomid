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
//! Restart tests: creating a database is idempotent, reopening never rebuilds
//! over durable state, and a missing piece of the layout produces a controlled
//! error instead of a silent rebuild.
use plomid_core::{DeviceId, ErrorKind};
use plomid_storage::{
    capacity_for_extents, has_durable_state, register_device, DatabaseLayout, StorageManager,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-restart-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn a_fresh_root_has_no_durable_state() {
    let root = scratch("fresh");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    // An initialized layout with nothing registered has never stored anything,
    // which is the only state a first-boot path may create a database in.
    assert!(!has_durable_state(&root).expect("probe"));

    cleanup(&root);
}

#[test]
fn a_registered_device_makes_the_root_a_durable_database() {
    let root = scratch("registered");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    register_device(
        &layout,
        DeviceId::new(1),
        capacity_for_extents(2).expect("capacity"),
    )
    .expect("register");

    assert!(has_durable_state(&root).expect("probe"));

    cleanup(&root);
}

#[test]
fn creating_a_database_again_reopens_the_one_already_there() {
    let root = scratch("idempotent");
    let outcome = (|| {
        let mut manager = StorageManager::create(&root, 32, 4096)?;
        manager.insert(b"alpha", b"one")?;
        let segment_ids: Vec<u64> = manager.segments().map(|segment| segment.id).collect();
        drop(manager);

        // Creation against an existing database must reopen it: opening a new
        // segment 1 would truncate the pages a durable segment already holds.
        let mut recreated = StorageManager::create(&root, 32, 4096)?;
        assert_eq!(recreated.get(b"alpha")?, Some(b"one".to_vec()));
        assert_eq!(
            recreated
                .segments()
                .map(|segment| segment.id)
                .collect::<Vec<u64>>(),
            segment_ids
        );
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&root);
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[test]
fn every_segment_is_rediscovered_from_its_device_after_a_restart() {
    let root = scratch("rediscover");
    let outcome = (|| {
        // A one-byte segment limit rotates on every insert, so the database
        // holds several segments placed by the allocator.
        let mut manager = StorageManager::create(&root, 32, 1)?;
        for index in 0..4 {
            manager.insert(format!("key-{index}").as_bytes(), b"value")?;
        }
        let layout = DatabaseLayout::new(&root);
        let placement: Vec<(u64, DeviceId, PathBuf)> = manager
            .segments()
            .map(|segment| (segment.id, segment.device_id, segment.path.clone()))
            .collect();
        assert!(placement.len() >= 2, "the database rotated segments");
        for (_, device_id, path) in &placement {
            assert!(
                path.starts_with(layout.device_packs_dir(*device_id)),
                "segment {} lives on its device's packs directory",
                path.display()
            );
            assert!(!path.starts_with(layout.objects_dir()));
        }
        drop(manager);

        let mut reopened = StorageManager::open(&root, 32, 1)?;
        for index in 0..4 {
            assert_eq!(
                reopened.get(format!("key-{index}").as_bytes())?,
                Some(b"value".to_vec())
            );
        }
        assert_eq!(reopened.segment_count(), placement.len());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&root);
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[test]
fn a_registered_device_without_its_container_is_a_controlled_error() {
    let root = scratch("missing-container");
    let outcome = (|| {
        // Segment size 1 forces a rotation on the next insert, which makes the
        // allocator place a new segment and therefore open the device.
        let mut manager = StorageManager::create(&root, 32, 1)?;
        let layout = DatabaseLayout::new(&root);
        let device_id = layout
            .discover_device_ids()?
            .first()
            .copied()
            .expect("database registers a device");
        std::fs::remove_file(layout.device_physical_path(device_id)).expect("remove container");

        let error = manager
            .insert(b"beta", b"two")
            .expect_err("a device without its container cannot be allocated from");
        assert_eq!(error.kind(), ErrorKind::Corruption);
        let detail = format!("{error:?}");
        assert!(
            detail.contains(&format!("device_id={}", device_id.get())),
            "the failing device is named: {detail}"
        );
        assert!(
            detail.contains("PHYSICAL.dat"),
            "the missing container is named: {detail}"
        );
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&root);
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[test]
fn an_existing_database_is_not_rebuilt_over_a_broken_layout() {
    let root = scratch("broken-layout");
    let mut manager = StorageManager::create(&root, 32, 4096).expect("create");
    manager.insert(b"key", b"value").expect("insert");
    let segments: Vec<(PathBuf, u64)> = manager
        .segments()
        .map(|segment| {
            let len = std::fs::metadata(&segment.path).expect("metadata").len();
            (segment.path.clone(), len)
        })
        .collect();
    drop(manager);

    // A required directory disappears, so the layout is no longer structurally
    // valid. Opening must report that, not recreate the tree and start over.
    let layout = DatabaseLayout::new(&root);
    std::fs::remove_dir(layout.temp_dir()).expect("remove temp");
    let error = StorageManager::open(&root, 32, 4096).expect_err("broken layout");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    assert!(!layout.temp_dir().exists(), "the root was rebuilt over");
    // The root still owns durable state, so a first-boot path must refuse to
    // create a database here even though opening failed.
    assert!(has_durable_state(&root).expect("probe"));

    // Nothing was rebuilt or truncated: every segment file keeps its exact size.
    for (path, len) in &segments {
        assert_eq!(std::fs::metadata(path).expect("metadata").len(), *len);
    }

    // Restoring the missing directory makes the database readable again with
    // its contents intact, which is what makes the failure safe.
    std::fs::create_dir_all(layout.temp_dir()).expect("restore temp");
    let mut reopened = StorageManager::open(&root, 32, 4096).expect("reopen");
    assert_eq!(reopened.get(b"key").expect("get"), Some(b"value".to_vec()));

    cleanup(&root);
}
