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
//! PLOMID columnar flush benchmarks (hot scan, conversion, write, read).
//!
//! Run with: `cargo bench -p plomid-columnar --bench flush_pipeline`.

use plomid_columnar::{
    encode_chunk, flush, ColumnEncoding, ColumnType, FlushConfig, SegmentReader,
};
use plomid_core::{CatalogVersion, GenerationId, Lsn, ObjectId, SchemaId, SegmentId};
use plomid_storage::{Field, Row};
use std::time::Instant;

fn gid(value: u64) -> GenerationId {
    GenerationId::new(value)
}

fn sid(value: u64) -> SegmentId {
    SegmentId::new(value)
}

const ROWS: usize = 5_000;

fn sample_rows(count: usize) -> Vec<Row> {
    (0..count)
        .map(|i| {
            Row::new(vec![
                Field::Integer(i as i64),
                Field::String(format!("name-{i:06}")),
                if i % 7 == 0 {
                    Field::Null
                } else {
                    Field::Bytes(vec![(i & 0xff) as u8; 16])
                },
            ])
        })
        .collect()
}

fn column_types() -> Vec<ColumnType> {
    vec![ColumnType::Integer, ColumnType::String, ColumnType::Bytes]
}

/// Literal for a filtered scan over column `col_idx`, taken from the middle
/// row so the predicate always matches at least one row.
fn filter_literal(row: &Row, types: &[ColumnType], col_idx: usize) -> Option<Field> {
    let field = row.fields().get(col_idx)?.clone();
    match (&field, types.get(col_idx)?) {
        (Field::Integer(_), ColumnType::Integer)
        | (Field::String(_), ColumnType::String)
        | (Field::Bytes(_), ColumnType::Bytes) => Some(field),
        _ => None,
    }
}

fn mib_per_sec(bytes: u64, secs: f64) -> f64 {
    if secs > 0.0 {
        bytes as f64 / secs / (1024.0 * 1024.0)
    } else {
        0.0
    }
}

fn report(name: &str, rows: u64, bytes: u64, secs: f64, extra: &str) {
    let rows_sec = if secs > 0.0 { rows as f64 / secs } else { 0.0 };
    println!(
        "  {name:.<30} {rows:>7} rows in {secs:8.4}s = {rows_sec:12.0} rows/s \
         ({:8.1} MiB/s, {bytes:>9} bytes) {extra}",
        mib_per_sec(bytes, secs),
    );
}

fn bench_conversion() {
    let rows = sample_rows(ROWS);
    let types = column_types();
    let first = flush(&rows, &types, gid(1), sid(1), &FlushConfig::default()).expect("flush");
    let start = Instant::now();
    let iters = 10_u64;
    for _ in 0..iters {
        let out = flush(&rows, &types, gid(1), sid(1), &FlushConfig::default()).expect("flush");
        std::hint::black_box(out.bytes.len());
    }
    let secs = start.elapsed().as_secs_f64();
    report(
        "row_to_columnar",
        ROWS as u64 * iters,
        first.bytes.len() as u64 * iters,
        secs,
        "",
    );
}

fn bench_write_read() {
    let rows = sample_rows(ROWS);
    let types = column_types();
    let flushed = flush(&rows, &types, gid(9), sid(77), &FlushConfig::default()).expect("flush");
    let raw: usize = rows
        .iter()
        .map(|r| r.encode().map(|b| b.len()).unwrap_or(0))
        .sum();
    let ratio = flushed.bytes.len() as f64 / raw.max(1) as f64;
    let start = Instant::now();
    let iters = 20_u64;
    for _ in 0..iters {
        let reader = SegmentReader::decode(&flushed.bytes).expect("decode");
        let ids: Vec<plomid_core::ColumnId> = reader.columns.iter().map(|c| c.column_id).collect();
        let back = reader
            .read_rows(&flushed.bytes, 0, reader.row_count, &ids)
            .expect("rows");
        std::hint::black_box(back.len());
    }
    let secs = start.elapsed().as_secs_f64();
    report(
        "segment_write_read",
        ROWS as u64 * iters,
        flushed.bytes.len() as u64 * iters,
        secs,
        &format!("ratio={ratio:.3} raw={raw}B image={}B", flushed.bytes.len()),
    );
}

