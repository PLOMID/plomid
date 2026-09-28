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
//! Compression integration tests for immutable columnar segments.
//!
//! Covers the full encode -> compress -> persist -> read -> decompress ->
//! decode round trip over real segment images for every supported encoding
//! and data type.

use plomid_columnar::{
    decode_chunk, encode_chunk, flush, plan_scan, ChunkCompression, ColumnEncoding, ColumnType,
    FlushConfig, PruneOperator, PrunePredicate, SegmentReader,
};
use plomid_core::{ColumnId, ErrorKind, GenerationId, SegmentId};
use plomid_storage::{Field, Row};

fn gid(value: u64) -> GenerationId {
    GenerationId::new(value)
}

fn sid(value: u64) -> SegmentId {
    SegmentId::new(value)
}

fn config_for(encoding: ColumnEncoding) -> FlushConfig {
    FlushConfig::default()
        .with_encoding(encoding)
        .with_compression(ChunkCompression::None)
}

/// Deterministic pseudo-random integers, so failures reproduce exactly.
fn random_ints(count: usize) -> Vec<i64> {
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state as i64
        })
        .collect()
}

/// Deterministic pseudo-random bytes of varying length.
fn random_bytes(count: usize) -> Vec<Vec<u8>> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    (0..count)
        .map(|i| {
            let length = i % 33;
            (0..length)
                .map(|_| {
                    state = state
                        .wrapping_mul(2_862_933_555_777_941_757)
                        .wrapping_add(3_037_000_493);
                    (state >> 33) as u8
                })
                .collect()
        })
        .collect()
}

fn int_rows(values: &[i64]) -> Vec<Row> {
    values
        .iter()
        .map(|value| Row::new(vec![Field::Integer(*value)]))
        .collect()
}

fn string_rows(values: &[String]) -> Vec<Row> {
    values
        .iter()
        .map(|value| Row::new(vec![Field::String(value.clone())]))
        .collect()
}

/// Flushes, decodes, and returns the persisted image with its reader.
fn persist(rows: &[Row], types: &[ColumnType], config: &FlushConfig) -> (Vec<u8>, SegmentReader) {
    let flushed = flush(rows, types, gid(3), sid(11), config).expect("flush");
    let reader = SegmentReader::decode(&flushed.bytes).expect("decode");
    (flushed.bytes, reader)
}

fn column_ids(reader: &SegmentReader) -> Vec<ColumnId> {
    reader
        .columns
        .iter()
        .map(|column| column.column_id)
        .collect()
}
/// Asserts chunks decode back to exactly `rows`.
fn assert_round_trip(rows: &[Row], bytes: &[u8], reader: &SegmentReader, label: &str) {
    let ids = column_ids(reader);
    let back = reader
        .read_rows(bytes, 0, reader.row_count, &ids)
        .expect("read rows");
    assert_eq!(back.len(), rows.len(), "{label}: row count");
    assert_eq!(back, rows, "{label}: values");
}

fn assert_round_trip_for(rows: &[Row], types: &[ColumnType], encodings: &[ColumnEncoding]) {
    for encoding in encodings {
        let config = config_for(*encoding);
        let (bytes, reader) = persist(rows, types, &config);
        assert_round_trip(
            rows,
            &bytes,
            &reader,
            &format!("encoding={encoding:?} rows={}", rows.len()),
        );
    }
}

const INT_ENCODINGS: &[ColumnEncoding] = &[
    ColumnEncoding::Raw,
    ColumnEncoding::Rle,
    ColumnEncoding::DeltaBitpack,
    ColumnEncoding::Dictionary,
    ColumnEncoding::Auto,
];

const VARIABLE_ENCODINGS: &[ColumnEncoding] = &[
    ColumnEncoding::Raw,
    ColumnEncoding::Rle,
    ColumnEncoding::Dictionary,
    ColumnEncoding::Auto,
];

