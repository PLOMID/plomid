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
//! Hot Row Store: the mutable SQL row write path for PLOMID.
//!
//! The Hot Row Store layers row semantics over the transactional storage
//! engine. Every logical row is identified by a stable [`RowId`] that carries
//! no physical location; placement is resolved independently through the
//! underlying B+Tree / storage manager. Row values reuse the canonical row
//! encoding from `plomid_storage::row`, and multi-version visibility is owned
//! entirely by the engine's MVCC `VersionStore`, so there is exactly one
//! version-chain and visibility implementation in the repository.
//!
//! # Durability
//!
//! All mutations flow through [`StorageEngineTransaction`], which appends a
//! WAL record before any durable data change and applies buffered operations
//! to storage only after the commit record is durably established (WAL before
//! data). Aborted or dropped transactions leave no durable trace and install
//! no MVCC versions.
//!
//! # Row identity
//!
//! Row identifiers are allocated from a monotonically increasing counter. The
//! high-water mark is persisted inside the same transaction that consumes it
//! (as an ordinary WAL-backed key), so after restart the recovered watermark
//! prevents identifier reuse. UPDATE preserves the logical `RowId`; DELETE
//! records a tombstone through MVCC so older snapshots keep seeing the row.
//!
//! # MVCC visibility
//!
//! Reads evaluate MVCC visibility through the engine's [`VersionStore`]:
//! outside a transaction, [`HotRowStore::read`] uses the committed snapshot;
//! inside a transaction, [`HotRowTransaction::read`] first checks the
//! transaction's own write buffer, then falls back to the version chain
//! visible to the transaction's snapshot. Uncommitted versions are never
//! exposed to other snapshots.

use plomid_core::{ErrorKind, PlomidError, Result, RowId, TxnId};
use plomid_storage::{
    CommitResult, Row, StorageEngine, StorageEngineTransaction, TransactionState, PAGE_DATA_SIZE,
};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::PlomidStorageEngine;

/// Key prefix for the durable row-id watermark record.
const META_KEY: [u8; 4] = [0x7E, 0x68, 0x72, 0x00];
/// Key prefix byte for Hot Row Store data rows.
const DATA_KEY: [u8; 4] = [0x7E, 0x68, 0x72, 0x01];
/// Exclusive end of the watermark key range.
const META_KEY_END: [u8; 5] = [0x7E, 0x68, 0x72, 0x00, 0x01];

/// Process-wide floor for row-id allocation. It only ever moves forward, so
/// identifiers stay unique across engines and transactions within a process.
static ROW_ID_FLOOR: AtomicU64 = AtomicU64::new(0);

/// Encodes the logical storage key for `row_id`.
///
/// The key encodes the logical identifier only; physical placement is
/// resolved by the storage layer. The identifier is big-endian so bytewise
/// key order matches numeric order, which keeps single-row scans exact.
#[must_use]
pub fn row_key(row_id: RowId) -> [u8; 12] {
    let mut key = [0u8; 12];
    key[..4].copy_from_slice(&DATA_KEY);
    key[4..].copy_from_slice(&row_id.get().to_be_bytes());
    key
}

/// Reads the encoded payload of one row through the transaction's point-read
/// contract. This avoids materializing a temporary one-row range result.
fn read_row_payload(
    txn: &mut dyn StorageEngineTransaction<'_>,
    row_id: RowId,
) -> Result<Option<Vec<u8>>> {
    let key = row_key(row_id);
    txn.get(&key)
}

/// Validates an encoded row against the page payload limit.
fn check_size(encoded: &[u8]) -> Result<()> {
    if encoded.len() > PAGE_DATA_SIZE {
        return Err(PlomidError::new(
            ErrorKind::InvalidArgument,
            "encoded row exceeds the page payload size",
        ));
    }
    Ok(())
}

