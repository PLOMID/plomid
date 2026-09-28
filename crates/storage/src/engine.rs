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
//! Transaction-aware storage engine contract for PLOMID.
//!
//! [`StorageEngine`] is the primary entry point for all higher-layer storage
//! operations. It coordinates B+Tree state, the WAL, the
//! [`TransactionManager`](plomid_txn::TransactionManager), and recovery so
//! that callers never interact with physical storage components directly.
//!
//! # V1 Read Semantics
//!
//! Reads observe the last committed B+Tree state. Uncommitted transaction
//! writes remain buffered and are invisible until [`commit`](StorageEngineTransaction::commit)
//! succeeds. Read-your-own-writes is not supported in V1; there is no MVCC,
//! snapshot isolation, or version chain.
//!
//! # V1 Write Semantics
//!
//! All writes must flow through a transaction obtained from [`begin`](StorageEngine::begin).
//! The transaction records every operation in the WAL, buffers them in memory,
//! and only applies them to the B+Tree on [`commit`](StorageEngineTransaction::commit).
//! [`abort`](StorageEngineTransaction::abort) discards buffered operations.
//!
//! # Concurrency
//!
//! V1 is single-writer. The engine and its transactions must be serialized by
//! the caller (for example, behind a `Mutex`). Concurrent access is undefined
//! behavior.

use plomid_core::{CommitTimestamp, Result, TxnId};
use std::path::Path;

/// Result of a successful transaction commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitResult {
    /// The transaction's identity.
    pub txn_id: TxnId,
    /// The logical commit timestamp assigned at commit time.
    pub commit_timestamp: CommitTimestamp,
}

/// Lifecycle state of a storage transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    /// Transaction is accepting operations.
    Active,
    /// Transaction has committed successfully.
    Committed,
    /// Transaction has been aborted.
    Aborted,
}

