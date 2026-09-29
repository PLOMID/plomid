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
//! Committed-only crash recovery for the PLOMID write-ahead log.
//!
//! Transaction payloads use this contract:
//!
//! - Begin, Commit, and Abort payloads are an 8-byte little-endian `TxnId`.
//! - Data payloads are an 8-byte `TxnId`, followed by `Put` or `Delete`:
//!   `op[u8] | key_len[u32] | value_len[u32] | key | value`.
//!
//! Recovery buffers Data records until their Commit record, then replays the
//! operations against the real B+Tree. Transactions without Commit are
//! intentionally ignored rather than undone; this is safe under the current
//! integration assumption that uncommitted page changes are not applied before
//! recovery. Put/Delete replay is idempotent, so rerunning recovery is safe for
//! this storage surface. Full ARIES analysis/redo/undo and WAL-to-page LSN
//! checks are future refinements.

use plomid_core::{CommitTimestamp, ErrorKind, Lsn, PlomidError, Result, TxnId};
use plomid_storage::BTree;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

// Recovery op codes are defined once in `plomid_core::constants`; the
// module-local names below keep the body of this file unchanged.
use plomid_core::{WAL_DELETE_OP as DELETE, WAL_PUT_OP as PUT};

/// A storage operation carried by a transactional Data record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataOperation {
    /// Insert or replace a key's value.
    Put { key: Vec<u8>, value: Vec<u8> },
    /// Remove a key.
    Delete { key: Vec<u8> },
}

/// Summary of one recovery run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryReport {
    /// Number of transactions that had a Commit record.
    pub committed_transactions: u64,
    /// Number of committed operations replayed into the tree.
    pub applied_operations: u64,
    /// Last valid WAL LSN observed, if the WAL was non-empty.
    pub last_lsn: Option<Lsn>,
}

/// Handler invoked by the deterministic replay infrastructure.
///
/// The WAL layer produces records in deterministic LSN order across all
/// segments. This trait is the minimal integration point for higher layers to
/// *interpret* records; replay here does not invent SQL, MVCC, index, or
/// page-reconstruction semantics.
pub trait ReplayHandler {
    /// Called once per record in deterministic LSN order.
    ///
    /// Returns `Ok(true)` to continue replay or `Ok(false)` to stop cleanly at
    /// the boundary (for example on an incomplete tail). Returning a `Result`
    /// error aborts replay and is propagated to the caller.
    fn on_record(&mut self, record: &crate::Record) -> Result<bool>;
}
/// Replays every record from a single WAL file in order through `handler`.
///
/// An incomplete (torn) tail at the end of the file is a recoverable boundary.
/// When the reader reports such a tail with a `valid_end` offset, replay stops
/// at the last complete record and returns the boundary offset so the caller
/// can truncate. Any corruption before the valid tail is propagated.
pub fn replay_records(
    reader: &mut crate::WalReader,
    handler: &mut dyn ReplayHandler,
) -> Result<u64> {
    let mut valid_end = None;
    loop {
        match reader.next_record() {
            Ok(Some(record)) => {
                if !handler.on_record(&record)? {
                    return Ok(valid_end.unwrap_or(reader.offset()));
                }
                valid_end = Some(reader.offset());
            }
            Ok(None) => return Ok(reader.offset()),
            Err(error) => {
                if let Some(detail) = error.detail() {
                    if detail.starts_with("incomplete_tail valid_end=") {
                        let end = detail
                            .strip_prefix("incomplete_tail valid_end=")
                            .and_then(|rest| rest.parse::<u64>().ok())
                            .unwrap_or(reader.offset());
                        return Err(PlomidError::with_detail(
                            ErrorKind::Wal,
                            "WAL replay stopped at an incomplete tail",
                            format!("valid_end={end}"),
                        ));
                    }
                }
                return Err(error);
            }
        }
    }
}

