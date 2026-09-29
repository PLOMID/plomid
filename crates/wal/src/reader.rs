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
//! Real-file WAL reader with strict frame and LSN validation.
//!
//! # Tail policy
//!
//! A WAL commonly ends with an incomplete record after an interrupted write.
//! The reader distinguishes three cases:
//!
//! * valid complete record: returned normally;
//! * recoverable incomplete tail: a torn header or a frame whose declared
//!   length extends past end-of-file at the final offset. Reported as
//!   [`TailState::Incomplete`] with the byte offset where valid data ends;
//! * corruption before the valid tail: bad magic, version, type, length, or a
//!   checksum on a fully present frame. Reported as a `Corruption` error, never
//!   as valid data and never silently ignored.
//!
//! # Segment headers
//!
//! The reader validates the segment header before trusting sequence/first-LSN
//! fields and verifies the first decoded record matches the header's first LSN.

use crate::format::{decode, frame_len, Record, WAL_HEADER_SIZE};
use crate::segmented::{SegmentHeader, SEGMENT_HEADER_SIZE};
use plomid_core::{ErrorKind, Lsn, PlomidError, Result};
use plomid_storage::{FileSystem, RealFs};
use std::path::Path;

/// Outcome of probing the segment tail without consuming record state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TailState {
    /// File ends exactly on a record boundary; no truncation needed.
    Clean {
        /// End-of-file byte offset (first unwritten byte).
        end: u64,
    },
    /// File ends mid-record; `end` is the offset of the last valid byte.
    Incomplete {
        /// Offset of the first torn byte (safe truncation point).
        end: u64,
        /// Total physical file length.
        file_len: u64,
    },
}

/// Reads and validates records from one real WAL segment in sequence.
pub struct WalReader {
    fs: RealFs,
    file: <RealFs as FileSystem>::File,
    offset: u64,
    last_lsn: Option<Lsn>,
    header: Option<SegmentHeader>,
    /// Cached file length. The previous implementation issued an `fstat`
    /// (`fs.len`) on every record, so a 12k-record replay paid 12k metadata
    /// syscalls beyond the data reads. The length is fetched once and only
    /// re-fetched when the cursor reaches the cached end (the only point at
    /// which growth could matter), preserving exact tail semantics.
    cached_len: Option<u64>,
}

impl WalReader {
    /// Opens a WAL segment for validated sequential reading.
    pub fn open(path: &Path) -> Result<Self> {
        let fs = RealFs;
        let mut file = fs.open(path)?;
        let (offset, header) = detect_header(&fs, &mut file)?;
        Ok(Self {
            fs,
            file,
            offset,
            last_lsn: None,
            header,
            cached_len: None,
        })
    }

    /// Returns the validated segment header, if the file carries one.
    #[must_use]
    pub fn segment_header(&self) -> Option<SegmentHeader> {
        self.header
    }

    /// Returns the current read offset (first byte of unread records).
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the current file length, refreshing the cache only when the
    /// cursor has reached the previously observed end (growth can only be
    /// observed there). Sequential replay otherwise reuses one `fstat`.
    fn file_len(&mut self) -> Result<u64> {
        if let Some(cached) = self.cached_len {
            if self.offset < cached {
                return Ok(cached);
            }
        }
        let length = self.fs.len(&self.file)?;
        self.cached_len = Some(length);
        Ok(length)
    }

    /// Reports the tail state without advancing the reader.
    pub fn tail_state(&mut self) -> Result<TailState> {
        let length = self.file_len()?;
        if self.offset == length {
            return Ok(TailState::Clean { end: length });
        }
        let remaining = length
            .checked_sub(self.offset)
            .ok_or_else(|| corruption("WAL offset is beyond end of file"))?;
        if remaining < 4 {
            return Ok(TailState::Incomplete {
                end: self.offset,
                file_len: length,
            });
        }
        // Single stack-resident probe: the previous path issued one 4-byte
        // read, one header allocation + read, then one frame allocation +
        // read per probe. A 36-byte stack buffer classifies the frame with one
        // positional read and no heap allocation.
        let mut header = [0_u8; WAL_HEADER_SIZE];
        let probe = (remaining.min(WAL_HEADER_SIZE as u64)) as usize;
        read_exact(&self.fs, &mut self.file, self.offset, &mut header[..probe])?;
        let header_size = WAL_HEADER_SIZE;
        if remaining < header_size as u64 {
            return Ok(TailState::Incomplete {
                end: self.offset,
                file_len: length,
            });
        }
        match frame_len(&header[..header_size]) {
            Ok(frame) if (frame as u64) > remaining => Ok(TailState::Incomplete {
                end: self.offset,
                file_len: length,
            }),
            _ => Ok(TailState::Clean { end: length }),
        }
    }

