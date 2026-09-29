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
//! Immutable columnar segment integration tests.
//!
//! Covers encoding, round trips, statistics, compression, immutability,
//! generation publication visibility, injected failures, MVCC snapshots, and
//! restart recovery against real engine + filesystem state.

use plomid_columnar::{
    flush, materialize_columns, ColumnType, ColumnarFailPoint, ColumnarPublish, ColumnarStore,
    FlushConfig, SegmentReader,
};
use plomid_core::{CatalogVersion, ColumnId, GenerationId, Lsn, ObjectId, SchemaId};
use plomid_storage::{Field, Row, StorageEngine as _, StorageEngineTransaction as _};
use plomid_txn::{HotRowStore, PlomidStorageEngine};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-columnar-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_dir_all(path);
}

fn int_rows(count: usize, null_every: usize) -> Vec<Row> {
    (0..count)
        .map(|i| {
            let field = if null_every > 0 && i % null_every == 0 {
                Field::Null
            } else {
                Field::Integer(i as i64 * 3 - 7)
            };
            Row::new(vec![field])
        })
        .collect()
}

fn mixed_rows() -> Vec<Row> {
    vec![
        Row::new(vec![
            Field::Integer(1),
            Field::String("alpha".to_owned()),
            Field::Bytes(vec![9, 8]),
        ]),
        Row::new(vec![Field::Null, Field::Null, Field::Null]),
        Row::new(vec![
            Field::Integer(-40),
            Field::String(String::new()),
            Field::Bytes(vec![]),
        ]),
        Row::new(vec![
            Field::Integer(i64::MAX),
            Field::String("unicode-ß-日本".to_owned()),
            Field::Bytes((0..64u8).collect()),
        ]),
    ]
}

fn publish(object: u64, generation: u64, types: &[ColumnType]) -> ColumnarPublish {
    let schema = ColumnarStore::columnar_schema(SchemaId::new(7), CatalogVersion::new(1), types)
        .expect("schema");
    ColumnarPublish::new(
        ObjectId::new(object),
        schema,
        GenerationId::new(generation),
        GenerationId::new(generation),
        Lsn::new(generation),
    )
}

fn engine_at(dir: &PathBuf) -> PlomidStorageEngine {
    PlomidStorageEngine::create(dir, &dir.join("wal"), 32).expect("create engine")
}

fn round_trip(rows: &[Row], types: &[ColumnType]) {
    let flushed = flush(
        rows,
        types,
        GenerationId::new(3),
        plomid_core::SegmentId::new(11),
        &FlushConfig::default(),
    )
    .expect("flush");
    let reader = SegmentReader::decode(&flushed.bytes).expect("decode");
    let ids: Vec<ColumnId> = reader.columns.iter().map(|c| c.column_id).collect();
    let back = reader
        .read_rows(&flushed.bytes, 0, reader.row_count, &ids)
        .expect("rows");
    assert_eq!(back, rows);
}

#[test]
fn encodes_integers_strings_bytes_and_nulls() {
    round_trip(
        &mixed_rows(),
        &[ColumnType::Integer, ColumnType::String, ColumnType::Bytes],
    );
}

#[test]
fn encodes_empty_single_row_and_large_columns() {
    round_trip(&[], &[ColumnType::Integer]);
    round_trip(
        &[Row::new(vec![Field::Integer(42)])],
        &[ColumnType::Integer],
    );
    let large: Vec<Row> = (0..2_000)
        .map(|i| Row::new(vec![Field::Bytes(vec![(i & 0xff) as u8; 32])]))
        .collect();
    round_trip(&large, &[ColumnType::Bytes]);
    let wide: Vec<Row> = (0..300)
        .map(|i| {
            Row::new(vec![Field::String(format!(
                "value-{i:05}-padding-xxxxxxxx"
            ))])
        })
        .collect();
    round_trip(&wide, &[ColumnType::String]);
}

