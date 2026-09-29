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
//! Lifecycle integration tests.
//!
//! These tests exercise the authoritative mount/demount lifecycle of
//! [`plomid_lifecycle::LifecycleManager`] against real on-disk storage:
//! clean mount, clean demount, reopen, unclean shutdown, recovery through the
//! real mount path, corruption detection, and illegal lifecycle transitions.

use plomid_core::{ErrorKind, GenerationId, Lsn, PlomidError, Result, TxnId};
use plomid_lifecycle::{
    DemountStage, LifecycleConfig, LifecycleManager, LifecycleState, MountStage,
};
use plomid_storage::checkpoint::discover;
use plomid_storage::layout::DatabaseLayout;
use plomid_wal::{DataOperation, DurabilityMode, RecordType, RecoveryState, SegmentHeaderSize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "plomid-lifecycle-{label}-{}-{id}",
        std::process::id()
    ))
}

fn cleanup(paths: &[&Path]) {
    for path in paths {
        let _ = fs::remove_dir_all(path);
    }
}

fn config() -> LifecycleConfig {
    LifecycleConfig {
        pool_capacity: 32,
        segment_size_bytes: 1024 * 1024,
        wal_segment_size_bytes: 1024 * 1024,
        wal_durability: DurabilityMode::Force,
    }
}

/// Appends one committed `Put` transaction through the gated WAL surface.
fn append_committed(
    manager: &mut LifecycleManager,
    txn_id: u64,
    key: &[u8],
    value: &[u8],
) -> Result<Lsn> {
    let txn = TxnId::new(txn_id);
    manager.wal_append(RecordType::Begin, &plomid_wal::encode_begin(txn))?;
    let payload = plomid_wal::encode_data(
        txn,
        &DataOperation::Put {
            key: key.to_vec(),
            value: value.to_vec(),
        },
    )?;
    manager.wal_append(RecordType::Data, &payload)?;
    let commit = manager.wal_append(RecordType::Commit, &plomid_wal::encode_commit(txn))?;
    manager.wal_commit(commit)?;
    Ok(commit)
}

/// Flips one byte of `path` to simulate on-disk corruption.
fn corrupt_byte(path: &Path, offset: u64) {
    let mut bytes = fs::read(path).expect("read file to corrupt");
    let index = offset as usize;
    assert!(index < bytes.len(), "corruption offset beyond file end");
    bytes[index] ^= 0xFF;
    fs::write(path, bytes).expect("write corrupted file");
}

/// WAL segment file paths of a storage root, in filename order.
fn wal_segment_paths(root: &Path) -> Vec<PathBuf> {
    let wal_dir = root.join("wal");
    let mut paths: Vec<PathBuf> = fs::read_dir(&wal_dir)
        .expect("read wal directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("checkpoint") | Some("checkpoint.tmp")
            )
        })
        .collect();
    paths.sort();
    paths
}

/// Newest published checkpoint file of a storage root, if any.
fn newest_checkpoint(root: &Path) -> Option<PathBuf> {
    discover(root).expect("discover checkpoints").pop()
}

/// CLEAN DEMOUNT: writes, demount, resources closed, reopen works.
/// Also proves the demount checkpoint policy published a storage checkpoint.
#[test]
fn clean_demount_closes_resources_and_reopens() {
    let root = scratch("clean-demount");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        manager.insert(b"beta", b"two")?;
        append_committed(&mut manager, 1, b"gamma", b"three")?;
        manager.demount()?;
        assert_eq!(manager.state(), LifecycleState::Closed);
        assert_eq!(manager.demount_trace(), &FULL_DEMOUNT_ORDER);
        assert!(
            newest_checkpoint(&root).is_some(),
            "clean demount checkpoints durable state"
        );

        manager.mount(&root)?;
        assert_eq!(manager.get(b"beta")?, Some(b"two".to_vec()));
        assert_eq!(manager.get(b"gamma")?, Some(b"three".to_vec()));
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "{result:?}");
}

/// REOPEN: state survives a full mount/demount/mount cycle.
#[test]
fn reopen_preserves_state_across_cycles() {
    let root = scratch("reopen");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        manager.insert(b"key1", b"value1")?;
        manager.sync()?;
        manager.demount()?;

        manager.mount(&root)?;
        assert_eq!(manager.get(b"key1")?, Some(b"value1".to_vec()));
        manager.insert(b"key2", b"value2")?;
        manager.demount()?;

        manager.mount(&root)?;
        assert_eq!(manager.get(b"key1")?, Some(b"value1".to_vec()));
        assert_eq!(manager.get(b"key2")?, Some(b"value2".to_vec()));
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "{result:?}");
}