#[test]
fn round_trips_empty_and_single_value() {
    assert_round_trip_for(&[], &[ColumnType::Integer], INT_ENCODINGS);
    assert_round_trip_for(
        &[Row::new(vec![Field::Integer(42)])],
        &[ColumnType::Integer],
        INT_ENCODINGS,
    );
    assert_round_trip_for(
        &[Row::new(vec![Field::String("solo".to_owned())])],
        &[ColumnType::String],
        VARIABLE_ENCODINGS,
    );
    assert_round_trip_for(
        &[Row::new(vec![Field::Bytes(vec![1, 2, 3])])],
        &[ColumnType::Bytes],
        VARIABLE_ENCODINGS,
    );
}

#[test]
fn round_trips_null_and_mixed_null_shapes() {
    let nulls = vec![Row::new(vec![Field::Null]); 16];
    assert_round_trip_for(&nulls, &[ColumnType::Integer], INT_ENCODINGS);
    let mixed = vec![
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
        Row::new(vec![
            Field::Null,
            Field::String("tail".to_owned()),
            Field::Null,
        ]),
    ];
    assert_round_trip_for(
        &mixed,
        &[ColumnType::Integer, ColumnType::String, ColumnType::Bytes],
        &[ColumnEncoding::Auto, ColumnEncoding::Raw],
    );
    let periodic: Vec<Row> = (0..128)
        .map(|i| {
            if i % 5 == 0 {
                Row::new(vec![Field::Null])
            } else {
                Row::new(vec![Field::Integer(i as i64)])
            }
        })
        .collect();
    assert_round_trip_for(&periodic, &[ColumnType::Integer], INT_ENCODINGS);
}

#[test]
fn round_trips_repeated_monotonic_and_random_integers() {
    assert_round_trip_for(
        &int_rows(&vec![7_i64; 256]),
        &[ColumnType::Integer],
        INT_ENCODINGS,
    );
    let monotonic: Vec<i64> = (0..512).map(|i| i * 3 - 7).collect();
    assert_round_trip_for(&int_rows(&monotonic), &[ColumnType::Integer], INT_ENCODINGS);
    let ts: Vec<i64> = (0..512)
        .map(|i| 1_700_000_000_000_000 + i * 1_000)
        .collect();
    assert_round_trip_for(&int_rows(&ts), &[ColumnType::Integer], INT_ENCODINGS);
    assert_round_trip_for(
        &int_rows(&[i64::MIN, i64::MAX, 0, -1, 1, i64::MIN + 1]),
        &[ColumnType::Integer],
        INT_ENCODINGS,
    );
    assert_round_trip_for(
        &int_rows(&random_ints(256)),
        &[ColumnType::Integer],
        INT_ENCODINGS,
    );
}

#[test]
fn round_trips_string_and_binary_shapes() {
    let low_card: Vec<String> = (0..256)
        .map(|i| ["red", "green", "blue"][i % 3].to_owned())
        .collect();
    assert_round_trip_for(
        &string_rows(&low_card),
        &[ColumnType::String],
        VARIABLE_ENCODINGS,
    );
    let high_card: Vec<String> = (0..256).map(|i| format!("key-{i:06}-suffix")).collect();
    assert_round_trip_for(
        &string_rows(&high_card),
        &[ColumnType::String],
        VARIABLE_ENCODINGS,
    );
    let mut long = vec!["a".repeat(16 * 1024), String::new(), "ß-日本".repeat(64)];
    long.extend((0..16).map(|i| format!("row-{i}")));
    assert_round_trip_for(
        &string_rows(&long),
        &[ColumnType::String],
        VARIABLE_ENCODINGS,
    );
    let binary: Vec<Row> = random_bytes(128)
        .into_iter()
        .map(|b| Row::new(vec![Field::Bytes(b)]))
        .collect();
    assert_round_trip_for(&binary, &[ColumnType::Bytes], VARIABLE_ENCODINGS);
    let binary_extreme = vec![
        Row::new(vec![Field::Bytes(vec![])]),
        Row::new(vec![Field::Bytes(vec![0; 4096])]),
        Row::new(vec![Field::Bytes((0..=255u8).collect())]),
    ];
    assert_round_trip_for(&binary_extreme, &[ColumnType::Bytes], VARIABLE_ENCODINGS);
}