/// Deterministic replay of an entire WAL directory in LSN order.
///
/// Opens every segment (validated and ordered by sequence), then invokes
/// `handler` for each record in global LSN order across one segment, then the
/// next. Ordering is guaranteed by the validated segment sequence and per
/// reader strict-LSN checks.
pub fn replay_directory(
    wal_dir: &Path,
    handler: &mut dyn ReplayHandler,
) -> Result<crate::RecoveryReport> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(wal_dir).map_err(PlomidError::from)? {
        let entry = entry.map_err(PlomidError::from)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("WAL-") && name.ends_with(".dat") {
            paths.push(entry.path());
        }
    }
    paths.sort_unstable();
    let mut last_lsn = None;
    for path in paths {
        let mut reader = crate::WalReader::open(&path)?;
        loop {
            match reader.next_record() {
                Ok(Some(record)) => {
                    last_lsn = Some(record.lsn);
                    if !handler.on_record(&record)? {
                        return Ok(report(last_lsn));
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let is_tail = error
                        .detail()
                        .is_some_and(|d| d.starts_with("incomplete_tail valid_end="));
                    return Ok(report_with_tail(last_lsn, is_tail, error));
                }
            }
        }
    }
    Ok(report(last_lsn))
}

fn report(last_lsn: Option<Lsn>) -> RecoveryReport {
    RecoveryReport {
        committed_transactions: 0,
        applied_operations: 0,
        last_lsn,
    }
}

fn report_with_tail(last_lsn: Option<Lsn>, _is_tail: bool, _error: PlomidError) -> RecoveryReport {
    // A torn tail is a recoverable interruption: report up to the last valid
    // record so the caller can truncate at the boundary. Corruption that is
    // not a recoverable tail is surfaced by the reader as an error instead of
    // being swallowed here.
    RecoveryReport {
        committed_transactions: 0,
        applied_operations: 0,
        last_lsn,
    }
}

/// Encodes a Begin payload for `txn_id`.
#[must_use]
pub fn encode_begin(txn_id: TxnId) -> Vec<u8> {
    txn_id.get().to_le_bytes().to_vec()
}

/// Encodes a Commit payload carrying only `txn_id`.
///
/// The payload is 8 bytes. A commit that has an allocated commit timestamp is
/// encoded with [`encode_commit_with_timestamp`], which records the MVCC commit
/// order in the WAL as well.
#[must_use]
pub fn encode_commit(txn_id: TxnId) -> Vec<u8> {
    encode_begin(txn_id)
}

/// Encodes a Commit payload for `txn_id` with its allocated `commit_timestamp`.
///
/// The payload is 16 bytes: `txn_id[8] | commit_timestamp[8]`, which preserves
/// the commit ordering needed for MVCC visibility checks after recovery.
#[must_use]
pub fn encode_commit_with_timestamp(txn_id: TxnId, commit_timestamp: CommitTimestamp) -> Vec<u8> {
    let mut payload = Vec::with_capacity(16);
    payload.extend_from_slice(&txn_id.get().to_le_bytes());
    payload.extend_from_slice(&commit_timestamp.get().to_le_bytes());
    payload
}

/// Encodes an Abort payload for `txn_id`.
#[must_use]
pub fn encode_abort(txn_id: TxnId) -> Vec<u8> {
    encode_begin(txn_id)
}

/// Encodes a transactional Data payload.
pub fn encode_data(txn_id: TxnId, operation: &DataOperation) -> Result<Vec<u8>> {
    let (kind, key, value) = match operation {
        DataOperation::Put { key, value } => (PUT, key.as_slice(), value.as_slice()),
        DataOperation::Delete { key } => (DELETE, key.as_slice(), &[][..]),
    };
    let key_len = u32::try_from(key.len()).map_err(|_| invalid("recovery key is too large"))?;
    let value_len =
        u32::try_from(value.len()).map_err(|_| invalid("recovery value is too large"))?;
    let mut payload = Vec::with_capacity(17 + key.len() + value.len());
    payload.extend_from_slice(&txn_id.get().to_le_bytes());
    payload.push(kind);
    payload.extend_from_slice(&key_len.to_le_bytes());
    payload.extend_from_slice(&value_len.to_le_bytes());
    payload.extend_from_slice(key);
    payload.extend_from_slice(value);
    Ok(payload)
}

