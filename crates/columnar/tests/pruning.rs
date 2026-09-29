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
//! Zone maps + BRIN pruning against real immutable columnar segments.
//!
//! Every test here goes through the *production* pipeline:
//!
//! ```text
//! rows → flush (chunks + statistics + zone maps/BRIN + seal) → segment bytes
//!      → SegmentReader::decode (checksums + trailer validation)
//!      → plan_scan / prune_candidates
//!      → read_rows over the surviving candidate ranges
//! ```
//!
//! The central invariant, checked against ground truth rather than against
//! the metadata itself, is:
//!
//! > A range classified PRUNE must contain zero rows for which the predicate
//! > is TRUE.
//!
//! The tests therefore never trust a verdict: they count matches by scanning
//! every row directly, then require that the pruned scan produce exactly the
//! same answer.

use plomid_columnar::{
    flush, plan_scan, row_matches, CandidateRange, ColumnType, FlushConfig, PruneOperator,
    PrunePredicate, SegmentPruning, SegmentReader, Tri,
};
use plomid_core::{ColumnId, GenerationId, SegmentId};
use plomid_storage::{Field, Row};

fn gid(value: u64) -> GenerationId {
    GenerationId::new(value)
}

fn sid(value: u64) -> SegmentId {
    SegmentId::new(value)
}

fn int_col() -> ColumnId {
    ColumnId::new(0)
}

fn text_col() -> ColumnId {
    ColumnId::new(1)
}

/// Flushes `rows` with pruning metadata cut every `rows_per_range` rows.
fn flush_with_pruning(rows: &[Row], types: &[ColumnType], rows_per_range: u64) -> Vec<u8> {
    let config = FlushConfig {
        brin_rows_per_range: rows_per_range,
        ..FlushConfig::default()
    };
    flush(rows, types, gid(1), sid(1), &config)
        .expect("flush")
        .bytes
}

/// Deterministic xorshift64 generator: fixed seeds keep failures replayable.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn next_i64(&mut self) -> i64 {
        self.next() as i64
    }
}

fn row(value: Option<i64>, text: Option<&str>) -> Row {
    Row::new(vec![
        value.map_or(Field::Null, Field::Integer),
        text.map_or(Field::Null, |t| Field::String(t.to_owned())),
    ])
}

fn types() -> Vec<ColumnType> {
    vec![ColumnType::Integer, ColumnType::String]
}

/// Ground truth: rows for which `predicate` evaluates TRUE, with SQL
/// three-valued logic (NULL is not TRUE, so NULL rows never match).
fn matching_rows(rows: &[Row], predicate: &PrunePredicate) -> Vec<usize> {
    (0..rows.len())
        .filter(|&idx| {
            row_matches(predicate, &|column_id| {
                rows[idx].fields().get(column_id.get() as usize).cloned()
            }) == Tri::True
        })
        .collect()
}

/// True when some candidate range covers physical row `position`.
fn covered(candidates: &[CandidateRange], position: u64) -> bool {
    candidates
        .iter()
        .any(|range| range.start_row <= position && position < range.end_row)
}

fn candidate_row_count(candidates: &[CandidateRange]) -> u64 {
    candidates.iter().map(CandidateRange::row_count).sum()
}

