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
//! Checkpoint-bounded startup recovery.
//!
//! Startup establishes the latest durable storage state in a bounded number of
//! steps: validate physical storage, select and validate the newest usable
//! checkpoint, then replay only the WAL that the checkpoint boundary leaves
//! unreachable.
//!
//! # Recovery boundary
//!
//! `checkpoint_lsn` is the recovery boundary. A checkpoint represents every WAL
//! record whose LSN is at or below that value, so replay begins strictly after
//! it. Records at or below the boundary are still decoded for validation and
//! LSN continuity but are not applied, which keeps recovery idempotent when it
//! runs repeatedly against the same storage image: a transaction the checkpoint
//! already represents cannot be applied a second time.
//!
//! # Crash tail
//!
//! A torn frame at the end of the newest segment is an interrupted, never
//! durable append. Recovery stops at the last complete record, which is the
//! existing WAL tail policy, and reports the safe truncation offset. A torn
//! frame anywhere else, a checksum failure, an unsupported version, a segment
//! sequence gap, or an LSN discontinuity inside the required range is
//! corruption and fails recovery instead of fabricating state.
//!
//! # Bounded work
//!
//! Work is bounded by checkpoint metadata plus the WAL after the boundary.
//! Storage validation reads only structural metadata, and replay reads only the
//! segments that can contain records above the boundary, so startup cost is
//! independent of the number of stored pages and records.

use crate::read_checkpoint_marker;
use crate::recovery::SegmentedReplayTarget;
use crate::WAL_DIR_NAME;
use plomid_core::{ErrorKind, GenerationId, Lsn, PlomidError, Result};
use plomid_storage::checkpoint::{discover, load_checkpoint, validate_storage_physical};
use plomid_storage::{CheckpointMetadata, StorageManager};
use std::path::{Path, PathBuf};

/// Lifecycle of startup recovery.
///
/// Storage becomes externally usable only in [`RecoveryState::Ready`]. A
/// failure at any earlier state returns a structured error and leaves the state
/// machine short of `Ready`, so a caller can never observe a partially
/// recovered store as usable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryState {
    /// Opening the physical storage files.
    Opening,
    /// Validating storage format, identity, and structural metadata.
    Validating,
    /// Locating and validating the newest usable checkpoint.
    LoadingCheckpoint,
    /// Replaying the WAL required after the checkpoint boundary.
    ReplayingWal,
    /// Flushing the reconstructed durable state.
    Reconstructing,
    /// Recovery completed; the store may serve traffic.
    Ready,
}

/// Checkpoint state selected as the recovery boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointSelection {
    /// Path of the selected checkpoint file.
    pub path: PathBuf,
    /// Generation of the selected checkpoint.
    pub checkpoint_generation: GenerationId,
    /// Last WAL LSN already represented by the checkpoint (0 = no checkpoint).
    pub checkpoint_lsn: Lsn,
    /// Durable storage-image generation the checkpoint describes.
    pub storage_generation: GenerationId,
    /// Catalog generation the checkpoint describes.
    pub catalog_generation: plomid_core::CatalogVersion,
}

impl CheckpointSelection {
    /// A selection describing "no checkpoint": the whole WAL is replayed.
    #[must_use]
    pub fn none() -> Self {
        Self {
            path: PathBuf::new(),
            checkpoint_generation: GenerationId::new(0),
            checkpoint_lsn: Lsn::new(0),
            storage_generation: GenerationId::new(0),
            catalog_generation: plomid_core::CatalogVersion::new(0),
        }
    }

    /// Captures the durable identity of a validated checkpoint.
    #[must_use]
    pub fn from_checkpoint(path: &Path, checkpoint: &CheckpointMetadata) -> Self {
        Self {
            path: path.to_path_buf(),
            checkpoint_generation: checkpoint.checkpoint_generation,
            checkpoint_lsn: checkpoint.checkpoint_lsn,
            storage_generation: checkpoint.storage_generation,
            catalog_generation: checkpoint.catalog_generation,
        }
    }

