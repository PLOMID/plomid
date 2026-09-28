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
//! Checkpoint integration tests spanning the storage and WAL crates.
//!
//! These tests exercise the full durable lifecycle: physical storage creation,
//! checkpoint publication (BUILD -> FLUSH -> VERIFY -> SYNC -> PUBLISH), WAL
//! appends after a boundary, crash/torn-tail handling, corruption fallback,
//! stale rejection, and startup recovery that reconstructs state by replaying
//! only the WAL required after the selected checkpoint.
use plomid_core::{CatalogVersion, GenerationId, Lsn, TxnId};
use plomid_storage::checkpoint::{
    create_checkpoint, create_checkpoint_with_fail_point, discover, latest_valid, load_checkpoint,
    validate_storage_physical, CheckpointMetadata, CheckpointRequest, FailPoint,
};
use plomid_storage::StorageManager;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "plomid-ckpt-it-{label}-{}-{id}",
        std::process::id()
    ))
}

fn request(lsn: u64, storage: u64, catalog: u64) -> CheckpointRequest {
    CheckpointRequest {
        checkpoint_lsn: Lsn::new(lsn),
        storage_generation: GenerationId::new(storage),
        catalog_generation: CatalogVersion::new(catalog),
        metadata: vec![7, 8, 9],
    }
}

/// Appends one committed `Put` transaction and forces it durable.
///
/// Returns the LSN of the Commit record, which is the durable boundary the
/// checkpoint metadata must not exceed.
fn append_kv(wal: &mut plomid_wal::SegmentedWal, txn_id: u64, key: &[u8], value: &[u8]) -> Lsn {
    let txn = TxnId::new(txn_id);
    wal.append(
        plomid_wal::RecordType::Begin,
        &plomid_wal::encode_begin(txn),
    )
    .expect("begin");
    let payload = plomid_wal::encode_data(
        txn,
        &plomid_wal::DataOperation::Put {
            key: key.to_vec(),
            value: value.to_vec(),
        },
    )
    .expect("data payload");
    wal.append(plomid_wal::RecordType::Data, &payload)
        .expect("data");
    let commit = wal
        .append(
            plomid_wal::RecordType::Commit,
            &plomid_wal::encode_commit(txn),
        )
        .expect("commit");
    wal.commit(commit).expect("group commit");
    commit
}

/// Counts every WAL record handed to it, to verify the replay boundary.
struct Counter {
    count: u64,
}

impl plomid_wal::ReplayTarget for Counter {
    fn apply_put(&mut self, _key: &[u8], _value: &[u8]) -> plomid_core::Result<()> {
        Ok(())
    }

    fn apply_delete(&mut self, _key: &[u8]) -> plomid_core::Result<()> {
        Ok(())
    }

    fn apply_sync(&mut self) -> plomid_core::Result<()> {
        Ok(())
    }
}

impl plomid_wal::ReplayHandler for Counter {
    fn on_record(&mut self, _record: &plomid_wal::Record) -> plomid_core::Result<bool> {
        self.count += 1;
        Ok(true)
    }
}