/// High-level interface to the Hot Row Store over one storage engine.
///
/// Statement-level methods take the engine by value-borrow for each call and
/// open their own transaction internally. Explicit multi-operation
/// transactions use [`HotRowStore::transaction`] directly on the engine.
pub struct HotRowStore<'e> {
    engine: &'e mut PlomidStorageEngine,
}

impl<'e> HotRowStore<'e> {
    /// Wraps an existing storage engine.
    #[must_use]
    pub fn new(engine: &'e mut PlomidStorageEngine) -> Self {
        Self { engine }
    }

    /// Begins an explicit Hot Row Store transaction directly on the engine.
    ///
    /// This is an associated function (no `self` borrow) so the returned
    /// transaction borrows the engine independently: `let mut txn =
    /// HotRowStore::transaction(&mut engine)?;`.
    pub fn transaction<'x>(engine: &'x mut PlomidStorageEngine) -> Result<HotRowTransaction<'x>> {
        let inner = engine.begin()?;
        Ok(HotRowTransaction { inner: Some(inner) })
    }

    /// Captures a read snapshot of the last committed state.
    pub fn committed_snapshot(&self) -> Result<plomid_mvcc::Snapshot> {
        self.engine.committed_snapshot()
    }

    /// Reads the row visible to `snapshot`, or `None` when absent.
    pub fn read_at(
        &mut self,
        row_id: RowId,
        snapshot: &plomid_mvcc::Snapshot,
    ) -> Result<Option<Row>> {
        self.engine
            .get_snapshot(&row_key(row_id), snapshot)
            .and_then(|payload| payload.map(|p| decode_row(&p)).transpose())
    }

    /// Inserts one row in its own transaction and returns its logical `RowId`.
    pub fn insert(&mut self, row: Row) -> Result<RowId> {
        let mut txn = HotRowTransaction {
            inner: Some(self.engine.begin()?),
        };
        let row_id = txn.insert(row)?;
        txn.commit()?;
        Ok(row_id)
    }

    /// Reads the newest committed version of `row_id`.
    pub fn read(&mut self, row_id: RowId) -> Result<Row> {
        let payload = self
            .engine
            .get(&row_key(row_id))?
            .ok_or_else(|| missing_row(row_id))?;
        decode_row(&payload)
    }

    /// Replaces the visible version of `row_id` in its own transaction.
    pub fn update(&mut self, row_id: RowId, row: Row) -> Result<()> {
        let mut txn = HotRowTransaction {
            inner: Some(self.engine.begin()?),
        };
        txn.update(row_id, row)?;
        txn.commit()?;
        Ok(())
    }

    /// Deletes `row_id` in its own transaction.
    pub fn delete(&mut self, row_id: RowId) -> Result<()> {
        let mut txn = HotRowTransaction {
            inner: Some(self.engine.begin()?),
        };
        txn.delete(row_id)?;
        txn.commit()?;
        Ok(())
    }

    /// Inserts `rows` atomically; returns one `RowId` per input row.
    pub fn batch_insert(&mut self, rows: Vec<Row>) -> Result<Vec<RowId>> {
        let mut txn = HotRowTransaction {
            inner: Some(self.engine.begin()?),
        };
        let ids = txn.batch_insert(rows)?;
        txn.commit()?;
        Ok(ids)
    }

    /// Replaces every listed row atomically; the batch fails as a whole when
    /// any row is missing.
    pub fn batch_update(&mut self, updates: Vec<(RowId, Row)>) -> Result<()> {
        let mut txn = HotRowTransaction {
            inner: Some(self.engine.begin()?),
        };
        txn.batch_update(updates)?;
        txn.commit()?;
        Ok(())
    }

    /// Deletes every listed row atomically; the batch fails as a whole when
    /// any row is missing.
    pub fn batch_delete(&mut self, row_ids: Vec<RowId>) -> Result<()> {
        let mut txn = HotRowTransaction {
            inner: Some(self.engine.begin()?),
        };
        txn.batch_delete(row_ids)?;
        txn.commit()?;
        Ok(())
    }
}