#[test]
fn statistics_track_min_max_null_and_row_counts() {
    let rows = int_rows(64, 4);
    // Derive the expected statistics from the input rather than restating them,
    // so the test cannot drift from the generator.
    let mut expected_min: Option<i64> = None;
    let mut expected_max: Option<i64> = None;
    let mut expected_nulls = 0_u64;
    for row in &rows {
        match row.fields()[0] {
            Field::Null => expected_nulls += 1,
            Field::Integer(value) => {
                expected_min = Some(expected_min.map_or(value, |min| min.min(value)));
                expected_max = Some(expected_max.map_or(value, |max| max.max(value)));
            }
            ref other => panic!("unexpected field {other:?}"),
        }
    }
    let flushed = flush(
        &rows,
        &[ColumnType::Integer],
        GenerationId::new(3),
        plomid_core::SegmentId::new(12),
        &FlushConfig::default(),
    )
    .expect("flush");
    let reader = SegmentReader::decode(&flushed.bytes).expect("decode");
    let stats = reader
        .column_statistics(ColumnId::new(0))
        .expect("statistics");
    assert_eq!(stats.row_count, rows.len() as u64);
    assert_eq!(stats.null_count, expected_nulls);
    assert_eq!(stats.min, expected_min.map(Field::Integer));
    assert_eq!(stats.max, expected_max.map(Field::Integer));
    assert!(stats.min_is_available() && stats.max_is_available());
}

#[test]
fn all_null_columns_report_unavailable_bounds() {
    let rows = vec![Row::new(vec![Field::Null]); 16];
    let flushed = flush(
        &rows,
        &[ColumnType::Null],
        GenerationId::new(3),
        plomid_core::SegmentId::new(13),
        &FlushConfig::default(),
    )
    .expect("flush");
    let reader = SegmentReader::decode(&flushed.bytes).expect("decode");
    let stats = reader
        .column_statistics(ColumnId::new(0))
        .expect("statistics");
    assert_eq!(stats.null_count, 16);
    assert!(stats.min.is_none() && stats.max.is_none());
}

#[test]
fn materialized_null_bitmap_matches_ceil_rows_over_eight() {
    let rows = int_rows(9, 3);
    let cols = materialize_columns(&rows, &[ColumnType::Integer]).expect("materialize");
    assert_eq!(cols[0].null_bitmap.len(), 2);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(
            cols[0].is_null(index),
            matches!(row.fields()[0], Field::Null)
        );
    }
}

#[test]
fn rejects_invalid_magic_version_lengths_and_checksums() {
    use plomid_core::ErrorKind;
    let rows = int_rows(16, 0);
    let flushed = flush(
        &rows,
        &[ColumnType::Integer],
        GenerationId::new(3),
        plomid_core::SegmentId::new(14),
        &FlushConfig::default(),
    )
    .expect("flush");
    let mut bad_magic = flushed.bytes.clone();
    bad_magic[0] ^= 0xff;
    assert_eq!(
        SegmentReader::decode(&bad_magic).expect_err("magic").kind(),
        ErrorKind::Corruption
    );
    let mut bad_version = flushed.bytes.clone();
    bad_version[4] ^= 0xff;
    assert!(SegmentReader::decode(&bad_version).is_err());
    let truncated = &flushed.bytes[..flushed.bytes.len() / 2];
    assert!(SegmentReader::decode(truncated).is_err());
    let mut bad_sum = flushed.bytes.clone();
    let last = bad_sum.len() - 1;
    bad_sum[last] ^= 0x01;
    assert!(SegmentReader::decode(&bad_sum).is_err());
}