/// The predicates every dataset is swept with: all supported operators over
/// in-range, out-of-range, boundary, zero, and extreme literals, both for the
/// integer column and the string column, plus NULL tests and compound forms.
fn sweep_predicates(rows: &[Row]) -> Vec<PrunePredicate> {
    let mut literals: Vec<i64> = Vec::new();
    let values: Vec<i64> = rows
        .iter()
        .filter_map(|row| match row.fields()[0] {
            Field::Integer(v) => Some(v),
            _ => None,
        })
        .collect();
    if let Some(min) = values.iter().copied().min() {
        // Saturating so a dataset holding i64::MIN / i64::MAX cannot panic
        // while building its own boundary literals.
        literals.extend([min.saturating_sub(1), min, min.saturating_add(1)]);
    }
    if let Some(max) = values.iter().copied().max() {
        literals.extend([max.saturating_sub(1), max, max.saturating_add(1)]);
    }
    literals.extend([
        0,
        -1,
        1,
        i64::MIN,
        i64::MIN + 1,
        i64::MAX,
        i64::MAX - 1,
        42,
        -42,
    ]);

    let operators = [
        PruneOperator::Equal,
        PruneOperator::NotEqual,
        PruneOperator::Less,
        PruneOperator::LessOrEqual,
        PruneOperator::Greater,
        PruneOperator::GreaterOrEqual,
    ];

    let mut predicates = Vec::new();
    for literal in &literals {
        for operator in operators {
            predicates.push(PrunePredicate::compare(
                int_col(),
                operator,
                Field::Integer(*literal),
            ));
        }
    }
    for text in ["", "a", "alpha", "zzz", "name-000001"] {
        for operator in operators {
            predicates.push(PrunePredicate::compare(
                text_col(),
                operator,
                Field::String(text.to_owned()),
            ));
        }
    }
    predicates.push(PrunePredicate::is_null(int_col()));
    predicates.push(PrunePredicate::is_not_null(int_col()));
    predicates.push(PrunePredicate::is_null(text_col()));
    predicates.push(PrunePredicate::is_not_null(text_col()));

    // Compound predicates: AND (prune when either side proves emptiness),
    // OR (prune only when both sides do), and NOT over safe duals.
    let mut compound = Vec::new();
    for (index, base) in predicates.iter().enumerate() {
        if index % 3 == 0 {
            compound.push(base.clone().and(PrunePredicate::is_not_null(int_col())));
        }
        if index % 5 == 0 {
            compound.push(base.clone().or(PrunePredicate::compare(
                int_col(),
                PruneOperator::Less,
                Field::Integer(0),
            )));
        }
        if index % 7 == 0 {
            compound.push(base.clone().not());
        }
    }
    predicates.extend(compound);
    predicates
}

/// Decodes a segment image and returns its reader plus its pruning metadata.
fn decode_with_pruning(bytes: &[u8]) -> (SegmentReader, SegmentPruning) {
    let reader = SegmentReader::decode(bytes).expect("decode");
    let pruning = reader
        .pruning
        .clone()
        .expect("flushed segment carries pruning metadata");
    pruning.validate().expect("metadata validates");
    (reader, pruning)
}

/// Full-scan ground truth: the rows for which `predicate` is TRUE, in order.
fn scan_all(reader: &SegmentReader, bytes: &[u8], predicate: &PrunePredicate) -> Vec<Row> {
    let ids: Vec<ColumnId> = vec![int_col(), text_col()];
    let rows = reader
        .read_rows(bytes, 0, reader.row_count, &ids)
        .expect("full scan");
    rows.into_iter()
        .filter(|row| {
            row_matches(predicate, &|column_id| {
                row.fields().get(column_id.get() as usize).cloned()
            }) == Tri::True
        })
        .collect()
}

/// Pruned scan: decodes only the candidate ranges the plan kept.
fn scan_candidates(
    reader: &SegmentReader,
    bytes: &[u8],
    candidates: &[CandidateRange],
) -> Vec<Row> {
    let ids: Vec<ColumnId> = vec![int_col(), text_col()];
    let mut rows = Vec::new();
    for candidate in candidates {
        rows.extend(
            reader
                .read_rows(bytes, candidate.start_row, candidate.end_row, &ids)
                .expect("candidate scan"),
        );
    }
    rows
}

/// Pruned scan with the predicate applied on top: exactly what an executor
/// returns from a candidate scan, in candidate order.
fn scan_candidates_matching(
    reader: &SegmentReader,
    bytes: &[u8],
    candidates: &[CandidateRange],
    predicate: &PrunePredicate,
) -> Vec<Row> {
    scan_candidates(reader, bytes, candidates)
        .into_iter()
        .filter(|row| {
            row_matches(predicate, &|column_id| {
                row.fields().get(column_id.get() as usize).cloned()
            }) == Tri::True
        })
        .collect()
}