fn bench_materialize_stats() {
    use plomid_columnar::materialize_columns;
    use plomid_columnar::statistics::ValueRef;
    let rows = sample_rows(ROWS);
    let types = column_types();
    let start = Instant::now();
    let iters = 20_u64;
    for _ in 0..iters {
        let cols = materialize_columns(&rows, &types).expect("materialize");
        for col in &cols {
            let mut stats = plomid_columnar::statistics::ColumnStatistics::with_type(
                col.column_id,
                col.row_count,
                0,
                col.column_type,
            );
            for row in 0..col.row_count as usize {
                if col.is_null(row) {
                    stats.observe_value(None);
                } else if let Some(bytes) = col.get_value(row) {
                    stats.observe_value(Some(ValueRef::new(col.column_type, bytes)));
                }
            }
            std::hint::black_box(stats.row_count);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    report("materialize_statistics", ROWS as u64 * iters, 0, secs, "");
}

fn bench_full_flush() {
    use plomid_txn::{HotRowStore, PlomidStorageEngine};
    let dir =
        std::env::temp_dir().join(format!("plomid-columnar-bench-full-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let rows = sample_rows(2_000);
    {
        let mut engine = PlomidStorageEngine::create(&dir, &dir.join("wal"), 32).expect("create");
        HotRowStore::new(&mut engine)
            .batch_insert(rows)
            .expect("seed");
    }
    let start = Instant::now();
    let iters = 3_u64;
    let mut total_bytes = 0_u64;
    let types = column_types();
    for i in 0..iters {
        let gen_root = dir.join(format!("gen-{i}"));
        std::fs::create_dir_all(&gen_root).expect("gen dir");
        let mut engine = PlomidStorageEngine::open(&dir, &dir.join("wal"), 32).expect("open");
        let store = plomid_columnar::ColumnarStore::open(&gen_root).expect("store");
        let schema = plomid_columnar::ColumnarStore::columnar_schema(
            SchemaId::new(7),
            CatalogVersion::new(1),
            &types,
        )
        .expect("schema");
        let publish = plomid_columnar::ColumnarPublish::new(
            ObjectId::new(1),
            schema,
            gid(100 + i),
            gid(100 + i),
            Lsn::new(100 + i),
        );
        let out = store
            .flush_hot_rows(
                &mut engine,
                &types,
                &FlushConfig::default(),
                &publish,
                plomid_columnar::ColumnarFailPoint::None,
            )
            .expect("flush");
        total_bytes += out.bytes.len() as u64;
    }
    let secs = start.elapsed().as_secs_f64();
    report("full_flush_pipeline", 2_000 * iters, total_bytes, secs, "");
    std::fs::remove_dir_all(&dir).ok();
}

fn bench_hot_scan_inner() {
    use plomid_txn::{HotRowStore, PlomidStorageEngine};
    let dir =
        std::env::temp_dir().join(format!("plomid-columnar-bench-scan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let rows = sample_rows(ROWS);
    {
        let mut engine = PlomidStorageEngine::create(&dir, &dir.join("wal"), 32).expect("create");
        HotRowStore::new(&mut engine)
            .batch_insert(rows)
            .expect("seed");
    }
    let mut engine = PlomidStorageEngine::open(&dir, &dir.join("wal"), 32).expect("open");
    let start = Instant::now();
    let iters = 20_u64;
    let mut scanned = 0_usize;
    for _ in 0..iters {
        let found = plomid_columnar::ColumnarStore::snapshot_hot_rows(&mut engine).expect("scan");
        scanned = found.len();
    }
    let secs = start.elapsed().as_secs_f64();
    report("hot_row_scan", scanned as u64 * iters, 0, secs, "");
    std::fs::remove_dir_all(&dir).ok();
}

fn bench_encoding_shapes() {
    use plomid_columnar::materialize_columns;
    // Three workload shapes; Auto picks the smallest stored payload per chunk.
    let seq: Vec<Row> = (0..5000)
        .map(|i| Row::new(vec![Field::Integer(1_700_000_000_000_000 + i * 1_000)]))
        .collect();
    let low_card: Vec<Row> = (0..5000)
        .map(|i| {
            Row::new(vec![Field::String(
                ["red", "green", "blue"][i % 3].to_owned(),
            )])
        })
        .collect();
    let mixed = sample_rows(ROWS);
    let datasets: Vec<(&str, Vec<Row>, Vec<ColumnType>)> = vec![
        (
            "time-series 5k regular timestamps",
            seq,
            vec![ColumnType::Integer],
        ),
        (
            "low-cardinality 5k strings",
            low_card,
            vec![ColumnType::String],
        ),
        ("mixed 5k ints/names/blobs", mixed, column_types()),
    ];
    println!("compression comparison (raw vs auto-encoded):");
    for (label, rows, types) in &datasets {
        let raw_cfg = FlushConfig::default().with_encoding(ColumnEncoding::Raw);
        let auto_cfg = FlushConfig::default().with_encoding(ColumnEncoding::Auto);
        let raw = flush(rows, types, gid(1), sid(1), &raw_cfg).expect("raw flush");
        let enc = flush(rows, types, gid(1), sid(1), &auto_cfg).expect("encoded flush");
        let ratio = if raw.bytes.is_empty() {
            1.0
        } else {
            enc.bytes.len() as f64 / raw.bytes.len() as f64
        };
        println!("  dataset: {label}");
        println!(
            "    bytes: raw={}B encoded={}B ratio={ratio:.3} (encoded/raw)",
            raw.bytes.len(),
            enc.bytes.len()
        );
        for (name, cfg) in [("raw", &raw_cfg), ("auto", &auto_cfg)] {
            let start = Instant::now();
            let iters = 10_u64;
            let mut stored = 0_u64;
            for _ in 0..iters {
                let out = flush(rows, types, gid(1), sid(1), cfg).expect("flush");
                stored += out.bytes.len() as u64;
                std::hint::black_box(out.bytes.len());
            }
            let secs = start.elapsed().as_secs_f64();
            report(
                &format!("compress_flush_{name}"),
                ROWS as u64 * iters,
                stored,
                secs,
                label,
            );
        }
        let materialized = materialize_columns(rows, types).expect("materialize");
        let mut streams: Vec<(Vec<u8>, u64)> = Vec::new();
        for col in &materialized {
            let mut stream = Vec::new();
            for row in 0..col.row_count as usize {
                let bytes = col.get_value(row).unwrap_or(&[]);
                stream.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                stream.extend_from_slice(bytes);
            }
            streams.push((stream, col.row_count));
        }
        for encoding in [ColumnEncoding::Raw, ColumnEncoding::Auto] {
            let start = Instant::now();
            let iters = 20_u64;
            let mut bytes_in = 0_u64;
            let mut bytes_out = 0_u64;
            for _ in 0..iters {
                for (stream, count) in &streams {
                    let out = encode_chunk(
                        stream,
                        *count,
                        encoding,
                        plomid_columnar::ChunkCompression::None,
                    )
                    .expect("encode");
                    bytes_in += stream.len() as u64;
                    bytes_out += out.payload.len() as u64;
                    std::hint::black_box(out.payload.len());
                }
            }
            let secs = start.elapsed().as_secs_f64();
            report(
                &format!("chunk_encode_{encoding:?}"),
                ROWS as u64 * iters,
                bytes_out,
                secs,
                &format!("in={bytes_in}B out={bytes_out}B {label}"),
            );
        }
        for (name, image) in [("raw", &raw.bytes), ("encoded", &enc.bytes)] {
            let reader = SegmentReader::decode(image).expect("decode");
            let ids: Vec<plomid_core::ColumnId> =
                reader.columns.iter().map(|c| c.column_id).collect();
            // Full scan latency.
            let start = Instant::now();
            let scan_iters = 20_u64;
            let mut decoded = 0_usize;
            for _ in 0..scan_iters {
                let back = reader
                    .read_rows(image, 0, reader.row_count, &ids)
                    .expect("rows");
                decoded = back.len();
            }
            let secs = start.elapsed().as_secs_f64();
            report(
                &format!("scan_full_{name}"),
                decoded as u64 * scan_iters,
                image.len() as u64 * scan_iters,
                secs,
                label,
            );
            // Filtered scan latency over BRIN candidates.
            let mid = &rows[rows.len() / 2];
            let filter =
                (0..types.len()).find_map(|i| filter_literal(mid, types, i).map(|lit| (i, lit)));
            if let (Some(pruning), Some((filter_col, filter_field))) =
                (reader.pruning.as_ref(), filter)
            {
                use plomid_columnar::{plan_scan, PruneOperator, PrunePredicate};
                let predicate = PrunePredicate::compare(
                    plomid_core::ColumnId::new(filter_col as u64),
                    PruneOperator::Equal,
                    filter_field,
                );
                let plan = plan_scan(pruning, reader.row_count, &predicate);
                let scanned = plan.scanned_rows;
                let start = Instant::now();
                let mut kept = 0_usize;
                for _ in 0..scan_iters {
                    kept = 0;
                    for range in &plan.candidates {
                        let part = reader
                            .read_rows(image, range.start_row, range.end_row, &ids)
                            .expect("range");
                        kept += part.len();
                    }
                    std::hint::black_box(kept);
                }
                let secs = start.elapsed().as_secs_f64();
                report(
                    &format!("scan_filtered_{name}"),
                    kept as u64 * scan_iters,
                    image.len() as u64 * scan_iters,
                    secs,
                    &format!("scanned={scanned}/{label}"),
                );
            }
        }
    }
}

fn main() {
    println!("PLOMID immutable columnar segment flush benchmarks");
    println!();
    bench_hot_scan_inner();
    bench_conversion();
    bench_materialize_stats();
    bench_write_read();
    bench_full_flush();
    bench_encoding_shapes();
    println!();
    println!("Benchmarks complete.");
}