#[test]
fn checkpoint_then_wal_recovery_reconstructs_state() {
    let root = scratch("recover");
    let result = (|| {
        let mut storage = StorageManager::create(&root, 32, 1)?;
        let wal_dir = root.join("wal");
        let mut wal = plomid_wal::SegmentedWal::create(
            &wal_dir,
            1024 * 1024,
            plomid_wal::DurabilityMode::Force,
        )?;
        storage.insert(b"base", b"0")?;
        storage.sync()?;
        let checkpoint = create_checkpoint(
            &root,
            CheckpointRequest {
                checkpoint_lsn: Lsn::new(0),
                storage_generation: GenerationId::new(1),
                catalog_generation: CatalogVersion::new(0),
                metadata: Vec::new(),
            },
        )?;
        assert_eq!(checkpoint.checkpoint_generation.get(), 1);
        append_kv(&mut wal, 1, b"a", b"1");
        append_kv(&mut wal, 2, b"b", b"2");
        assert!(
            plomid_wal::durable_lsn(&wal_dir)?.is_none(),
            "no WAL checkpoint marker has been published"
        );
        drop(wal);
        drop(storage);
        let mut reopened = StorageManager::open(&root, 32, 1)?;
        let report = plomid_wal::recover_storage(&root, &mut reopened)?;
        assert_eq!(report.state, plomid_wal::RecoveryState::Ready);
        assert_eq!(report.applied_operations, 2);
        assert_eq!(reopened.get(b"base")?, Some(b"0".to_vec()));
        assert_eq!(reopened.get(b"a")?, Some(b"1".to_vec()));
        assert_eq!(reopened.get(b"b")?, Some(b"2".to_vec()));
        drop(reopened);
        let mut again = StorageManager::open(&root, 32, 1)?;
        let second = plomid_wal::recover_storage(&root, &mut again)?;
        assert_eq!(second.applied_operations, 2);
        assert_eq!(again.get(b"b")?, Some(b"2".to_vec()));
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn wal_replay_respects_checkpoint_boundary() {
    let root = scratch("boundary");
    let result = (|| {
        let _storage = StorageManager::create(&root, 32, 1)?;
        let wal_dir = root.join("wal");
        let mut wal = plomid_wal::SegmentedWal::create(
            &wal_dir,
            1024 * 1024,
            plomid_wal::DurabilityMode::Force,
        )?;
        let boundary = append_kv(&mut wal, 1, b"pre", b"0");
        append_kv(&mut wal, 2, b"post", b"1");
        drop(wal);
        // No checkpoint file: recovery replays the whole WAL.
        let mut counter = Counter { count: 0 };
        let report = plomid_wal::recover_into(&root, &mut counter)?;
        assert_eq!(report.applied_records, 6);
        assert_eq!(report.applied_operations, 2);
        assert_eq!(counter.count, 6);
        // A checkpoint at the first Commit LSN leaves only the second
        // transaction's records to replay.
        create_checkpoint(&root, request(boundary.get(), 1, 0))?;
        let mut counter = Counter { count: 0 };
        let report = plomid_wal::recover_into(&root, &mut counter)?;
        assert_eq!(report.applied_records, 3);
        assert_eq!(report.applied_operations, 1);
        assert_eq!(counter.count, 3);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn corrupt_checkpoint_falls_back_and_stale_is_rejected() {
    let root = scratch("corrupt");
    let result = (|| {
        create_checkpoint(&root, request(1, 1, 0))?;
        create_checkpoint(&root, request(2, 2, 1))?;
        let dir = root.join("checkpoints");
        let newest = dir.join("checkpoint-00000000000000000002.ckpt");
        let mut bytes = fs::read(&newest)?;
        bytes[48] ^= 0xFF;
        fs::write(&newest, bytes)?;
        let fallback = latest_valid(&root)?.expect("fallback");
        assert_eq!(fallback.1.checkpoint_generation.get(), 1);
        assert!(create_checkpoint(&root, request(1, 1, 0)).is_err());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn incomplete_publication_is_ignored() {
    let root = scratch("incomplete");
    let result = (|| {
        create_checkpoint(&root, request(1, 1, 0))?;
        let dir = root.join("checkpoints");
        fs::write(
            dir.join("checkpoint-00000000000000000002.ckpt.tmp"),
            b"torn",
        )?;
        let selected = latest_valid(&root)?.expect("previous");
        assert_eq!(selected.1.checkpoint_generation.get(), 1);
        for point in [
            FailPoint::BeforeBuild,
            FailPoint::AfterBuild,
            FailPoint::AfterFlush,
            FailPoint::DuringVerify,
            FailPoint::BeforeSync,
            FailPoint::AfterSync,
            FailPoint::DuringPublish,
        ] {
            assert!(create_checkpoint_with_fail_point(&root, request(2, 2, 1), point).is_err());
            assert_eq!(
                latest_valid(&root)?
                    .expect("prev")
                    .1
                    .checkpoint_generation
                    .get(),
                1
            );
        }
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn physical_validation_rejects_missing_storage() {
    let root = scratch("physical");
    let result = (|| {
        let _storage = StorageManager::create(&root, 32, 1)?;
        assert_eq!(validate_storage_physical(&root)?.get(), 1);
        assert!(validate_storage_physical(&root.join("does-not-exist")).is_err());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn truncated_wal_tail_stops_at_boundary() {
    let root = scratch("tail");
    let result = (|| {
        let _storage = StorageManager::create(&root, 32, 1)?;
        let wal_dir = root.join("wal");
        let mut wal = plomid_wal::SegmentedWal::create(
            &wal_dir,
            1024 * 1024,
            plomid_wal::DurabilityMode::Force,
        )?;
        append_kv(&mut wal, 1, b"k", b"v");
        drop(wal);
        let segment = wal_dir.join("WAL-000000000001.dat");
        let len = fs::metadata(&segment)?.len();
        let file = fs::OpenOptions::new().write(true).open(&segment)?;
        file.set_len(len - 1)?;
        file.sync_all()?;
        drop(file);
        let mut counter = Counter { count: 0 };
        let report = plomid_wal::recover_into(&root, &mut counter)?;
        // The torn Commit frame is an incomplete crash tail, not durable data.
        let boundary = report.crash_tail_boundary.expect("incomplete crash tail");
        assert!(
            boundary < len,
            "tail boundary {boundary} must precede {len}"
        );
        assert_eq!(counter.count, 2);
        // An uncommitted tail never contributes to the reconstructed state.
        assert_eq!(report.applied_operations, 0);
        assert_eq!(report.applied_transactions, 0);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn discovery_is_deterministic_and_ignores_foreign_files() {
    let root = scratch("discover");
    let result = (|| {
        create_checkpoint(&root, request(1, 1, 0))?;
        create_checkpoint(&root, request(2, 2, 1))?;
        let dir = root.join("checkpoints");
        fs::write(dir.join("notes.txt"), b"foreign")?;
        let candidates = discover(&root)?;
        assert_eq!(candidates.len(), 2);
        let loaded = load_checkpoint(&candidates[1])?;
        assert_eq!(loaded, latest_valid(&root)?.expect("latest").1);
        assert_eq!(CheckpointMetadata::decode(&loaded.encode()?)?, loaded);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}

/// A deployment whose catalog lives in the key-value store never publishes a
/// storage checkpoint file (a checkpoint must name a catalog version that
/// exists), but it does publish the WAL checkpoint marker. That marker is the
/// durable proof that the prefix it covers is already in the flushed pages, so
/// recovery must start above it instead of replaying the whole write history:
/// measured before this behaviour existed, a 20 000-row table replayed 40 064
/// records / 4.3 MiB / 865 ms on *every* restart, growing with every commit
/// ever made.
#[test]
fn wal_checkpoint_marker_bounds_replay_without_a_storage_checkpoint() {
    let root = scratch("marker-boundary");
    let result = (|| {
        let mut storage = StorageManager::create(&root, 32, 1)?;
        let wal_dir = root.join("wal");
        let mut wal = plomid_wal::SegmentedWal::create(
            &wal_dir,
            1024 * 1024,
            plomid_wal::DurabilityMode::Force,
        )?;
        // Covered by the boundary: must not be applied again.
        append_kv(&mut wal, 1, b"before-1", b"1");
        append_kv(&mut wal, 2, b"before-2", b"2");
        storage.sync()?;
        let boundary = wal.checkpoint()?;
        // After the boundary: must be replayed.
        append_kv(&mut wal, 3, b"after-1", b"3");
        append_kv(&mut wal, 4, b"after-2", b"4");
        drop(wal);
        drop(storage);

        let mut counter = Counter { count: 0 };
        let report = plomid_wal::recover_into(&root, &mut counter)?;
        assert_eq!(report.durable_lsn, Some(boundary));
        let selection = report
            .checkpoint
            .expect("recovery reports the boundary it used");
        assert_eq!(
            selection.checkpoint_lsn, boundary,
            "the WAL checkpoint marker is the replay boundary"
        );
        assert!(
            selection.is_none(),
            "no storage checkpoint was involved in this boundary"
        );
        // Exactly the two post-marker transactions: Begin + Data + Commit each.
        assert_eq!(report.applied_records, 6);
        assert_eq!(report.applied_transactions, 2);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}
