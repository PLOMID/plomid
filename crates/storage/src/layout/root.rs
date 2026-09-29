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
//! Root layout of a PLOMID database directory.
//!
//! [`DatabaseLayout`] is the filesystem-layout abstraction of a database root.
//! It answers one question — *where* does durable state live — and nothing else:
//! it does not allocate storage, select devices, write pages, run transactions,
//! perform WAL or recovery work, own catalog state, or inspect physical
//! content. Those responsibilities stay with the components that own them.

use super::{legacy, validation};
use crate::durable;
use plomid_core::Result;
use std::path::{Path, PathBuf};

// Directory and file names that several crates share (the WAL crate and the
// lifecycle coordinator write into the same tree) have one definition in
// `plomid_core::constants`; the layout re-exports them so path algebra stays
// in one place without a second copy of the same string.
pub use plomid_core::constants::{CATALOG_DIR_NAME, WAL_DIR_NAME};

/// Name of the published checkpoint directory.
pub const CHECKPOINTS_DIR_NAME: &str = plomid_core::constants::CHECKPOINT_DIR_NAME;
/// Name of the logical-object directory.
pub const OBJECTS_DIR_NAME: &str = "objects";
/// Name of the physical-device directory.
pub const DEVICES_DIR_NAME: &str = "devices";
/// Name of the database-wide shared-state directory.
pub const SHARED_DIR_NAME: &str = "shared";
/// Name of the transient scratch directory.
pub const TEMP_DIR_NAME: &str = "temp";
/// Name of the quarantine directory for unreachable artifacts.
pub const LOST_FOUND_DIR_NAME: &str = "lost+found";

/// Filesystem layout of one PLOMID database directory.
///
/// A layout is a pure path algebra over a root directory. Two layouts with the
/// same root resolve the same paths, and a layout can be constructed for a root
/// that does not exist yet, which is what makes database creation possible
/// before any directory exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseLayout {
    root: PathBuf,
}

impl DatabaseLayout {
    /// Creates a layout for `root`. Nothing is read or created.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The database root directory (`PLOMID_DATA/`).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Immutable published catalog states.
    #[must_use]
    pub fn catalog_dir(&self) -> PathBuf {
        self.root.join(CATALOG_DIR_NAME)
    }

    /// Write-ahead log segments.
    #[must_use]
    pub fn wal_dir(&self) -> PathBuf {
        self.root.join(WAL_DIR_NAME)
    }

    /// Published checkpoints.
    #[must_use]
    pub fn checkpoints_dir(&self) -> PathBuf {
        self.root.join(CHECKPOINTS_DIR_NAME)
    }

    /// Logical database objects.
    #[must_use]
    pub fn objects_dir(&self) -> PathBuf {
        self.root.join(OBJECTS_DIR_NAME)
    }

    /// Physical storage devices.
    #[must_use]
    pub fn devices_dir(&self) -> PathBuf {
        self.root.join(DEVICES_DIR_NAME)
    }

    /// Database-wide shared state.
    #[must_use]
    pub fn shared_dir(&self) -> PathBuf {
        self.root.join(SHARED_DIR_NAME)
    }

    /// Transient scratch space. Nothing here is ever authoritative.
    #[must_use]
    pub fn temp_dir(&self) -> PathBuf {
        self.root.join(TEMP_DIR_NAME)
    }

    /// Quarantine space for artifacts that cannot be proven reachable.
    #[must_use]
    pub fn lost_found_dir(&self) -> PathBuf {
        self.root.join(LOST_FOUND_DIR_NAME)
    }

    /// Creates the required database tree.
    ///
    /// Initialization is deterministic, idempotent, and restart safe: it
    /// creates the directories the layout requires when they are missing and
    /// never rewrites, truncates, or replaces durable state. Running it against
    /// an existing valid database is a no-op, so it is safe on every creation
    /// and open path.
    ///
    /// A root that carries an incompatible legacy layout is reported as an
    /// error instead of being adopted, migrated, or overwritten.
    pub fn initialize(&self) -> Result<()> {
        legacy::reject_legacy_layout(self)?;
        durable::ensure_dir(self.root())?;
        for dir in validation::required_dirs(self) {
            durable::ensure_dir(&dir)?;
        }
        Ok(())
    }

    /// Validates the structural layout of the database root.
    ///
    /// The check is structural: required directories must exist as directories
    /// and no incompatible legacy layout may be present. No page, block, pack,
    /// or segment content is read, so the cost is independent of database size.
    pub fn validate(&self) -> Result<()> {
        validation::validate_root(self)
    }
}

#[cfg(test)]
mod tests {
    use super::DatabaseLayout;
    use std::path::{Path, PathBuf};

    fn root() -> PathBuf {
        std::env::temp_dir().join("plomid-layout-root-unit")
    }

    #[test]
    fn root_level_paths_are_stable() {
        let layout = DatabaseLayout::new(root());
        assert_eq!(layout.root(), Path::new(&root()));
        assert_eq!(layout.current_path(), root().join("CURRENT"));
        assert_eq!(layout.current_staged_path(), root().join("CURRENT.tmp"));
        assert_eq!(layout.catalog_dir(), root().join("catalog"));
        assert_eq!(layout.wal_dir(), root().join("wal"));
        assert_eq!(layout.checkpoints_dir(), root().join("checkpoints"));
        assert_eq!(layout.objects_dir(), root().join("objects"));
        assert_eq!(layout.devices_dir(), root().join("devices"));
        assert_eq!(layout.shared_dir(), root().join("shared"));
        assert_eq!(layout.temp_dir(), root().join("temp"));
        assert_eq!(layout.lost_found_dir(), root().join("lost+found"));
    }

    #[test]
    fn equal_roots_produce_equal_layouts() {
        assert_eq!(DatabaseLayout::new(root()), DatabaseLayout::new(root()));
    }
}
