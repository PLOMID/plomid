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
//! Real-file WAL writer with explicit durability boundaries.
//!
//! # Ordering model
//!
//! * `append` reserves the next LSN and writes the complete frame to the file.
//!   It makes the record available to sequential readers but claims nothing
//!   about media persistence.
//! * `flush` pushes buffered data through the write-buffering layer without
//!   claiming durability on the storage medium.
//! * `sync` (and `commit` in [`DurabilityMode::Force`]) issues the filesystem
//!   fsync that forms the durability boundary. Higher layers must order
//!   `append WAL -> sync WAL -> write data -> sync data`; this writer never
//!   reports durability after a failed fsync.
//!
//! # LSN rule
//!
//! LSNs start at 1, increase by exactly one per appended record, never repeat,
//! never go backwards, and never wrap: allocation at `u64::MAX` fails
//! structurally instead of overflowing. The ordering is pinned to the physical
//! frame write order under the writer lock, so persisted order always matches
//! LSN order.

use crate::format::{encode, Record, RecordType};
use crate::segmented::{SegmentHeader, SEGMENT_HEADER_SIZE};
use plomid_core::{ErrorKind, Lsn, ObjectId, PlomidError, Result, TxId};
use plomid_storage::{FileSystem, RealFs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Defined once in `plomid_core::constants`.
pub use plomid_core::LOCATION_SIZE;

/// Physical placement of an appended record within the WAL.
///
/// Deterministically little-endian encoded: `sequence[u64] | offset[u64]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppendLocation {
    /// Segment sequence number (1-based) that holds the record.
    pub sequence: u64,
    /// Byte offset of the record's frame within that segment.
    pub offset: u64,
}

impl AppendLocation {
    /// Encodes the location deterministically.
    #[must_use]
    pub fn encode(self) -> [u8; LOCATION_SIZE] {
        let mut b = [0_u8; LOCATION_SIZE];
        b[0..8].copy_from_slice(&self.sequence.to_le_bytes());
        b[8..16].copy_from_slice(&self.offset.to_le_bytes());
        b
    }

    /// Decodes a little-endian location, rejecting a zero sequence.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != LOCATION_SIZE {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "WAL location is not 16 bytes",
            ));
        }
        let sequence = u64::from_le_bytes(
            bytes[0..8]
                .try_into()
                .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid WAL location"))?,
        );
        if sequence == 0 {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "WAL location has a zero sequence",
            ));
        }
        let offset = u64::from_le_bytes(
            bytes[8..16]
                .try_into()
                .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid WAL location"))?,
        );
        Ok(Self { sequence, offset })
    }
}

/// Durability policy for the writer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurabilityMode {
    /// Fsync before `commit` returns and before `sync` returns. Production-safe.
    Force,
    /// `commit` returns without fsync. Tests / cache-loss-tolerant workloads
    /// only; does not claim durability. `sync` still fsyncs.
    Buffered,
}
/// Thread-safe shared WAL handle.
///
/// Clones share one writer. Concurrent `append` calls never interleave
/// framing because LSN allocation and the file offset advance under a single
/// mutex; a record only becomes visible to a reader as one complete frame.
#[derive(Clone)]
pub struct SharedWal {
    inner: Arc<Mutex<WalWriter>>,
}

impl SharedWal {
    /// Wraps an existing writer in a shareable handle.
    #[must_use]
    pub fn new(writer: WalWriter) -> Self {
        Self {
            inner: Arc::new(Mutex::new(writer)),
        }
    }

    /// Creates a fresh single-segment writer behind a shareable handle.
    pub fn create(path: &Path) -> Result<Self> {
        Ok(Self::new(WalWriter::create(path)?))
    }

