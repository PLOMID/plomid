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
//! Portable positioned (offset-based) file I/O.
//!
//! PLOMID's storage layout is position-addressable: pages, block headers,
//! pack footers and device metadata are read and written at absolute file
//! offsets, and the same file handle is shared by many readers. That makes
//! `pread`/`pwrite`-style I/O — not a `Seek` cursor — the primitive every
//! storage layer is built on.
//!
//! The standard library exposes those primitives per platform and under
//! different names:
//!
//! | operation | Unix (`std::os::unix::fs::FileExt`) | Windows (`std::os::windows::fs::FileExt`) |
//! |---|---|---|
//! | read at offset | `read_at` | `seek_read` |
//! | write at offset | `write_at` | `seek_write` |
//! | read exactly at offset | `read_exact_at` | *(not provided)* |
//! | write all at offset | `write_all_at` | *(not provided)* |
//!
//! [`FileExt`] is the crate-internal abstraction over that difference. It
//! mirrors the Unix signatures exactly — `read_exact_at`/`write_all_at`
//! included — so storage code calls `file.read_exact_at(buf, offset)`
//! uniformly and stays platform-agnostic. On Unix every method forwards to
//! the standard library's implementation; on Windows the two "exact/all"
//! methods are the standard library's documented completion loops over
//! `seek_read`/`seek_write`.
//!
//! Keeping this behind one trait means the hot paths add no work on Unix
//! (callers pay only a static dispatch) and no storage module needs
//! `#[cfg]` branches of its own.

use std::fs::File;
use std::io;

/// Positioned read/write operations available on every supported platform.
///
/// Mirrors [`std::os::unix::fs::FileExt`]'s method set so Unix callers are
/// byte-for-byte equivalent to their previous code. Windows has no such
/// combined trait, so the equivalents are [`std::os::windows::fs::FileExt`]'s
/// `seek_read`/`seek_write` plus the completion loops defined below.
pub(crate) trait FileExt {
    /// Reads bytes starting at `offset`.
    ///
    /// Returns the number of bytes read, which may be short of
    /// `buffer.len()` at end of file. Never modifies the shared file cursor.
    fn read_at(&self, buffer: &mut [u8], offset: u64) -> io::Result<usize>;

    /// Writes bytes starting at `offset`.
    ///
    /// Returns the number of bytes written, which may be short. Never modifies
    /// the shared file cursor.
    fn write_at(&self, data: &[u8], offset: u64) -> io::Result<usize>;

    /// Reads exactly `buffer.len()` bytes starting at `offset`.
    ///
    /// Fails with [`io::ErrorKind::UnexpectedEof`] when the file ends first.
    fn read_exact_at(&self, buffer: &mut [u8], offset: u64) -> io::Result<()>;

    /// Writes all of `data` starting at `offset`.
    fn write_all_at(&self, data: &[u8], offset: u64) -> io::Result<()>;
}

/// Forwarding to the standard library's Unix implementation. Each call is
/// fully qualified because this trait declares the same method names, so a
/// bare `File::read_at(...)` would be ambiguous between the two traits.
#[cfg(unix)]
impl FileExt for File {
    fn read_at(&self, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        <File as std::os::unix::fs::FileExt>::read_at(self, buffer, offset)
    }

    fn write_at(&self, data: &[u8], offset: u64) -> io::Result<usize> {
        <File as std::os::unix::fs::FileExt>::write_at(self, data, offset)
    }

    fn read_exact_at(&self, buffer: &mut [u8], offset: u64) -> io::Result<()> {
        <File as std::os::unix::fs::FileExt>::read_exact_at(self, buffer, offset)
    }

    fn write_all_at(&self, data: &[u8], offset: u64) -> io::Result<()> {
        <File as std::os::unix::fs::FileExt>::write_all_at(self, data, offset)
    }
}

#[cfg(windows)]
impl FileExt for File {
    fn read_at(&self, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        use std::os::windows::fs::FileExt as _;
        File::seek_read(self, buffer, offset)
    }

    fn write_at(&self, data: &[u8], offset: u64) -> io::Result<usize> {
        use std::os::windows::fs::FileExt as _;
        File::seek_write(self, data, offset)
    }

    /// Same completion rule as the Unix implementation: a short read is
    /// retried at the advanced offset, and a clean end of file before the
    /// buffer is filled is `UnexpectedEof`.
    fn read_exact_at(&self, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
        while !buffer.is_empty() {
            match self.read_at(buffer, offset)? {
                0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "failed to fill whole buffer",
                    ))
                }
                read => {
                    offset += read as u64;
                    buffer = &mut buffer[read..];
                }
            }
        }
        Ok(())
    }

    /// Same completion rule as `write_all`: the write loop continues until
    /// every byte is written, so a partial `seek_write` is not an error.
    fn write_all_at(&self, mut data: &[u8], mut offset: u64) -> io::Result<()> {
        while !data.is_empty() {
            match self.write_at(data, offset)? {
                0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ))
                }
                written => {
                    offset += written as u64;
                    data = &data[written..];
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::FileExt;
    use std::fs::OpenOptions;
    use std::io;

    fn scratch(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("plomid-platform-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn positioned_write_and_read_round_trip() {
        let path = scratch("round-trip");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();

        file.write_all_at(b"hello world", 8).unwrap();
        let mut buffer = [0_u8; 11];
        file.read_exact_at(&mut buffer, 8).unwrap();
        assert_eq!(&buffer, b"hello world");

        // Overwrite a slice in place, leaving neighbours intact.
        file.write_all_at(b"WORLD", 14).unwrap();
        let mut buffer = [0_u8; 11];
        file.read_exact_at(&mut buffer, 8).unwrap();
        assert_eq!(&buffer, b"hello WORLD");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn short_read_at_end_of_file_is_unexpected_eof() {
        let path = scratch("eof");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all_at(b"abc", 0).unwrap();

        let mut buffer = [0_u8; 8];
        let error = file.read_exact_at(&mut buffer, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_all_at_extends_a_sparse_file() {
        let path = scratch("sparse");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();

        // Writing past the end (device/extent layout does this) must extend
        // the file durably at the requested offset.
        file.write_all_at(&[0xAB], 1 << 20).unwrap();
        assert_eq!(file.metadata().unwrap().len(), (1 << 20) + 1);
        let mut byte = [0_u8; 1];
        file.read_exact_at(&mut byte, 1 << 20).unwrap();
        assert_eq!(byte[0], 0xAB);

        let _ = std::fs::remove_file(&path);
    }
}