/// Datasets the central invariant is checked against. Every shape the task
/// calls out explicitly is present: empty, single-row, identical, ascending,
/// descending, random, negative, extreme, NULL-only, and mixed NULL.
fn datasets() -> Vec<(&'static str, Vec<Row>)> {
    let mut sets: Vec<(&'static str, Vec<Row>)> = Vec::new();
    sets.push(("empty", Vec::new()));
    sets.push(("single", vec![row(Some(7), Some("solo"))]));
    sets.push((
        "identical",
        (0..40).map(|_| row(Some(5), Some("same"))).collect(),
    ));
    sets.push((
        "ascending",
        (0..120)
            .map(|i| row(Some(i as i64 * 3 - 7), Some("asc")))
            .collect(),
    ));
    sets.push((
        "descending",
        (0..120)
            .map(|i| row(Some(200 - i as i64 * 2), Some("desc")))
            .collect(),
    ));
    sets.push((
        "clustered",
        (0..128)
            .map(|i| row(Some((i as i64 / 16) * 1_000), Some("clustered")))
            .collect(),
    ));
    sets.push((
        "negative",
        (0..64)
            .map(|i| row(Some(-1_000 + i as i64), Some("neg")))
            .collect(),
    ));
    sets.push((
        "extremes",
        vec![
            row(Some(i64::MIN), Some("lo")),
            row(Some(i64::MIN + 1), Some("lo1")),
            row(Some(0), Some("zero")),
            row(Some(i64::MAX - 1), Some("hi1")),
            row(Some(i64::MAX), Some("hi")),
        ],
    ));
    sets.push((
        "null_only",
        (0..48).map(|_| row(None, Some("nulls"))).collect(),
    ));
    sets.push((
        "mixed_null",
        (0..96)
            .map(|i| {
                if i % 3 == 0 {
                    row(None, None)
                } else {
                    row(Some(i as i64 - 48), Some("mixed"))
                }
            })
            .collect(),
    ));

    // Deterministic pseudo-random shapes: several seeds so both "every range
    // has bounds" and "ranges are sparse" are exercised.
    for seed in [1_u64, 7, 99, 12_345] {
        let mut rng = Rng::new(seed);
        let rows = (0..150)
            .map(|i| {
                if rng.next() % 11 == 0 {
                    row(None, None)
                } else {
                    let value = (rng.next_i64() % 900) - 450;
                    row(Some(value), Some(&format!("name-{:06}", i)))
                }
            })
            .collect();
        let name: &'static str = Box::leak(format!("random-seed-{seed}").into_boxed_str());
        sets.push((name, rows));
    }
    sets
}

/// THE mandatory property: for every dataset and every predicate, no range
/// the metadata classified PRUNE may contain a matching row, and the pruned
/// scan must return exactly the rows a full scan returns.
#[test]
fn pruned_scan_matches_full_scan_for_every_dataset_and_predicate() {
    for (name, rows) in datasets() {
        let bytes = flush_with_pruning(&rows, &types(), 16);
        let (reader, pruning) = decode_with_pruning(&bytes);
        assert_eq!(reader.row_count, rows.len() as u64, "{name}: row count");

        for predicate in sweep_predicates(&rows) {
            let expected = matching_rows(&rows, &predicate);
            let plan = plan_scan(&pruning, reader.row_count, &predicate);

            // (1) False-negative defense: every matching row must still be
            //     inside some candidate range.
            for &index in &expected {
                assert!(
                    covered(&plan.candidates, index as u64),
                    "{name}: predicate {predicate:?} pruned row {index}, which matches"
                );
            }

            // (2) The pruned scan must return the same rows as the full scan.
            let scanned = scan_candidates(&reader, &bytes, &plan.candidates);
            let actual: Vec<Row> = scanned
                .into_iter()
                .filter(|row| {
                    row_matches(&predicate, &|column_id| {
                        row.fields().get(column_id.get() as usize).cloned()
                    }) == Tri::True
                })
                .collect();
            let expected_rows: Vec<Row> =
                expected.iter().map(|&index| rows[index].clone()).collect();
            assert_eq!(actual, expected_rows, "{name}: predicate {predicate:?}");
        }
    }
}

/// Cluster-then-check: pruning must actually fire on clustered data, and the
/// candidate scan must decode exactly the kept rows.
#[test]
fn clustered_data_prunes_while_scan_stays_exact() {
    let clustered: Vec<Row> = (0..160)
        .map(|i| row(Some((i as i64 / 32) * 100), Some("c")))
        .collect();
    let predicate = PrunePredicate::eq(int_col(), Field::Integer(200));
    let expected = matching_rows(&clustered, &predicate);
    assert!(!expected.is_empty(), "the literal must match somewhere");

    let bytes = flush_with_pruning(&clustered, &types(), 32);
    let (reader, pruning) = decode_with_pruning(&bytes);
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert!(plan.pruned_rows() > 0, "clustered data must prune: {plan}");
    for &index in &expected {
        assert!(covered(&plan.candidates, index as u64));
    }
    let scanned = scan_candidates(&reader, &bytes, &plan.candidates);
    assert_eq!(scanned.len() as u64, plan.scanned_rows);

    // One row per range makes each range exact.
    let bytes = flush_with_pruning(&clustered, &types(), 1);
    let (reader, pruning) = decode_with_pruning(&bytes);
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert_eq!(plan.scanned_rows, expected.len() as u64);
}

/// An empty segment produces no candidates and needs no metadata.
#[test]
fn empty_segment_scan_plan_is_empty() {
    let rows: Vec<Row> = Vec::new();
    let bytes = flush_with_pruning(&rows, &types(), 16);
    let (reader, pruning) = decode_with_pruning(&bytes);
    assert_eq!(reader.row_count, 0);
    assert!(pruning.is_empty(), "nothing to summarize");
    let plan = plan_scan(
        &pruning,
        reader.row_count,
        &PrunePredicate::eq(int_col(), Field::Integer(1)),
    );
    assert!(plan.candidates.is_empty());
    assert_eq!(candidate_row_count(&plan.candidates), 0);
    assert_eq!(plan.scanned_rows, 0);
    assert_eq!(plan.pruned_rows(), 0);
    assert_eq!(plan.pruning_ratio(), 0.0);
    assert!(scan_candidates(&reader, &bytes, &plan.candidates).is_empty());
}
/// Returns the zone map of `column_id` inside a BRIN range.
fn zone_of(
    pruning: &SegmentPruning,
    range_index: usize,
    column_id: ColumnId,
) -> plomid_columnar::ZoneMap {
    let brin = pruning.brin.as_ref().expect("brin");
    brin.ranges[range_index]
        .zone_maps
        .iter()
        .find(|zone| zone.column_id == column_id)
        .cloned()
        .expect("zone map for column")
}

/// Ground truth for one physical row interval, derived from the rows rather
/// than from the metadata.
fn interval_summary(rows: &[Row], start: u64, end: u64) -> (Option<i64>, Option<i64>, u64, u64) {
    let mut min = None;
    let mut max = None;
    let mut nulls = 0_u64;
    for row in &rows[start as usize..end as usize] {
        match row.fields()[0] {
            Field::Null => nulls += 1,
            Field::Integer(value) => {
                min = Some(min.map_or(value, |current: i64| current.min(value)));
                max = Some(max.map_or(value, |current: i64| current.max(value)));
            }
            ref other => panic!("unexpected field {other:?}"),
        }
    }
    (min, max, nulls, end - start)
}

/// One range, many ranges, and adjacent ranges: BRIN ranges tile the segment
/// and each range's zone map describes exactly its own rows.
#[test]
fn brin_ranges_tile_the_segment_and_summarize_exactly_their_rows() {
    let rows: Vec<Row> = (0..100)
        .map(|i| row(Some(i as i64 * 5 - 200), Some("t")))
        .collect();
    for rows_per_range in [1_u64, 3, 16, 64, 100, 250] {
        let bytes = flush_with_pruning(&rows, &types(), rows_per_range);
        let (reader, pruning) = decode_with_pruning(&bytes);
        let brin = pruning.brin.as_ref().expect("brin");
        assert_eq!(brin.row_count, reader.row_count, "rpr={rows_per_range}");
        let mut expected_start = 0_u64;
        for (index, range) in brin.ranges.iter().enumerate() {
            assert_eq!(range.start_row, expected_start, "ranges must be contiguous");
            assert!(range.end_row > range.start_row, "no empty ranges");
            assert!(range.end_row <= reader.row_count);
            let zone = zone_of(&pruning, index, int_col());
            let (min, max, nulls, count) = interval_summary(&rows, range.start_row, range.end_row);
            assert_eq!(zone.row_count, count, "rpr={rows_per_range} range {index}");
            assert_eq!(zone.min, min.map(Field::Integer));
            assert_eq!(zone.max, max.map(Field::Integer));
            let expected_state = if nulls == 0 {
                plomid_columnar::NullState::NoNulls
            } else if nulls == count {
                plomid_columnar::NullState::AllNulls
            } else {
                plomid_columnar::NullState::HasNulls
            };
            assert_eq!(zone.null_state, expected_state);
            expected_start = range.end_row;
        }
        assert_eq!(expected_start, reader.row_count, "ranges cover every row");
    }
}

/// Adjacent ranges with touching values: a predicate selecting one side of the
/// boundary prunes the other side, and nothing else.
#[test]
fn brin_adjacent_ranges_respect_exact_boundaries() {
    // Rows 0..32 hold 0..31; rows 32..64 hold 1000..1031.
    let rows: Vec<Row> = (0..64)
        .map(|i| {
            let value = if i < 32 {
                i as i64
            } else {
                1_000 + (i as i64 - 32)
            };
            row(Some(value), Some("b"))
        })
        .collect();
    let bytes = flush_with_pruning(&rows, &types(), 32);
    let (reader, pruning) = decode_with_pruning(&bytes);
    assert_eq!(pruning.brin.as_ref().expect("brin").ranges.len(), 2);

    // `<= 31` selects the first cluster; the second is provably impossible.
    let predicate =
        PrunePredicate::compare(int_col(), PruneOperator::LessOrEqual, Field::Integer(31));
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert_eq!(plan.candidates, vec![CandidateRange::new(0, 32)]);
    assert_eq!(plan.scanned_rows, 32);
    assert_eq!(
        scan_all(&reader, &bytes, &predicate),
        scan_candidates_matching(&reader, &bytes, &plan.candidates, &predicate)
    );

    // `>= 32` cannot match the first cluster (max 31 < 32).
    let predicate =
        PrunePredicate::compare(int_col(), PruneOperator::GreaterOrEqual, Field::Integer(32));
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert_eq!(plan.candidates, vec![CandidateRange::new(32, 64)]);
    assert_eq!(plan.scanned_rows, 32);
    assert_eq!(
        scan_all(&reader, &bytes, &predicate),
        scan_candidates_matching(&reader, &bytes, &plan.candidates, &predicate)
    );

    // `= 999` (between the clusters) prunes both ranges.
    let predicate = PrunePredicate::eq(int_col(), Field::Integer(999));
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert!(plan.candidates.is_empty());
    assert_eq!(plan.pruned_rows(), 64);
    assert!(scan_all(&reader, &bytes, &predicate).is_empty());

    // A literal just above the first cluster's maximum still keeps the range
    // that holds it: pruning is per range, not per segment.
    let predicate = PrunePredicate::compare(int_col(), PruneOperator::Greater, Field::Integer(31));
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert!(plan.candidates.contains(&CandidateRange::new(32, 64)));
    assert_eq!(plan.scanned_rows, 32);
}

/// Clustered values prune; unclustered values must not (and must stay exact).
#[test]
fn clustered_data_prunes_and_unclustered_data_keeps() {
    let clustered: Vec<Row> = (0..128)
        .map(|i| row(Some((i as i64 / 32) * 100), Some("c")))
        .collect();
    let unclustered: Vec<Row> = (0..128)
        .map(|i| row(Some((i % 4) as i64 * 100), Some("u")))
        .collect();
    let predicate = PrunePredicate::eq(int_col(), Field::Integer(200));

    let bytes = flush_with_pruning(&clustered, &types(), 32);
    let (reader, pruning) = decode_with_pruning(&bytes);
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert_eq!(plan.candidates, vec![CandidateRange::new(64, 96)]);
    assert_eq!(plan.scanned_rows, 32);
    assert_eq!(plan.pruned_rows(), 96);
    assert_eq!(
        scan_all(&reader, &bytes, &predicate),
        scan_candidates_matching(&reader, &bytes, &plan.candidates, &predicate)
    );

    // The same values, interleaved so that every range spans the whole domain
    // (min 0, max 300): bounds can no longer prove absence, so nothing prunes
    // even though half the rows still answer TRUE to `= 200`.
    let bytes = flush_with_pruning(&unclustered, &types(), 32);
    let (reader, pruning) = decode_with_pruning(&bytes);
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert_eq!(
        plan.pruned_rows(),
        0,
        "unclustered data must not prune: {plan}"
    );
    assert_eq!(plan.scanned_rows, reader.row_count);
    assert_eq!(scan_all(&reader, &bytes, &predicate).len(), 32);
    assert_eq!(
        scan_all(&reader, &bytes, &predicate),
        scan_candidates_matching(&reader, &bytes, &plan.candidates, &predicate)
    );
}

/// Sparse ranges (few non-NULL values among many NULLs) still report exact
/// bounds and NULL state, and `IS NULL` / `IS NOT NULL` stay correct.
#[test]
fn sparse_ranges_keep_exact_bounds_and_null_state() {
    // Every 16th row carries a value; the rest are NULL.
    let rows: Vec<Row> = (0..64)
        .map(|i| {
            if i % 16 == 0 {
                row(Some(i as i64), Some("sparse"))
            } else {
                row(None, None)
            }
        })
        .collect();
    let bytes = flush_with_pruning(&rows, &types(), 16);
    let (reader, pruning) = decode_with_pruning(&bytes);
    for (index, range) in pruning
        .brin
        .as_ref()
        .expect("brin")
        .ranges
        .iter()
        .enumerate()
    {
        let zone = zone_of(&pruning, index, int_col());
        let (min, max, nulls, count) = interval_summary(&rows, range.start_row, range.end_row);
        assert_eq!(zone.min, min.map(Field::Integer));
        assert_eq!(zone.max, max.map(Field::Integer));
        assert_eq!(zone.null_state, plomid_columnar::NullState::HasNulls);
        assert_eq!(nulls, count - 1);
    }
    // `IS NOT NULL` keeps every range that holds a value, prunes none of them.
    let predicate = PrunePredicate::is_not_null(int_col());
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert_eq!(plan.scanned_rows, reader.row_count);
    assert_eq!(plan.pruned_rows(), 0);
    assert_eq!(scan_all(&reader, &bytes, &predicate).len(), 4);
    // `IS NULL` must keep every range too: each range holds 15 NULLs, so no
    // range can be proven free of NULLs.
    let predicate = PrunePredicate::is_null(int_col());
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert_eq!(plan.pruned_rows(), 0);
    assert_eq!(scan_all(&reader, &bytes, &predicate).len(), 60);
    // The text column carries its value in exactly the same rows, so its NULL
    // state matches and the candidate scan stays exact there as well.
    let predicate = PrunePredicate::is_not_null(text_col());
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert_eq!(plan.pruned_rows(), 0);
    assert_eq!(
        scan_all(&reader, &bytes, &predicate),
        scan_candidates_matching(&reader, &bytes, &plan.candidates, &predicate)
    );
}

/// A column that is NULL in every row: `IS NOT NULL` prunes the whole
/// segment, `IS NULL` keeps all of it, and each range's metadata is exact.
#[test]
fn all_null_column_prunes_is_not_null() {
    let rows: Vec<Row> = (0..48).map(|i| row(Some(i as i64), None)).collect();
    let bytes = flush_with_pruning(&rows, &types(), 16);
    let (reader, pruning) = decode_with_pruning(&bytes);
    for index in 0..pruning.brin.as_ref().expect("brin").ranges.len() {
        let zone = zone_of(&pruning, index, text_col());
        assert_eq!(zone.null_state, plomid_columnar::NullState::AllNulls);
        assert!(!zone.has_bounds(), "an all-NULL zone has no bounds");
        assert_eq!(zone.row_count, 16);
    }
    let predicate = PrunePredicate::is_not_null(text_col());
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert!(plan.candidates.is_empty());
    assert_eq!(plan.pruned_rows(), reader.row_count);
    assert!(scan_all(&reader, &bytes, &predicate).is_empty());

    let predicate = PrunePredicate::is_null(text_col());
    let plan = plan_scan(&pruning, reader.row_count, &predicate);
    assert_eq!(plan.pruned_rows(), 0);
    assert_eq!(scan_all(&reader, &bytes, &predicate).len(), 48);
}

/// SQL AST → PrunePredicate bridge: lowers a real `plomid_sql::Expression` and
/// confirms the resulting plan matches a hand-built predicate, and that an
/// unsupported expression degrades to "scan everything".
#[test]
fn sql_bridge_lowers_expressions_to_equivalent_predicates() {
    use plomid_columnar::{lower_sql_expression, plan_scan, SqlLowering};
    use plomid_sql::Expression;
    use std::collections::BTreeMap;

    let rows: Vec<Row> = (0..128).map(|i| row(Some(i as i64), Some("x"))).collect();
    let bytes = flush_with_pruning(&rows, &types(), 32);
    let (reader, pruning) = decode_with_pruning(&bytes);

    let columns = BTreeMap::from([
        ("id".to_owned(), int_col()),
        ("name".to_owned(), text_col()),
    ]);

    // `id = 64` lowers to the same predicate as a hand-built one.
    let lowered = lower_sql_expression(
        &Expression::Equal(
            Box::new(Expression::ColumnRef("id".to_owned())),
            Box::new(Expression::Literal(plomid_sql::Value::Int8(64))),
        ),
        &columns,
    );
    let prunable = lowered.predicate().expect("lowers");
    let hand = PrunePredicate::eq(int_col(), Field::Integer(64));
    assert_eq!(prunable, &hand);
    let plan = plan_scan(&pruning, reader.row_count, prunable);
    assert_eq!(
        scan_all(&reader, &bytes, prunable),
        scan_candidates_matching(&reader, &bytes, &plan.candidates, prunable)
    );

    // `id BETWEEN 32 AND 64` → conjunction; scan stays exact.
    let lowered = lower_sql_expression(
        &Expression::Between {
            expr: Box::new(Expression::ColumnRef("id".to_owned())),
            low: Box::new(Expression::Literal(plomid_sql::Value::Int8(32))),
            high: Box::new(Expression::Literal(plomid_sql::Value::Int8(64))),
            negated: false,
        },
        &columns,
    );
    let prunable = lowered.predicate().expect("between lowers");
    let plan = plan_scan(&pruning, reader.row_count, prunable);
    assert!(plan.pruned_rows() < reader.row_count, "between prunes some");
    assert_eq!(
        scan_all(&reader, &bytes, prunable),
        scan_candidates_matching(&reader, &bytes, &plan.candidates, prunable)
    );

    // `lower(name)` is opaque: the bridge reports no lowering, and the plan
    // scans everything (metadata untouched for the unsupported predicate).
    let lowered = lower_sql_expression(
        &Expression::FunctionCall {
            name: "lower".to_owned(),
            args: vec![Expression::ColumnRef("name".to_owned())],
            distinct: false,
            filter: None,
            order_by: Vec::new(),
            returning: None,
            null_handling: None,
            unique_keys: None,
        },
        &columns,
    );
    assert!(!lowered.prunable());
    assert!(matches!(lowered, SqlLowering::Unprunable(_)));
}