    fn lock(&self) -> Result<MutexGuard<'_, WalWriter>> {
        self.inner
            .lock()
            .map_err(|_| PlomidError::new(ErrorKind::Internal, "WAL writer lock is poisoned"))
    }

    /// Appends one record; returns its assigned LSN.
    pub fn append(&self, record_type: RecordType, payload: &[u8]) -> Result<Lsn> {
        self.lock()?.append(record_type, payload)
    }

    /// Appends a record carrying explicit transaction and object identity.
    pub fn append_with_ids(
        &self,
        record_type: RecordType,
        tx_id: TxId,
        object_id: ObjectId,
        payload: &[u8],
    ) -> Result<Lsn> {
        self.lock()?
            .append_with_ids(record_type, tx_id, object_id, payload)
    }

    /// Appends a batch; commit the highest LSN once for a group barrier.
    pub fn append_batch(&self, records: &[(RecordType, &[u8])]) -> Result<Vec<Lsn>> {
        self.lock()?.append_batch(records)
    }

    /// Group-commit barrier: makes every record through `lsn` durable.
    pub fn commit(&self, lsn: Lsn) -> Result<()> {
        self.lock()?.commit(lsn)
    }

    /// Pushes buffered data without claiming media persistence.
    pub fn flush(&self) -> Result<()> {
        self.lock()?.flush()
    }

    /// Flushes and fsyncs the complete segment (the durability boundary).
    pub fn sync(&self) -> Result<()> {
        self.lock()?.sync()
    }

    /// Returns the configured durability mode.
    #[must_use]
    pub fn durability_mode(&self) -> DurabilityMode {
        self.inner
            .lock()
            .map(|writer| writer.durability_mode())
            .unwrap_or(DurabilityMode::Force)
    }

    /// Returns the next LSN that will be assigned.
    #[must_use]
    pub fn next_lsn(&self) -> Lsn {
        self.inner
            .lock()
            .map(|writer| writer.next_lsn())
            .unwrap_or(Lsn::new(1))
    }
}

/// A single open WAL segment file on disk.
///
/// A published segment always starts with a versioned [`SegmentHeader`]. A file
/// whose header is missing or incomplete is an unpublished segment — the header
/// is written before any record, so such a file carries no durable record — and
/// is read from offset zero without a header. The reader validates every record
/// frame it decodes, so an unpublished file can never contribute records that
/// were not written as complete frames.
pub struct WalWriter {
    path: PathBuf,
    fs: RealFs,
    file: <RealFs as FileSystem>::File,
    sequence: u64,
    offset: u64,
    next_lsn: Lsn,
    durable_lsn: Lsn,
    mode: DurabilityMode,
    #[cfg(test)]
    inject: TestInject,
}

/// Test-only fault injection used by durability-failure tests. This is
/// compiled in only under `cfg(test)`; production builds contain no injection
/// path or branch.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TestInject {
    /// No injected failure.
    None,
    /// Fail the next `append` with an I/O error before any frame is written.
    FailAppend,
    /// Fail the next `flush` with an I/O error.
    FailFlush,
    /// Fail the next `sync`/`commit` fsync with an I/O error.
    FailSync,
}

impl WalWriter {
    /// Creates or truncates a WAL segment (sequence 1, first LSN 1).
    pub fn create(path: &Path) -> Result<Self> {
        Self::create_with_mode(path, DurabilityMode::Force)
    }

    /// Creates or truncates a WAL segment with an explicit durability mode.
    pub fn create_with_mode(path: &Path, mode: DurabilityMode) -> Result<Self> {
        Self::create_segment(path, mode, 1, Lsn::new(1))
    }