    /// Captures the boundary published by the WAL layer alone.
    ///
    /// Deployments whose catalog lives in the key-value store never publish a
    /// storage checkpoint file (a checkpoint must name a catalog version, and
    /// there is no catalog file to name), yet they still publish the WAL
    /// checkpoint marker. That marker is written under the engine's checkpoint
    /// ordering -- data pages are fsynced first, then the boundary record is
    /// written and fsynced, then the marker is replaced atomically -- so it
    /// proves the same thing about the prefix it covers: every record at or
    /// below it is already represented by durable pages and never needs
    /// replaying.
    ///
    /// Using it as the replay boundary is what keeps restart cost proportional
    /// to the WAL written *since the last checkpoint* instead of to the entire
    /// write history (measured: a 20 000-row table replayed 40 064 records /
    /// 4.3 MiB / 865 ms on every restart before this, and it grew with every
    /// commit ever made).
    ///
    /// `checkpoint_generation` stays 0, so [`Self::is_none`] still reports
    /// "no storage checkpoint": the identity fields describe state this
    /// selection knows nothing about, and only the boundary is authoritative.
    #[must_use]
    pub fn from_wal_marker(boundary: Lsn, storage_generation: GenerationId) -> Self {
        Self {
            path: PathBuf::new(),
            checkpoint_generation: GenerationId::new(0),
            checkpoint_lsn: boundary,
            storage_generation,
            catalog_generation: plomid_core::CatalogVersion::new(0),
        }
    }

    /// Returns `true` when no durable checkpoint was usable.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.checkpoint_generation.get() == 0
    }
}

/// Outcome of a checkpoint-bounded recovery run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointReplayReport {
    /// Checkpoint selected as the replay boundary, if a usable one existed.
    pub checkpoint: Option<CheckpointSelection>,
    /// Durable storage generation validated from structural metadata.
    pub storage_generation: GenerationId,
    /// Durable WAL boundary proven by the WAL checkpoint marker, if any.
    pub durable_lsn: Option<Lsn>,
    /// Number of WAL records applied strictly after the boundary.
    pub applied_records: u64,
    /// Total WAL payload bytes applied strictly after the boundary.
    pub applied_bytes: u64,
    /// Committed transactions applied strictly after the boundary.
    pub applied_transactions: u64,
    /// Storage operations applied strictly after the boundary.
    pub applied_operations: u64,
    /// Last WAL LSN observed during recovery, if any WAL was present.
    pub last_lsn: Option<Lsn>,
    /// Number of WAL segments read during recovery.
    pub segments_read: u64,
    /// Exclusive boundary of an incomplete crash tail, if the newest segment
    /// ended mid-record. Bytes at and beyond this offset were never durable and
    /// are not part of the reconstructed state.
    pub crash_tail_boundary: Option<u64>,
    /// Terminal recovery state (always [`RecoveryState::Ready`] on success).
    pub state: RecoveryState,
}

impl CheckpointReplayReport {
    /// Starts a recovery report in the opening state.
    #[must_use]
    fn initial() -> Self {
        Self {
            checkpoint: None,
            storage_generation: GenerationId::new(0),
            durable_lsn: None,
            applied_records: 0,
            applied_bytes: 0,
            applied_transactions: 0,
            applied_operations: 0,
            last_lsn: None,
            segments_read: 0,
            crash_tail_boundary: None,
            state: RecoveryState::Opening,
        }
    }
}

/// Validates a checkpoint against the recognized durable storage state.
///
/// A checkpoint is usable only when it describes a state the WAL can prove it
/// made durable. Two rules apply:
///
/// * the checkpoint LSN may not exceed the durable WAL boundary, because a
///   checkpoint must never advertise a position the WAL did not persist;
/// * the checkpoint's storage generation may not name an image newer than the
///   one actually present, because that checkpoint belongs to a state this
///   storage image does not contain.
///
/// A checkpoint that names an equal or older storage generation remains usable:
/// replaying the WAL after its boundary brings the image forward from that
/// point, which is exactly the recovery contract.
pub fn validate_checkpoint_against_storage(
    checkpoint: &CheckpointMetadata,
    storage_generation: GenerationId,
    durable: Option<Lsn>,
) -> Result<()> {
    if let Some(durable) = durable {
        checkpoint.check_durable(durable)?;
    }
    if checkpoint.storage_generation.get() > storage_generation.get() {
        return Err(PlomidError::with_detail(
            ErrorKind::Corruption,
            "checkpoint describes a newer storage generation than the storage image",
            format!(
                "checkpoint_storage_generation={} storage_generation={}",
                checkpoint.storage_generation.get(),
                storage_generation.get()
            ),
        ));
    }
    Ok(())
}