/// Replays committed WAL transactions into an arbitrary [`ReplayTarget`].
///
/// This is the generic counterpart of [`recover`]: instead of always opening a
/// storage B+Tree, it accepts any [`ReplayTarget`]. The persistent SQL B+Tree
/// index reuses this entry point to reconstruct itself from a WAL without
/// duplicating recovery logic — one `replay_file_after` drives every target.
///
/// `from_exclusive` defaults to zero so every record in the file is visited;
/// callers that participate in checkpoint-based retention pass the checkpoint
/// boundary instead.
pub fn recover_target(wal_path: &Path, target: &mut dyn ReplayTarget) -> Result<RecoveryReport> {
    tracing::info!(
        target: "wal::recovery",
        "start target recovery wal_path={}",
        wal_path.display()
    );
    let outcome = replay_file_after(wal_path, Lsn::new(0), target)?;
    tracing::info!(
        target: "wal::recovery",
        "complete committed_transactions={} applied_operations={}",
        outcome.recovery.committed_transactions,
        outcome.recovery.applied_operations
    );
    Ok(outcome.recovery)
}

/// Replays committed WAL transactions into the real on-disk B+Tree.
pub fn recover(
    wal_path: &Path,
    storage_path: &Path,
    pool_capacity: usize,
) -> Result<RecoveryReport> {
    tracing::info!(target: "wal::recovery", "start wal_path={} storage_path={}", wal_path.display(), storage_path.display());
    let mut tree = BTree::open(storage_path, pool_capacity)?;
    let outcome = replay_file_after(wal_path, Lsn::new(0), &mut tree)?;
    tracing::info!(target: "wal::recovery", "complete committed_transactions={} applied_operations={}", outcome.recovery.committed_transactions, outcome.recovery.applied_operations);
    Ok(outcome.recovery)
}

/// Durable target that receives replayed storage operations.
///
/// Recovery is separated from the storage surface it reconstructs so the same
/// transaction-aware interpreter drives both physical-page and segment-backed
/// storage without duplicating WAL decoding.
///
/// Every target is also a [`ReplayHandler`]: storage appliers observe records
/// through the same deterministic callback while they reconstruct state, which
/// keeps the legacy handler contract and the storage surface on one replay
/// implementation instead of two WAL readers.
pub trait ReplayTarget: ReplayHandler {
    /// Applies an insert or replace without claiming durability.
    fn apply_put(&mut self, key: &[u8], value: &[u8]) -> Result<()>;
    /// Applies a deletion without claiming durability.
    fn apply_delete(&mut self, key: &[u8]) -> Result<()>;
    /// Makes everything applied so far durable. Called only when at least one
    /// operation was applied.
    fn apply_sync(&mut self) -> Result<()>;

    /// Declares the exclusive recovery boundary before replay begins.
    ///
    /// Records at or below the boundary belong to a checkpoint and are decoded
    /// only for validation and LSN continuity. A target that reports or filters
    /// records uses this boundary to distinguish the required range from the
    /// already-represented prefix. The default target has no boundary of its
    /// own and ignores the notification.
    fn set_replay_boundary(&mut self, _boundary: Lsn) {}
}

impl ReplayHandler for BTree {
    fn on_record(&mut self, _record: &crate::Record) -> Result<bool> {
        Ok(true)
    }
}

impl ReplayTarget for BTree {
    fn apply_put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.insert(key, value)
    }

    fn apply_delete(&mut self, key: &[u8]) -> Result<()> {
        self.delete(key)?;
        Ok(())
    }

    fn apply_sync(&mut self) -> Result<()> {
        self.sync()
    }
}