    /// Creates a segment `sequence` whose first record starts at `first_lsn`.
    pub(crate) fn create_segment(
        path: &Path,
        mode: DurabilityMode,
        sequence: u64,
        first_lsn: Lsn,
    ) -> Result<Self> {
        tracing::trace!(target: "wal", "create_segment path={} seq={} mode={:?}", path.display(), sequence, mode);
        let fs = RealFs;
        let mut file = fs.create(path)?;
        let header = SegmentHeader {
            sequence,
            first_lsn,
        };
        let frame = header.encode();
        let mut written = 0;
        while written < frame.len() {
            let count = fs.write_at(&mut file, written as u64, &frame[written..])?;
            if count == 0 {
                return Err(PlomidError::new(
                    ErrorKind::Io,
                    "WAL header accepted no bytes",
                ));
            }
            written += count;
        }
        let offset = SEGMENT_HEADER_SIZE as u64;
        Ok(Self {
            path: path.to_path_buf(),
            fs,
            file,
            sequence,
            offset,
            next_lsn: first_lsn,
            durable_lsn: Lsn::new(first_lsn.get().saturating_sub(1)),
            mode,
            #[cfg(test)]
            inject: TestInject::None,
        })
    }

    /// Opens an existing WAL segment and continues after its valid records.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_mode(path, DurabilityMode::Force)
    }

    /// Opens an existing WAL segment with an explicit durability mode.
    pub fn open_with_mode(path: &Path, mode: DurabilityMode) -> Result<Self> {
        tracing::trace!(target: "wal", "open path={} mode={:?}", path.display(), mode);
        let mut reader = crate::WalReader::open(path)?;
        let header = reader.segment_header();
        let mut last = None;
        while let Some(record) = reader.next_record()? {
            last = Some(record.lsn);
        }
        let fs = RealFs;
        let file = fs.open(path)?;
        let offset = reader.offset();
        let sequence = header.map_or(1, |h| h.sequence);
        let next_lsn = last
            .map(|lsn| {
                lsn.get()
                    .checked_add(1)
                    .map(Lsn::new)
                    .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "WAL LSN exhausted"))
            })
            .transpose()?
            .unwrap_or_else(|| header.map_or_else(|| Lsn::new(1), |h| h.first_lsn));
        tracing::debug!(target: "wal", "opened path={} seq={} next_lsn={}", path.display(), sequence, next_lsn.get());
        Ok(Self {
            path: path.to_path_buf(),
            fs,
            file,
            sequence,
            offset,
            next_lsn,
            durable_lsn: last.unwrap_or_else(|| Lsn::new(0)),
            mode,
            #[cfg(test)]
            inject: TestInject::None,
        })
    }

    /// Returns the configured durability mode.
    #[must_use]
    pub fn durability_mode(&self) -> DurabilityMode {
        self.mode
    }

    /// Returns the segment sequence number.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the filesystem path of this segment.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the next LSN that will be assigned.
    #[must_use]
    pub fn next_lsn(&self) -> Lsn {
        self.next_lsn
    }

    /// Returns the current write offset (bytes already written, header + records).
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the highest LSN known durable after the last sync/commit.
    #[must_use]
    pub fn durable_lsn(&self) -> Lsn {
        self.durable_lsn
    }
    /// Appends a batch; commit the highest returned LSN once for a group
    /// durability barrier.
    pub fn append_batch(&mut self, records: &[(RecordType, &[u8])]) -> Result<Vec<Lsn>> {
        records
            .iter()
            .map(|(record_type, payload)| self.append(*record_type, payload))
            .collect()
    }

    /// Advances the durable watermark to `lsn` without issuing an fsync.
    ///
    /// This is the bookkeeping half of [`Self::commit`], split out so a group
    /// durability coordinator can make the records durable through its own
    /// handle to the same segment file and then publish the watermark. The
    /// watermark only ever moves forward and never past the last appended
    /// record, so a caller must have actually flushed the file through some
    /// handle before calling this.
    pub fn mark_durable(&mut self, lsn: Lsn) {
        if lsn > self.durable_lsn && lsn < self.next_lsn {
            self.durable_lsn = lsn;
        }
    }

    /// Appends one record and returns its assigned LSN.
    pub fn append(&mut self, record_type: RecordType, payload: &[u8]) -> Result<Lsn> {
        self.append_with_ids(record_type, TxId::new(0), ObjectId::new(0), payload)
    }

    /// Appends one record carrying explicit transaction and object identity.
    pub fn append_with_ids(
        &mut self,
        record_type: RecordType,
        tx_id: TxId,
        object_id: ObjectId,
        payload: &[u8],
    ) -> Result<Lsn> {
        #[cfg(test)]
        {
            if self.inject == TestInject::FailAppend {
                return Err(injected_io_error("append"));
            }
        }
        if self.next_lsn.get() == u64::MAX {
            return Err(PlomidError::new(ErrorKind::Internal, "WAL LSN exhausted"));
        }
        let lsn = self.next_lsn;
        let record = Record {
            lsn,
            record_type,
            tx_id,
            object_id,
            payload: payload.to_vec(),
        };
        let bytes = encode(&record)?;
        let mut written = 0;
        while written < bytes.len() {
            let count = self.fs.write_at(
                &mut self.file,
                self.offset + written as u64,
                &bytes[written..],
            )?;
            if count == 0 {
                tracing::error!(target: "wal", "append_failed lsn={} record_type={:?} zero_bytes", lsn.get(), record_type);
                return Err(PlomidError::new(
                    ErrorKind::Io,
                    "WAL filesystem accepted no bytes",
                ));
            }
            written += count;
        }
        self.offset += written as u64;
        self.next_lsn = self
            .next_lsn
            .get()
            .checked_add(1)
            .map(Lsn::new)
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "WAL LSN exhausted"))?;
        tracing::trace!(target: "wal", "append lsn={} record_type={:?}", lsn.get(), record_type);
        Ok(lsn)
    }

    /// Appends a fully formed record, assigning it a fresh LSN.
    ///
    /// `append(record)`-style entry point: the caller supplies type, tx, object
    /// and payload; the writer allocates the LSN, serializes deterministically,
    /// checksums, places the frame, and advances WAL state. The input record's
    /// `lsn` field is ignored.
    pub fn append_record(&mut self, record: &Record) -> Result<Lsn> {
        self.append_with_ids(
            record.record_type,
            record.tx_id,
            record.object_id,
            &record.payload,
        )
    }

    /// Group-commit barrier: makes every record through `lsn` durable.
    ///
    /// In [`DurabilityMode::Force`], one fsync makes the whole batch durable
    /// together, so transactions A/B/C appended sequentially become durable in
    /// one durability operation. A commit for LSN `lsn` never reports
    /// durability for records beyond it, and never reports success after a
    /// failed fsync (the durable watermark is only advanced on success).
    pub fn commit(&mut self, lsn: Lsn) -> Result<()> {
        if lsn.get() == 0 || lsn.get() >= self.next_lsn.get() {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "WAL commit LSN is not appended",
            ));
        }
        if self.mode == DurabilityMode::Force && lsn > self.durable_lsn {
            #[cfg(test)]
            if self.inject == TestInject::FailSync {
                return Err(injected_io_error("commit fsync"));
            }
            self.fs.fsync(&mut self.file)?;
            self.durable_lsn = lsn;
            tracing::debug!(target: "wal", "commit_fsync lsn={}", lsn.get());
        }
        Ok(())
    }

    /// Pushes buffered data through the buffering layer.
    ///
    /// `flush` alone does not guarantee media persistence; follow it with
    /// [`Self::sync`] (or rely on [`Self::commit`] in
    /// [`DurabilityMode::Force`]) where durability is required.
    pub fn flush(&mut self) -> Result<()> {
        #[cfg(test)]
        {
            if self.inject == TestInject::FailFlush {
                return Err(injected_io_error("flush"));
            }
        }
        // RealFs exposes no buffering flush hook of its own; the write path is
        // unbuffered at this layer. This method is the explicit ordering point
        // so higher layers can push writer-side buffers before sync.
        Ok(())
    }

    /// Flushes the complete segment to durable storage regardless of mode.
    ///
    /// On success the durable watermark advances to the last appended record
    /// and the WAL-before-data invariant guarantees every prior record is
    /// durable before any dependent data write may be reported.
    pub fn sync(&mut self) -> Result<()> {
        #[cfg(test)]
        {
            if self.inject == TestInject::FailSync {
                return Err(injected_io_error("sync"));
            }
        }
        self.fs.fsync(&mut self.file)?;
        self.durable_lsn = self
            .next_lsn
            .get()
            .checked_sub(1)
            .map(Lsn::new)
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "invalid WAL LSN"))?;
        tracing::debug!(target: "wal", "sync complete durable_lsn={}", self.durable_lsn.get());
        Ok(())
    }

    /// Test-only hook: arm the next operation to fail under `cfg(test)`.
    #[cfg(test)]
    pub(crate) fn set_inject(&mut self, inject: TestInject) {
        self.inject = inject;
    }
}