/// Returns the durable WAL boundary published by the WAL layer, if any.
///
/// The boundary comes from the WAL checkpoint marker, which the WAL layer
/// publishes atomically only after the records it describes are durable.
pub fn durable_lsn(wal_dir: &Path) -> Result<Option<Lsn>> {
    read_checkpoint_marker(wal_dir)
}

/// Replays the required WAL after the selected checkpoint boundary.
///
/// Only records strictly above `selection.checkpoint_lsn` are applied. Records
/// at or below the boundary are decoded and validated for continuity but never
/// re-applied, so repeated recovery converges on the same durable state.
///
/// Segments that cannot contain a record above the boundary are skipped using
/// their validated headers, so the work done is proportional to the required
/// WAL rather than to the retained WAL.
fn replay_selection(
    wal_dir: &Path,
    selection: &CheckpointSelection,
    target: &mut dyn crate::recovery::ReplayTarget,
) -> Result<RequiredReplay> {
    let manifest = crate::recovery::segment_manifest(wal_dir)?;
    let required = required_segments(&manifest, selection.checkpoint_lsn)?;
    let refs: Vec<&Path> = required.iter().map(PathBuf::as_path).collect();
    target.set_replay_boundary(selection.checkpoint_lsn);
    let outcome = crate::recovery::replay_paths_after(&refs, selection.checkpoint_lsn, target)?;
    Ok(RequiredReplay {
        records_after: outcome.records_after,
        payload_bytes_after: outcome.payload_bytes_after,
        segments_read: refs.len() as u64,
        crash_tail_boundary: outcome.tail.map(|tail| tail.boundary),
        last_lsn: outcome.recovery.last_lsn,
        applied_transactions: outcome.recovery.committed_transactions,
        applied_operations: outcome.recovery.applied_operations,
    })
}
/// Startup recovery that forwards every decoded WAL record to `handler` in
/// deterministic LSN order instead of mutating storage.
///
/// Physical validation, checkpoint selection, segment ordering, and LSN
/// continuity checks are shared with [`recover_into`], so a compatibility
/// observer cannot diverge from the storage recovery boundary. Only records
/// above the checkpoint boundary are reported.
pub fn replay_after_checkpoint(
    storage_root: &Path,
    handler: &mut dyn crate::recovery::ReplayHandler,
) -> Result<CheckpointReplayReport> {
    let mut target = HandlerTarget {
        handler,
        boundary: Lsn::new(0),
    };
    recover_into(storage_root, &mut target)
}

/// Summary of the WAL work required after a checkpoint boundary.
struct RequiredReplay {
    records_after: u64,
    payload_bytes_after: u64,
    segments_read: u64,
    crash_tail_boundary: Option<u64>,
    last_lsn: Option<Lsn>,
    applied_transactions: u64,
    applied_operations: u64,
}