/// UNCLEAN SHUTDOWN: drop the manager without demount so the WAL
/// holds a committed transaction the storage image does not contain. Mount
/// must recover it.
#[test]
fn unclean_shutdown_recovers_committed_wal_state() {
    let root = scratch("unclean-shutdown");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        manager.insert(b"base", b"0")?;
        // WAL-before-data: commit the transaction to the log, apply it to the
        // tree without flushing, then vanish like a crashed process.
        append_committed(&mut manager, 1, b"unclean", b"recovered")?;
        manager.insert_buffered(b"unclean", b"recovered")?;
        drop(manager);

        let mut manager = LifecycleManager::new(config());
        manager.mount(&root)?;
        assert_eq!(manager.state(), LifecycleState::Ready);
        let report = manager
            .recovery_report()
            .expect("recovery ran during mount");
        assert_eq!(report.state, RecoveryState::Ready);
        assert_eq!(report.applied_transactions, 1);
        assert_eq!(manager.get(b"unclean")?, Some(b"recovered".to_vec()));
        assert_eq!(manager.get(b"base")?, Some(b"0".to_vec()));
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "{result:?}");
}

/// RECOVERY VIA MOUNT: mount (never a direct recovery call) must run
/// checkpoint discovery and replay only the WAL after the boundary.
#[test]
fn mount_invokes_recovery_after_checkpoint_boundary() {
    let root = scratch("recovery-boundary");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        manager.insert(b"base", b"0")?;
        let boundary = manager.checkpoint()?;
        assert!(boundary.get() > 0);
        // After the checkpoint: a committed transaction in the WAL only.
        append_committed(&mut manager, 1, b"after", b"checkpoint")?;
        drop(manager);

        let mut manager = LifecycleManager::new(config());
        manager.mount(&root)?;
        assert_eq!(manager.mount_trace(), &FULL_MOUNT_ORDER);
        let report = manager
            .recovery_report()
            .expect("recovery ran during mount");
        assert_eq!(report.state, RecoveryState::Ready);
        let selection = report.checkpoint.as_ref().expect("checkpoint selected");
        assert_eq!(selection.checkpoint_lsn, boundary);
        assert_eq!(
            report.applied_operations, 1,
            "only post-boundary work applied"
        );
        assert_eq!(manager.get(b"base")?, Some(b"0".to_vec()));
        assert_eq!(manager.get(b"after")?, Some(b"checkpoint".to_vec()));
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "{result:?}");
}

/// CORRUPTED METADATA: a broken device record must fail the
/// mount at the device stage; the manager must never become Ready.
#[test]
fn corrupted_metadata_fails_mount() {
    let root = scratch("corrupted-metadata");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        manager.insert(b"doomed", b"value")?;
        manager.demount()?;

        // Corrupt the durable device identity record. `StorageManager::open`
        // validates it through `DeviceRegistry::discover`, which reports
        // corruption at the device mount stage.
        let layout = DatabaseLayout::new(&root);
        let device_id = layout
            .discover_device_ids()
            .expect("discover devices")
            .into_iter()
            .next()
            .expect("at least one device should exist");
        corrupt_byte(&layout.device_meta_path(device_id), 0);
        let error = manager
            .mount(&root)
            .expect_err("corrupt metadata must fail mount");
        assert_eq!(error.stage(), MountStage::Device);
        assert_eq!(error.kind(), ErrorKind::Corruption);
        assert_eq!(manager.state(), LifecycleState::Failed);
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "{result:?}");
}