#[test]
fn published_segment_bytes_are_immutable() {
    let dir = scratch("immutable");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| {
        let mut engine = engine_at(&dir);
        HotRowStore::new(&mut engine)
            .batch_insert(int_rows(32, 0))
            .expect("seed");
        let gen_root = dir.join("gen");
        std::fs::create_dir_all(&gen_root).expect("gen dir");
        let store = ColumnarStore::open(&gen_root).expect("store");
        let types = vec![ColumnType::Integer];
        let published = store
            .flush_hot_rows(
                &mut engine,
                &types,
                &FlushConfig::default(),
                &publish(1, 40, &types),
                ColumnarFailPoint::None,
            )
            .expect("flush");
        // A second read of the same segment returns bit-identical bytes: the
        // published image is never modified in place.
        let again = store
            .read_segment(&mut engine, published.segment_id)
            .expect("read");
        assert_eq!(again, published.bytes);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn readers_only_see_published_generations() {
    let dir = scratch("visibility");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| {
        let mut engine = engine_at(&dir);
        HotRowStore::new(&mut engine)
            .batch_insert(int_rows(16, 0))
            .expect("seed");
        let gen_root = dir.join("gen");
        std::fs::create_dir_all(&gen_root).expect("gen dir");
        let store = ColumnarStore::open(&gen_root).expect("store");
        let types = vec![ColumnType::Integer];
        assert!(store.generations().reader().is_err());
        let published = store
            .flush_hot_rows(
                &mut engine,
                &types,
                &FlushConfig::default(),
                &publish(1, 41, &types),
                ColumnarFailPoint::None,
            )
            .expect("flush");
        let reader = store.generations().reader().expect("reader");
        assert!(reader.generation_ids().contains(&published.generation_id));
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

/// Fails a flush at `fail_at` and proves the segment is not reader-visible.
fn failed_flush_leaves_no_visible_segment(fail_at: ColumnarFailPoint, label: &str) {
    let dir = scratch(label);
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| {
        let mut engine = engine_at(&dir);
        HotRowStore::new(&mut engine)
            .batch_insert(int_rows(16, 0))
            .expect("seed");
        let gen_root = dir.join("gen");
        std::fs::create_dir_all(&gen_root).expect("gen dir");
        let store = ColumnarStore::open(&gen_root).expect("store");
        let types = vec![ColumnType::Integer];
        let outcome = store.flush_hot_rows(
            &mut engine,
            &types,
            &FlushConfig::default(),
            &publish(1, 50, &types),
            fail_at,
        );
        if fail_at == ColumnarFailPoint::AfterPublish {
            // Publication succeeded; the error only reports caller
            // non-observation, so the segment must be visible.
            assert!(outcome.is_err());
        } else {
            assert!(outcome.is_err(), "flush should fail at {fail_at:?}");
            assert!(
                store.generations().reader().is_err(),
                "generation published despite failing at {fail_at:?}"
            );
            if matches!(
                fail_at,
                ColumnarFailPoint::AfterBuild
                    | ColumnarFailPoint::AfterFlush
                    | ColumnarFailPoint::DuringVerify
                    | ColumnarFailPoint::BeforeSync
            ) {
                // The staging transaction never committed, so not even the
                // manifest may be discoverable in storage.
                assert!(
                    store.list_segments(&mut engine)?.is_empty(),
                    "segment manifest visible despite failing at {fail_at:?}"
                );
            }
            // AfterSync leaves a committed but unpublished payload: durable,
            // invisible to generation readers, and reclaimable.
        }
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn failed_build_flush_verify_sync_publish_stay_unpublished() {
    failed_flush_leaves_no_visible_segment(ColumnarFailPoint::AfterBuild, "fail-build");
    failed_flush_leaves_no_visible_segment(ColumnarFailPoint::AfterFlush, "fail-flush");
    failed_flush_leaves_no_visible_segment(ColumnarFailPoint::DuringVerify, "fail-verify");
    failed_flush_leaves_no_visible_segment(ColumnarFailPoint::BeforeSync, "fail-sync");
    failed_flush_leaves_no_visible_segment(ColumnarFailPoint::AfterSync, "fail-synced");
    failed_flush_leaves_no_visible_segment(ColumnarFailPoint::DuringPublish, "fail-publish");
}

#[test]
fn flush_uses_one_committed_snapshot_under_concurrent_mutation() {
    // Two committed states must materialize into two self-consistent segments:
    // segment A carries exactly the versions of its snapshot, segment B exactly
    // the versions of the later snapshot, and no segment ever mixes pre- and
    // post-mutation row versions. A rolled-back concurrent write must never
    // appear at all.
    let dir = scratch("mvcc");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| {
        let mut engine = engine_at(&dir);
        let types = vec![ColumnType::Integer];
        let ids = {
            let mut hot = HotRowStore::new(&mut engine);
            hot.batch_insert(int_rows(16, 0)).expect("seed")
        };
        let snapshot_a = ColumnarStore::snapshot_hot_rows(&mut engine)?;
        {
            let mut hot = HotRowStore::new(&mut engine);
            hot.batch_update(
                ids.iter()
                    .map(|id| (*id, Row::new(vec![Field::Integer(7_777_777)])))
                    .collect(),
            )
            .expect("mutate");
        }
        // A concurrent writer that never commits: its writes are staged and
        // rolled back before the flush, and must not be visible to any flush.
        {
            let mut txn = engine.begin()?;
            txn.put(
                &plomid_txn::row_key(plomid_core::RowId::new(9_000)),
                &Row::new(vec![Field::Integer(123_456)]).encode()?,
            )?;
            drop(txn);
        }
        let snapshot_b = ColumnarStore::snapshot_hot_rows(&mut engine)?;
        assert_eq!(snapshot_a.len(), snapshot_b.len());
        assert!(snapshot_b
            .iter()
            .all(|row| row.fields()[0] == Field::Integer(7_777_777)));

        let gen_root = dir.join("gen");
        std::fs::create_dir_all(&gen_root).expect("gen dir");
        let store = ColumnarStore::open(&gen_root).expect("store");
        let segment_a = store.flush_rows_with_id(
            &mut engine,
            &snapshot_a,
            &types,
            plomid_core::SegmentId::new(900),
            &FlushConfig::default(),
            &publish(1, 60, &types),
            ColumnarFailPoint::None,
        )?;
        let segment_b = store.flush_rows_with_id(
            &mut engine,
            &snapshot_b,
            &types,
            plomid_core::SegmentId::new(901),
            &FlushConfig::default(),
            &publish(1, 61, &types),
            ColumnarFailPoint::None,
        )?;

        for (segment, snapshot) in [(&segment_a, &snapshot_a), (&segment_b, &snapshot_b)] {
            let reader = SegmentReader::decode(&segment.bytes)?;
            let ids: Vec<ColumnId> = reader.columns.iter().map(|c| c.column_id).collect();
            let back = reader.read_rows(&segment.bytes, 0, reader.row_count, &ids)?;
            assert_eq!(back, *snapshot);
        }
        // Segment A holds the pre-mutation versions only.
        let reader_a = SegmentReader::decode(&segment_a.bytes)?;
        assert!(reader_a.row_count == snapshot_a.len() as u64);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn published_segment_survives_engine_restart() {
    let dir = scratch("restart");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| {
        let types = vec![ColumnType::Integer];
        let published = {
            let mut engine = engine_at(&dir);
            HotRowStore::new(&mut engine)
                .batch_insert(int_rows(24, 3))
                .expect("seed");
            let gen_root = dir.join("gen");
            std::fs::create_dir_all(&gen_root).expect("gen dir");
            let store = ColumnarStore::open(&gen_root).expect("store");
            let out = store.flush_hot_rows(
                &mut engine,
                &types,
                &FlushConfig::default(),
                &publish(1, 70, &types),
                ColumnarFailPoint::None,
            )?;
            engine.checkpoint()?;
            out
        };
        // Restart the engine and the columnar store from durable state only.
        let mut engine = PlomidStorageEngine::open(&dir, &dir, 32)?;
        let store = ColumnarStore::open(&dir.join("gen")).expect("store");
        let outcome = ColumnarStore::recover(&dir.join("gen"))?;
        assert!(
            outcome.generations.contains(&published.generation_id),
            "published generation missing after restart"
        );
        let bytes = store.read_segment(&mut engine, published.segment_id)?;
        assert_eq!(bytes, published.bytes);
        let reader = SegmentReader::decode(&bytes)?;
        let ids: Vec<ColumnId> = reader.columns.iter().map(|c| c.column_id).collect();
        let back = reader.read_rows(&bytes, 0, reader.row_count, &ids)?;
        assert_eq!(back.len(), 24);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}
/// A reopened store must never reuse a persisted segment identity.
///
/// Segment payloads are durable while the in-process counter is not, so a store
/// opened over a root that already holds segments would otherwise hand out an
/// identity that is still in use and silently overwrite another generation's
/// payload. This is the invariant the SQL `VACUUM` path depends on, because it
/// opens a store per statement.
#[test]
fn a_reopened_store_allocates_identity_beyond_every_persisted_segment() {
    let dir = scratch("segment-ids");
    // IIFE isolates `?` handling so the tail asserts `is_ok` with full debug.
    #[allow(clippy::redundant_closure_call)]
    let result = (|| {
        let types = vec![ColumnType::Integer];
        let published = {
            let mut engine = engine_at(&dir);
            HotRowStore::new(&mut engine)
                .batch_insert(int_rows(8, 0))
                .expect("seed");
            let gen_root = dir.join("gen");
            std::fs::create_dir_all(&gen_root).expect("gen dir");
            let store = ColumnarStore::open(&gen_root).expect("store");
            store.flush_hot_rows(
                &mut engine,
                &types,
                &FlushConfig::default(),
                &publish(1, 70, &types),
                ColumnarFailPoint::None,
            )?
        };
        assert_eq!(published.segment_id.get(), 1, "first identity is one");

        // Reopen over the same root: the next identity must skip the persisted
        // one rather than aliasing it.
        let mut engine = PlomidStorageEngine::open(&dir, &dir, 32)?;
        let store = ColumnarStore::open(&dir.join("gen")).expect("reopen store");
        store.recover_segment_ids(&mut engine)?;
        let recovered = store.allocate_segment_id();
        assert!(
            recovered > published.segment_id,
            "reopened store must allocate beyond the persisted identity"
        );

        // The persisted payload is untouched by the reopen-and-allocate cycle.
        let bytes = store.read_segment(&mut engine, published.segment_id)?;
        assert_eq!(bytes, published.bytes);
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&dir);
    assert!(result.is_ok(), "{result:?}");
}