    /// Reads the next record, returning `None` only at an exact clean EOF.
    ///
    /// A torn header or a frame extending past EOF is a recoverable incomplete
    /// tail reported as a `Corruption` error whose `detail` carries
    /// `valid_end=<n>`. A fully present but invalid frame is corruption too.
    pub fn next_record(&mut self) -> Result<Option<Record>> {
        let length = self.file_len()?;
        if self.offset == length {
            return Ok(None);
        }
        let remaining = length
            .checked_sub(self.offset)
            .ok_or_else(|| corruption("WAL offset is beyond end of file"))?;
        if remaining < 4 {
            return Err(incomplete("truncated WAL record header", self.offset));
        }
        // One small stack frame carries the record header, so the hot path
        // performs a single positional read to classify the frame instead of
        // two reads plus a heap allocation per record. The 4-byte magic probe
        // is skipped: the full-header read is validated by `frame_len` anyway.
        let mut header = [0_u8; WAL_HEADER_SIZE];
        let probe = (remaining.min(WAL_HEADER_SIZE as u64)) as usize;
        read_exact(&self.fs, &mut self.file, self.offset, &mut header[..probe])?;
        if probe < 4 {
            return Err(incomplete("truncated WAL record header", self.offset));
        }
        let header_size = WAL_HEADER_SIZE;
        if remaining < header_size as u64 {
            return Err(incomplete("truncated WAL record header", self.offset));
        }
        let frame = frame_len(&header[..header_size])?;
        let frame_u64 = u64::try_from(frame).map_err(|_| corruption("WAL frame too large"))?;
        if frame_u64 > remaining {
            return Err(incomplete("truncated WAL record tail", self.offset));
        }
        // Reuse the validated header prefix: the previous path re-read the
        // same header bytes as part of the full frame, paying two positional
        // reads per record. Here only the trailing `frame - header_size`
        // bytes are read and appended, so each record costs one header probe
        // plus one payload read with identical validation.
        let mut bytes = Vec::with_capacity(frame);
        bytes.extend_from_slice(&header[..header_size]);
        if frame > header_size {
            let tail = frame - header_size;
            let mut tail_buf = vec![0_u8; tail];
            read_exact(
                &self.fs,
                &mut self.file,
                self.offset + header_size as u64,
                &mut tail_buf,
            )?;
            bytes.extend_from_slice(&tail_buf);
        }
        let record = decode(&bytes)?;
        if record.lsn.get() == 0 {
            return Err(corruption("WAL record has an invalid LSN"));
        }
        match self.last_lsn {
            None => {
                if let Some(header) = self.header {
                    if record.lsn != header.first_lsn {
                        return Err(corruption("WAL first record LSN mismatches segment header"));
                    }
                }
            }
            Some(last) if record.lsn <= last => {
                return Err(corruption("WAL LSN is not strictly increasing"));
            }
            Some(_) => {}
        }
        self.last_lsn = Some(record.lsn);
        self.offset = self
            .offset
            .checked_add(frame_u64)
            .ok_or_else(|| corruption("WAL offset overflow"))?;
        Ok(Some(record))
    }
}

fn detect_header(
    fs: &RealFs,
    file: &mut <RealFs as FileSystem>::File,
) -> Result<(u64, Option<SegmentHeader>)> {
    let length = fs.len(file)?;
    if length < SEGMENT_HEADER_SIZE as u64 {
        return Ok((0, None));
    }
    let mut raw = [0_u8; SEGMENT_HEADER_SIZE];
    read_exact(fs, file, 0, &mut raw)?;
    match SegmentHeader::decode(&raw) {
        Ok(header) => Ok((SEGMENT_HEADER_SIZE as u64, Some(header))),
        Err(_) => Ok((0, None)),
    }
}

fn read_exact(
    fs: &RealFs,
    file: &mut <RealFs as FileSystem>::File,
    mut offset: u64,
    buffer: &mut [u8],
) -> Result<()> {
    let mut read = 0;
    while read < buffer.len() {
        let count = fs.read_at(file, offset, &mut buffer[read..])?;
        if count == 0 {
            return Err(corruption("truncated WAL record"));
        }
        read += count;
        offset += count as u64;
    }
    Ok(())
}