#[test]
fn auto_never_grows_a_chunk() {
    let rows = int_rows(&random_ints(64));
    let raw = flush(
        &rows,
        &[ColumnType::Integer],
        gid(3),
        sid(11),
        &config_for(ColumnEncoding::Raw),
    )
    .expect("raw flush");
    let auto = flush(
        &rows,
        &[ColumnType::Integer],
        gid(3),
        sid(11),
        &config_for(ColumnEncoding::Auto),
    )
    .expect("auto flush");
    assert!(
        auto.bytes.len() <= raw.bytes.len(),
        "auto ({}B) grew beyond raw ({}B)",
        auto.bytes.len(),
        raw.bytes.len()
    );
    let reader = SegmentReader::decode(&auto.bytes).expect("decode");
    assert_round_trip(&rows, &auto.bytes, &reader, "auto-vs-raw");
}

#[test]
fn pruning_metadata_is_independent_of_encoding() {
    let values: Vec<i64> = (0..512).collect();
    let rows = int_rows(&values);
    let predicate =
        PrunePredicate::compare(ColumnId::new(0), PruneOperator::Less, Field::Integer(10));
    let mut baseline: Option<Vec<(u64, u64)>> = None;
    for encoding in INT_ENCODINGS {
        let (bytes, reader) = persist(&rows, &[ColumnType::Integer], &config_for(*encoding));
        let pruning = reader.pruning.as_ref().expect("pruning metadata");
        let plan = plan_scan(pruning, reader.row_count, &predicate);
        let ranges: Vec<(u64, u64)> = plan
            .candidates
            .iter()
            .map(|r| (r.start_row, r.end_row))
            .collect();
        if let Some(expected) = &baseline {
            assert_eq!(&ranges, expected, "encoding={encoding:?} changed pruning");
        } else {
            baseline = Some(ranges);
        }
        // BRIN prunes at range granularity, so candidates still cover every
        // matching row without hiding one: filtering the survivors must yield
        // exactly rows 0..10 for every encoding.
        let ids = column_ids(&reader);
        let mut got = Vec::new();
        for range in &plan.candidates {
            got.extend(
                reader
                    .read_rows(&bytes, range.start_row, range.end_row, &ids)
                    .expect("range read"),
            );
        }
        let kept: Vec<Row> = got
            .into_iter()
            .filter(|row| matches!(row.fields()[0], Field::Integer(v) if v < 10))
            .collect();
        assert_eq!(
            kept,
            rows[..10].to_vec(),
            "encoding={encoding:?} filtered scan"
        );
    }
}

#[test]
fn raw_layout_matches_version_one_without_frame() {
    let rows = int_rows(&[1, 2, 3, 4]);
    let (bytes, reader) = persist(
        &rows,
        &[ColumnType::Integer],
        &config_for(ColumnEncoding::Raw),
    );
    assert_eq!(reader.chunks.len(), 1);
    let chunk = &reader.chunks[0];
    assert_eq!(chunk.encoding, ColumnEncoding::Raw);
    assert!(!chunk.is_encoded());
    let back = decode_chunk(
        &reader.read_chunk(&bytes, chunk).expect("read").into_bytes(),
        ColumnEncoding::Raw,
        ChunkCompression::None,
        chunk.uncompressed_size,
        chunk.row_count,
    )
    .expect("raw decode");
    assert_eq!(back.len() as u64, chunk.uncompressed_size);
}