/// Returns the retained segments that can contain a record above `boundary`.
///
/// Records are contiguous across segments, so a segment is unnecessary exactly
/// when the next retained segment starts at or below `boundary + 1`: every LSN
/// in that segment would then be at or below the boundary. Only the 32-byte
/// validated segment header of the following segment is read, so the decision
/// costs a bounded amount of I/O per segment rather than a payload scan.
fn required_segments(manifest: &[(u64, PathBuf)], boundary: Lsn) -> Result<Vec<PathBuf>> {
    if boundary.get() == 0 {
        return Ok(manifest.iter().map(|(_, path)| path.clone()).collect());
    }
    let mut first_required = 0_usize;
    for index in 1..manifest.len() {
        let (_, path) = &manifest[index];
        let mut bytes = [0_u8; crate::segmented::SegmentHeaderSize];
        if !read_exact_at(path, &mut bytes)? {
            break;
        }
        let header = crate::segmented::SegmentHeader::decode(&bytes)?;
        let start = header.first_lsn.get();
        if start > 0 && start <= boundary.get().saturating_add(1) {
            first_required = index;
        } else {
            break;
        }
    }
    Ok(manifest[first_required..]
        .iter()
        .map(|(_, path)| path.clone())
        .collect())
}

/// Reads exactly the first `buffer.len()` bytes of `path`.
///
/// Returns `Ok(false)` when the file is shorter than the buffer, which is a
/// recoverable observation for a segment that is still being created.
fn read_exact_at(path: &Path, buffer: &mut [u8]) -> Result<bool> {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(PlomidError::from(error)),
    };
    let mut filled = 0_usize;
    while filled < buffer.len() {
        let read = file
            .read(&mut buffer[filled..])
            .map_err(PlomidError::from)?;
        if read == 0 {
            return Ok(false);
        }
        filled += read;
    }
    Ok(true)
}

/// Validates the WAL environment required for recovery and returns the durable
/// boundary the WAL layer has published, if any.
///
/// A missing WAL directory is created for a storage image that has no WAL yet,
/// which is the state of a freshly created store. A present WAL directory must
/// expose contiguous retained segments, because a gap means a durable region
/// needed for recovery was lost rather than reclaimed.
pub fn validate_wal_environment(storage_root: &Path) -> Result<(PathBuf, Option<Lsn>)> {
    let wal_dir = storage_root.join(WAL_DIR_NAME);
    if !wal_dir.exists() {
        std::fs::create_dir_all(&wal_dir).map_err(PlomidError::from)?;
        return Ok((wal_dir, None));
    }
    crate::recovery::segment_manifest(&wal_dir)?;
    let durable = durable_lsn(&wal_dir)?;
    Ok((wal_dir, durable))
}

/// Selects the newest usable checkpoint for a storage root.
///
/// Candidates are enumerated in deterministic generation order and validated in
/// descending generation order. A candidate is usable only when its contents
/// validate, its stored generation matches its file name, and its metadata is
/// consistent with the recognized durable storage state. A malformed candidate
/// is skipped so that an older usable checkpoint can still define the boundary,
/// and a candidate whose generation merely happens to be highest is never
/// trusted on that basis alone.
pub fn select_checkpoint(
    storage_root: &Path,
    storage_generation: GenerationId,
    durable: Option<Lsn>,
) -> Result<Option<(PathBuf, CheckpointMetadata)>> {
    let candidates = discover(storage_root)?;
    let mut newest: Option<(PathBuf, CheckpointMetadata)> = None;
    for path in candidates {
        let metadata = match load_checkpoint(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let name_generation = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(plomid_storage::checkpoint::generation_from_file_name);
        if name_generation != Some(metadata.checkpoint_generation.get()) {
            continue;
        }
        if validate_checkpoint_against_storage(&metadata, storage_generation, durable).is_err() {
            continue;
        }
        let is_newer = newest.as_ref().is_none_or(|(_, current)| {
            metadata.checkpoint_generation.get() > current.checkpoint_generation.get()
        });
        if is_newer {
            newest = Some((path, metadata));
        }
    }
    Ok(newest)
}

/// Startup recovery: physical validation, checkpoint selection, bounded WAL
/// replay, and durable-state reconstruction.
///
/// The lifecycle advances strictly in order and returns `Ready` only after the
/// reconstructed state has been made durable. Any failure returns a structured
/// error before `Ready`, so a caller can never observe a partially recovered
/// store as usable.
///
/// Recovery is deterministic: candidate checkpoints are ordered by generation,
/// segments by validated sequence, and records by LSN, so timestamps, directory
/// enumeration order, and hash iteration never influence the outcome.
///
/// `storage` is synced once replay reaches the reconstructed durable state.
pub fn recover_storage(
    storage_root: &Path,
    storage: &mut StorageManager,
) -> Result<CheckpointReplayReport> {
    let mut target = SegmentedReplayTarget(storage);
    let report = recover_into(storage_root, &mut target)?;
    target.0.sync()?;
    Ok(report)
}

/// Read-only [`ReplayTarget`] adapter that forwards records to a
/// [`ReplayHandler`] without mutating storage.
///
/// Compatibility observers (for example the checkpoint integration test
/// counter) only implement `on_record`, while storage appliers only implement
/// the put/delete surface. This adapter lets both share one
/// transaction-aware replay implementation, so the legacy callback contract
/// and the storage surface never diverge into duplicate WAL readers.
struct HandlerTarget<'a> {
    handler: &'a mut dyn crate::recovery::ReplayHandler,
    boundary: Lsn,
}