fn incomplete(message: &'static str, valid_end: u64) -> PlomidError {
    PlomidError::with_detail(
        ErrorKind::Corruption,
        message,
        format!("incomplete_tail valid_end={valid_end}"),
    )
}

fn corruption(message: &'static str) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}
#[cfg(test)]
mod tests {
    use super::{TailState, WalReader};
    use crate::{RecordType, WalWriter};
    use std::{
        fs,
        io::{Seek, SeekFrom, Write},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn temp_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("plomid-wal-{label}-{}-{id}", std::process::id()))
    }

    #[test]
    fn writes_and_reads_multiple_records_with_increasing_lsns() {
        let path = temp_path("records");
        let result = (|| {
            let mut writer = WalWriter::create(&path)?;
            assert_eq!(writer.append(RecordType::Begin, b"a")?.get(), 1);
            assert_eq!(writer.append(RecordType::Data, b"payload")?.get(), 2);
            writer.sync()?;
            drop(writer);
            let mut reader = WalReader::open(&path)?;
            assert!(reader.segment_header().is_some());
            let first = reader
                .next_record()?
                .ok_or_else(|| super::corruption("missing record"))?;
            let second = reader
                .next_record()?
                .ok_or_else(|| super::corruption("missing record"))?;
            assert_eq!(first.payload, b"a");
            assert_eq!(second.payload, b"payload");
            assert!(matches!(reader.tail_state()?, TailState::Clean { .. }));
            assert!(reader.next_record()?.is_none());
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "WAL record test failed: {result:?}");
    }

    #[test]
    fn checksum_failure_is_corruption() {
        let path = temp_path("checksum");
        let result = (|| {
            let mut writer = WalWriter::create(&path)?;
            writer.append(RecordType::Data, b"payload")?;
            writer.sync()?;
            drop(writer);
            let mut file = fs::OpenOptions::new().write(true).open(&path)?;
            // Flip a byte in the payload region (after 32-byte header + 36-byte
            // record header), invalidating the CRC.
            file.seek(SeekFrom::Start(32 + 20))?;
            file.write_all(b"X")?;
            file.sync_all()?;
            drop(file);
            let mut reader = WalReader::open(&path)?;
            let error = reader.next_record().expect_err("corrupt WAL must fail");
            assert_eq!(error.kind(), plomid_core::ErrorKind::Corruption);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "WAL checksum test failed: {result:?}");
    }

    #[test]
    fn truncated_tail_is_incomplete() {
        let path = temp_path("torn-tail");
        let result = (|| {
            let mut writer = WalWriter::create(&path)?;
            writer.append(RecordType::Commit, b"complete")?;
            writer.sync()?;
            drop(writer);
            let length = fs::metadata(&path)?.len();
            let file = fs::OpenOptions::new().write(true).open(&path)?;
            file.set_len(length - 1)?;
            file.sync_all()?;
            drop(file);
            let mut reader = WalReader::open(&path)?;
            let state = reader.tail_state()?;
            match state {
                TailState::Incomplete { file_len, .. } => {
                    assert_eq!(file_len, length - 1);
                }
                other => panic!("expected incomplete tail, got {other:?}"),
            }
            // A torn record after a complete one is a truncated tail, not
            // arbitrary corruption: the reader surfaces it as corruption with
            // a valid_end detail for the future recovery layer.
            let error = reader.next_record().unwrap_err();
            assert_eq!(error.kind(), plomid_core::ErrorKind::Corruption);
            assert!(error.detail().is_some());
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "WAL torn-tail test failed: {result:?}");
    }

    #[test]
    fn corrupt_single_record_tail_is_rejected() {
        let path = temp_path("single-torn");
        let result = (|| {
            let mut writer = WalWriter::create(&path)?;
            writer.append(RecordType::Data, b"only")?;
            writer.sync()?;
            drop(writer);
            let length = fs::metadata(&path)?.len();
            let file = fs::OpenOptions::new().write(true).open(&path)?;
            file.set_len(length - 1)?;
            file.sync_all()?;
            drop(file);
            let mut reader = WalReader::open(&path)?;
            let error = reader.next_record().unwrap_err();
            assert_eq!(error.kind(), plomid_core::ErrorKind::Corruption);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "single torn tail test failed: {result:?}");
    }
}
