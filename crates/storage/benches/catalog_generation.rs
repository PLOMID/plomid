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
//! PLOMID catalog and generation benchmarks.
//!
//! Measures catalog load, catalog publication, generation creation, generation
//! publication, generation discovery, generation GC, and reader snapshot
//! acquisition. All data is deterministic, uses unique process-scoped scratch
//! directories, performs no network access, and cleans up after each run.
//!
//! Run with: `cargo bench -p plomid-storage --bench catalog_generation`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use plomid_core::{CatalogVersion, GenerationId, Lsn, ObjectId, Result};
use plomid_storage::{
    discover_generation_ids, GenerationManager, ObjectChange, PhysicalReference, PhysicalStructure,
    PublicationRequest, SchemaColumn, SchemaMetadata,
};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-catgen-bench-{label}-{}-{}-{p}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos(),
        p = id
    ));
    std::fs::create_dir_all(&path).expect("bench dir");
    path
}

/// Reports one measured operation together with its dataset shape.
///
/// Reported values are the measured wall-clock cost of the operation itself:
/// latency per operation in microseconds and throughput in operations per
/// second. Durability guarantees are identical to the production path; nothing
/// is relaxed to make a measurement look better.
fn report(name: &str, context: &str, iters: u64, secs: f64) {
    let per_op_us = if secs > 0.0 {
        secs * 1_000_000.0 / iters as f64
    } else {
        0.0
    };
    let per_sec = if secs > 0.0 { iters as f64 / secs } else { 0.0 };
    println!(
        "  {name:.<34} {context:<30} {iters:>6} ops in {secs:8.4}s = {per_op_us:9.3}us/op ({per_sec:10.0} ops/s)"
    );
}

fn structure(page: u64) -> PhysicalStructure {
    PhysicalStructure::new(
        plomid_core::SegmentId::new(1),
        plomid_core::PackId::new(1),
        plomid_core::BlockId::new(1),
        plomid_core::PageId::new(page),
        Some(plomid_core::RowId::new(page)),
    )
}

fn reference(object: u64, generation: u64, page: u64) -> PhysicalReference {
    PhysicalReference::new(
        ObjectId::new(object),
        GenerationId::new(generation),
        structure(page),
    )
}

fn schema(version: u64) -> SchemaMetadata {
    SchemaMetadata::new(
        plomid_core::SchemaId::new(7),
        CatalogVersion::new(version),
        vec![
            SchemaColumn {
                column_id: plomid_core::ColumnId::new(1),
                type_code: 23,
            },
            SchemaColumn {
                column_id: plomid_core::ColumnId::new(2),
                type_code: 25,
            },
        ],
    )
    .expect("schema")
}

fn change(object: u64, generation: u64) -> ObjectChange {
    ObjectChange::new(
        ObjectId::new(object),
        schema(1),
        GenerationId::new(generation),
        vec![reference(object, generation, 1)],
    )
    .expect("change")
}

fn write_req(storage: u64, lsn: u64, changes: Vec<ObjectChange>) -> PublicationRequest {
    PublicationRequest::write(GenerationId::new(storage), Lsn::new(lsn), changes).expect("request")
}

fn benchmark_catalog_load() {
    let dir = scratch("catalog_load");
    let result = {
        let manager = open_arc(&dir);
        manager
            .publish(write_req(
                1,
                1,
                vec![change(1, 5), change(2, 6), change(3, 7)],
            ))
            .expect("first publish");
        let iters = 200u64;
        let start = Instant::now();
        for _ in 0..iters {
            let _catalog = manager.load().expect("catalog load");
        }
        let secs = start.elapsed().as_secs_f64();
        report("catalog_load", "objects=3 generations=1", iters, secs);
        std::fs::remove_dir_all(&dir).ok();
        Result::<()>::Ok(())
    };
    if let Err(e) = result {
        eprintln!("catalog_load benchmark failed: {e:?}");
    }
}

fn benchmark_catalog_publication() {
    let dir = scratch("catalog_pub");
    let result = {
        let manager = open_arc(&dir);
        let objects = 32u64;
        let iters = 50u64;
        let start = Instant::now();
        for i in 0..iters {
            let request = write_req(
                1,
                (i + 1),
                (1..=objects)
                    .map(|obj| {
                        ObjectChange::new(
                            ObjectId::new(obj),
                            SchemaMetadata::new(
                                plomid_core::SchemaId::new(7),
                                CatalogVersion::new(i + 1),
                                vec![SchemaColumn {
                                    column_id: plomid_core::ColumnId::new(1),
                                    type_code: 23,
                                }],
                            )
                            .expect("schema"),
                            GenerationId::new((i + 1) * 100 + obj),
                            vec![reference(obj, (i + 1) * 100 + obj, obj % 16 + 1)],
                        )
                        .expect("change")
                    })
                    .collect(),
            );
            manager.publish(request).expect("publish");
        }
        let secs = start.elapsed().as_secs_f64();
        report(
            "catalog_publication",
            &format!("objects={objects} generations={iters}"),
            iters,
            secs,
        );
        std::fs::remove_dir_all(&dir).ok();
        Result::<()>::Ok(())
    };
    if let Err(e) = result {
        eprintln!("catalog_publication benchmark failed: {e:?}");
    }
}

fn open_arc(dir: &PathBuf) -> Arc<GenerationManager> {
    Arc::new(GenerationManager::open(dir).expect("manager"))
}

