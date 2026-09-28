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
//! Durable single-file publication policy for persisted metadata.
//!
//! Catalog states, generation records, and the publication pointer are all
//! published with one policy, the same policy checkpoints use:
//!
//! ```text
//! BUILD stage the complete image into <final>.tmp
//! FLUSH push the staged bytes through the file's buffered writer
//! VERIFY re-read the staged image and validate it before it can publish
//! SYNC   fsync the staged file: the durability boundary
//! PUBLISH atomically rename the staged file onto its final name, then fsync
//!         the containing directory so the rename itself is durable
//! ```
//!
//! A crash before the rename leaves only a `.tmp` artifact, which discovery
//! ignores, so the previously published file stays authoritative. Final names
//! are never written in place, so a reader observes either the old complete
//! image or the new complete image.
//!
//! Checkpoints keep their own phase-instrumented staging loop (failure
//! injection points and phase timings are part of the checkpoint contract) and
//! share the synchronization helpers below, so the crate has exactly one
//! durability boundary implementation for metadata files.

use crate::codec::{corruption, invalid};
use plomid_core::{ErrorKind, PlomidError, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Creates the directory at `path` if it does not exist.
pub(crate) fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(PlomidError::from)
}

/// BUILD + FLUSH: writes the complete image and flushes it through the file.
///
/// The staged file is created or truncated, so a leftover artifact from an
/// interrupted publication can never contribute bytes to a new image.
pub(crate) fn stage_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(path)
        .map_err(PlomidError::from)?;
    let mut written = 0_usize;
    while written < bytes.len() {
        let count = file.write(&bytes[written..]).map_err(PlomidError::from)?;
        if count == 0 {
            return Err(PlomidError::new(
                ErrorKind::Io,
                "staged metadata file accepted no bytes",
            ));
        }
        written = written
            .checked_add(count)
            .ok_or_else(|| invalid("staged metadata write overflow"))?;
    }
    file.flush().map_err(PlomidError::from)
}

/// VERIFY: re-reads a staged image and requires it to be exactly `expected`.
pub(crate) fn verify_staged(path: &Path, expected: &[u8]) -> Result<()> {
    let staged = fs::read(path).map_err(PlomidError::from)?;
    if staged.len() != expected.len() {
        return Err(corruption("staged metadata length changed"));
    }
    if staged != expected {
        return Err(corruption("staged metadata contents changed"));
    }
    Ok(())
}

/// SYNC: makes the contents of a file durable according to the host
/// filesystem.
pub(crate) fn sync_file(path: &Path) -> Result<()> {
    let file = File::open(path).map_err(PlomidError::from)?;
    file.sync_all().map_err(PlomidError::from)
}

/// SYNC: makes the directory entries themselves durable.
///
/// A rename is atomic, but it only survives a crash once the containing
/// directory is synchronized.
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    let file = File::open(dir).map_err(PlomidError::from)?;
    file.sync_all().map_err(PlomidError::from)
}

/// PUBLISH: atomically replaces `final_path` with the staged file.
pub(crate) fn publish_rename(staged: &Path, final_path: &Path) -> Result<()> {
    fs::rename(staged, final_path).map_err(PlomidError::from)
}

/// Removes a file, ignoring absence.
///
/// Used to clear staging artifacts and to reclaim generation storage, never to
/// remove a file that is still referenced by durable metadata.
pub(crate) fn remove_if_exists(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::{publish_rename, stage_bytes, sync_dir, sync_file, verify_staged};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "plomid-durable-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("scratch");
        path
    }

    #[test]
    fn staged_publication_replaces_atomically() {
        let dir = scratch("publish");
        let result = (|| {
            let staged = dir.join("state.tmp");
            let final_path = dir.join("state");
            stage_bytes(&staged, b"first")?;
            verify_staged(&staged, b"first")?;
            sync_file(&staged)?;
            publish_rename(&staged, &final_path)?;
            sync_dir(&dir)?;
            assert_eq!(std::fs::read(&final_path)?, b"first");
            stage_bytes(&staged, b"second")?;
            publish_rename(&staged, &final_path)?;
            assert_eq!(std::fs::read(&final_path)?, b"second");
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn staged_content_mismatch_is_detected() {
        let dir = scratch("verify");
        let result = (|| {
            let staged = dir.join("state.tmp");
            stage_bytes(&staged, b"published bytes")?;
            assert!(verify_staged(&staged, b"other bytes").is_err());
            assert!(verify_staged(&staged, b"published").is_err());
            verify_staged(&staged, b"published bytes")?;
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_ok(), "{result:?}");
    }
}