/// CORRUPTED CHECKPOINT: a corrupt newest checkpoint must fail the
/// mount with a checkpoint-stage error instead of being silently skipped.
#[test]
fn corrupted_checkpoint_fails_mount() {
    let root = scratch("corrupted-checkpoint");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        manager.insert(b"first", b"1")?;
        manager.checkpoint()?;
        manager.demount()?;
        manager.mount(&root)?;
        manager.insert(b"second", b"2")?;
        manager.checkpoint()?;
        manager.demount()?;

        let newest = newest_checkpoint(&root).expect("two demount checkpoints exist");
        // The checksummed header region [0, 48) is part of every checkpoint
        // file, so a flip there always breaks decode regardless of metadata.
        corrupt_byte(&newest, 20);
        let error = manager
            .mount(&root)
            .expect_err("corrupt checkpoint must fail mount");
        assert_eq!(error.stage(), MountStage::Checkpoint);
        assert_eq!(error.kind(), ErrorKind::Corruption);
        assert_eq!(manager.state(), LifecycleState::Failed);
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "{result:?}");
}

/// CORRUPTED WAL: existing recovery semantics must hold. A torn tail
/// is an incomplete crash tail (recovery succeeds), while corruption of a
/// fully present record fails the mount at the recovery stage.
#[test]
fn corrupted_wal_follows_existing_recovery_semantics() {
    // 8a. Torn tail: garbage after the last valid record is a crash tail.
    let root = scratch("corrupted-wal-tail");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        manager.insert(b"base", b"0")?;
        append_committed(&mut manager, 1, b"tail", b"data")?;
        drop(manager);

        let segment = wal_segment_paths(&root).pop().expect("wal segment exists");
        let mut bytes = fs::read(&segment)?;
        bytes.extend_from_slice(&[0xDE, 0xAD, 0x00]);
        fs::write(&segment, bytes)?;

        let mut manager = LifecycleManager::new(config());
        manager.mount(&root)?;
        let report = manager
            .recovery_report()
            .expect("recovery ran during mount");
        assert!(
            report.crash_tail_boundary.is_some(),
            "tail reported as crash boundary"
        );
        assert_eq!(manager.get(b"tail")?, Some(b"data".to_vec()));
        assert_eq!(manager.get(b"base")?, Some(b"0".to_vec()));
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "torn tail: {result:?}");

    // 8b. Mid-file corruption: a fully present but invalid frame is corruption.
    let root = scratch("corrupted-wal-middle");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        append_committed(&mut manager, 1, b"one", b"1")?;
        append_committed(&mut manager, 2, b"two", b"2")?;
        drop(manager);

        // Corrupt a byte inside the second transaction's payload region:
        // segment header + begin/data/commit frames of txn 1 + begin of txn 2.
        let txn = |id: u64| TxnId::new(id);
        let frame = |payload: usize| {
            u64::try_from(plomid_wal::frame_len_for_payload(payload).expect("frame length"))
                .expect("frame length fits u64")
        };
        let offset = (SegmentHeaderSize as u64)
            + frame(plomid_wal::encode_begin(txn(1)).len())
            + frame(
                plomid_wal::encode_data(
                    txn(1),
                    &DataOperation::Put {
                        key: b"one".to_vec(),
                        value: b"1".to_vec(),
                    },
                )
                .expect("encode")
                .len(),
            )
            + frame(plomid_wal::encode_commit(txn(1)).len())
            + frame(plomid_wal::encode_begin(txn(2)).len())
            + 4;
        let segment = wal_segment_paths(&root).pop().expect("wal segment exists");
        corrupt_byte(&segment, offset);

        let mut manager = LifecycleManager::new(config());
        let error = manager
            .mount(&root)
            .expect_err("corrupt WAL record must fail mount");
        assert_eq!(error.stage(), MountStage::Recovery);
        assert_eq!(error.kind(), ErrorKind::Corruption);
        assert_eq!(manager.state(), LifecycleState::Failed);
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "mid-file corruption: {result:?}");
}

/// DOUBLE MOUNT: mounting a mounted manager is a Conflict and leaves
/// the ready state intact.
#[test]
fn double_mount_is_rejected() {
    let root = scratch("double-mount");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        let error = manager.mount(&root).expect_err("double mount must fail");
        assert_eq!(error.stage(), MountStage::Lifecycle);
        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(manager.state(), LifecycleState::Ready);
        assert_eq!(manager.get(b"none")?, None, "storage still usable");
        manager.demount()?;
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "{result:?}");
}