/// Test-only helper producing a deterministic I/O error for injected faults.
#[cfg(test)]
fn injected_io_error(op: &str) -> PlomidError {
    PlomidError::with_source(
        ErrorKind::Io,
        format!("injected {op} failure"),
        std::io::Error::new(std::io::ErrorKind::Other, format!("injected {op} failure")),
    )
}

#[cfg(test)]
mod tests {
    use super::{AppendLocation, DurabilityMode, TestInject, WalWriter};
    use crate::{RecordType, SharedWal, WalReader};
    use plomid_core::ErrorKind;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn temp_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "plomid-wal-group-{label}-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn batch_commit_makes_all_records_readable_after_reopen() {
        let path = temp_path("durable");
        let result = (|| {
            let mut writer = WalWriter::create(&path)?;
            let lsns = writer.append_batch(&[
                (RecordType::Begin, b"begin"),
                (RecordType::Data, b"change"),
                (RecordType::Commit, b"commit"),
            ])?;
            assert_eq!(
                lsns,
                vec![
                    plomid_core::Lsn::new(1),
                    plomid_core::Lsn::new(2),
                    plomid_core::Lsn::new(3),
                ]
            );
            assert_eq!(writer.durability_mode(), DurabilityMode::Force);
            writer.commit(*lsns.last().ok_or_else(|| {
                plomid_core::PlomidError::new(
                    plomid_core::ErrorKind::InvalidArgument,
                    "empty WAL batch",
                )
            })?)?;
            drop(writer);

            let mut reader = WalReader::open(&path)?;
            assert_eq!(
                reader.next_record()?.map(|record| record.payload),
                Some(b"begin".to_vec())
            );
            assert_eq!(
                reader.next_record()?.map(|record| record.payload),
                Some(b"change".to_vec())
            );
            assert_eq!(
                reader.next_record()?.map(|record| record.payload),
                Some(b"commit".to_vec())
            );
            assert!(reader.next_record()?.is_none());
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "group commit test failed: {result:?}");
    }