fn benchmark_generation_creation() {
    let dir = scratch("gen_creation");
    let result = {
        let mgr = open_arc(&dir);
        mgr.publish(write_req(1, 1, vec![change(1, 5)]))
            .expect("seed");
        let iters = 200u64;
        let start = Instant::now();
        for i in 0..iters {
            let metadata = plomid_storage::GenerationMetadata::new(
                GenerationId::new(i + 2),
                ObjectId::new(1),
                CatalogVersion::new(i + 2),
                GenerationId::new(1),
                GenerationId::new(1),
                Lsn::new(i + 1),
                Some(GenerationId::new(i + 1)),
                plomid_storage::catalog::PublicationState::Published,
                vec![reference(1, i + 2, 1)],
            )
            .expect("metadata");
            let _bytes = metadata.encode().expect("encode");
        }
        let secs = start.elapsed().as_secs_f64();
        report(
            "generation_creation_encode",
            &format!("objects=1 generations={iters}"),
            iters,
            secs,
        );
        std::fs::remove_dir_all(&dir).ok();
        Result::<()>::Ok(())
    };
    if let Err(e) = result {
        eprintln!("generation_creation benchmark failed: {e:?}");
    }
}

fn benchmark_generation_publication() {
    let dir = scratch("gen_pub");
    let result = {
        let manager = open_arc(&dir);
        let iters = 30u64;
        let start = Instant::now();
        for i in 0..iters {
            let request = write_req(1, (i + 1), vec![change(1, (i + 2))]);
            let _outcome = manager.publish(request).expect("publish");
        }
        let secs = start.elapsed().as_secs_f64();
        report(
            "generation_publication",
            &format!("objects=1 generations={iters}"),
            iters,
            secs,
        );
        std::fs::remove_dir_all(&dir).ok();
        Result::<()>::Ok(())
    };
    if let Err(e) = result {
        eprintln!("generation_publication benchmark failed: {e:?}");
    }
}

fn benchmark_generation_discovery() {
    let dir = scratch("gen_discovery");
    let result = {
        let mgr = open_arc(&dir);
        let gen_count = 200u64;
        for i in 0..gen_count {
            mgr.publish(write_req(1, i + 1, vec![change(1, i + 5)]))
                .expect("publish");
        }
        let iters = 50u64;
        let start = Instant::now();
        for _ in 0..iters {
            let ids = discover_generation_ids(&dir).expect("discover");
            let _count = ids.len();
        }
        let secs = start.elapsed().as_secs_f64();
        report(
            "generation_discovery",
            &format!("objects=1 generations={gen_count}"),
            iters,
            secs,
        );
        std::fs::remove_dir_all(&dir).ok();
        Result::<()>::Ok(())
    };
    if let Err(e) = result {
        eprintln!("generation_discovery benchmark failed: {e:?}");
    }
}

fn benchmark_generation_gc() {
    let dir = scratch("gen_gc");
    let result = {
        let manager = open_arc(&dir);
        manager
            .publish(write_req(1, 1, vec![change(1, 5)]))
            .expect("first");
        manager
            .publish(write_req(1, 2, vec![change(1, 6)]))
            .expect("second");
        manager
            .publish(write_req(1, 3, vec![change(1, 7)]))
            .expect("third");
        manager
            .publish(write_req(1, 4, vec![change(1, 8)]))
            .expect("fourth");
        manager.release(GenerationId::new(5)).expect("release");
        manager.release(GenerationId::new(6)).expect("release");
        manager.release(GenerationId::new(7)).expect("release");
        let iters = 20u64;
        let start = Instant::now();
        for _ in 0..iters {
            let _outcome = manager.gc().expect("gc");
        }
        let secs = start.elapsed().as_secs_f64();
        report(
            "generation_gc",
            "objects=1 generations=4 released=3",
            iters,
            secs,
        );
        std::fs::remove_dir_all(&dir).ok();
        Result::<()>::Ok(())
    };
    if let Err(e) = result {
        eprintln!("generation_gc benchmark failed: {e:?}");
    }
}

fn benchmark_reader_snapshot() {
    let dir = scratch("reader_snap");
    let result = {
        let manager = open_arc(&dir);
        manager
            .publish(write_req(1, 1, vec![change(1, 5), change(2, 6)]))
            .expect("seed");
        let iters = 200u64;
        let start = Instant::now();
        for _ in 0..iters {
            let reader = manager.reader().expect("reader");
            let _catalog = reader.catalog();
            let _generations = reader.generation_ids();
            drop(reader);
        }
        let secs = start.elapsed().as_secs_f64();
        report("reader_snapshot", "objects=2 generations=1", iters, secs);
        std::fs::remove_dir_all(&dir).ok();
        Result::<()>::Ok(())
    };
    if let Err(e) = result {
        eprintln!("reader_snapshot benchmark failed: {e:?}");
    }
}

fn main() {
    println!("PLOMID catalog and generation benchmarks");
    println!();
    println!("  Dataset sizes are illustrative; durability guarantees are unchanged.");
    println!();

    benchmark_catalog_load();
    benchmark_catalog_publication();
    benchmark_generation_creation();
    benchmark_generation_publication();
    benchmark_generation_discovery();
    benchmark_generation_gc();
    benchmark_reader_snapshot();

    println!();
    println!("Benchmarks complete.");
}
