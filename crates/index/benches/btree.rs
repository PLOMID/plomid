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
//! Release benchmarks for the persistent B+Tree index.
//!
//! Measures insert, unique insert, duplicate append, point-lookup hit/miss,
//! delete, sequential and range scans, tree build, and reopen/load at 1K,
//! 10K, and 100K entries. Run with `cargo bench -p plomid-index --bench btree`.

use std::hint::black_box;
use std::time::Instant;

use plomid_core::{GenerationId, IndexId, ObjectId, RowId};
use plomid_index::btree::{BTreeIndex, Bound};

fn temp_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("plomid-bench-btree");
    std::fs::create_dir_all(&dir).expect("create bench dir");
    dir.join(name)
}

fn key(buffer: &mut Vec<u8>, n: u64) {
    buffer.clear();
    buffer.extend_from_slice(&n.to_be_bytes());
}

struct Timer {
    label: &'static str,
    entries: u64,
    start: Instant,
}

impl Timer {
    fn start(label: &'static str, entries: u64) -> Self {
        Self {
            label,
            entries,
            start: Instant::now(),
        }
    }

    fn finish(self) -> (f64, f64) {
        let elapsed = self.start.elapsed().as_secs_f64();
        let per_second = self.entries as f64 / elapsed;
        println!(
            "{:>22} n={:<7} {:>9.1} ops/s  ({elapsed:.3}s)",
            self.label, self.entries, per_second
        );
        (elapsed, per_second)
    }
}

fn bench_insert(entries: u64) {
    let path = temp_path(&format!("insert-{entries}"));
    let _ = std::fs::remove_file(&path);
    let mut index = BTreeIndex::create(
        &path,
        64,
        IndexId::new(1),
        ObjectId::new(1),
        GenerationId::new(1),
        false,
    )
    .expect("create index");
    let mut key_buffer = Vec::with_capacity(16);
    let timer = Timer::start("insert", entries);
    for n in 0..entries {
        key(&mut key_buffer, n);
        index.insert(&key_buffer, RowId::new(n)).expect("insert");
    }
    timer.finish();
    index.sync().expect("sync");

    let timer = Timer::start("point lookup hit", entries);
    for n in 0..entries {
        key(&mut key_buffer, n % entries);
        black_box(index.contains(&key_buffer).expect("contains"));
    }
    timer.finish();

    let missing = [entries, entries + 1, entries + 2];
    let timer = Timer::start("point lookup miss", entries);
    for n in 0..entries {
        key(&mut key_buffer, missing[n as usize % missing.len()] + n);
        black_box(index.contains(&key_buffer).expect("contains"));
    }
    timer.finish();

    let timer = Timer::start("sequential scan", entries);
    let scanned = index.scan_all().expect("scan").len();
    timer.finish();
    assert_eq!(scanned, entries as usize);

    let timer = Timer::start("range scan (1/8 span)", entries);
    let mut scanned = 0_usize;
    for window in 0..8_u64 {
        let start = window * entries / 8;
        let end = start + entries / 8;
        key(&mut key_buffer, start);
        let from = Bound::Included(key_buffer.clone());
        key(&mut key_buffer, end);
        let to = Bound::Excluded(key_buffer.clone());
        scanned += index.range_scan(from, to).expect("range").len();
    }
    timer.finish();
    assert_eq!(scanned, entries as usize);

    let timer = Timer::start("delete", entries);
    for n in 0..entries {
        key(&mut key_buffer, n);
        index
            .delete(&key_buffer, Some(RowId::new(n)), false)
            .expect("delete");
    }
    timer.finish();
    index.close().expect("close");
    let _ = std::fs::remove_file(&path);
}

fn bench_reopen(entries: u64) {
    let path = temp_path(&format!("reopen-{entries}"));
    let _ = std::fs::remove_file(&path);
    {
        let mut index = BTreeIndex::create(
            &path,
            64,
            IndexId::new(1),
            ObjectId::new(1),
            GenerationId::new(1),
            false,
        )
        .expect("create");
        let mut key_buffer = Vec::with_capacity(16);
        for n in 0..entries {
            key(&mut key_buffer, n);
            index.insert(&key_buffer, RowId::new(n)).expect("insert");
        }
        index.sync().expect("sync");
    }
    let timer = Timer::start("reopen/load", entries);
    let mut index = BTreeIndex::open(&path, 64).expect("reopen");
    timer.finish();
    let mut key_buffer = Vec::with_capacity(16);
    key(&mut key_buffer, entries / 2);
    assert!(index.contains(&key_buffer).expect("contains"));
    index.close().expect("close");
    let _ = std::fs::remove_file(&path);
}

fn main() {
    for entries in [1_000_u64, 10_000, 100_000] {
        println!("--- {entries} entries ---");
        bench_insert(entries);
        bench_reopen(entries);
    }
}
