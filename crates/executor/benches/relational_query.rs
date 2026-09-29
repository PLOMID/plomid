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
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use plomid_executor::Executor;
use plomid_txn::PlomidStorageEngine;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn root(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "plomid-bench-relational-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn seed(label: &str, rows: usize) -> Executor<PlomidStorageEngine> {
    let storage = root(label);
    let wal = storage.with_extension("wal");
    let engine = PlomidStorageEngine::create(&storage, &wal, 64).expect("engine");
    let mut executor = Executor::new(engine).expect("executor");
    executor
        .execute("CREATE TABLE events (id INTEGER, group_id INTEGER, amount INTEGER);")
        .expect("table");
    for start in (0..rows).step_by(500) {
        let end = (start + 500).min(rows);
        let values = (start..end)
            .map(|id| format!("({}, {}, {})", id, id % 100, id % 1000))
            .collect::<Vec<_>>()
            .join(",");
        executor
            .execute(&format!("INSERT INTO events VALUES {values};"))
            .expect("insert");
    }
    executor
}

fn bench_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("relational_queries");
    group.sample_size(10);
    for rows in [10_000usize, 100_000, 1_000_000] {
        let mut executor = seed(&format!("{rows}"), rows);
        group.throughput(Throughput::Elements(rows as u64));
        group.bench_with_input(BenchmarkId::new("order_by_limit_10", rows), &rows, |b, _| {
            b.iter(|| {
                black_box(
                    executor
                        .execute("SELECT id, amount FROM events ORDER BY amount DESC, id ASC LIMIT 10;")
                        .expect("order query"),
                )
            })
        });
        group.bench_with_input(BenchmarkId::new("group_count_sum", rows), &rows, |b, _| {
            b.iter(|| {
                black_box(
                    executor
                        .execute(
                            "SELECT group_id, COUNT(*), SUM(amount) FROM events GROUP BY group_id;",
                        )
                        .expect("aggregate query"),
                )
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_queries);
criterion_main!(benches);