/// Transaction-aware replay interpreter over one WAL file.
///
/// Records at or before `from_exclusive` are decoded for validation and LSN
/// continuity but are not applied: a checkpoint already represents that
/// prefix. Data records are buffered until their Commit record, so aborted and
/// incomplete transactions never reach the target. Put/Delete application is
/// idempotent, so redirecting an already-checkpointed prefix through the same
/// target cannot corrupt durable state.
pub(crate) struct TransactionReplay<'a> {
    target: &'a mut dyn ReplayTarget,
    pending: HashMap<TxnId, Vec<DataOperation>>,
    from_exclusive: Lsn,
    expected_lsn: Option<u64>,
    report: RecoveryReport,
    records_after: u64,
    payload_bytes_after: u64,
    stopped: bool,
}

impl<'a> TransactionReplay<'a> {
    pub(crate) fn new(target: &'a mut dyn ReplayTarget, from_exclusive: Lsn) -> Self {
        target.set_replay_boundary(from_exclusive);
        Self {
            target,
            pending: HashMap::new(),
            from_exclusive,
            expected_lsn: None,
            report: RecoveryReport {
                committed_transactions: 0,
                applied_operations: 0,
                last_lsn: None,
            },
            records_after: 0,
            payload_bytes_after: 0,
            stopped: false,
        }
    }

    /// Validates and interprets one record, then notifies the target.
    ///
    /// Only records strictly above the recovery boundary reach
    /// [`ReplayHandler::on_record`], so a target's record observer and a target
    /// that applies operations see exactly the required post-boundary range.
    /// Receipt is in deterministic LSN order. Returning `Ok(false)` stops
    /// replay cleanly.
    pub(crate) fn observe(&mut self, record: &crate::Record) -> Result<bool> {
        self.report.last_lsn = Some(record.lsn);
        if let Some(expected) = self.expected_lsn {
            if record.lsn.get() != expected {
                return Err(PlomidError::new(
                    ErrorKind::Corruption,
                    "WAL LSN discontinuity in the recovery range",
                ));
            }
        }
        self.expected_lsn = Some(
            record
                .lsn
                .get()
                .checked_add(1)
                .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "WAL LSN exhausted"))?,
        );
        if record.lsn.get() > self.from_exclusive.get() {
            self.records_after = self
                .records_after
                .checked_add(1)
                .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "recovery count overflow"))?;
            self.payload_bytes_after = self
                .payload_bytes_after
                .checked_add(record.payload.len() as u64)
                .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "recovery size overflow"))?;
        }
        match record.record_type {
            crate::RecordType::Begin => {
                let txn_id = decode_txn_id(&record.payload)?;
                if self.pending.insert(txn_id, Vec::new()).is_some() {
                    return Err(corruption("duplicate WAL Begin record"));
                }
            }
            crate::RecordType::Data => {
                let (txn_id, operation) = decode_data(&record.payload)?;
                self.pending
                    .get_mut(&txn_id)
                    .ok_or_else(|| corruption("WAL Data record has no Begin"))?
                    .push(operation);
            }
            crate::RecordType::Commit => {
                let (txn_id, _) = decode_commit(&record.payload)?;
                let operations = self
                    .pending
                    .remove(&txn_id)
                    .ok_or_else(|| corruption("WAL Commit record has no Begin"))?;
                if record.lsn.get() > self.from_exclusive.get() {
                    self.apply(operations)?;
                }
                // Otherwise the checkpoint already represents this prefix: the
                // transaction is validated but not applied again.
            }
            crate::RecordType::Abort => {
                self.pending.remove(&decode_txn_id(&record.payload)?);
            }
            crate::RecordType::Checkpoint => {}
        }
        if !self.stopped && record.lsn.get() > self.from_exclusive.get() {
            if !self.target.on_record(record)? {
                self.stopped = true;
            }
        }
        Ok(!self.stopped)
    }

    fn apply(&mut self, operations: Vec<DataOperation>) -> Result<()> {
        for operation in operations {
            match operation {
                DataOperation::Put { key, value } => {
                    self.target.apply_put(&key, &value)?;
                }
                DataOperation::Delete { key } => {
                    self.target.apply_delete(&key)?;
                }
            }
            self.report.applied_operations = self
                .report
                .applied_operations
                .checked_add(1)
                .ok_or_else(|| {
                    PlomidError::new(ErrorKind::Internal, "recovery operation count overflow")
                })?;
        }
        self.report.committed_transactions = self
            .report
            .committed_transactions
            .checked_add(1)
            .ok_or_else(|| {
                PlomidError::new(ErrorKind::Internal, "recovery transaction count overflow")
            })?;
        Ok(())
    }
}

