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
//! `CURRENT`: the durable pointer to the published state of the database.
//!
//! `PLOMID_DATA/CURRENT` records which published catalog state is authoritative
//! and which durable storage generation and WAL boundary that state describes.
//! It is the single durable authority for "what does this database currently
//! contain": readers and recovery never infer the current state from directory
//! enumeration, they read this record, validate it, and follow it.
//!
//! # Semantics
//!
//! * **Absent** means the database has never published a state. That is not an
//!   error: a new database publishes its first state through the generation
//!   manager, which creates `CURRENT` as the last step of publication.
//! * **Present** means the named state is authoritative. A record that fails to
//!   decode is corruption and is reported, never silently replaced.
//!
//! # Publication order
//!
//! A publication writes generation metadata, then the catalog state, and only
//! then replaces `CURRENT` through the shared atomic publication policy
//! (stage, flush, verify, sync, rename, directory sync). Because the pointer is
//! replaced last, a crash at any earlier point leaves the previous state
//! authoritative, and a crash during replacement leaves either the old complete
//! record or the new complete record.
//!
//! State recovery that cannot trust the pointer is owned by the generation
//! manager, which falls back to the newest valid checkpoint and the newest
//! published catalog state before giving up.

use super::names::STAGING_SUFFIX;
use super::DatabaseLayout;
use std::path::PathBuf;

/// Name of the durable pointer to the published state.
///
/// The pointer file name is shared with the generation manager, which writes
/// it; there is one definition in `plomid_core::constants`.
pub const CURRENT_FILE_NAME: &str = plomid_core::constants::PUBLICATION_POINTER_FILE_NAME;

impl DatabaseLayout {
    /// Path of the durable pointer to the published state.
    #[must_use]
    pub fn current_path(&self) -> PathBuf {
        self.root().join(CURRENT_FILE_NAME)
    }

    /// Staging path used while publishing [`Self::current_path`].
    #[must_use]
    pub fn current_staged_path(&self) -> PathBuf {
        self.root()
            .join(format!("{CURRENT_FILE_NAME}{STAGING_SUFFIX}"))
    }

    /// Whether a published pointer exists.
    #[must_use]
    pub fn current_exists(&self) -> bool {
        self.current_path().is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::{DatabaseLayout, CURRENT_FILE_NAME};
    use std::path::PathBuf;

    #[test]
    fn current_lives_at_the_database_root() {
        let root = PathBuf::from("/tmp/plomid-layout-current");
        let layout = DatabaseLayout::new(&root);
        assert_eq!(layout.current_path(), root.join(CURRENT_FILE_NAME));
        assert_eq!(layout.current_staged_path(), root.join("CURRENT.tmp"));
        assert!(!layout.current_exists());
    }
}