impl crate::recovery::ReplayHandler for HandlerTarget<'_> {
    fn on_record(&mut self, record: &crate::Record) -> Result<bool> {
        if record.lsn.get() <= self.boundary.get() {
            // The checkpoint already represents this prefix; it is decoded for
            // validation only and is not reported as required replay work.
            return Ok(true);
        }
        self.handler.on_record(record)
    }
}

impl crate::recovery::ReplayTarget for HandlerTarget<'_> {
    fn apply_put(&mut self, _key: &[u8], _value: &[u8]) -> Result<()> {
        Ok(())
    }

    fn apply_delete(&mut self, _key: &[u8]) -> Result<()> {
        Ok(())
    }

    fn apply_sync(&mut self) -> Result<()> {
        Ok(())
    }

    fn set_replay_boundary(&mut self, boundary: Lsn) {
        self.boundary = boundary;
    }
}

/// Startup recovery against an arbitrary replay target.
///
/// This is the same path as [`recover`] with the storage applier supplied by the
/// caller, which lets a compatibility target reuse one recovery implementation
/// instead of a second copy of the boundary and validation rules.
pub fn recover_into(
    storage_root: &Path,
    target: &mut dyn crate::recovery::ReplayTarget,
) -> Result<CheckpointReplayReport> {
    let mut report = CheckpointReplayReport::initial();
    let storage_generation = validate_storage_physical(storage_root)?;
    report.storage_generation = storage_generation;
    report.state = RecoveryState::Validating;
    let (wal_dir, durable) = validate_wal_environment(storage_root)?;
    report.durable_lsn = durable;
    report.state = RecoveryState::LoadingCheckpoint;
    let selected = select_checkpoint(storage_root, storage_generation, durable)?;
    let selection = match &selected {
        Some((path, metadata)) => CheckpointSelection::from_checkpoint(path, metadata),
        // No storage checkpoint to trust. If the WAL layer published a
        // checkpoint boundary, that marker is the same kind of proof for the
        // records it covers (pages fsynced before the marker was published), so
        // replay starts above it instead of re-reading the entire history. When
        // no marker and no checkpoint exist there is nothing to bound replay:
        // the whole WAL is replayed, exactly as before.
        None => match durable {
            Some(boundary) if boundary.get() > 0 => {
                CheckpointSelection::from_wal_marker(boundary, storage_generation)
            }
            _ => CheckpointSelection::none(),
        },
    };
    report.checkpoint = Some(selection.clone());
    report.state = RecoveryState::ReplayingWal;
    let outcome = replay_selection(&wal_dir, &selection, target)?;
    report.state = RecoveryState::Reconstructing;
    report.applied_records = outcome.records_after;
    report.applied_bytes = outcome.payload_bytes_after;
    report.applied_transactions = outcome.applied_transactions;
    report.applied_operations = outcome.applied_operations;
    report.segments_read = outcome.segments_read;
    report.crash_tail_boundary = outcome.crash_tail_boundary;
    report.last_lsn = outcome.last_lsn;
    report.state = RecoveryState::Ready;
    Ok(report)
}