/// A WAL file that ends in an incomplete non-durable tail.
pub(crate) struct TailObservation {
    /// Byte offset in the file where valid durable data ends.
    pub(crate) boundary: u64,
}

/// Result of replaying one WAL file.
pub(crate) struct ReplayOutcome {
    /// Transaction and record counts observed during replay.
    pub(crate) recovery: RecoveryReport,
    /// Records observed strictly after the replay boundary.
    pub(crate) records_after: u64,
    /// WAL payload bytes observed strictly after the replay boundary.
    pub(crate) payload_bytes_after: u64,
    /// Incomplete tail detected at end of file, if any.
    pub(crate) tail: Option<TailObservation>,
}

/// Extracts the valid-data boundary from a reader error that reports a torn
/// tail. Any other error is corruption and must be propagated unchanged.
pub(crate) fn incomplete_tail_boundary(error: &PlomidError) -> Option<u64> {
    let detail = error.detail()?;
    detail
        .strip_prefix("incomplete_tail valid_end=")
        .and_then(|rest| rest.parse::<u64>().ok())
}

/// Drives one WAL file through a persistent interpreter.
///
/// Transaction state lives in the interpreter, not in this function, so a
/// transaction that begins in one segment and commits in a later one keeps its
/// buffered operations across the boundary and across files.
pub(crate) fn replay_segment_into(
    path: &Path,
    interpreter: &mut TransactionReplay<'_>,
) -> Result<Option<TailObservation>> {
    let mut reader = crate::WalReader::open(path)?;
    loop {
        match reader.next_record() {
            Ok(Some(record)) => {
                if !interpreter.observe(&record)? {
                    return Ok(None);
                }
            }
            Ok(None) => return Ok(None),
            Err(error) => {
                if let Some(boundary) = incomplete_tail_boundary(&error) {
                    return Ok(Some(TailObservation { boundary }));
                }
                return Err(error);
            }
        }
    }
}

/// Replays ordered WAL segments after `from_exclusive` through `target`.
///
/// Every record is decoded, so a corrupt frame, a failed checksum, or an
/// unsupported version inside the required range fails recovery instead of
/// being skipped. Transaction state lives in one interpreter across all files,
/// so a transaction that begins in one segment and commits in a later one is
/// applied exactly once.
///
/// Only the final segment may end in an incomplete tail. A torn frame in any
/// earlier segment means a durable region of the WAL is missing, which is
/// corruption rather than a recoverable crash tail.
pub(crate) fn replay_paths_after(
    paths: &[&Path],
    from_exclusive: Lsn,
    target: &mut dyn ReplayTarget,
) -> Result<ReplayOutcome> {
    let mut interpreter = TransactionReplay::new(target, from_exclusive);
    let mut tail = None;
    let final_index = paths.len().checked_sub(1);
    for (index, path) in paths.iter().enumerate() {
        if let Some(observation) = replay_segment_into(path, &mut interpreter)? {
            if Some(index) != final_index {
                return Err(PlomidError::new(
                    ErrorKind::Corruption,
                    "WAL segment ends in an incomplete tail before the final segment",
                ));
            }
            tail = Some(observation);
        }
        if interpreter.stopped {
            break;
        }
    }
    if !interpreter.stopped && interpreter.report.applied_operations > 0 {
        interpreter.target.apply_sync()?;
    }
    Ok(ReplayOutcome {
        recovery: interpreter.report,
        records_after: interpreter.records_after,
        payload_bytes_after: interpreter.payload_bytes_after,
        tail,
    })
}

/// Replays one WAL file after `from_exclusive` into `target`.
pub(crate) fn replay_file_after(
    path: &Path,
    from_exclusive: Lsn,
    target: &mut dyn ReplayTarget,
) -> Result<ReplayOutcome> {
    replay_paths_after(&[path], from_exclusive, target)
}

