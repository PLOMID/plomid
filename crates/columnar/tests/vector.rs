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
//! Vector aggregation integration tests: store-level end-to-end.
//!
//! Each test publishes a real generation, runs `vector_aggregate`, and
//! compares against an independent scalar accumulation over
//! `read_current_generation_rows`. Decline paths (unsupported shapes) must
//! return `None` so the caller keeps the scalar path.

use plomid_columnar::vector::{VectorAggKind, VectorAggregate, VectorRequest, F64_EXACT_INT_RANGE};
use plomid_columnar::{
    read_current_generation_rows, ColumnType, ColumnarPublish, ColumnarStore, FlushConfig,
    PrunePredicate,
};
use plomid_core::{CatalogVersion, ColumnId, GenerationId, Lsn, ObjectId, SchemaId};
use plomid_storage::{Field, Row, StorageEngine as _};
use plomid_txn::PlomidStorageEngine;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-vector-it-{label}-{}-{id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_dir_all(path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
}

fn engine_at(dir: &PathBuf) -> PlomidStorageEngine {
    PlomidStorageEngine::create(dir, &dir.join("wal"), 32).expect("create engine")
}

/// Publishes `rows` as generation 1 of object 7 with the given types.
fn publish(
    store: &ColumnarStore,
    engine: &mut PlomidStorageEngine,
    rows: &[Row],
    types: &[ColumnType],
) {
    let publish = ColumnarPublish::new(
        ObjectId::new(7),
        ColumnarStore::columnar_schema(SchemaId::new(4), CatalogVersion::new(1), types)
            .expect("schema"),
        GenerationId::new(1),
        GenerationId::new(1),
        Lsn::new(1),
    );
    let segment_id = store.allocate_segment_id();
    store
        .flush_rows_with_id(
            engine,
            rows,
            types,
            segment_id,
            &FlushConfig::default(),
            &publish,
            plomid_columnar::ColumnarFailPoint::None,
        )
        .expect("publish");
}

/// Builds the int/int/string fixture: col0 = i*3-500 (NULL every 7th),
/// col1 = 1000-2i (NULL every 11th), col2 = tag text.
fn fixture_rows(n: usize) -> Vec<Row> {
    (0..n)
        .map(|i| {
            Row::new(vec![
                if i % 7 == 0 {
                    Field::Null
                } else {
                    Field::Integer(i as i64 * 3 - 500)
                },
                if i % 11 == 0 {
                    Field::Null
                } else {
                    Field::Integer(1000 - i as i64 * 2)
                },
                Field::String(["a", "b", "c"][i % 3].to_owned()),
            ])
        })
        .collect()
}

/// Independent scalar expectation over visible rows.
#[derive(Default)]
struct Expect {
    count_star: i64,
    count_c0: i64,
    sum_c0: i64,
    sum_any: bool,
    min_c0: Option<i64>,
    max_c0: Option<i64>,
    avg_sum: i64,
    avg_count: i64,
    distinct_c1: HashSet<i64>,
}

fn expect_over(rows: &[Row], keep: &dyn Fn(&[Field]) -> bool) -> Expect {
    let mut out = Expect::default();
    for row in rows {
        let fields = row.fields();
        if !keep(fields) {
            continue;
        }
        out.count_star += 1;
        if let Field::Integer(v) = fields[0] {
            out.count_c0 += 1;
            out.sum_c0 += v;
            out.sum_any = true;
            out.min_c0 = Some(out.min_c0.map_or(v, |m: i64| m.min(v)));
            out.max_c0 = Some(out.max_c0.map_or(v, |m: i64| m.max(v)));
            out.avg_sum += v;
            out.avg_count += 1;
        }
        if let Field::Integer(v) = fields[1] {
            out.distinct_c1.insert(v);
        }
    }
    out
}

fn int_cell(fields: &[Field], index: usize) -> Option<i64> {
    match fields.get(index) {
        Some(Field::Integer(v)) => Some(*v),
        _ => None,
    }
}

#[test]
fn vector_global_matches_scalar() {
    let root = scratch("global");
    let mut engine = engine_at(&root);
    let store = ColumnarStore::open(&root).expect("store");
    let rows = fixture_rows(5000);
    publish(
        &store,
        &mut engine,
        &rows,
        &[ColumnType::Integer, ColumnType::Integer, ColumnType::String],
    );

    // WHERE col0 > 100 AND col1 <= 0 (both integer leaves, in f64 range).
    let predicate = PrunePredicate::compare(
        ColumnId::new(0),
        plomid_columnar::PruneOperator::Greater,
        Field::Integer(100),
    )
    .and(PrunePredicate::compare(
        ColumnId::new(1),
        plomid_columnar::PruneOperator::LessOrEqual,
        Field::Integer(0),
    ));
    let request = VectorRequest {
        aggregates: vec![
            VectorAggregate {
                kind: VectorAggKind::CountStar,
                column: None,
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Count,
                column: Some(ColumnId::new(0)),
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Sum,
                column: Some(ColumnId::new(0)),
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Min,
                column: Some(ColumnId::new(0)),
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Max,
                column: Some(ColumnId::new(0)),
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Avg,
                column: Some(ColumnId::new(0)),
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::CountDistinct,
                column: Some(ColumnId::new(1)),
                filter: None,
            },
        ],
        filter: Some(predicate),
        group_by: vec![],
        batch_rows: 0,
        text_ordering_exact: false,
    };
    let result =
        plomid_columnar::vector::vector_aggregate(&store, &mut engine, ObjectId::new(7), &request)
            .expect("vector")
            .expect("supported");
    assert_eq!(result.groups.len(), 1);
    let accs = &result.groups[0].accs;

    let expected = expect_over(&rows, &|f| {
        matches!(
            (int_cell(f, 0), int_cell(f, 1)),
            (Some(a), Some(b)) if a > 100 && b <= 0
        )
    });
    assert_eq!(accs[0].count_star, expected.count_star);
    assert_eq!(accs[1].count, expected.count_c0);
    assert_eq!(
        (accs[2].sum, accs[2].sum_has_any),
        (expected.sum_c0, expected.sum_any)
    );
    assert_eq!(accs[3].min, expected.min_c0);
    assert_eq!(accs[4].max, expected.max_c0);
    assert_eq!(
        (accs[5].avg_sum, accs[5].avg_count),
        (expected.avg_sum, expected.avg_count)
    );
    assert_eq!(accs[6].distinct, expected.distinct_c1);
    // Empty result: impossible predicate still yields the global row.
    let empty_request = VectorRequest {
        aggregates: vec![VectorAggregate {
            kind: VectorAggKind::Sum,
            column: Some(ColumnId::new(0)),
            filter: None,
        }],
        filter: Some(PrunePredicate::compare(
            ColumnId::new(0),
            plomid_columnar::PruneOperator::Greater,
            Field::Integer(999_999_999),
        )),
        group_by: vec![],
        batch_rows: 0,
        text_ordering_exact: false,
    };
    let empty = plomid_columnar::vector::vector_aggregate(
        &store,
        &mut engine,
        ObjectId::new(7),
        &empty_request,
    )
    .expect("vector")
    .expect("supported");
    assert_eq!(empty.groups.len(), 1);
    assert!(!empty.groups[0].accs[0].sum_has_any);
    cleanup(&root);
}

#[test]
fn vector_grouped_matches_scalar() {
    let root = scratch("grouped");
    let mut engine = engine_at(&root);
    let store = ColumnarStore::open(&root).expect("store");
    // Group key col0 in {0,1,2} with NULLs; value col1 sparse.
    let rows: Vec<Row> = (0..3000)
        .map(|i| {
            Row::new(vec![
                if i % 13 == 0 {
                    Field::Null
                } else {
                    Field::Integer((i % 3) as i64)
                },
                if i % 5 == 0 {
                    Field::Null
                } else {
                    Field::Integer(i as i64)
                },
            ])
        })
        .collect();
    publish(
        &store,
        &mut engine,
        &rows,
        &[ColumnType::Integer, ColumnType::Integer],
    );

    let request = VectorRequest {
        aggregates: vec![
            VectorAggregate {
                kind: VectorAggKind::CountStar,
                column: None,
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Sum,
                column: Some(ColumnId::new(1)),
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Count,
                column: Some(ColumnId::new(1)),
                filter: Some(PrunePredicate::compare(
                    ColumnId::new(1),
                    plomid_columnar::PruneOperator::GreaterOrEqual,
                    Field::Integer(1000),
                )),
            },
        ],
        filter: None,
        group_by: vec![ColumnId::new(0)],
        batch_rows: 512,
        text_ordering_exact: false,
    };
    let result =
        plomid_columnar::vector::vector_aggregate(&store, &mut engine, ObjectId::new(7), &request)
            .expect("vector")
            .expect("supported");
    // Independent expectation.
    let mut expected: std::collections::BTreeMap<Option<i64>, (i64, i64, i64)> = Default::default();
    for row in &rows {
        let key = int_cell(row.fields(), 0);
        let entry = expected.entry(key).or_insert((0, 0, 0));
        entry.0 += 1;
        if let Some(v) = int_cell(row.fields(), 1) {
            entry.1 += v;
            if v >= 1000 {
                entry.2 += 1;
            }
        }
    }
    assert_eq!(
        result.groups.len(),
        expected.len(),
        "NULL key forms its own group"
    );
    for group in &result.groups {
        assert_eq!(group.keys.len(), 1);
        let expected = expected.get(&group.keys[0]).expect("known group");
        assert_eq!(
            group.accs[0].count_star, expected.0,
            "group {:?}",
            group.keys
        );
        assert_eq!(group.accs[1].sum, expected.1);
        assert!(group.accs[1].sum_has_any || expected.1 == 0);
        assert_eq!(group.accs[2].count, expected.2);
    }
    cleanup(&root);
}

#[test]
fn vector_declines_unsupported_shapes() {
    let root = scratch("decline");
    let mut engine = engine_at(&root);
    let store = ColumnarStore::open(&root).expect("store");
    let rows = fixture_rows(500);
    publish(
        &store,
        &mut engine,
        &rows,
        &[ColumnType::Integer, ColumnType::Integer, ColumnType::String],
    );
    let sum_c0 = || VectorAggregate {
        kind: VectorAggKind::Sum,
        column: Some(ColumnId::new(0)),
        filter: None,
    };
    // String argument column for SUM.
    let declined = plomid_columnar::vector::vector_aggregate(
        &store,
        &mut engine,
        ObjectId::new(7),
        &VectorRequest {
            aggregates: vec![VectorAggregate {
                kind: VectorAggKind::Sum,
                column: Some(ColumnId::new(2)),
                filter: None,
            }],
            filter: None,
            group_by: vec![],
            batch_rows: 0,
            text_ordering_exact: false,
        },
    )
    .expect("no error");
    assert!(declined.is_none(), "SUM over a String column declines");
    // Cross-type predicate leaf.
    let declined = plomid_columnar::vector::vector_aggregate(
        &store,
        &mut engine,
        ObjectId::new(7),
        &VectorRequest {
            aggregates: vec![sum_c0()],
            filter: Some(PrunePredicate::compare(
                ColumnId::new(0),
                plomid_columnar::PruneOperator::Equal,
                Field::String("5".to_owned()),
            )),
            group_by: vec![],
            batch_rows: 0,
            text_ordering_exact: false,
        },
    )
    .expect("no error");
    assert!(declined.is_none(), "cross-type predicate declines");
    // Huge literal beyond f64-exact range.
    let declined = plomid_columnar::vector::vector_aggregate(
        &store,
        &mut engine,
        ObjectId::new(7),
        &VectorRequest {
            aggregates: vec![sum_c0()],
            filter: Some(PrunePredicate::compare(
                ColumnId::new(0),
                plomid_columnar::PruneOperator::Greater,
                Field::Integer(F64_EXACT_INT_RANGE + 1),
            )),
            group_by: vec![],
            batch_rows: 0,
            text_ordering_exact: false,
        },
    )
    .expect("no error");
    assert!(declined.is_none(), "out-of-range literal declines");
    // String GROUP BY key.
    let declined = plomid_columnar::vector::vector_aggregate(
        &store,
        &mut engine,
        ObjectId::new(7),
        &VectorRequest {
            aggregates: vec![sum_c0()],
            filter: None,
            group_by: vec![ColumnId::new(2)],
            batch_rows: 0,
            text_ordering_exact: false,
        },
    )
    .expect("no error");
    assert!(declined.is_none(), "String group key declines");
    // Unknown column.
    let declined = plomid_columnar::vector::vector_aggregate(
        &store,
        &mut engine,
        ObjectId::new(7),
        &VectorRequest {
            aggregates: vec![VectorAggregate {
                kind: VectorAggKind::Sum,
                column: Some(ColumnId::new(99)),
                filter: None,
            }],
            filter: None,
            group_by: vec![],
            batch_rows: 0,
            text_ordering_exact: false,
        },
    )
    .expect("no error");
    assert!(declined.is_none(), "unknown column declines");
    cleanup(&root);
}

#[test]
fn vector_declines_out_of_range_segment() {
    // Values beyond ±2⁵³: the f64 guard must decline so the scalar path
    // (with its f64 comparison semantics) answers exactly as before.
    let root = scratch("range");
    let mut engine = engine_at(&root);
    let store = ColumnarStore::open(&root).expect("store");
    let big = (1i64 << 60) + 12345;
    let rows: Vec<Row> = (0..100)
        .map(|i| Row::new(vec![Field::Integer(big + i)]))
        .collect();
    publish(&store, &mut engine, &rows, &[ColumnType::Integer]);
    for request in [
        VectorRequest {
            aggregates: vec![VectorAggregate {
                kind: VectorAggKind::Sum,
                column: Some(ColumnId::new(0)),
                filter: None,
            }],
            filter: Some(PrunePredicate::compare(
                ColumnId::new(0),
                plomid_columnar::PruneOperator::Greater,
                Field::Integer(0),
            )),
            group_by: vec![],
            batch_rows: 0,
            text_ordering_exact: false,
        },
        VectorRequest {
            aggregates: vec![VectorAggregate {
                kind: VectorAggKind::Min,
                column: Some(ColumnId::new(0)),
                filter: None,
            }],
            filter: None,
            group_by: vec![],
            batch_rows: 0,
            text_ordering_exact: false,
        },
    ] {
        let declined = plomid_columnar::vector::vector_aggregate(
            &store,
            &mut engine,
            ObjectId::new(7),
            &request,
        )
        .expect("no error");
        assert!(declined.is_none(), "out-of-range segment declines");
    }
    // Untouched read path still serves the rows for the scalar fallback.
    let scan = read_current_generation_rows(&store, &mut engine, ObjectId::new(7), None, None)
        .expect("read")
        .expect("generation");
    assert_eq!(scan.rows.len(), 100);
    cleanup(&root);
}

#[test]
fn vector_batch_sizes_agree() {
    // Batch-size sweep: every size must produce identical accumulators.
    let root = scratch("batches");
    let mut engine = engine_at(&root);
    let store = ColumnarStore::open(&root).expect("store");
    let rows = fixture_rows(8000);
    publish(
        &store,
        &mut engine,
        &rows,
        &[ColumnType::Integer, ColumnType::Integer, ColumnType::String],
    );
    let mut reference: Option<String> = None;
    for batch in [256usize, 1024, 4096, 16384, 65536] {
        let request = VectorRequest {
            aggregates: vec![
                VectorAggregate {
                    kind: VectorAggKind::CountStar,
                    column: None,
                    filter: None,
                },
                VectorAggregate {
                    kind: VectorAggKind::Sum,
                    column: Some(ColumnId::new(0)),
                    filter: None,
                },
            ],
            filter: None,
            group_by: vec![],
            batch_rows: batch,
            text_ordering_exact: false,
        };
        let started = std::time::Instant::now();
        let result = plomid_columnar::vector::vector_aggregate(
            &store,
            &mut engine,
            ObjectId::new(7),
            &request,
        )
        .expect("vector")
        .expect("supported");
        let elapsed = started.elapsed();
        let signature = format!("{:?}", result.groups);
        eprintln!("batch {batch:>6}: {elapsed:?}");
        match &reference {
            Some(expected) => assert_eq!(&signature, expected, "batch {batch} agrees"),
            None => reference = Some(signature),
        }
    }
    cleanup(&root);
}

#[test]
fn vector_raw_text_paths_match_scalar() {
    // String/Bytes leaves, extrema, and distinctness over raw byte order,
    // with text_ordering_exact set (the executor verifies SQL types first).
    let root = scratch("rawtext");
    let mut engine = engine_at(&root);
    let store = ColumnarStore::open(&root).expect("store");
    let words = ["apple", "banana", "cherry", "date", "apple", "fig", "grape"];
    let rows: Vec<Row> = (0..3000)
        .map(|i| {
            Row::new(vec![
                if i % 9 == 0 {
                    Field::Null
                } else {
                    Field::String(words[i % words.len()].to_owned())
                },
                Field::Bytes(vec![(i % 251) as u8, (i % 17) as u8]),
                Field::Integer(i as i64),
            ])
        })
        .collect();
    publish(
        &store,
        &mut engine,
        &rows,
        &[ColumnType::String, ColumnType::Bytes, ColumnType::Integer],
    );

    // Text predicate + text MIN/MAX + text DISTINCT + bytes predicate.
    let request = VectorRequest {
        aggregates: vec![
            VectorAggregate {
                kind: VectorAggKind::CountStar,
                column: None,
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Min,
                column: Some(ColumnId::new(0)),
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Max,
                column: Some(ColumnId::new(0)),
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::CountDistinct,
                column: Some(ColumnId::new(0)),
                filter: None,
            },
            VectorAggregate {
                kind: VectorAggKind::Min,
                column: Some(ColumnId::new(1)),
                filter: None,
            },
        ],
        filter: Some(
            PrunePredicate::compare(
                ColumnId::new(0),
                plomid_columnar::PruneOperator::GreaterOrEqual,
                Field::String("banana".to_owned()),
            )
            .and(PrunePredicate::compare(
                ColumnId::new(1),
                plomid_columnar::PruneOperator::NotEqual,
                Field::Bytes(vec![0, 0]),
            )),
        ),
        group_by: vec![],
        batch_rows: 0,
        text_ordering_exact: true,
    };
    let result =
        plomid_columnar::vector::vector_aggregate(&store, &mut engine, ObjectId::new(7), &request)
            .expect("vector")
            .expect("text paths supported with the flag");
    // Independent scalar expectation over the same rows.
    let mut count = 0i64;
    let mut min: Option<&[u8]> = None;
    let mut max: Option<&[u8]> = None;
    let mut distinct: HashSet<Vec<u8>> = HashSet::new();
    let mut min_b: Option<Vec<u8>> = None;
    for row in &rows {
        let fields = row.fields();
        let text = match &fields[0] {
            Field::String(s) => s.as_bytes(),
            _ => continue,
        };
        let bytes = match &fields[1] {
            Field::Bytes(b) => b.as_slice(),
            _ => continue,
        };
        if text < b"banana" || bytes == [0, 0] {
            continue;
        }
        count += 1;
        min = Some(match min {
            Some(m) if m <= text => m,
            _ => text,
        });
        max = Some(match max {
            Some(m) if m >= text => m,
            _ => text,
        });
        distinct.insert(text.to_vec());
        min_b = Some(match min_b {
            Some(m) if m <= bytes.to_vec() => m,
            _ => bytes.to_vec(),
        });
    }
    let accs = &result.groups[0].accs;
    assert_eq!(accs[0].count_star, count);
    assert_eq!(accs[1].min_raw.as_deref(), min);
    assert_eq!(accs[2].max_raw.as_deref(), max);
    assert_eq!(accs[3].distinct_raw, distinct);
    assert_eq!(accs[4].min_raw.as_deref(), min_b.as_deref());

    // Without the flag, string leaves decline.
    let mut noflag = request.clone();
    noflag.text_ordering_exact = false;
    let declined =
        plomid_columnar::vector::vector_aggregate(&store, &mut engine, ObjectId::new(7), &noflag)
            .expect("no error");
    assert!(declined.is_none(), "string leaves need the text flag");
    cleanup(&root);
}
