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
//! Transactional write path for PLOMID V1.
//!
//! A [`Transaction`] ties together a [`TransactionManager`], a [`WalWriter`],
//! and a [`plomid_storage::BTree`] so that data-modifying operations are
//! recorded in the WAL under an authoritative [`TxnId`] and only become
//! visible in durable storage when the transaction commits.
//!
//! # V1 Durability model
//!
//! A transaction is considered durable only after its `Commit` record has
//! been flushed to the WAL (`wal.commit(lsn)` in [`DurabilityMode::Force`])
//! **and** its buffered operations have been applied to the B+Tree and
//! synced to disk. Recovery replays committed transactions idempotently, so
//! partial or repeated application of the same operations is safe.
//!
//! Uncommitted changes are **never** applied to the B+Tree before commit.
//! Aborted or incomplete transactions are ignored by recovery.

use crate::TransactionManager;
use plomid_core::{CommitTimestamp, ErrorKind, PlomidError, Result, TxnId};
use plomid_storage::{BTree, StorageManager};
use plomid_wal::{
    encode_abort, encode_begin, encode_commit_with_timestamp, encode_data, DataOperation,
    DurabilityMode, RecordType, SegmentedWal, WalWriter,
};

pub trait DataStore: Send {
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()>;
    fn delete(&mut self, key: &[u8]) -> Result<bool>;
    fn range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
    fn sync(&mut self) -> Result<()>;

    /// Applies a complete commit write-set. Stores with a page-aware batch
    /// implementation may override this to reuse mutation state across keys.
    fn apply_operations(&mut self, operations: &[DataOperation]) -> Result<()> {
        for operation in operations {
            match operation {
                DataOperation::Put { key, value } => self.insert(key, value)?,
                DataOperation::Delete { key } => {
                    self.delete(key)?;
                }
            }
        }
        Ok(())
    }
}

impl DataStore for BTree {
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        BTree::get(self, key)
    }
    fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        BTree::insert(self, key, value)
    }
    fn delete(&mut self, key: &[u8]) -> Result<bool> {
        BTree::delete(self, key)
    }
    fn range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        BTree::range(self, start, end)
    }
    fn sync(&mut self) -> Result<()> {
        BTree::sync(self)
    }
}

impl DataStore for StorageManager {
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        StorageManager::get(self, key)
    }
    fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        StorageManager::insert_buffered(self, key, value)
    }
    fn delete(&mut self, key: &[u8]) -> Result<bool> {
        StorageManager::delete_buffered(self, key)
    }
    fn range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        StorageManager::range(self, start, end)
    }
    fn sync(&mut self) -> Result<()> {
        StorageManager::sync(self)
    }
    fn apply_operations(&mut self, operations: &[DataOperation]) -> Result<()> {
        let mut batch = Vec::with_capacity(operations.len());
        for operation in operations {
            match operation {
                DataOperation::Put { key, value } => batch.push((key.clone(), Some(value.clone()))),
                DataOperation::Delete { key } => batch.push((key.clone(), None)),
            }
        }
        StorageManager::apply_batch(self, &batch)
    }
}

pub trait LogStore: Send {
    fn append(&mut self, record_type: RecordType, payload: &[u8]) -> Result<plomid_core::Lsn>;
    fn commit(&mut self, lsn: plomid_core::Lsn) -> Result<()>;
    fn sync(&mut self) -> Result<()>;
    fn durability_mode(&self) -> DurabilityMode;
}

impl LogStore for WalWriter {
    fn append(&mut self, record_type: RecordType, payload: &[u8]) -> Result<plomid_core::Lsn> {
        WalWriter::append(self, record_type, payload)
    }
    fn commit(&mut self, lsn: plomid_core::Lsn) -> Result<()> {
        WalWriter::commit(self, lsn)
    }
    fn sync(&mut self) -> Result<()> {
        WalWriter::sync(self)
    }
    fn durability_mode(&self) -> DurabilityMode {
        WalWriter::durability_mode(self)
    }
}

impl LogStore for SegmentedWal {
    fn append(&mut self, record_type: RecordType, payload: &[u8]) -> Result<plomid_core::Lsn> {
        SegmentedWal::append(self, record_type, payload)
    }
    fn commit(&mut self, lsn: plomid_core::Lsn) -> Result<()> {
        SegmentedWal::commit(self, lsn)
    }
    fn sync(&mut self) -> Result<()> {
        SegmentedWal::sync(self)
    }
    fn durability_mode(&self) -> DurabilityMode {
        SegmentedWal::durability_mode(self)
    }
}