/// An explicit Hot Row Store transaction.
///
/// Operations are recorded in the WAL under the transaction's `TxnId` and
/// applied to storage only on [`commit`](Self::commit). The handle exposes
/// read-your-own-writes through the engine's MVCC snapshot overlay.
pub struct HotRowTransaction<'e> {
    inner: Option<<PlomidStorageEngine as StorageEngine>::Transaction<'e>>,
}

impl<'e> HotRowTransaction<'e> {
    fn inner(&mut self) -> Result<&mut <PlomidStorageEngine as StorageEngine>::Transaction<'e>> {
        self.inner.as_mut().ok_or_else(|| {
            PlomidError::new(ErrorKind::Transaction, "hot row transaction is not active")
        })
    }

    /// Returns the transaction's identity.
    #[must_use]
    pub fn txn_id(&self) -> TxnId {
        self.inner
            .as_ref()
            .map_or(TxnId::new(0), |txn| txn.txn_id())
    }

    /// Returns the transaction's lifecycle state.
    #[must_use]
    pub fn state(&self) -> TransactionState {
        self.inner
            .as_ref()
            .map_or(TransactionState::Aborted, |txn| txn.state())
    }

    /// Reads the durable row-id watermark through this transaction.
    fn persisted_watermark(&mut self) -> Result<u64> {
        let entries = self.inner()?.scan(Some(&META_KEY), Some(&META_KEY_END))?;
        match entries.len() {
            0 => Ok(0),
            1 => {
                let value = entries.into_iter().next().expect("len is 1").1;
                let raw: [u8; 8] = value.try_into().map_err(|_| {
                    PlomidError::new(ErrorKind::Corruption, "row-id watermark is malformed")
                })?;
                Ok(u64::from_le_bytes(raw))
            }
            _ => Err(PlomidError::new(
                ErrorKind::Corruption,
                "row-id watermark key range returned multiple entries",
            )),
        }
    }

    /// Allocates `count` row ids, persisting the new watermark in this
    /// transaction so recovered state never reissues an identifier.
    ///
    /// The persisted watermark is the last allocated id. An empty allocation
    /// writes nothing and advances nothing.
    fn allocate_ids(&mut self, count: usize) -> Result<u64> {
        let count = u64::try_from(count).map_err(|_| {
            PlomidError::new(ErrorKind::InvalidArgument, "batch size overflows row ids")
        })?;
        let persisted = self.persisted_watermark()?;
        let floor = ROW_ID_FLOOR.load(Ordering::Acquire);
        let start = persisted.max(floor).saturating_add(1);
        if count == 0 {
            return Ok(start);
        }
        let last = start.checked_add(count - 1).ok_or_else(|| {
            PlomidError::new(ErrorKind::InvalidArgument, "row-id space is exhausted")
        })?;
        ROW_ID_FLOOR.store(last, Ordering::Release);
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&last.to_le_bytes());
        self.inner()?.put(&META_KEY, &payload)?;
        Ok(start)
    }

    /// Inserts `row`, returning its stable logical `RowId`.
    pub fn insert(&mut self, row: Row) -> Result<RowId> {
        let encoded = encode_row(&row)?;
        let row_id = RowId::new(self.allocate_ids(1)?);
        let key = row_key(row_id);
        self.inner()?.put(&key, &encoded)?;
        Ok(row_id)
    }

    /// Reads the version of `row_id` visible to this transaction, including
    /// the transaction's own buffered writes.
    pub fn read(&mut self, row_id: RowId) -> Result<Row> {
        let payload = read_row_payload(self.inner()?, row_id)?;
        let payload = payload.ok_or_else(|| missing_row(row_id))?;
        decode_row(&payload)
    }

    /// Returns `true` when a version of `row_id` is visible to this
    /// transaction.
    pub fn exists(&mut self, row_id: RowId) -> Result<bool> {
        Ok(read_row_payload(self.inner()?, row_id)?.is_some())
    }

    /// Replaces the visible version of `row_id`, preserving its identity.
    ///
    /// Fails with [`ErrorKind::NotFound`] when no version of the row is
    /// visible to this transaction; no WAL record is written in that case.
    pub fn update(&mut self, row_id: RowId, row: Row) -> Result<()> {
        if !self.exists(row_id)? {
            return Err(missing_row(row_id));
        }
        let encoded = encode_row(&row)?;
        let key = row_key(row_id);
        self.inner()?.put(&key, &encoded)
    }

    /// Records the delete of `row_id` through the MVCC tombstone path.
    ///
    /// Older snapshots keep observing the previous version; the row reads as
    /// absent once the delete commits.
    pub fn delete(&mut self, row_id: RowId) -> Result<()> {
        if !self.exists(row_id)? {
            return Err(missing_row(row_id));
        }
        let key = row_key(row_id);
        self.inner()?.delete(&key)
    }

    /// Inserts every row in one transaction, returning one `RowId` each.
    pub fn batch_insert(&mut self, rows: Vec<Row>) -> Result<Vec<RowId>> {
        let first = self.allocate_ids(rows.len())?;
        let mut ids = Vec::with_capacity(rows.len());
        for (offset, row) in rows.into_iter().enumerate() {
            let row_id = RowId::new(first + offset as u64);
            let encoded = encode_row(&row)?;
            let key = row_key(row_id);
            self.inner()?.put(&key, &encoded)?;
            ids.push(row_id);
        }
        Ok(ids)
    }

    /// Replaces every listed row in one transaction. Any missing row aborts
    /// the whole batch without writing WAL records for the remaining rows.
    pub fn batch_update(&mut self, updates: Vec<(RowId, Row)>) -> Result<()> {
        for (row_id, row) in updates {
            self.update(row_id, row)?;
        }
        Ok(())
    }

    /// Deletes every listed row in one transaction. Any missing row aborts
    /// the whole batch without writing WAL records for the remaining rows.
    pub fn batch_delete(&mut self, row_ids: Vec<RowId>) -> Result<()> {
        for row_id in row_ids {
            self.delete(row_id)?;
        }
        Ok(())
    }

    /// Commits the transaction: the commit record is durably established in
    /// the WAL first, then buffered operations are applied to storage and
    /// installed as committed MVCC versions.
    pub fn commit(mut self) -> Result<CommitResult> {
        let mut inner = self.inner.take().ok_or_else(|| {
            PlomidError::new(ErrorKind::Transaction, "hot row transaction is not active")
        })?;
        inner.commit()
    }

    /// Aborts the transaction, discarding every buffered mutation.
    pub fn abort(mut self) -> Result<()> {
        let mut inner = self.inner.take().ok_or_else(|| {
            PlomidError::new(ErrorKind::Transaction, "hot row transaction is not active")
        })?;
        inner.abort()
    }
}

/// Decodes an encoded row payload, failing safely on corruption or truncation.
/// Any malformed, truncated, or ambiguous payload is rejected with a controlled
/// error instead of treated as an empty or missing row.
///
/// The canonical decoder lives in `plomid_storage::Row::decode`. This module
/// calls it directly; no private, second row format is defined here.
fn decode_row(payload: &[u8]) -> Result<Row> {
    Row::decode(payload)
}

/// Encodes and validates a logical row into its durable, deterministic byte
/// representation.
///
/// The row encoding lives in `plomid_storage::Row::encode`. This module wraps
/// that canonical encoding and validates only that the result fits within the
/// storage engine's page payload boundary, because the Hot Row Store stores
/// encoded rows inside one storage key. No private row files are created and
/// no second row format is defined here.
fn encode_row(row: &Row) -> Result<Vec<u8>> {
    let encoded = Row::encode(row)?;
    check_size(&encoded)?;
    Ok(encoded)
}

/// Builds the controlled "missing row" error for `row_id`.
fn missing_row(row_id: RowId) -> PlomidError {
    PlomidError::new(ErrorKind::NotFound, format!("row {row_id} is not visible"))
}
