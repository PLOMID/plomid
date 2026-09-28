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
//! Shared state directories: `shared/{tx,rowid,statistics,filters,dictionaries}/`.
//!
//! `shared/` holds database-wide state that belongs to no single table:
//!
//! ```text
//! shared/
//! ├── tx/            transaction state
//! ├── rowid/         database-wide row identifier allocation
//! ├── statistics/    optimizer statistics
//! ├── filters/       shared filter data
//! └── dictionaries/  shared dictionaries
//! ```
//!
//! The authoritative implementation of each of those mechanisms stays with the
//! component that owns it; the layout owns only where each one keeps its files.
//! `shared/` is deliberately not a dumping ground: a new subdirectory is added
//! only when an existing component needs a database-wide location.

use super::DatabaseLayout;
use std::path::PathBuf;

/// Transaction state of the database.
pub const SHARED_TX_DIR_NAME: &str = "tx";
/// Database-wide row identifier allocation state.
pub const SHARED_ROWID_DIR_NAME: &str = "rowid";
/// Optimizer statistics.
pub const SHARED_STATISTICS_DIR_NAME: &str = "statistics";
/// Shared filter data.
pub const SHARED_FILTERS_DIR_NAME: &str = "filters";
/// Shared dictionaries.
pub const SHARED_DICTIONARIES_DIR_NAME: &str = "dictionaries";

impl DatabaseLayout {
    /// Transaction state directory.
    #[must_use]
    pub fn shared_tx_dir(&self) -> PathBuf {
        self.shared_dir().join(SHARED_TX_DIR_NAME)
    }

    /// Database-wide row identifier directory.
    #[must_use]
    pub fn shared_rowid_dir(&self) -> PathBuf {
        self.shared_dir().join(SHARED_ROWID_DIR_NAME)
    }

    /// Optimizer statistics directory.
    #[must_use]
    pub fn shared_statistics_dir(&self) -> PathBuf {
        self.shared_dir().join(SHARED_STATISTICS_DIR_NAME)
    }

    /// Shared filter data directory.
    #[must_use]
    pub fn shared_filters_dir(&self) -> PathBuf {
        self.shared_dir().join(SHARED_FILTERS_DIR_NAME)
    }

    /// Shared dictionary directory.
    #[must_use]
    pub fn shared_dictionaries_dir(&self) -> PathBuf {
        self.shared_dir().join(SHARED_DICTIONARIES_DIR_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::DatabaseLayout;
    use std::path::PathBuf;

    #[test]
    fn shared_paths_are_grouped_under_shared() {
        let root = PathBuf::from("/tmp/plomid-layout-shared");
        let layout = DatabaseLayout::new(&root);
        let shared = root.join("shared");
        assert_eq!(layout.shared_tx_dir(), shared.join("tx"));
        assert_eq!(layout.shared_rowid_dir(), shared.join("rowid"));
        assert_eq!(layout.shared_statistics_dir(), shared.join("statistics"));
        assert_eq!(layout.shared_filters_dir(), shared.join("filters"));
        assert_eq!(
            layout.shared_dictionaries_dir(),
            shared.join("dictionaries")
        );
    }
}
