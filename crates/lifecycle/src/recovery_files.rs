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
//! Filesystem helpers for checkpoint discovery and WAL crash-tail truncation.

use plomid_core::Result;
use plomid_storage::checkpoint::discover;
use plomid_wal::WAL_DIR_NAME;
use std::path::{Path, PathBuf};

/// Returns the newest published checkpoint file for a storage root, if any.
///
/// Discovery is the existing deterministic generation-order enumeration; the
/// last entry is the newest published generation.
pub(crate) fn newest_checkpoint_path(root: &Path) -> Result<Option<PathBuf>> {
    Ok(discover(root)?.into_iter().next_back())
}

/// Restores the newest WAL segment to its durable prefix.
///
/// The recovery contract defines bytes at and beyond the crash-tail boundary
/// as never durable; truncating to that boundary removes only bytes that were
/// never part of the log. The boundary is computed by the existing recovery
/// reader, not by this helper.
pub(crate) fn truncate_wal_crash_tail(root: &Path, boundary: u64) -> Result<()> {
    let wal_dir = root.join(WAL_DIR_NAME);
    let newest = std::fs::read_dir(&wal_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("checkpoint") | Some("checkpoint.tmp")
            )
        })
        .max();
    let Some(segment) = newest else {
        return Ok(());
    };
    let file = std::fs::OpenOptions::new().write(true).open(&segment)?;
    file.set_len(boundary)?;
    file.sync_all()?;
    Ok(())
}
