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
//! Filesystem abstraction used by durable PLOMID storage components.
//!
//! Production code must use [`RealFs`] through [`FileSystem`]. No in-memory
//! or fake filesystem is provided by this crate; tests should use real files
//! under a temporary directory. `rename` follows the platform's atomic
//! replacement guarantees. Callers that need a durable replacement should
//! fsync the new file before renaming, perform the rename, and apply the
//! platform-appropriate parent-directory fsync policy.

use plomid_core::{PlomidError, Result};
use std::{fs, path::Path};

/// IO operations required by WAL, page, and checkpoint implementations.
///
/// The associated file type lets production callers retain an open file.
/// Implementations must be `Send + Sync`; individual file handles are passed
/// mutably because the portable standard-library implementation updates the
/// underlying file cursor.
pub trait FileSystem: Send + Sync {
    /// The handle returned by [`Self::create`] and [`Self::open`].
    type File: Send + Sync;

    /// Creates or truncates a file at `path`.
    fn create(&self, path: &Path) -> Result<Self::File>;

    /// Opens an existing file at `path` for reading and writing.
    fn open(&self, path: &Path) -> Result<Self::File>;

    /// Reads at most `buffer.len()` bytes beginning at `offset`.
    fn read_at(&self, file: &mut Self::File, offset: u64, buffer: &mut [u8]) -> Result<usize>;

    /// Writes bytes beginning at `offset`, returning the number accepted.
    fn write_at(&self, file: &mut Self::File, offset: u64, data: &[u8]) -> Result<usize>;

    /// Makes file contents durable according to the host filesystem.
    fn fsync(&self, file: &mut Self::File) -> Result<()>;

    /// Renames a path. Replacement behavior follows the host platform.
    fn rename(&self, from: &Path, to: &Path) -> Result<()>;

    /// Returns the current logical file length.
    fn len(&self, file: &Self::File) -> Result<u64>;
}

/// Production filesystem backed by the host operating system.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealFs;

/// Open file handle owned by [`RealFs`].
#[derive(Debug)]
pub struct RealFile(fs::File);

impl From<fs::File> for RealFile {
    fn from(file: fs::File) -> Self {
        Self(file)
    }
}

impl FileSystem for RealFs {
    type File = RealFile;

    fn create(&self, path: &Path) -> Result<Self::File> {
        fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .map(RealFile::from)
            .map_err(PlomidError::from)
    }

    fn open(&self, path: &Path) -> Result<Self::File> {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(RealFile::from)
            .map_err(PlomidError::from)
    }

    fn read_at(&self, file: &mut Self::File, offset: u64, buffer: &mut [u8]) -> Result<usize> {
        use crate::platform::FileExt as _;

        file.0.read_at(buffer, offset).map_err(PlomidError::from)
    }

    fn write_at(&self, file: &mut Self::File, offset: u64, data: &[u8]) -> Result<usize> {
        use crate::platform::FileExt as _;

        file.0.write_at(data, offset).map_err(PlomidError::from)
    }

    fn fsync(&self, file: &mut Self::File) -> Result<()> {
        // `sync_all` is the strongest portable durability barrier. On Linux and
        // Windows it is the platform's native durable flush (`fsync(2)` /
        // `FlushFileBuffers`); on Apple platforms the standard library
        // implements it with `fcntl(F_FULLFSYNC)`, which is the only primitive
        // macOS/APFS actually documents as flushing to stable media. Using the
        // weaker `fsync(2)` there would silently trade durability for speed.
        //
        // Measured on APFS: `F_FULLFSYNC` ~4.5ms, plain `fsync(2)` ~0.03ms.
        // PostgreSQL's default `wal_sync_method=fsync` therefore performs no
        // real media flush on macOS; an equivalent-durability comparison must
        // configure PostgreSQL with `wal_sync_method=fsync_writethrough`.
        file.0.sync_all().map_err(PlomidError::from)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        fs::rename(from, to).map_err(PlomidError::from)
    }

    fn len(&self, file: &Self::File) -> Result<u64> {
        file.0
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(PlomidError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::{FileSystem, RealFs};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn temp_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "plomid-storage-{label}-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn real_fs_round_trip_with_fsync() {
        let path = temp_path("round-trip");
        let fs = RealFs;
        let result = (|| {
            let mut file = fs.create(&path)?;
            assert_eq!(fs.write_at(&mut file, 0, b"durable page")?, 12);
            fs.fsync(&mut file)?;
            let mut buffer = [0_u8; 12];
            assert_eq!(fs.read_at(&mut file, 0, &mut buffer)?, 12);
            assert_eq!(&buffer, b"durable page");
            assert_eq!(fs.len(&file)?, 12);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(
            result.is_ok(),
            "real filesystem round trip failed: {result:?}"
        );
    }
}
