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
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use plomid_executor::Executor;
use plomid_txn::PlomidStorageEngine;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn root(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "plomid-bench-timeseries-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn seed(label: &str, vacuum: bool) -> Executor<PlomidStorageEngine> {
    let storage = root(label);
    let wal = storage.with_extension("wal");
    let engine = PlomidStorageEngine::create(&storage, &wal, 64).expect("engine");
    let mut session = Executor::new(engine).expect("executor");
    session
        .execute("CREATE TABLE readings (id INTEGER, observed_at TIMESTAMP, value INTEGER, payload TEXT);")
        .expect("table");
    let values = (0..10_000)
        .map(|id| {
            let day = 1 + id / 28;
            let year = 2024 + (day - 1) / 336;
            let month = 1 + ((day - 1) % 336) / 28;
            let month_day = 1 + (day - 1) % 28;
            format!("({id}, '{year:04}-{month:02}-{month_day:02} 00:00:00', {id}, 'wide-{id:04}')")
        })
        .collect::<Vec<_>>()
        .join(",");
    session
        .execute(&format!("INSERT INTO readings VALUES {values};"))
        .expect("rows");
    if vacuum {
        session.execute("VACUUM readings;").expect("vacuum");
    }
    session
}

fn bench_query(c: &mut Criterion) {
    let mut hot = seed("hot", false);
    let mut columnar = seed("columnar", true);
    let sql = "SELECT id, value FROM readings WHERE observed_at BETWEEN TIMESTAMP '2024-01-10 00:00:00' AND TIMESTAMP '2024-01-12 00:00:00' ORDER BY observed_at DESC";
    let mut group = c.benchmark_group("time_series_select");
    group.sample_size(10);
    group.bench_function("hot_row_store", |b| {
        b.iter(|| black_box(hot.execute(sql).expect("hot query")))
    });
    group.bench_function("columnar_brin_zonemap", |b| {
        b.iter(|| black_box(columnar.execute(sql).expect("columnar query")))
    });
    group.finish();
}

criterion_group!(benches, bench_query);
criterion_main!(benches);
