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
//! Hot Row Store benchmarks: single-row and batch CRUD throughput.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use plomid_core::RowId;
use plomid_storage::{Field, Row, StorageEngine};
use plomid_txn::{row_key, HotRowStore, PlomidStorageEngine};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn row(id: i64, text: &str) -> Row {
    Row::new(vec![
        Field::Integer(id),
        Field::String(text.to_owned()),
        Field::Null,
    ])
}

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "plomid-bench-hot-row-{}-{}-{}",
        label,
        std::process::id(),
        id
    ))
}

fn cleanup(p: &PathBuf) {
    let _ = std::fs::remove_dir_all(p);
}

fn make_engine(dir: &PathBuf) -> PlomidStorageEngine {
    PlomidStorageEngine::create(dir, &dir.join("wal"), 32).expect("create")
}

fn bench_insert(c: &mut Criterion) {
    let dir = scratch("insert");
    let mut group = c.benchmark_group("insert");
    group.sample_size(20);
    // batch_100 takes >1.5 s per iteration; give criterion enough budget to
    // finish its samples instead of warning about an incomplete run.
    group.measurement_time(std::time::Duration::from_secs(35));
    for &batch in &[1, 10, 100] {
        let d = scratch(&format!("ins-{batch}"));
        let eng = Arc::new(Mutex::new(make_engine(&d)));
        group.bench_with_input(format!("batch_{batch}"), &batch, |b, &sz| {
            b.iter(|| {
                let mut g = eng.lock().expect("mutex");
                let mut s = HotRowStore::new(&mut g);
                for i in 0..sz {
                    black_box(s.insert(row(i as i64, "bench")).expect("insert"));
                }
            })
        });
        cleanup(&d);
    }
    group.finish();
    cleanup(&dir);
}

fn bench_read(c: &mut Criterion) {
    let dir = scratch("read");
    // Capture the actual seeded ids: the row-id allocator has a process-global
    // floor shared with other benches, so hard-coded id ranges can miss.
    let ids = {
        let mut eng = make_engine(&dir);
        let mut s = HotRowStore::new(&mut eng);
        (0..50)
            .map(|i| s.insert(row(i, "seed")).expect("seed"))
            .collect::<Vec<_>>()
    };
    let eng = PlomidStorageEngine::open(&dir, &dir.join("wal"), 32).expect("open");
    let eng = Arc::new(Mutex::new(eng));
    let mut group = c.benchmark_group("point_read");
    // Criterion enforces a minimum sample size of 10; anything below panics.
    group.sample_size(10);
    group.bench_function("lookup_25", |b| {
        b.iter(|| {
            let mut g = eng.lock().expect("mutex");
            let mut s = HotRowStore::new(&mut g);
            for id in &ids[..25] {
                black_box(s.read(*id).expect("read"));
            }
        })
    });
    group.finish();
    cleanup(&dir);
}

fn bench_scan(c: &mut Criterion) {
    let dir = scratch("scan");
    let ids = {
        let mut eng = make_engine(&dir);
        let mut s = HotRowStore::new(&mut eng);
        (0..1_000)
            .map(|i| s.insert(row(i, "scan")).expect("seed"))
            .collect::<Vec<_>>()
    };
    let mut eng = PlomidStorageEngine::open(&dir, &dir.join("wal"), 32).expect("open");
    let point_key = row_key(ids[500]);
    let range_start = row_key(ids[250]);
    let range_end = row_key(ids[750]);
    let mut group = c.benchmark_group("mvcc_scan");
    group.sample_size(10);
    group.bench_function("point", |b| {
        b.iter(|| black_box(eng.get(&point_key).expect("point")))
    });
    group.bench_function("bounded_range_500", |b| {
        b.iter(|| {
            black_box(
                eng.scan(Some(&range_start), Some(&range_end))
                    .expect("range"),
            )
        })
    });
    group.bench_function("full", |b| {
        b.iter(|| black_box(eng.scan(None, None).expect("full")))
    });
    group.finish();
    cleanup(&dir);
}

fn bench_update(c: &mut Criterion) {
    let dir = scratch("update");
    // Capture the seeded ids: the row-id allocator has a process-global floor
    // shared with other benches, so hard-coded id ranges can miss.
    let ids = {
        let mut eng = make_engine(&dir);
        let mut s = HotRowStore::new(&mut eng);
        (0..100)
            .map(|i| s.insert(row(i, "orig")).expect("seed"))
            .collect::<Vec<_>>()
    };
    let eng = PlomidStorageEngine::open(&dir, &dir.join("wal"), 32).expect("open");
    let eng = Arc::new(Mutex::new(eng));
    let mut group = c.benchmark_group("update");
    group.sample_size(100);
    group.bench_function("update_100", |b| {
        b.iter(|| {
            let mut g = eng.lock().expect("mutex");
            let mut s = HotRowStore::new(&mut g);
            for id in &ids {
                s.update(*id, row(-1, "upd")).expect("update");
                black_box(());
            }
        })
    });
    group.finish();
    cleanup(&dir);
}

