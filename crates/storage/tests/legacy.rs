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
//! Legacy-layout tests: an old storage root is refused instead of silently
//! becoming authoritative, and nothing is moved, rewritten, or deleted.
use plomid_core::ErrorKind;
use plomid_storage::layout::{is_legacy_layout, reject_legacy_layout, DatabaseLayout};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-legacy-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn a_fresh_root_is_not_a_legacy_layout() {
    let root = scratch("fresh");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    assert!(!is_legacy_layout(&layout));
    reject_legacy_layout(&layout).expect("no legacy layout");
    cleanup(&root);
}

#[test]
fn every_legacy_marker_is_refused_without_touching_the_root() {
    for marker in [
        "data",
        "manifest",
        "volumes",
        "generations",
        "database.db",
        "storage.db",
        "generation.db",
    ] {
        let root = scratch(marker);
        let layout = DatabaseLayout::new(&root);
        let target = root.join(marker);
        if marker.ends_with(".db") {
            std::fs::write(&target, b"legacy database").expect("write");
        } else {
            std::fs::create_dir(&target).expect("mkdir");
        }

        // The marker is recognised and refused as an incompatible layout.
        let error = reject_legacy_layout(&layout).expect_err("legacy layout");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert!(is_legacy_layout(&layout));

        // Initialization refuses it too, rather than migrating or overwriting.
        let error = layout.initialize().expect_err("legacy initialize");
        assert_eq!(error.kind(), ErrorKind::Unsupported);

        // Nothing was renamed, moved, or deleted.
        assert!(target.exists(), "the legacy marker must be left untouched");
        assert!(
            !layout.catalog_dir().exists(),
            "initialization must not create a new layout beside a legacy one"
        );

        cleanup(&root);
    }
}

#[test]
fn an_old_pointer_location_is_refused() {
    let root = scratch("old-pointer");
    let layout = DatabaseLayout::new(&root);
    std::fs::create_dir_all(layout.catalog_dir()).expect("catalog");
    std::fs::write(layout.catalog_dir().join("CURRENT"), b"legacy").expect("pointer");

    let error = reject_legacy_layout(&layout).expect_err("legacy pointer");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    // The old pointer is not the new one: `CURRENT` lives at the root.
    assert!(!layout.current_exists());
    assert!(layout.catalog_dir().join("CURRENT").is_file());

    cleanup(&root);
}

#[test]
fn a_root_level_database_file_is_refused() {
    let root = scratch("root-db");
    let layout = DatabaseLayout::new(&root);
    std::fs::write(root.join("plomid.db"), b"legacy single file").expect("write");

    let error = reject_legacy_layout(&layout).expect_err("legacy single file");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert!(root.join("plomid.db").is_file(), "left untouched");

    cleanup(&root);
}

#[test]
fn a_new_layout_is_never_fallen_back_to() {
    // The new layout is authoritative: a valid database is not reported as
    // legacy, and a legacy root is not adopted as a valid database.
    let root = scratch("no-fallback");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    layout
        .ensure_table(plomid_core::TableId::new(1))
        .expect("table");
    assert!(!is_legacy_layout(&layout));
    layout.validate().expect("valid new layout");

    cleanup(&root);
}