/// Enumerates WAL segments in deterministic sequence order with their
/// validated file-name sequence numbers.
///
/// Ordering is derived from the zero-padded sequence in the file name, never
/// from filesystem enumeration order. Retention removes only proven-complete
/// prefixes, so the retained sequences must be contiguous; a gap means a
/// durable region of the WAL is missing and is reported as corruption.
pub(crate) fn segment_manifest(wal_dir: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let mut found: Vec<(u64, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(wal_dir).map_err(PlomidError::from)? {
        let entry = entry.map_err(PlomidError::from)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "checkpoint" || name == "checkpoint.tmp" {
            continue;
        }
        if entry.file_type().map_err(PlomidError::from)?.is_dir() {
            continue;
        }
        if let Some(sequence) = crate::segmented::parse_sequence(&name) {
            found.push((sequence, entry.path()));
        }
    }
    found.sort_by_key(|(sequence, _)| *sequence);
    for window in found.windows(2) {
        if window[1].0 != window[0].0.saturating_add(1) {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "WAL segment sequence gap",
            ));
        }
    }
    Ok(found)
}

/// Enumerates WAL segment files in deterministic sequence order.
pub(crate) fn segment_paths(wal_dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(segment_manifest(wal_dir)?
        .into_iter()
        .map(|(_, path)| path)
        .collect())
}

/// Replays ordered WAL segments into the real segment-backed data manager.
pub fn recover_segmented(
    wal_dir: &Path,
    storage: &mut plomid_storage::StorageManager,
) -> Result<RecoveryReport> {
    let paths = segment_paths(wal_dir)?;
    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let mut target = SegmentedReplayTarget(storage);
    let outcome = replay_paths_after(&refs, Lsn::new(0), &mut target)?;
    Ok(outcome.recovery)
}

/// Segment-backed storage is a replay target: buffered application keeps one
/// flush per WAL file instead of one flush per operation.
pub struct SegmentedReplayTarget<'a>(pub &'a mut plomid_storage::StorageManager);

impl ReplayHandler for SegmentedReplayTarget<'_> {
    fn on_record(&mut self, _record: &crate::Record) -> Result<bool> {
        Ok(true)
    }
}

impl ReplayTarget for SegmentedReplayTarget<'_> {
    fn apply_put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.0.insert_buffered(key, value)
    }

    fn apply_delete(&mut self, key: &[u8]) -> Result<()> {
        self.0.delete_buffered(key)?;
        Ok(())
    }

    fn apply_sync(&mut self) -> Result<()> {
        self.0.sync()
    }
}

fn decode_txn_id(payload: &[u8]) -> Result<TxnId> {
    if payload.len() != 8 {
        return Err(corruption("transaction WAL payload must be 8 bytes"));
    }
    Ok(TxnId::new(u64::from_le_bytes(
        payload
            .try_into()
            .map_err(|_| corruption("invalid transaction ID"))?,
    )))
}

/// Decodes a Commit payload. Both supported forms are accepted: the 8-byte
/// `txn_id` payload and the 16-byte `txn_id | commit_timestamp` payload.
fn decode_commit(payload: &[u8]) -> Result<(TxnId, Option<CommitTimestamp>)> {
    if payload.len() == 8 {
        let txn_id = decode_txn_id(&payload[..8])?;
        return Ok((txn_id, None));
    }
    if payload.len() == 16 {
        let txn_id = decode_txn_id(&payload[..8])?;
        let commit_timestamp = CommitTimestamp::new(u64::from_le_bytes(
            payload[8..16]
                .try_into()
                .map_err(|_| corruption("invalid commit timestamp"))?,
        ));
        return Ok((txn_id, Some(commit_timestamp)));
    }
    Err(corruption("WAL Commit payload must be 8 or 16 bytes"))
}