    #[test]
    fn segment_header_is_written_and_validated() {
        let path = temp_path("header");
        let result = (|| {
            let mut writer = WalWriter::create(&path)?;
            writer.append(RecordType::Data, b"payload")?;
            writer.sync()?;
            drop(writer);
            let mut reader = WalReader::open(&path)?;
            let header = reader.segment_header().ok_or_else(|| {
                plomid_core::PlomidError::new(
                    plomid_core::ErrorKind::Corruption,
                    "missing segment header",
                )
            })?;
            assert_eq!(header.sequence, 1);
            assert_eq!(header.first_lsn.get(), 1);
            let record = reader.next_record()?.ok_or_else(|| {
                plomid_core::PlomidError::new(plomid_core::ErrorKind::Corruption, "missing record")
            })?;
            assert_eq!(record.lsn.get(), 1);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "segment header test failed: {result:?}");
    }

    #[test]
    fn buffered_mode_is_explicit_and_sync_still_forces_durability() {
        let path = temp_path("buffered");
        let result = (|| {
            let mut writer = WalWriter::create_with_mode(&path, DurabilityMode::Buffered)?;
            let lsn = writer.append(RecordType::Data, b"buffered")?;
            writer.commit(lsn)?;
            writer.sync()?;
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "buffered mode test failed: {result:?}");
    }

    #[test]
    fn shared_wal_concurrent_append_produces_contiguous_lsns() {
        let path = temp_path("shared");
        let result = (|| {
            let wal = SharedWal::create(&path)?;
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let wal = wal.clone();
                    std::thread::spawn(move || {
                        for i in 0..50 {
                            wal.append(RecordType::Data, format!("rec-{i}").as_bytes())
                                .unwrap();
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
            wal.sync()?;
            drop(wal);
            let mut reader = WalReader::open(&path)?;
            let mut expected = 1_u64;
            while let Some(record) = reader.next_record()? {
                assert_eq!(record.lsn.get(), expected, "LSN must be contiguous");
                expected += 1;
            }
            assert_eq!(expected, 201, "expected 200 records");
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "shared concurrent test failed: {result:?}");
    }

    #[test]
    fn append_location_round_trip() {
        let loc = AppendLocation {
            sequence: 7,
            offset: 12345,
        };
        assert_eq!(AppendLocation::decode(&loc.encode()).unwrap(), loc);
    }
    #[test]
    fn append_failure_does_not_advance_lsn_or_report_durability() {
        let path = temp_path("append-fail");
        let result = (|| {
            let mut writer = WalWriter::create(&path)?;
            writer.set_inject(TestInject::FailAppend);
            let error = writer
                .append(RecordType::Data, b"will-fail")
                .expect_err("injected append must fail");
            assert_eq!(error.kind(), ErrorKind::Io);
            // LSN and durable watermark are unchanged.
            assert_eq!(writer.next_lsn().get(), 1);
            assert_eq!(writer.durable_lsn().get(), 0);
            // The writer may still operate after the transient failure.
            writer.set_inject(TestInject::None);
            let lsn = writer.append(RecordType::Data, b"ok")?;
            writer.commit(lsn)?;
            assert_eq!(writer.durable_lsn().get(), 1);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "append failure test failed: {result:?}");
    }

    #[test]
    fn sync_failure_is_not_reported_as_durable() {
        let path = temp_path("sync-fail");
        let result = (|| {
            let mut writer = WalWriter::create(&path)?;
            writer.append(RecordType::Data, b"pending")?;
            writer.set_inject(TestInject::FailSync);
            let error = writer.sync().expect_err("injected sync must fail");
            assert_eq!(error.kind(), ErrorKind::Io);
            // The record was written but must NOT be reported durable.
            assert_eq!(writer.durable_lsn().get(), 0);
            // Clearing the fault and syncing again makes it durable.
            writer.set_inject(TestInject::None);
            writer.sync()?;
            assert_eq!(writer.durable_lsn().get(), 1);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "sync failure test failed: {result:?}");
    }

    #[test]
    fn commit_fsync_failure_does_not_advance_durable_watermark() {
        let path = temp_path("commit-fail");
        let result = (|| {
            let mut writer = WalWriter::create(&path)?;
            let lsn = writer.append(RecordType::Data, b"pending")?;
            writer.set_inject(TestInject::FailSync);
            let error = writer.commit(lsn).expect_err("injected commit must fail");
            assert_eq!(error.kind(), ErrorKind::Io);
            assert_eq!(writer.durable_lsn().get(), 0);
            writer.set_inject(TestInject::None);
            writer.commit(lsn)?;
            assert_eq!(writer.durable_lsn().get(), 1);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "commit failure test failed: {result:?}");
    }

    #[test]
    fn flush_failure_propagates_in_buffered_mode() {
        let path = temp_path("flush-fail");
        let result = (|| {
            let mut writer = WalWriter::create_with_mode(&path, DurabilityMode::Buffered)?;
            writer.set_inject(TestInject::FailFlush);
            let error = writer.flush().expect_err("injected flush must fail");
            assert_eq!(error.kind(), ErrorKind::Io);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "flush failure test failed: {result:?}");
    }
}