/// Lifecycle state of a [`Transaction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    /// Transaction is accepting operations.
    Active,
    /// Transaction has committed successfully.
    Committed,
    /// Transaction has been aborted.
    Aborted,
}

/// Result of a successful [`Transaction::commit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitResult {
    /// The transaction's identity.
    pub txn_id: TxnId,
    /// The logical commit timestamp assigned at commit time.
    pub commit_timestamp: CommitTimestamp,
}

/// A single active transaction.
///
/// All data-modifying operations are buffered in memory and written to the
/// WAL immediately, but are not applied to the underlying store until
/// [`Self::commit`] is called. Each transaction carries its MVCC snapshot
/// ([`TransactionContext`]): own writes overlay snapshot reads, other
/// transactions' uncommitted writes stay invisible.
pub struct Transaction<'a, T: DataStore = BTree, W: LogStore = WalWriter> {
    context: plomid_mvcc::TransactionContext,
    manager: &'a TransactionManager,
    wal: &'a mut W,
    tree: &'a mut T,
    operations: Vec<DataOperation>,
    last_lsn: Option<plomid_core::Lsn>,
}

impl<'a, T: DataStore, W: LogStore> Transaction<'a, T, W> {
    /// Begins a new transaction, writing a `Begin` record to the WAL.
    ///
    /// The returned transaction is in the [`TransactionState::Active`] state
    /// and is ready to accept [`Self::put`] and [`Self::delete`] operations.
    /// A consistent MVCC snapshot is captured at begin for visibility.
    pub fn begin(manager: &'a TransactionManager, wal: &'a mut W, tree: &'a mut T) -> Result<Self> {
        Self::begin_raw(manager, wal, tree)
    }

    /// Internal begin used by the engine: same as [`Self::begin`] but named
    /// to document the disjoint-borrow bridge at the call site.
    pub(crate) fn begin_raw(
        manager: &'a TransactionManager,
        wal: &'a mut W,
        tree: &'a mut T,
    ) -> Result<Self> {
        let txn_id = manager.begin()?;
        let snapshot = manager.snapshot(txn_id)?;
        let payload = encode_begin(txn_id);
        let lsn = wal.append(RecordType::Begin, &payload)?;
        tracing::info!(target: "transaction", "begin txn_id={}", txn_id.get());
        Ok(Self {
            context: plomid_mvcc::TransactionContext::new(txn_id, snapshot),
            manager,
            wal,
            tree,
            operations: Vec::new(),
            last_lsn: Some(lsn),
        })
    }