fn decode_data(payload: &[u8]) -> Result<(TxnId, DataOperation)> {
    if payload.len() < 17 {
        return Err(corruption("WAL Data payload is truncated"));
    }
    let txn_id = decode_txn_id(&payload[..8])?;
    let kind = payload[8];
    let key_len = usize::try_from(u32::from_le_bytes(
        payload[9..13]
            .try_into()
            .map_err(|_| corruption("invalid recovery key length"))?,
    ))
    .map_err(|_| corruption("invalid recovery key length"))?;
    let value_len = usize::try_from(u32::from_le_bytes(
        payload[13..17]
            .try_into()
            .map_err(|_| corruption("invalid recovery value length"))?,
    ))
    .map_err(|_| corruption("invalid recovery value length"))?;
    let key_start: usize = 17;
    let value_start = key_start
        .checked_add(key_len)
        .ok_or_else(|| corruption("recovery key length overflow"))?;
    let end = value_start
        .checked_add(value_len)
        .ok_or_else(|| corruption("recovery value length overflow"))?;
    if end != payload.len() {
        return Err(corruption("recovery Data payload length mismatch"));
    }
    let key = payload[key_start..value_start].to_vec();
    let operation = match kind {
        PUT => DataOperation::Put {
            key,
            value: payload[value_start..end].to_vec(),
        },
        DELETE if value_len == 0 => DataOperation::Delete { key },
        _ => return Err(corruption("unknown recovery Data operation")),
    };
    Ok((txn_id, operation))
}

fn invalid(message: &'static str) -> PlomidError {
    PlomidError::new(ErrorKind::InvalidArgument, message)
}

fn corruption(message: &'static str) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}

#[cfg(test)]
mod tests {
    use super::{encode_begin, encode_commit, encode_data, recover, DataOperation};
    use crate::{RecordType, WalWriter};
    use plomid_core::TxnId;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn temp_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "plomid-recovery-{label}-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn recovery_replays_only_committed_transaction() {
        let wal_path = temp_path("committed-wal");
        let storage_path = temp_path("committed-storage");
        let result = (|| {
            let mut tree = plomid_storage::BTree::create(&storage_path, 32)?;
            tree.sync()?;
            drop(tree);
            let mut writer = WalWriter::create(&wal_path)?;
            let txn = TxnId::new(1);
            writer.append(RecordType::Begin, &encode_begin(txn))?;
            writer.append(
                RecordType::Data,
                &encode_data(
                    txn,
                    &DataOperation::Put {
                        key: b"committed".to_vec(),
                        value: b"yes".to_vec(),
                    },
                )?,
            )?;
            writer.append(RecordType::Commit, &encode_commit(txn))?;
            writer.sync()?;
            let report = recover(&wal_path, &storage_path, 32)?;
            assert_eq!(report.committed_transactions, 1);
            let mut tree = plomid_storage::BTree::open(&storage_path, 32)?;
            assert_eq!(tree.get(b"committed")?, Some(b"yes".to_vec()));
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&wal_path);
        let _ = fs::remove_file(&storage_path);
        assert!(result.is_ok(), "committed recovery failed: {result:?}");
    }

    #[test]
    fn recovery_ignores_uncommitted_transaction() {
        let wal_path = temp_path("uncommitted-wal");
        let storage_path = temp_path("uncommitted-storage");
        let result = (|| {
            let tree = plomid_storage::BTree::create(&storage_path, 32)?;
            drop(tree);
            let mut writer = WalWriter::create(&wal_path)?;
            let txn = TxnId::new(2);
            writer.append(RecordType::Begin, &encode_begin(txn))?;
            writer.append(
                RecordType::Data,
                &encode_data(
                    txn,
                    &DataOperation::Put {
                        key: b"uncommitted".to_vec(),
                        value: b"no".to_vec(),
                    },
                )?,
            )?;
            writer.sync()?;
            let report = recover(&wal_path, &storage_path, 32)?;
            assert_eq!(report.committed_transactions, 0);
            let mut tree = plomid_storage::BTree::open(&storage_path, 32)?;
            assert_eq!(tree.get(b"uncommitted")?, None);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(&wal_path);
        let _ = fs::remove_file(&storage_path);
        assert!(result.is_ok(), "uncommitted recovery failed: {result:?}");
    }
}