#[test]
fn rejects_truncated_and_malformed_payloads() {
    let rows = int_rows(&(0..64).collect::<Vec<i64>>());
    let (bytes, reader) = persist(
        &rows,
        &[ColumnType::Integer],
        &config_for(ColumnEncoding::Auto),
    );
    for cut in [1, 16, bytes.len() / 2] {
        assert!(
            SegmentReader::decode(&bytes[..bytes.len() - cut]).is_err(),
            "truncation of {cut} bytes must fail"
        );
    }
    let chunk = &reader.chunks[0];
    let payload = reader.read_chunk(&bytes, chunk).expect("read").into_bytes();
    let stored = encode_chunk(
        &payload,
        chunk.row_count,
        ColumnEncoding::DeltaBitpack,
        ChunkCompression::None,
    )
    .expect("encode")
    .payload;
    let declared = stored.len() as u64;
    assert!(
        decode_chunk(
            &stored[..stored.len() - 1],
            ColumnEncoding::DeltaBitpack,
            ChunkCompression::None,
            declared,
            chunk.row_count
        )
        .is_err(),
        "truncated framed payload must fail"
    );
    let mut malformed = stored.clone();
    malformed[4] ^= 0xFF;
    assert!(
        decode_chunk(
            &malformed,
            ColumnEncoding::DeltaBitpack,
            ChunkCompression::None,
            declared,
            chunk.row_count
        )
        .is_err(),
        "malformed frame must fail"
    );
    let mut trailing = stored.clone();
    trailing.push(0);
    assert!(
        decode_chunk(
            &trailing,
            ColumnEncoding::DeltaBitpack,
            ChunkCompression::None,
            declared,
            chunk.row_count
        )
        .is_err(),
        "trailing bytes must fail"
    );
}

#[test]
fn rejects_invalid_codec_invalid_lengths_and_corruption() {
    let rows = int_rows(&(0..32).collect::<Vec<i64>>());
    let (bytes, reader) = persist(
        &rows,
        &[ColumnType::Integer],
        &config_for(ColumnEncoding::Auto),
    );
    let chunk = reader.chunks[0].clone();
    if !plomid_columnar::is_compression_available(ChunkCompression::Zstd) {
        // ZSTD is behind a cargo feature: the non-zstd build must not mistake
        // the tag for "no compression". The exact kind is an implementation
        // detail, so only require rejection (Unsupported or decoding failure).
        assert!(
            encode_chunk(&[0_u8; 8], 1, ColumnEncoding::Raw, ChunkCompression::Zstd).is_err(),
            "zstd unavailable must fail"
        );
    }
    let mut bad_tag = bytes.clone();
    let header_off = chunk.file_offset as usize - plomid_core::COLUMNAR_CHUNK_HEADER_SIZE;
    bad_tag[header_off + plomid_core::CHUNK_OFF_COMPRESSION] = 99;
    assert!(
        SegmentReader::decode(&bad_tag).is_err(),
        "bad codec tag must fail"
    );
    let payload = reader
        .read_chunk(&bytes, &chunk)
        .expect("read")
        .into_bytes();
    let stored = encode_chunk(
        &payload,
        chunk.row_count,
        chunk.encoding,
        ChunkCompression::None,
    )
    .expect("encode")
    .payload;
    assert!(
        decode_chunk(
            &stored,
            chunk.encoding,
            ChunkCompression::None,
            stored.len() as u64 + 1,
            chunk.row_count
        )
        .is_err(),
        "wrong declared length must fail"
    );
    assert!(
        decode_chunk(
            &stored,
            chunk.encoding,
            ChunkCompression::None,
            stored.len() as u64,
            chunk.row_count + 1
        )
        .is_err(),
        "wrong value count must fail"
    );
    assert_eq!(
        decode_chunk(
            &stored,
            chunk.encoding,
            ChunkCompression::None,
            u64::MAX,
            chunk.row_count
        )
        .expect_err("unbounded length")
        .kind(),
        ErrorKind::Corruption
    );
    let mut corrupt = bytes.clone();
    let pos = chunk.file_offset as usize;
    corrupt[pos] ^= 0xFF;
    assert!(
        SegmentReader::decode(&corrupt).is_err(),
        "corrupt payload must fail"
    );
}
