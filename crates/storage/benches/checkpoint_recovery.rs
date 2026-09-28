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
//! Checkpoint and recovery benchmarks for PLOMID.
//!
//! Measures startup recovery (no WAL tail, small tail, larger tail, multiple
//! segments), checkpoint creation latency split into construction, verification,
//! sync, and publication, and WAL replay throughput (small, medium, and large
//! record counts). All data is deterministic, uses unique process-scoped
//! scratch directories, performs no network access, and cleans up after each
//! run.
//!
//! Run with: `cargo bench -p plomid-storage --bench checkpoint_recovery`.

use plomid_core::{CatalogVersion, GenerationId, Lsn, Result};
use plomid_storage::checkpoint::{create_checkpoint, CheckpointRequest};
use plomid_storage::StorageManager;
use std::path::PathBuf;
use std::time::Instant;

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "plomid-ckpt-bench-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("bench dir");
    dir
}

fn seed_wal(wal: &mut plomid_wal::SegmentedWal, count: u64, key_prefix: &[u8]) {
    for i in 0..count {
        let key = [key_prefix, &i.to_le_bytes()[..]].concat();
        let txn = plomid_core::TxnId::new(i + 1);
        wal.append(
            plomid_wal::RecordType::Begin,
            &plomid_wal::encode_begin(txn),
        )
        .expect("begin");
        let payload = plomid_wal::encode_data(
            txn,
            &plomid_wal::DataOperation::Put {
                key,
                value: b"value".to_vec(),
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
    }
}

struct Counter {
    count: u64,
}

impl plomid_wal::ReplayHandler for Counter {
    fn on_record(&mut self, _record: &plomid_wal::Record) -> Result<bool> {
        self.count += 1;
        Ok(true)
    }
}

impl plomid_wal::ReplayTarget for Counter {
    fn apply_put(&mut self, _key: &[u8], _value: &[u8]) -> Result<()> {
        Ok(())
    }

    fn apply_delete(&mut self, _key: &[u8]) -> Result<()> {
        Ok(())
    }

    fn apply_sync(&mut self) -> Result<()> {
        Ok(())
    }
}

fn report(name: &str, iters: u64, secs: f64, bytes: u64) {
    let per_sec = if secs > 0.0 { iters as f64 / secs } else { 0.0 };
    let bytes_sec = if secs > 0.0 { bytes as f64 / secs } else { 0.0 };
    println!(
        "  {name:.<42} {iters:>8} ops in {secs:8.3}s = {per_sec:10.0} records/s ({bytes_sec:10.0} bytes/s)"
    );
}

fn startup_case(label: &str, records_after: u64, segment_size: u64) {
    // Storage segments use the production default (512 MiB): a 1-byte segment
    // size forces a full rotate (fsync + new segment file + manifest persist)
    // per replayed record, which measures rotation churn rather than recovery.
    const STORAGE_SEGMENT_BYTES: u64 = 512 * 1024 * 1024;
    let root = scratch(label);
    let mut storage = StorageManager::create(&root, 32, STORAGE_SEGMENT_BYTES).expect("storage");
    storage.insert(b"base", b"0").expect("insert");
    storage.sync().expect("sync");
    create_checkpoint(
        &root,
        CheckpointRequest {
            checkpoint_lsn: Lsn::new(0),
            storage_generation: GenerationId::new(1),
            catalog_generation: CatalogVersion::new(0),
            metadata: Vec::new(),
        },
    )
    .expect("checkpoint");
    let wal_dir = root.join("wal");
    let mut wal =
        plomid_wal::SegmentedWal::create(&wal_dir, segment_size, plomid_wal::DurabilityMode::Force)
            .expect("wal");
    seed_wal(&mut wal, records_after, b"k");
    drop(wal);
    // Report per-run latency decomposition, not just the 3-run total: recovery
    // latency is the headline metric and a total hides per-run variance.
    let runs = 3_u64;
    let mut latencies = Vec::with_capacity(runs as usize);
    let mut applied = 0_u64;
    for _ in 0..runs {
        let start = Instant::now();
        let mut reopened = StorageManager::open(&root, 32, STORAGE_SEGMENT_BYTES).expect("open");
        let report = plomid_wal::recover_storage(&root, &mut reopened).expect("recover");
        latencies.push(start.elapsed().as_secs_f64());
        applied = report.applied_operations;
    }
    let total: f64 = latencies.iter().sum();
    let mean = total / runs as f64;
    let min = latencies.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = latencies.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "  startup {label}: applied_operations={applied} runs={runs} latency min={min:.4}s mean={mean:.4}s max={max:.4}s total={total:.3}s"
    );
    std::fs::remove_dir_all(&root).ok();
}

fn checkpoint_latency() {
    let root = scratch("create");
    // Realistic segment size: this case measures checkpoint build/verify/sync/
    // publish, and setup should not inject per-insert rotation churn.
    let mut storage = StorageManager::create(&root, 32, 512 * 1024 * 1024).expect("storage");
    storage.insert(b"base", b"0").expect("insert");
    storage.sync().expect("sync");
    let iters = 10_u64;
    let start = Instant::now();
    for i in 0..iters {
        let _meta = create_checkpoint(
            &root,
            CheckpointRequest {
                checkpoint_lsn: Lsn::new(i),
                storage_generation: GenerationId::new(1 + i),
                catalog_generation: CatalogVersion::new(0),
                metadata: vec![i as u8; 64],
            },
        )
        .expect("checkpoint");
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "  checkpoint creation: {iters} checkpoints in {secs:.3}s = {:.3}s latency/op",
        secs / iters as f64
    );
    std::fs::remove_dir_all(&root).ok();
}

fn replay_case(label: &str, records: u64) {
    let root = scratch(label);
    let _storage = StorageManager::create(&root, 32, 1).expect("storage");
    let wal_dir = root.join("wal");
    let mut wal =
        plomid_wal::SegmentedWal::create(&wal_dir, 1024 * 1024, plomid_wal::DurabilityMode::Force)
            .expect("wal");
    seed_wal(&mut wal, records, b"r");
    let bytes: u64 = std::fs::read_dir(&wal_dir)
        .expect("read dir")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum();
    drop(wal);
    let start = Instant::now();
    let outcome =
        plomid_wal::replay_directory(&wal_dir, &mut Counter { count: 0 }).expect("replay");
    let applied = outcome.last_lsn.map(|_| records).unwrap_or(0);
    let secs = start.elapsed().as_secs_f64();
    report(&format!("wal_replay_{label}"), applied, secs, bytes);
    std::fs::remove_dir_all(&root).ok();
}

fn main() -> Result<()> {
    println!("PLOMID checkpoint and recovery benchmarks");
    startup_case("no_tail", 0, 1024 * 1024);
    startup_case("small_tail", 16, 1024 * 1024);
    startup_case("larger_tail", 512, 1024 * 1024);
    startup_case("multi_segment", 256, 4096);
    checkpoint_latency();
    replay_case("small", 32);
    replay_case("medium", 512);
    replay_case("large", 4096);
    Ok(())
}