    /// Inserts or replaces `value` for `key` within this transaction.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.ensure_active()?;
        let operation = DataOperation::Put {
            key: key.to_vec(),
            value: value.to_vec(),
        };
        let payload = encode_data(self.context.txn_id, &operation)?;
        let lsn = self.wal.append(RecordType::Data, &payload)?;
        self.last_lsn = Some(lsn);
        self.operations.push(operation);
        tracing::debug!(target: "transaction", "put txn_id={} key_len={} lsn={}", self.context.txn_id.get(), key.len(), lsn.get());
        Ok(())
    }

    /// Removes `key` within this transaction.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.ensure_active()?;
        let operation = DataOperation::Delete { key: key.to_vec() };
        let payload = encode_data(self.context.txn_id, &operation)?;
        let lsn = self.wal.append(RecordType::Data, &payload)?;
        self.last_lsn = Some(lsn);
        self.operations.push(operation);
        tracing::debug!(target: "transaction", "delete txn_id={} key_len={} lsn={}", self.context.txn_id.get(), key.len(), lsn.get());
        Ok(())
    }

    /// Reads committed rows while retaining this transaction's buffered writes.
    ///
    /// The result overlays this transaction's pending operations on top of the
    /// committed store state (read-your-own-writes): pending puts replace or
    /// add entries, pending deletes hide entries. Other transactions'
    /// uncommitted writes live only in their owner's buffer, so they are
    /// never visible here.
    pub fn scan(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.ensure_active()?;
        let mut rows: Vec<(Vec<u8>, Vec<u8>)> = self.tree.range(start, end)?;
        let mut pending_puts: std::collections::HashMap<Vec<u8>, Vec<u8>> =
            std::collections::HashMap::new();
        let mut pending_deletes: std::collections::HashSet<Vec<u8>> =
            std::collections::HashSet::new();
        for operation in &self.operations {
            match operation {
                DataOperation::Put { key, value } => {
                    pending_deletes.remove(key);
                    pending_puts.insert(key.clone(), value.clone());
                }
                DataOperation::Delete { key } => {
                    pending_puts.remove(key);
                    pending_deletes.insert(key.clone());
                }
            }
        }
        if !pending_puts.is_empty() || !pending_deletes.is_empty() {
            rows.retain(|(key, _)| {
                !pending_deletes.contains(key) && !pending_puts.contains_key(key)
            });
            for (key, value) in pending_puts {
                let in_range = start.is_none_or(|start| key.as_slice() >= start)
                    && end.is_none_or(|end| key.as_slice() < end);
                if in_range {
                    rows.push((key, value));
                }
            }
            rows.sort_by(|left, right| left.0.cmp(&right.0));
        }
        Ok(rows)
    }

    /// Commits the transaction, making all buffered changes durable.
    ///
    /// This writes a `Commit` record carrying the allocated commit timestamp,
    /// fsyncs the WAL, applies all buffered operations to the store, and
    /// syncs the store to disk. The commit timestamp is the MVCC commit order.
    /// The buffered operations are drained and returned so the engine can
    /// install them as committed versions after durability.
    pub fn commit(&mut self) -> Result<CommitResult> {
        self.ensure_active()?;
        let ts = self.manager.commit_txn(self.context.txn_id)?;
        let payload = encode_commit_with_timestamp(self.context.txn_id, ts);
        let lsn = self.wal.append(RecordType::Commit, &payload)?;
        self.last_lsn = Some(lsn);
        self.wal.commit(lsn)?;

        let operations: Vec<DataOperation> = std::mem::take(&mut self.operations);
        self.tree.apply_operations(&operations)?;
        self.tree.sync()?;

        self.context.mark_committed().map_err(|e| {
            PlomidError::with_detail(
                e.kind(),
                e.message().to_string(),
                format!("txn_id={}", self.context.txn_id.get()),
            )
        })?;
        tracing::info!(target: "transaction", "commit txn_id={} commit_timestamp={}", self.context.txn_id.get(), ts.get());
        Ok(CommitResult {
            txn_id: self.context.txn_id,
            commit_timestamp: ts,
        })
    }

    /// Aborts the transaction, discarding all buffered changes.
    pub fn abort(&mut self) -> Result<()> {
        self.ensure_active()?;
        let payload = encode_abort(self.context.txn_id);
        let lsn = self.wal.append(RecordType::Abort, &payload)?;
        self.last_lsn = Some(lsn);
        self.wal.commit(lsn)?;
        let _ = self.manager.abort_txn(self.context.txn_id);
        self.context.mark_aborted().map_err(|e| {
            PlomidError::with_detail(
                e.kind(),
                e.message().to_string(),
                format!("txn_id={}", self.context.txn_id.get()),
            )
        })?;
        tracing::info!(target: "transaction", "abort txn_id={}", self.context.txn_id.get());
        Ok(())
    }

    /// Returns the transaction's identity.
    #[must_use]
    pub fn txn_id(&self) -> TxnId {
        self.context.txn_id
    }

    /// Returns the transaction's current lifecycle state.
    #[must_use]
    pub fn state(&self) -> TransactionState {
        match self.context.state {
            plomid_mvcc::TxnState::Active => TransactionState::Active,
            plomid_mvcc::TxnState::Committed => TransactionState::Committed,
            plomid_mvcc::TxnState::Aborted => TransactionState::Aborted,
        }
    }

    /// Returns the MVCC snapshot captured at begin.
    #[must_use]
    pub fn snapshot(&self) -> &plomid_mvcc::Snapshot {
        &self.context.snapshot
    }

    /// Buffered operations not yet committed (engine version install reads this).
    #[must_use]
    pub fn buffered_operations(&self) -> &[DataOperation] {
        &self.operations
    }

    /// Returns the WAL durability mode.
    #[must_use]
    pub fn durability_mode(&self) -> DurabilityMode {
        self.wal.durability_mode()
    }

    fn ensure_active(&self) -> Result<()> {
        match self.context.state {
            plomid_mvcc::TxnState::Active => Ok(()),
            plomid_mvcc::TxnState::Committed => Err(PlomidError::with_detail(
                ErrorKind::Transaction,
                "transaction is already committed",
                format!("txn_id={}", self.context.txn_id.get()),
            )),
            plomid_mvcc::TxnState::Aborted => Err(PlomidError::with_detail(
                ErrorKind::Transaction,
                "transaction is already aborted",
                format!("txn_id={}", self.context.txn_id.get()),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        sync::Mutex,
        thread,
    };

    fn temp_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("plomid-txn-{label}-{}-{id}", std::process::id()))
    }

    #[test]
    fn put_transaction_survives_restart() {
        let wal_path = temp_path("put-restart");
        let storage_path = temp_path("put-restart-storage");
        let result = (|| {
            {
                let manager = TransactionManager::new();
                let mut tree = BTree::create(&storage_path, 32)?;
                let mut wal = WalWriter::create(&wal_path)?;
                let mut txn = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn.put(b"alpha", b"one")?;
                txn.put(b"beta", b"two")?;
                txn.commit()?;
                drop(tree);
                drop(wal);
            }

            let mut tree = BTree::open(&storage_path, 32)?;
            assert_eq!(tree.get(b"alpha")?, Some(b"one".to_vec()));
            assert_eq!(tree.get(b"beta")?, Some(b"two".to_vec()));
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(temp_path("put-restart-wal"));
        let _ = fs::remove_file(temp_path("put-restart-storage"));
        assert!(
            result.is_ok(),
            "put transaction restart test failed: {result:?}"
        );
    }

    #[test]
    fn delete_transaction_survives_restart() {
        let wal_path = temp_path("delete-restart-wal");
        let storage_path = temp_path("delete-restart-storage");
        let result = (|| {
            {
                let mut tree = BTree::create(&storage_path, 32)?;
                tree.insert(b"gamma", b"three")?;
                tree.sync()?;
                drop(tree);

                let manager = TransactionManager::new();
                let mut tree = BTree::open(&storage_path, 32)?;
                let mut wal = WalWriter::create(&wal_path)?;
                let mut txn = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn.delete(b"gamma")?;
                txn.commit()?;
                drop(tree);
                drop(wal);
            }

            let mut tree = BTree::open(&storage_path, 32)?;
            assert_eq!(tree.get(b"gamma")?, None);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(temp_path("delete-restart-wal"));
        let _ = fs::remove_file(temp_path("delete-restart-storage"));
        assert!(
            result.is_ok(),
            "delete transaction restart test failed: {result:?}"
        );
    }

    #[test]
    fn aborted_transaction_does_not_appear_after_restart() {
        let wal_path = temp_path("aborted-restart-wal");
        let storage_path = temp_path("aborted-restart-storage");
        let result = (|| {
            {
                let manager = TransactionManager::new();
                let mut tree = BTree::create(&storage_path, 32)?;
                let mut wal = WalWriter::create(&wal_path)?;
                let mut txn = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn.put(b"aborted", b"should-not-appear")?;
                txn.abort()?;
                drop(tree);
                drop(wal);
            }

            let mut tree = BTree::open(&storage_path, 32)?;
            assert_eq!(tree.get(b"aborted")?, None);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(temp_path("aborted-restart-wal"));
        let _ = fs::remove_file(temp_path("aborted-restart-storage"));
        assert!(
            result.is_ok(),
            "aborted transaction restart test failed: {result:?}"
        );
    }

    #[test]
    fn incomplete_transaction_does_not_appear_after_restart() {
        let wal_path = temp_path("incomplete-restart-wal");
        let storage_path = temp_path("incomplete-restart-storage");
        let result = (|| {
            {
                let manager = TransactionManager::new();
                let mut tree = BTree::create(&storage_path, 32)?;
                let mut wal = WalWriter::create(&wal_path)?;
                let mut txn = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn.put(b"incomplete", b"no-commit")?;
                drop(txn);
                drop(tree);
                drop(wal);
            }

            let mut tree = BTree::open(&storage_path, 32)?;
            assert_eq!(tree.get(b"incomplete")?, None);
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(temp_path("incomplete-restart-wal"));
        let _ = fs::remove_file(temp_path("incomplete-restart-storage"));
        assert!(
            result.is_ok(),
            "incomplete transaction restart test failed: {result:?}"
        );
    }

    #[test]
    fn mixed_committed_aborted_incomplete_transactions_survive_restart() {
        let wal_path = temp_path("mixed-restart");
        let storage_path = temp_path("mixed-restart-storage");
        let result = (|| {
            {
                let manager = TransactionManager::new();
                let mut tree = BTree::create(&storage_path, 32)?;
                let mut wal = WalWriter::create(&wal_path)?;

                let mut txn1 = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn1.put(b"committed1", b"yes")?;
                txn1.commit()?;

                let mut txn2 = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn2.put(b"aborted", b"no")?;
                txn2.abort()?;

                let mut txn3 = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn3.put(b"incomplete", b"no")?;
                drop(txn3);

                let mut txn4 = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn4.put(b"committed2", b"yes")?;
                txn4.commit()?;

                drop(tree);
                drop(wal);
            }

            let mut tree = BTree::open(&storage_path, 32)?;
            assert_eq!(tree.get(b"committed1")?, Some(b"yes".to_vec()));
            assert_eq!(tree.get(b"aborted")?, None);
            assert_eq!(tree.get(b"incomplete")?, None);
            assert_eq!(tree.get(b"committed2")?, Some(b"yes".to_vec()));
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(temp_path("mixed-restart-wal"));
        let _ = fs::remove_file(temp_path("mixed-restart-storage"));
        assert!(
            result.is_ok(),
            "mixed transaction restart test failed: {result:?}"
        );
    }

    #[test]
    fn reopen_continues_allocating_unique_txn_ids() {
        let wal_path = temp_path("reopen-txn-wal");
        let storage_path = temp_path("reopen-txn-storage");
        let result = (|| {
            {
                let manager = TransactionManager::new();
                let mut tree = BTree::create(&storage_path, 32)?;
                let mut wal = WalWriter::create(&wal_path)?;
                let mut txn = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn.put(b"first", b"1")?;
                txn.commit()?;
                drop(tree);
                drop(wal);
            }

            let manager = TransactionManager::open(&wal_path)?;
            let mut tree = BTree::open(&storage_path, 32)?;
            let mut wal = WalWriter::open(&wal_path)?;
            let mut txn = Transaction::begin(&manager, &mut wal, &mut tree)?;
            assert!(
                txn.txn_id().get() > 1,
                "TxnId must not reuse prior IDs after reopen"
            );
            txn.put(b"second", b"2")?;
            txn.commit()?;
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(temp_path("reopen-txn-wal"));
        let _ = fs::remove_file(temp_path("reopen-txn-storage"));
        assert!(result.is_ok(), "reopen TxnId test failed: {result:?}");
    }

    #[test]
    fn reopen_continues_allocating_unique_commit_timestamps() {
        let wal_path = temp_path("reopen-ts-wal");
        let storage_path = temp_path("reopen-ts-storage");
        let result = (|| {
            {
                let manager = TransactionManager::new();
                let mut tree = BTree::create(&storage_path, 32)?;
                let mut wal = WalWriter::create(&wal_path)?;
                let mut txn = Transaction::begin(&manager, &mut wal, &mut tree)?;
                txn.put(b"first", b"1")?;
                txn.commit()?;
                drop(tree);
                drop(wal);
            }

            let manager = TransactionManager::open(&wal_path)?;
            let mut tree = BTree::open(&storage_path, 32)?;
            let mut wal = WalWriter::open(&wal_path)?;
            let mut txn = Transaction::begin(&manager, &mut wal, &mut tree)?;
            txn.put(b"second", b"2")?;
            let result = txn.commit()?;
            assert!(
                result.commit_timestamp.get() > 1,
                "CommitTimestamp must not reuse prior values after reopen"
            );
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(temp_path("reopen-ts-wal"));
        let _ = fs::remove_file(temp_path("reopen-ts-storage"));
        assert!(
            result.is_ok(),
            "reopen commit timestamp test failed: {result:?}"
        );
    }

    #[test]
    fn commit_twice_is_rejected() {
        let manager = TransactionManager::new();
        let storage_path = temp_path("double-commit-storage");
        let wal_path = temp_path("double-commit-wal");
        let mut tree = BTree::create(&storage_path, 32).unwrap();
        let mut wal = WalWriter::create(&wal_path).unwrap();
        let mut txn = Transaction::begin(&manager, &mut wal, &mut tree).unwrap();
        txn.put(b"k", b"v").unwrap();
        txn.commit().unwrap();
        let error = txn.commit();
        assert!(error.is_err(), "double commit must fail");
        let _ = fs::remove_file(storage_path);
        let _ = fs::remove_file(wal_path);
    }

    #[test]
    fn abort_then_commit_is_rejected() {
        let manager = TransactionManager::new();
        let storage_path = temp_path("abort-commit-storage");
        let wal_path = temp_path("abort-commit-wal");
        let mut tree = BTree::create(&storage_path, 32).unwrap();
        let mut wal = WalWriter::create(&wal_path).unwrap();
        let mut txn = Transaction::begin(&manager, &mut wal, &mut tree).unwrap();
        txn.abort().unwrap();
        let error = txn.commit();
        assert!(error.is_err(), "commit after abort must fail");
        let _ = fs::remove_file(storage_path);
        let _ = fs::remove_file(wal_path);
    }

    #[test]
    fn write_after_commit_is_rejected() {
        let manager = TransactionManager::new();
        let storage_path = temp_path("post-commit-storage");
        let wal_path = temp_path("post-commit-wal");
        let mut tree = BTree::create(&storage_path, 32).unwrap();
        let mut wal = WalWriter::create(&wal_path).unwrap();
        let mut txn = Transaction::begin(&manager, &mut wal, &mut tree).unwrap();
        txn.commit().unwrap();
        let error = txn.put(b"k", b"v");
        assert!(error.is_err(), "put after commit must fail");
        let _ = fs::remove_file(storage_path);
        let _ = fs::remove_file(wal_path);
    }

    #[test]
    fn write_after_abort_is_rejected() {
        let manager = TransactionManager::new();
        let storage_path = temp_path("post-abort-storage");
        let wal_path = temp_path("post-abort-wal");
        let mut tree = BTree::create(&storage_path, 32).unwrap();
        let mut wal = WalWriter::create(&wal_path).unwrap();
        let mut txn = Transaction::begin(&manager, &mut wal, &mut tree).unwrap();
        txn.abort().unwrap();
        let error = txn.put(b"k", b"v");
        assert!(error.is_err(), "put after abort must fail");
        let _ = fs::remove_file(storage_path);
        let _ = fs::remove_file(wal_path);
    }

    #[test]
    fn concurrent_transactions_serialized_via_mutex() {
        let wal_path = temp_path("concurrent-txn");
        let storage_path = temp_path("concurrent-txn-storage");
        let result = (|| {
            let manager = std::sync::Arc::new(TransactionManager::new());
            let wal = std::sync::Arc::new(Mutex::new(WalWriter::create(&wal_path)?));
            let tree = std::sync::Arc::new(Mutex::new(BTree::create(&storage_path, 32)?));
            let thread_count = 8;
            let per_thread = 10;
            let mut handles = Vec::with_capacity(thread_count);

            for t in 0..thread_count {
                let manager = manager.clone();
                let wal = wal.clone();
                let tree = tree.clone();
                handles.push(thread::spawn(move || {
                    for i in 0..per_thread {
                        let mut wal_guard = wal.lock().unwrap();
                        let mut tree_guard = tree.lock().unwrap();
                        let mut txn =
                            Transaction::begin(&manager, &mut *wal_guard, &mut *tree_guard)
                                .unwrap();
                        let key = format!("t{t}-i{i}");
                        txn.put(key.as_bytes(), b"value").unwrap();
                        txn.commit().unwrap();
                    }
                }));
            }

            for handle in handles {
                handle.join().unwrap();
            }

            let mut tree = BTree::open(&storage_path, 32)?;
            for t in 0..thread_count {
                for i in 0..per_thread {
                    let key = format!("t{t}-i{i}");
                    assert_eq!(tree.get(key.as_bytes())?, Some(b"value".to_vec()));
                }
            }
            Ok::<(), plomid_core::PlomidError>(())
        })();
        let _ = fs::remove_file(temp_path("concurrent-txn-wal"));
        let _ = fs::remove_file(temp_path("concurrent-txn-storage"));
        assert!(
            result.is_ok(),
            "concurrent transaction test failed: {result:?}"
        );
    }
}