fn bench_delete(c: &mut Criterion) {
    let dir = scratch("delete");
    // Deleting is destructive, so each iteration is seeded by `setup` and the
    // timed routine deletes those fresh rows only.
    let eng = Arc::new(Mutex::new(make_engine(&dir)));
    let mut group = c.benchmark_group("delete");
    group.sample_size(100);
    group.bench_function("delete_100", |b| {
        b.iter_batched(
            || {
                let mut g = eng.lock().expect("mutex");
                let mut s = HotRowStore::new(&mut g);
                (0..100)
                    .map(|i| s.insert(row(i, "del")).expect("seed"))
                    .collect::<Vec<_>>()
            },
            |ids| {
                let mut g = eng.lock().expect("mutex");
                let mut s = HotRowStore::new(&mut g);
                for id in &ids {
                    s.delete(*id).expect("delete");
                    black_box(());
                }
            },
            criterion::BatchSize::PerIteration,
        )
    });
    group.finish();
    cleanup(&dir);
}

fn bench_batch_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_insert");
    group.sample_size(50);
    for &sz in &[10, 50, 200] {
        let d = scratch(&format!("bins-{sz}"));
        let eng = Arc::new(Mutex::new(make_engine(&d)));
        group.bench_with_input(format!("bs_{sz}"), &sz, |b, &size| {
            b.iter(|| {
                let mut g = eng.lock().expect("mutex");
                let mut s = HotRowStore::new(&mut g);
                let rows: Vec<Row> = (0..size).map(|i| row(i as i64, "batch")).collect();
                black_box(s.batch_insert(rows).expect("batch"));
            })
        });
        cleanup(&d);
    }
    group.finish();
}

fn bench_batch_update(c: &mut Criterion) {
    let dir = scratch("bupd");
    // Capture the seeded ids; see bench_update for why ids must not be guessed.
    let ids = {
        let mut eng = make_engine(&dir);
        let mut s = HotRowStore::new(&mut eng);
        (0..100)
            .map(|i| s.insert(row(i, "seed")).expect("seed"))
            .collect::<Vec<_>>()
    };
    let eng = PlomidStorageEngine::open(&dir, &dir.join("wal"), 32).expect("open");
    let eng = Arc::new(Mutex::new(eng));
    let mut group = c.benchmark_group("batch_update");
    group.sample_size(50);
    group.bench_function("bs_20", |b| {
        b.iter(|| {
            let mut g = eng.lock().expect("mutex");
            let mut s = HotRowStore::new(&mut g);
            let ups: Vec<(RowId, Row)> = ids[..20].iter().map(|&id| (id, row(-1, "x"))).collect();
            s.batch_update(ups).expect("batch");
            black_box(());
        })
    });
    group.finish();
    cleanup(&dir);
}

fn bench_batch_delete(c: &mut Criterion) {
    let dir = scratch("bdel");
    // Deleting is destructive: setup seeds fresh rows each iteration and the
    // timed routine removes only the first 20 of them.
    let eng = Arc::new(Mutex::new(make_engine(&dir)));
    let mut group = c.benchmark_group("batch_delete");
    group.sample_size(50);
    group.bench_function("bs_20", |b| {
        b.iter_batched(
            || {
                let mut g = eng.lock().expect("mutex");
                let mut s = HotRowStore::new(&mut g);
                (0..100)
                    .map(|i| s.insert(row(i, "seed")).expect("seed"))
                    .collect::<Vec<_>>()
            },
            |ids| {
                let mut g = eng.lock().expect("mutex");
                let mut s = HotRowStore::new(&mut g);
                let tids = ids[..20].to_vec();
                s.batch_delete(tids).expect("batch");
                black_box(());
            },
            criterion::BatchSize::PerIteration,
        )
    });
    group.finish();
    cleanup(&dir);
}

criterion_group!(
    benches,
    bench_insert,
    bench_read,
    bench_scan,
    bench_update,
    bench_delete,
    bench_batch_insert,
    bench_batch_update,
    bench_batch_delete
);
criterion_main!(benches);
