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
//! Detection of storage layouts this build does not adopt.
//!
//! Earlier storage roots used a flat layout: physical segment files under
//! `<root>/data/`, a `<root>/manifest` segment listing, a `<root>/volumes`
//! registry, generation metadata under a database-level `<root>/generation/`
//! or `<root>/generations/` directory, and the publication pointer inside
//! `<root>/catalog/CURRENT`.
//!
//! This build does not adopt that layout and does not fall back to it. Two
//! layouts cannot both be authoritative, and silently reading the old one would
//! expose a database whose logical and physical organization no longer match
//! the code that reads it. Migration is not implemented, so an old root is
//! reported as an incompatible layout and left untouched: nothing is renamed,
//! moved, rewritten, or deleted by this module.

use super::DatabaseLayout;
use plomid_core::{ErrorKind, PlomidError, Result};
use std::path::Path;

/// Refuses a database root that carries an incompatible legacy layout.
pub fn reject_legacy_layout(layout: &DatabaseLayout) -> Result<()> {
    let markers = legacy_markers(layout);
    if markers.is_empty() {
        return Ok(());
    }
    Err(PlomidError::with_detail(
        ErrorKind::Unsupported,
        "storage root uses an incompatible legacy layout; \
         automatic migration is not implemented, so the root was left unchanged",
        format!(
            "root={} markers={}",
            layout.root().display(),
            markers.join(",")
        ),
    ))
}

/// Reports whether a database root carries an incompatible legacy layout.
#[must_use]
pub fn is_legacy_layout(layout: &DatabaseLayout) -> bool {
    !legacy_markers(layout).is_empty()
}

/// Paths whose presence proves the root uses the pre-`PLOMID_DATA` layout.
fn legacy_markers(layout: &DatabaseLayout) -> Vec<String> {
    let root = layout.root();
    let mut markers = Vec::new();
    for name in [
        "data",
        "manifest",
        "manifest.tmp",
        "volumes",
        "volumes.tmp",
        "generation",
        "generations",
        "database.db",
        "storage.db",
        "generation.db",
    ] {
        let path = root.join(name);
        if path.exists() {
            markers.push(name.to_string());
        }
    }
    // The publication pointer used to live inside the catalog directory.
    if root.join("catalog").join("CURRENT").exists() {
        markers.push("catalog/CURRENT".to_string());
    }
    // A single-file database anywhere at the root.
    if let Some(name) = database_file_at_root(root) {
        markers.push(name);
    }
    markers.sort();
    markers.dedup();
    markers
}

/// Returns the name of a root-level `.db` file, if one exists.
fn database_file_at_root(root: &Path) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.ends_with(".db").then_some(name)
        })
        .collect();
    names.sort();
    names.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::{is_legacy_layout, reject_legacy_layout};
    use crate::layout::DatabaseLayout;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "plomid-layout-legacy-{label}-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn a_fresh_root_is_not_legacy() {
        let root = scratch("fresh");
        let layout = DatabaseLayout::new(&root);
        assert!(!is_legacy_layout(&layout));
        assert!(reject_legacy_layout(&layout).is_ok());
    }

    #[test]
    fn every_legacy_marker_is_detected() {
        for marker in [
            "data",
            "manifest",
            "volumes",
            "generation",
            "generations",
            "database.db",
            "storage.db",
            "generation.db",
        ] {
            let root = scratch(marker);
            let layout = DatabaseLayout::new(&root);
            std::fs::create_dir_all(root.join("catalog")).expect("root");
            if marker.ends_with(".db") {
                std::fs::write(root.join(marker), b"legacy").expect("file");
            } else {
                std::fs::create_dir_all(root.join(marker)).expect("directory");
            }
            let error = reject_legacy_layout(&layout).expect_err("legacy layout");
            assert_eq!(error.kind(), plomid_core::ErrorKind::Unsupported);
            assert!(is_legacy_layout(&layout));
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_old_pointer_location_is_detected() {
        let root = scratch("old-pointer");
        let layout = DatabaseLayout::new(&root);
        std::fs::create_dir_all(layout.catalog_dir()).expect("catalog");
        std::fs::write(layout.catalog_dir().join("CURRENT"), b"legacy").expect("pointer");
        assert!(is_legacy_layout(&layout));
        // The old pointer is not the new one, which lives at the root.
        assert!(!layout.current_exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