/// A transaction handle returned by [`StorageEngine::begin`].
///
/// All data-modifying operations are buffered in memory and recorded in the
/// WAL immediately, but are not applied to the underlying B+Tree until
/// [`Self::commit`] is called.
pub trait StorageEngineTransaction<'a>: Send {
    /// Reserves the database write lane for a read/modify/write statement.
    ///
    /// `domain` scopes the reservation (conventionally the statement's target
    /// table name bytes): concurrent statements on independent domains run in
    /// parallel, while statements in the same domain keep serialized
    /// read/modify/write semantics. Lightweight transaction implementations
    /// do not need a separate write reservation, so the default is a no-op.
    fn lock_for_write(&mut self, _domain: &[u8]) -> Result<()> {
        Ok(())
    }

    /// Reserves exclusive write access to specific row keys for this
    /// transaction (row-level write locking for UPDATE/DELETE).
    ///
    /// Called by a statement AFTER it has discovered the affected row keys and
    /// BEFORE it re-reads/stages those rows, so concurrent statements on
    /// different rows run in parallel while two statements targeting the same
    /// row serialize. Implementations must guarantee that a transaction that
    /// re-reads a key after this call observes every transaction that held
    /// the key's lock before (read-committed semantics), and that releasing
    /// happens at commit/abort. The default is a no-op for engines that
    /// serialize writes elsewhere.
    fn lock_rows(&mut self, _keys: &[Vec<u8>]) -> Result<()> {
        Ok(())
    }

    /// Takes a SHARED lane on `domain` (conventionally the table bytes): any
    /// number of transactions may hold it concurrently; it conflicts only
    /// with an exclusive table lane taken by a conservative writer that the
    /// reservation scheme cannot cover. Released at commit/abort. The
    /// default is a no-op for engines that serialize writes elsewhere.
    fn lock_shared(&mut self, _domain: &[u8]) -> Result<()> {
        Ok(())
    }

    /// Reserves unique-key conflict domains for this transaction: one entry
    /// per `(index name, canonical normalized value)` the statement proposes.
    /// While held, another transaction attempting the same reservation
    /// fails or waits (per the implementation's existing conflict
    /// semantics); different values never interact. Reservations are
    /// transient runtime coordination — the durable B+Tree unique index
    /// remains the authoritative uniqueness state — and are released at
    /// commit/abort. Keys are acquired in the implementation's canonical
    /// (bytewise) order so concurrent multi-reservation statements cannot
    /// form a wait cycle. The default is a no-op for engines that serialize
    /// writes elsewhere.
    fn lock_unique(&mut self, _keys: &[(String, Vec<u8>)]) -> Result<()> {
        Ok(())
    }

    /// Allocates the next internal row id for `table`.
    ///
    /// Engines that serialize writes keep their transactional meta-key
    /// protocol and therefore do NOT implement this method: the executor
    /// detects the absent implementation (empty `Err` detail) and falls back
    /// to the meta-key allocation. Concurrent engines MUST implement this
    /// atomically so independent INSERTs on the same table do not stage the
    /// same row id while racing, and so ids never collide with ids already
    /// present in durable state (the seed derives from committed rows).
    fn allocate_rowid(&mut self, _table: &str) -> Result<i64> {
        Err(plomid_core::PlomidError::new(
            plomid_core::ErrorKind::Unsupported,
            "",
        ))
    }

    /// Inserts or replaces `value` for `key` within this transaction.
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()>;

    /// Removes `key` within this transaction.
    fn delete(&mut self, key: &[u8]) -> Result<()>;

    /// Returns the value visible to this transaction for one key.
    ///
    /// Implementations may use a direct point lookup. The default keeps the
    /// contract compatible for small test implementations by narrowing to a
    /// one-key range.
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut end = key.to_vec();
        end.push(0);
        Ok(self
            .scan(Some(key), Some(&end))?
            .into_iter()
            .next()
            .map(|(_, value)| value))
    }

    /// Resolves many keys under this transaction's snapshot in one batch.
    ///
    /// Semantically this is [`Self::get`] applied to every key in order, with
    /// the same read-your-writes overlay and the same visibility rules. Batch
    /// resolution exists because an index probe or join probe fetches K
    /// candidate keys that all belong to one statement: the default keeps
    /// per-key semantics, while engines with a concurrent read path can take
    /// the read state once for the whole batch instead of once per key.
    fn get_many(&mut self, keys: &[Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>> {
        keys.iter().map(|key| self.get(key)).collect()
    }

    /// Scans committed storage while retaining this transaction's write
    /// buffer. V1 uses this for transactional UPDATE and DELETE.
    fn scan(
        &mut self,
        _start: Option<&[u8]>,
        _end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        Err(plomid_core::PlomidError::new(
            plomid_core::ErrorKind::Unsupported,
            "transactional scan is not supported",
        ))
    }

    /// Commits the transaction, making all buffered changes durable.
    ///
    /// Writes a `Commit` record carrying the allocated commit timestamp,
    /// fsyncs the WAL, applies all buffered operations to the B+Tree, and
    /// syncs the tree to disk. Returns the [`CommitResult`] on success.
    /// Further operations on the transaction after a successful commit will
    /// return an error.
    fn commit(&mut self) -> Result<CommitResult>;

    /// Aborts the transaction, discarding all buffered changes.
    ///
    /// Writes an `Abort` record and fsyncs the WAL. Buffered operations are
    /// not applied to the B+Tree. Further operations on the transaction after
    /// a successful abort will return an error.
    fn abort(&mut self) -> Result<()>;

    /// Discards a validation-only transaction without claiming a durable
    /// rollback. The default preserves durable behavior for other engines.
    fn abort_preview(&mut self) -> Result<()> {
        self.abort()
    }

    /// Returns the transaction's identity.
    fn txn_id(&self) -> TxnId;

    /// Returns the transaction's current lifecycle state.
    fn state(&self) -> TransactionState;
}

/// Transaction-aware durable key/value storage engine.
///
/// This is the contract consumed by higher layers. The production
/// implementation lives in [`plomid_txn`](plomid_txn) and coordinates the
/// B+Tree, WAL, and [`TransactionManager`](plomid_txn::TransactionManager).
/// Keys and values are opaque byte strings.
///
/// Implementations must be `Send + Sync`. Individual methods must provide the
/// engine's documented concurrency and durability guarantees. V1 is
/// single-writer; callers must serialize access.
pub trait StorageEngine: Send + Sync {
    /// The transaction type returned by [`Self::begin`].
    type Transaction<'a>: StorageEngineTransaction<'a>
    where
        Self: 'a;

    /// Opens an existing storage instance and recovers committed transactions.
    ///
    /// If the WAL contains uncommitted or aborted transactions they are
    /// ignored. The returned engine is ready for new transactions.
    fn open(storage_path: &Path, wal_path: &Path, pool_capacity: usize) -> Result<Self>
    where
        Self: Sized;

    /// Creates a fresh storage instance, truncating any existing state.
    ///
    /// Both the page file and the WAL are truncated. The returned engine has
    /// no prior data.
    fn create(storage_path: &Path, wal_path: &Path, pool_capacity: usize) -> Result<Self>
    where
        Self: Sized;

    /// Begins a new transaction.
    ///
    /// The returned transaction is in the [`TransactionState::Active`] state
    /// and is ready to accept [`Self::Transaction::put`] and
    /// [`Self::Transaction::delete`] operations.
    fn begin(&mut self) -> Result<Self::Transaction<'_>>;

    /// Returns the value for `key`, or `None` when the key is absent.
    ///
    /// Reads observe the last committed state. Uncommitted transaction writes
    /// are not visible.
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Returns key/value pairs in bytewise key order.
    ///
    /// `start` is inclusive and `end` is exclusive when present. An absent
    /// bound means unbounded in that direction. Only committed data is
    /// returned.
    fn scan(&mut self, start: Option<&[u8]>, end: Option<&[u8]>)
        -> Result<Vec<(Vec<u8>, Vec<u8>)>>;

    /// Returns the committed value for each key in `keys`, or `None` where the
    /// key is absent, all resolved under **one** read state.
    ///
    /// Semantically this is exactly [`Self::get`] applied to every key, in
    /// order — the same visibility rules, the same committed snapshot, the same
    /// per-key result. The difference is cost: an engine may take its snapshot
    /// and its read locks once for the whole batch instead of once per key.
    ///
    /// That distinction matters because resolving a batch of candidate keys is
    /// a first-class operation: an index probe returns N row keys and needs N
    /// values, and a join probe does the same per outer row. Resolving those one
    /// at a time meant one snapshot construction and two lock acquisitions per
    /// candidate.
    ///
    /// Implementations that do not override this keep per-key `get` semantics,
    /// so the contract is satisfied by construction rather than by convention.
    fn get_many(&mut self, keys: &[Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>> {
        keys.iter().map(|key| self.get(key)).collect()
    }

    /// Flushes pending state to durable storage.
    ///
    /// For the production implementation this fsyncs both the WAL and the
    /// B+Tree page file.
    fn sync(&mut self) -> Result<()>;

    /// Returns the root directory this engine's durable state lives in.
    ///
    /// Higher layers use it to locate the logical object tree
    /// (`objects/databases/...`) that materializes database, schema, and table
    /// objects. The engine owns the root; callers never guess it.
    fn root(&self) -> &Path;

    /// Returns the active transaction identities and the last committed
    /// transaction timestamp.
    ///
    /// MVCC-safe maintenance (compaction, retention release, reclamation) uses
    /// this to prove that no live snapshot can still require a version before
    /// that version is dropped. It exposes the state the engine already keeps;
    /// it does not introduce a second transaction or visibility system.
    fn mvcc_safety(&self) -> Result<(Vec<u64>, u64)>;

    /// Returns the next transaction identity the engine will hand out.
    ///
    /// Retention release compares this mark against [`Self::mvcc_safety`]:
    /// every transaction active *below* the mark began before the release
    /// evaluation and may hold a snapshot older than the newest published
    /// generation, so its generations stay retained. Transactions at or above
    /// the mark began afterwards and can only observe the newest state.
    ///
    /// The default returns `u64::MAX`, which makes the comparison strict:
    /// engines without transaction-identity tracking only release when no
    /// transaction is active at all (the historical behavior).
    fn txn_issue_mark(&self) -> Result<u64> {
        Ok(u64::MAX)
    }

    /// Range scan stopping after `limit` visible rows.
    ///
    /// Semantically `self.scan(start, end)` truncated to `limit` rows; the
    /// limit applies to visible rows, so predicates evaluated downstream
    /// still observe scan order. Engines whose read path can stop the walk
    /// early override this; the default preserves behavior exactly.
    fn scan_limit(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut rows = self.scan(start, end)?;
        rows.truncate(limit);
        Ok(rows)
    }

    /// Counts visible rows in `[start, end)` without materializing them.
    ///
    /// Semantically `self.scan(start, end).len()`. Engines whose read path
    /// can count without cloning payloads override this; the default
    /// preserves behavior exactly.
    fn count_range(&mut self, start: Option<&[u8]>, end: Option<&[u8]>) -> Result<u64> {
        Ok(self.scan(start, end)?.len() as u64)
    }

    /// Streams visible rows in `[start, end)` to `f` in bounded chunks.
    ///
    /// Semantically `self.scan(start, end)` split into `chunk_size` pieces:
    /// `f` observes every visible `(key, payload)` pair in key order exactly
    /// once under a single statement snapshot. Returning `false` stops the
    /// walk early (e.g. `LIMIT`); returning `true` continues.
    ///
    /// The bound is on executor-held memory: at most one chunk of entries is
    /// materialized at a time, so `GROUP BY`/`COUNT`/`ORDER BY ... LIMIT`
    /// accumulate only aggregate/heap state instead of a full-table `Vec`.
    /// Production engines override this to hold one snapshot across chunks
    /// while releasing the version lock between them; the default preserves
    /// behavior exactly for engines without a paged read path.
    // Callback shape is the stable trait contract (implementors + callers
    // across crates); spelling it inline keeps the contract visible.
    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    fn scan_for_each(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        chunk_size: usize,
        f: &mut dyn FnMut(&[(Vec<u8>, Vec<u8>)]) -> Result<bool>,
    ) -> Result<()> {
        let chunk = chunk_size.max(1);
        let rows = self.scan(start, end)?;
        for window in rows.chunks(chunk) {
            if !f(window)? {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_result_fields_are_accessible() {
        let txn_id = TxnId::new(1);
        let ts = CommitTimestamp::new(1);
        let result = CommitResult {
            txn_id,
            commit_timestamp: ts,
        };
        assert_eq!(result.txn_id, txn_id);
        assert_eq!(result.commit_timestamp, ts);
    }

    #[test]
    fn transaction_state_variants() {
        let states = [
            TransactionState::Active,
            TransactionState::Committed,
            TransactionState::Aborted,
        ];
        assert_eq!(states.len(), 3);
    }
}