/// DEMOUNT BEFORE MOUNT: rejected as a lifecycle Conflict; mount of
/// a missing root also fails instead of creating a database.
#[test]
fn demount_before_mount_is_rejected() {
    let missing = scratch("demount-missing");
    let result = {
        let mut manager = LifecycleManager::new(config());
        let error = manager
            .demount()
            .expect_err("demount before mount must fail");
        assert_eq!(error.stage(), DemountStage::Lifecycle);
        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(manager.state(), LifecycleState::Closed);

        let error = manager
            .mount(&missing)
            .expect_err("mount must not create a database");
        assert_eq!(error.stage(), MountStage::Device);
        assert_eq!(manager.state(), LifecycleState::Failed);
        Ok::<(), PlomidError>(())
    };
    cleanup(&[&missing]);
    assert!(result.is_ok(), "{result:?}");
}

/// OPERATION AFTER DEMOUNT: every gated operation fails cleanly
/// instead of touching closed resources; remount still works.
#[test]
fn operations_after_demount_fail_cleanly() {
    let root = scratch("after-demount");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        manager.insert(b"survivor", b"kept")?;
        manager.demount()?;
        assert_eq!(manager.state(), LifecycleState::Closed);

        let attempts = [
            "insert",
            "get",
            "delete",
            "range",
            "sync",
            "wal_append",
            "wal_commit",
            "checkpoint",
            "storage_generation",
        ];
        for attempt in attempts {
            let outcome: Result<()> = (|| {
                match attempt {
                    "insert" => manager.insert(b"x", b"y")?,
                    "get" => {
                        manager.get(b"survivor")?;
                    }
                    "delete" => {
                        manager.delete(b"survivor")?;
                    }
                    "range" => {
                        manager.range(None, None)?;
                    }
                    "sync" => manager.sync()?,
                    "wal_append" => {
                        manager.wal_append(RecordType::Data, b"p")?;
                    }
                    "wal_commit" => {
                        manager.wal_commit(Lsn::new(1))?;
                    }
                    "checkpoint" => {
                        manager.checkpoint()?;
                    }
                    "storage_generation" => {
                        manager.storage_generation()?;
                    }
                    other => unreachable!("uncovered operation {other}"),
                }
                Ok(())
            })();
            let error = outcome.expect_err("closed storage must reject the operation");
            assert_eq!(error.kind(), ErrorKind::Conflict, "operation {attempt}");
            assert!(
                error.detail().unwrap_or_default().contains(attempt),
                "error must identify the operation: {error:?}"
            );
            assert_eq!(manager.state(), LifecycleState::Closed);
        }

        manager.mount(&root)?;
        assert_eq!(manager.get(b"survivor")?, Some(b"kept".to_vec()));
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "{result:?}");
}

const FULL_MOUNT_ORDER: [MountStage; 7] = [
    MountStage::Device,
    MountStage::FormatValidation,
    MountStage::MetadataValidation,
    MountStage::Checkpoint,
    MountStage::Recovery,
    MountStage::Catalog,
    MountStage::Generation,
];

const FULL_DEMOUNT_ORDER: [DemountStage; 5] = [
    DemountStage::StopMutations,
    DemountStage::Flush,
    DemountStage::Checkpoint,
    DemountStage::Sync,
    DemountStage::Close,
];

/// CLEAN MOUNT: create, write, demount, mount again, assert Ready.
#[test]
fn clean_mount_reaches_ready_in_stage_order() {
    let root = scratch("clean-mount");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> Result<()> {
        let mut manager = LifecycleManager::new(config());
        manager.create(&root)?;
        assert_eq!(manager.state(), LifecycleState::Ready);
        assert_eq!(manager.mount_trace(), &FULL_MOUNT_ORDER);
        assert_eq!(manager.active_generation(), Some(GenerationId::new(1)));
        assert!(
            manager.catalog().is_none(),
            "fresh store has no published catalog"
        );

        manager.insert(b"alpha", b"one")?;
        assert_eq!(manager.get(b"alpha")?, Some(b"one".to_vec()));

        manager.demount()?;
        assert_eq!(manager.state(), LifecycleState::Closed);
        assert_eq!(manager.demount_trace(), &FULL_DEMOUNT_ORDER);

        manager.mount(&root)?;
        assert_eq!(manager.state(), LifecycleState::Ready);
        assert_eq!(manager.mount_trace(), &FULL_MOUNT_ORDER);
        assert_eq!(manager.get(b"alpha")?, Some(b"one".to_vec()));
        Ok(())
    })();
    cleanup(&[&root]);
    assert!(result.is_ok(), "{result:?}");
}
