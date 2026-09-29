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
//! Vectorized batch aggregation over columnar generations.
//!
//! # Why this exists
//!
//! The scalar SQL path answers columnar queries by converting every cell
//! through `Row → Field → Value` and evaluating expressions row-at-a-time
//! (measured ~21µs/row in debug: ~417ms of a 448ms aggregation over 20K
//! rows, versus ~31ms for the columnar crate's own decode + materialize).
//! This module evaluates supported aggregate shapes directly over typed
//! column data: tight passes over dense integer arrays, with no per-cell
//! allocation.
//!
//! # Exactness contract
//!
//! The vector path never guesses. It answers a query only when every piece
//! is provably identical to the scalar executor:
//!
//! * Predicates reuse the already-lowered [`PrunePredicate`], but a lowered
//!   predicate is only an *exact* row filter under narrow conditions (see
//!   [`normalize_predicate`]): integer comparisons over physically-`Integer`
//!   columns with integer literals, plus `IS NULL` / `IS NOT NULL` over any
//!   column. Anything else declines and the caller keeps the scalar path.
//!   Cross-type leaves (e.g. an integer column against a text literal, which
//!   the scalar executor resolves through casts) always decline.
//! * `NOT` is pushed through operator duals and De Morgan's law before
//!   evaluation, which preserves SQL three-valued logic for `WHERE` (only
//!   `TRUE` keeps a row). Evaluating `NOT` naively over a boolean mask would
//!   keep `NULL` rows and be wrong.
//! * The scalar executor compares integers through `f64`, which rounds beyond
//!   ±2⁵³. Exact `i64` evaluation matches it only inside that range, so any
//!   integer comparison or `MIN`/`MAX` requires the [`F64_EXACT_INT_RANGE`]
//!   guard: every literal and every involved segment's column bounds must lie
//!   inside ±2⁵³ (bounds come from persisted statistics; missing statistics
//!   decline). Outside the range the caller keeps the scalar path, i.e. the
//!   historical behavior is preserved bit-for-bit.
//! * Aggregates mirror the scalar accumulator semantics (the executor maps the
//!   typed states to `Value`s with the same formulas): `COUNT(*)` counts
//!   mask-passing rows, `COUNT(col)` skips `NULL`s, `SUM`/`AVG` skip `NULL`s
//!   and yield `NULL` on empty input with identical `i64` overflow behavior
//!   (`+=`), `MIN`/`MAX` skip `NULL`s and yield `NULL` on empty input.
//!   Malformed integer payloads (wrong length) decline the request, keeping
//!   the scalar path authoritative for damaged data.
//! * `GROUP BY` keys are integer columns only; `NULL` keys form one group,
//!   matching the scalar behavior for uniformly-typed integer keys.
//!
//! Anything outside these shapes returns `Ok(None)` ("not supported, use the
//! scalar path"). Corruption still returns `Err`.

use crate::column::ColumnType;
use crate::pruning::{CandidateRange, PruneOperator, PrunePredicate, ScanPlan};
use crate::read::SegmentReader;
use crate::store::ColumnarStore;
use plomid_core::{ColumnId, ObjectId, Result};
use plomid_storage::{Field, StorageEngine};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Row-block size for the compute loops.
///
/// Column data is materialized per pruned candidate span; the predicate and
/// aggregate passes then run over `batch_rows` windows for cache locality.
/// Measured on 8K rows (global COUNT+SUM, debug): 256→9.8ms, 1024→8.6ms,
/// 4096→7.4ms, 16384→6.8ms, 65536→6.3ms (mostly segment I/O; larger blocks
/// amortize per-block mask allocations). 16384 keeps masks at 16KB while
/// sitting at the knee of the curve.
pub const VECTOR_BATCH_ROWS: usize = 16384;

/// Largest magnitude for which `f64` comparison of integers is exact.
///
/// The scalar executor funnels every integer comparison through `f64`, which
/// represents all integers in `[-2⁵³, 2⁵³]` exactly. Inside this range exact
/// `i64` evaluation provably agrees with the scalar path; outside it the
/// vector path declines. See the module docs.
pub const F64_EXACT_INT_RANGE: i64 = 1 << 53;

/// One supported aggregate over a batch.
#[derive(Clone, Debug)]
pub struct VectorAggregate {
    /// What to compute.
    pub kind: VectorAggKind,
    /// Argument column (`None` only for `COUNT(*)`).
    pub column: Option<ColumnId>,
    /// Per-aggregate `FILTER (WHERE ...)` predicate, already lowered.
    pub filter: Option<PrunePredicate>,
}

/// Supported aggregate operators.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorAggKind {
    /// `COUNT(*)`: rows passing the mask.
    CountStar,
    /// `COUNT(col)`: non-`NULL` values passing the mask.
    Count,
    /// `SUM(intcol)`.
    Sum,
    /// `MIN(intcol)`.
    Min,
    /// `MAX(intcol)`.
    Max,
    /// `AVG(intcol)`.
    Avg,
    /// `COUNT(DISTINCT intcol)`.
    CountDistinct,
}

/// A vector aggregation request over one table's published generation.
#[derive(Clone, Debug)]
pub struct VectorRequest {
    /// Aggregates to compute in one shared scan.
    pub aggregates: Vec<VectorAggregate>,
    /// Statement `WHERE` predicate, already lowered. `None` selects all rows.
    pub filter: Option<PrunePredicate>,
    /// `GROUP BY` key columns (positional columnar identities). Empty means
    /// a single global group. Integer columns only.
    pub group_by: Vec<ColumnId>,
    /// Compute block size; [`VECTOR_BATCH_ROWS`] when zero.
    pub batch_rows: usize,
    /// Allows `String` comparison leaves to evaluate by byte order.
    ///
    /// The columnar layer cannot tell identity-flushed text (`TEXT`,
    /// `VARCHAR`, ...) from canonicalized encodings (timestamps, booleans),
    /// for which byte order is not value order. The SQL layer sets this only
    /// after verifying every string-compared column is a plain text family
    /// type; `Bytes` leaves need no flag (physical `Bytes` always holds raw
    /// `BYTEA`, compared by byte order on both paths).
    pub text_ordering_exact: bool,
}

/// Accumulator state for one (group, aggregate) cell.
#[derive(Clone, Debug, Default)]
pub struct VectorAcc {
    /// Rows seen (for `COUNT(*)`).
    pub count_star: i64,
    /// Non-`NULL` values seen (for `COUNT(col)`).
    pub count: i64,
    /// `SUM` state plus whether any non-`NULL` value was seen.
    pub sum: i64,
    pub sum_has_any: bool,
    /// `MIN` / `MAX` state over integer columns.
    pub min: Option<i64>,
    pub max: Option<i64>,
    /// `MIN` / `MAX` state over raw string/bytes columns (byte order).
    pub min_raw: Option<Vec<u8>>,
    pub max_raw: Option<Vec<u8>>,
    /// `AVG` state.
    pub avg_sum: i64,
    pub avg_count: i64,
    /// `COUNT(DISTINCT ...)` state over integer columns.
    pub distinct: HashSet<i64>,
    /// `COUNT(DISTINCT ...)` state over raw string/bytes columns.
    pub distinct_raw: HashSet<Vec<u8>>,
}

/// One result group: integer keys (`None` = SQL `NULL` key) plus one
/// accumulator per requested aggregate, in request order.
#[derive(Clone, Debug)]
pub struct VectorGroup {
    /// Group key values aligned with `VectorRequest::group_by`.
    pub keys: Vec<Option<i64>>,
    /// Accumulators aligned with `VectorRequest::aggregates`.
    pub accs: Vec<VectorAcc>,
}

/// Result of [`vector_aggregate`], with the same physical counters as
/// [`ColumnarScan`](crate::compaction::ColumnarScan) so callers can log what
/// the query physically did.
#[derive(Debug)]
pub struct VectorResult {
    /// Result groups (exactly one with empty keys for global aggregation).
    pub groups: Vec<VectorGroup>,
    /// Segments in the published generation.
    pub segments_considered: usize,
    /// Segments skipped entirely by zone-map pruning.
    pub segments_skipped: usize,
    /// Rows covered by pruned candidate ranges.
    pub rows_examined: u64,
    /// Rows eliminated by zone-map pruning before decoding.
    pub rows_skipped: u64,
    /// Physical columns materialized.
    pub columns_read: usize,
}

/// Runs `request` over the current published generation of `object`.
///
/// Returns `Ok(None)` when the request is not supported (physical types,
/// predicate shapes, or exactness guards fail) — the caller must use the
/// scalar path. Returns `Ok(Some(_))` with the aggregated result otherwise.
pub fn vector_aggregate<E: StorageEngine>(
    store: &ColumnarStore,
    engine: &mut E,
    object_id: ObjectId,
    request: &VectorRequest,
) -> Result<Option<VectorResult>> {
    if request.aggregates.is_empty() {
        return Ok(None);
    }
    // The statement filter must be exactly evaluable (see
    // `normalize_predicate` for the proof obligations); per-aggregate
    // FILTERs get the same check. Unacceptable shapes decline before any I/O.
    let allow_text = request.text_ordering_exact;
    let filter = match request.filter.as_ref() {
        Some(predicate) => match normalize_predicate(predicate, allow_text) {
            Some(normalized) => Some(normalized),
            None => return Ok(None),
        },
        None => None,
    };
    let mut agg_filters: Vec<Option<PrunePredicate>> = Vec::with_capacity(request.aggregates.len());
    for aggregate in &request.aggregates {
        match aggregate.filter.as_ref() {
            Some(predicate) => match normalize_predicate(predicate, allow_text) {
                Some(normalized) => agg_filters.push(Some(normalized)),
                None => return Ok(None),
            },
            None => agg_filters.push(None),
        }
    }

    let snapshot = store.generations().reader()?;
    let Some(record) = snapshot.object(object_id) else {
        return Ok(None);
    };
    if !record.is_published() {
        return Ok(None);
    };
    let generation = record.current_generation;
    let segments =
        crate::compaction::generation_segments(store.generations(), object_id, generation)?;
    drop(snapshot);

    let batch_rows = if request.batch_rows == 0 {
        VECTOR_BATCH_ROWS
    } else {
        request.batch_rows.max(1)
    };

    let mut result = VectorResult {
        groups: Vec::new(),
        segments_considered: segments.len(),
        segments_skipped: 0,
        rows_examined: 0,
        rows_skipped: 0,
        columns_read: 0,
    };

    // Group state: one accumulator row per group, created on demand. The
    // global shape keeps a single row so the hot loop never hashes.
    let mut global: Vec<VectorAcc> = Vec::new();
    let mut grouped: HashMap<Vec<Option<i64>>, Vec<VectorAcc>> = HashMap::new();
    if request.group_by.is_empty() {
        global = request
            .aggregates
            .iter()
            .map(|_| VectorAcc::default())
            .collect();
    }

    for segment in segments {
        let bytes = store.read_segment(engine, segment)?;
        let reader = SegmentReader::decode(&bytes)?;
        // Metadata-only type check BEFORE any column I/O: every value
        // column must exist as physically-`Integer`, every bitmap column
        // must exist. Failures decline in microseconds (no chunk decoding),
        // so non-vector queries never pay column I/O for the attempt.
        let (need_int, need_flex, need_bytes, need_string, need_nulls) =
            collect_needed(request, filter.as_ref(), &agg_filters);
        let mut types_ok = true;
        for column in need_int
            .iter()
            .chain(need_flex.iter())
            .chain(need_bytes.iter())
            .chain(need_string.iter())
            .chain(need_nulls.iter())
        {
            let want_int = need_int.contains(column);
            let want_flex = need_flex.contains(column);
            let want_bytes = need_bytes.contains(column);
            let want_string = need_string.contains(column);
            let wants_values = want_int || want_flex || want_bytes || want_string;
            let type_ok = match reader.column(*column) {
                // Integer serves int/flex wants; String serves flex/string;
                // Bytes serves flex/bytes. Anything else declines.
                Some(meta) => match meta.column_type {
                    ColumnType::Integer => !want_bytes && !want_string,
                    ColumnType::String => !want_int && !want_bytes,
                    ColumnType::Bytes => !want_int && !want_string,
                    _ => !wants_values,
                },
                None => false,
            };
            if !type_ok {
                types_ok = false;
                break;
            }
        }
        if !types_ok {
            return Ok(None);
        }
        // The statement filter doubles as the pruning predicate, exactly like
        // the scalar scan: zone maps eliminate provably-empty ranges first.
        let plan = match filter.as_ref() {
            Some(predicate) => crate::pruning::plan_scan(
                reader
                    .pruning
                    .as_ref()
                    .unwrap_or(&crate::pruning::SegmentPruning::empty(reader.row_count)),
                reader.row_count,
                predicate,
            ),
            None => ScanPlan {
                candidates: vec![CandidateRange::new(0, reader.row_count)],
                total_rows: reader.row_count,
                scanned_rows: reader.row_count,
            },
        };
        result.rows_examined += plan.scanned_rows;
        result.rows_skipped += plan.pruned_rows();
        if plan.candidates.is_empty() {
            result.segments_skipped += 1;
            continue;
        }
        // Exactness guard: integer comparisons and MIN/MAX agree with the
        // scalar f64 path only inside ±2⁵³. Bounds come from persisted
        // per-column statistics; anything missing or out of range declines
        // the whole query (the scalar path then answers it exactly).
        if needs_f64_guard(request, filter.as_ref(), &agg_filters)
            && !segment_in_f64_range(&reader, request, filter.as_ref(), &agg_filters)
        {
            return Ok(None);
        }
        let processed = process_segment(
            &reader,
            &bytes,
            &plan.candidates,
            request,
            filter.as_ref(),
            &agg_filters,
            batch_rows,
            &need_int,
            &need_flex,
            &need_bytes,
            &need_string,
            &need_nulls,
            &mut global,
            &mut grouped,
            &mut result,
        )?;
        if !processed {
            return Ok(None);
        }
    }
    if request.group_by.is_empty() {
        result.groups.push(VectorGroup {
            keys: Vec::new(),
            accs: std::mem::take(&mut global),
        });
    } else {
        result.groups = grouped
            .into_iter()
            .map(|(keys, accs)| VectorGroup { keys, accs })
            .collect();
    }
    Ok(Some(result))
}

/// True when the request needs the f64-exactness guard: any integer
/// comparison leaf in a predicate, or any `MIN`/`MAX` aggregate.
fn needs_f64_guard(
    request: &VectorRequest,
    filter: Option<&PrunePredicate>,
    agg_filters: &[Option<PrunePredicate>],
) -> bool {
    if request
        .aggregates
        .iter()
        .any(|a| matches!(a.kind, VectorAggKind::Min | VectorAggKind::Max))
    {
        return true;
    }
    if filter.is_some_and(has_int_comparison) {
        return true;
    }
    agg_filters
        .iter()
        .any(|f| f.as_ref().is_some_and(has_int_comparison))
}

/// True when the (normalized) predicate contains an integer comparison leaf.
fn has_int_comparison(predicate: &PrunePredicate) -> bool {
    match predicate {
        PrunePredicate::Compare {
            operator, literal, ..
        } => {
            !matches!(operator, PruneOperator::IsNull | PruneOperator::IsNotNull)
                && matches!(literal, Some(Field::Integer(_)))
        }
        PrunePredicate::And(left, right) | PrunePredicate::Or(left, right) => {
            has_int_comparison(left) || has_int_comparison(right)
        }
        // Normalized predicates never contain `Not`.
        PrunePredicate::Not(_) => false,
    }
}

/// Checks every guarded column against the segment's persisted bounds.
///
/// Integer-comparison leaves and integer `MIN`/`MAX` agree with the scalar
/// f64 path only inside ±2⁵³, so their columns need integer statistics inside
/// that range. Raw string/bytes paths compare by byte order on both paths
/// and need no guard — their statistics pass unconditionally. Anything
/// missing or otherwise typed fails closed (the scalar path answers).
fn segment_in_f64_range(
    reader: &SegmentReader,
    request: &VectorRequest,
    filter: Option<&PrunePredicate>,
    agg_filters: &[Option<PrunePredicate>],
) -> bool {
    let mut int_columns: BTreeSet<ColumnId> = BTreeSet::new();
    if let Some(predicate) = filter {
        collect_int_columns(predicate, &mut int_columns);
    }
    for filter in agg_filters.iter().flatten() {
        collect_int_columns(filter, &mut int_columns);
    }
    for column in int_columns {
        let Some(stats) = reader.column_statistics(column) else {
            return false;
        };
        if !matches!(stats.min.as_ref(), Some(Field::Integer(min)) if min.abs() <= F64_EXACT_INT_RANGE)
            || !matches!(stats.max.as_ref(), Some(Field::Integer(max)) if max.abs() <= F64_EXACT_INT_RANGE)
        {
            return false;
        }
    }
    // MIN/MAX argument columns need bounded values only when compared as
    // integers; raw string/bytes extrema compare by byte order exactly.
    for aggregate in &request.aggregates {
        if !matches!(aggregate.kind, VectorAggKind::Min | VectorAggKind::Max) {
            continue;
        }
        let Some(column) = aggregate.column else {
            continue;
        };
        let Some(stats) = reader.column_statistics(column) else {
            return false;
        };
        match (&stats.min, &stats.max) {
            (Some(Field::Integer(min)), Some(Field::Integer(max))) => {
                if min.abs() > F64_EXACT_INT_RANGE || max.abs() > F64_EXACT_INT_RANGE {
                    return false;
                }
            }
            (Some(Field::String(_)), Some(Field::String(_)))
            | (Some(Field::Bytes(_)), Some(Field::Bytes(_))) => {}
            _ => return false,
        }
    }
    true
}

/// Collects columns compared as integers by a normalized predicate.
fn collect_int_columns(predicate: &PrunePredicate, out: &mut BTreeSet<ColumnId>) {
    match predicate {
        PrunePredicate::Compare {
            column_id,
            operator,
            literal,
        } => {
            if !matches!(operator, PruneOperator::IsNull | PruneOperator::IsNotNull)
                && matches!(literal, Some(Field::Integer(_)))
            {
                out.insert(*column_id);
            }
        }
        PrunePredicate::And(left, right) | PrunePredicate::Or(left, right) => {
            collect_int_columns(left, out);
            collect_int_columns(right, out);
        }
        PrunePredicate::Not(_) => {}
    }
}

/// Normalizes a lowered predicate into the exactly-evaluable subset, pushing
/// `NOT` through duals/De Morgan so three-valued logic is preserved.
///
/// `allow_text` gates `String` comparison leaves (see
/// [`VectorRequest::text_ordering_exact`]); `Bytes` leaves are always exact
/// and integer leaves are range-checked. Returns `None` when any leaf cannot
/// be evaluated exactly.
fn normalize_predicate(predicate: &PrunePredicate, allow_text: bool) -> Option<PrunePredicate> {
    normalize_inner(predicate, false, allow_text)
}

fn normalize_inner(
    predicate: &PrunePredicate,
    negated: bool,
    allow_text: bool,
) -> Option<PrunePredicate> {
    match predicate {
        PrunePredicate::Compare {
            column_id,
            operator,
            literal,
        } => {
            let operator = if negated {
                operator.negate()
            } else {
                *operator
            };
            accept_leaf(operator, literal.as_ref(), allow_text)?;
            Some(PrunePredicate::Compare {
                column_id: *column_id,
                operator,
                literal: literal.clone(),
            })
        }
        PrunePredicate::And(left, right) => {
            // NOT(A AND B) = (NOT A) OR (NOT B); positive form stays AND.
            if negated {
                Some(
                    normalize_inner(left, true, allow_text)?
                        .or(normalize_inner(right, true, allow_text)?),
                )
            } else {
                Some(
                    normalize_inner(left, false, allow_text)?
                        .and(normalize_inner(right, false, allow_text)?),
                )
            }
        }
        PrunePredicate::Or(left, right) => {
            if negated {
                Some(
                    normalize_inner(left, true, allow_text)?
                        .and(normalize_inner(right, true, allow_text)?),
                )
            } else {
                Some(
                    normalize_inner(left, false, allow_text)?
                        .or(normalize_inner(right, false, allow_text)?),
                )
            }
        }
        PrunePredicate::Not(inner) => normalize_inner(inner, !negated, allow_text),
    }
}

/// Accepts one comparison leaf as exactly evaluable over typed batches.
///
/// * `IS NULL` / `IS NOT NULL` read the authoritative null bitmap: exact for
///   every physical type.
/// * Integer literals compare against physically-`Integer` columns (verified
///   at execution time), inside ±2⁵³ (verified here; the f64 guard covers the
///   segment side).
/// * `Bytes` literals compare by byte order against physically-`Bytes`
///   columns (verified at execution time): physical `Bytes` always holds raw
///   `BYTEA`, compared by byte order on the scalar path too.
/// * `String` literals compare by byte order only when `allow_text` holds
///   (see [`VectorRequest::text_ordering_exact`]).
/// * Cross-type leaves (e.g. an integer column against a text literal, which
///   the scalar executor resolves through casts) decline.
fn accept_leaf(operator: PruneOperator, literal: Option<&Field>, allow_text: bool) -> Option<()> {
    match operator {
        PruneOperator::IsNull | PruneOperator::IsNotNull => {
            if literal.is_none() {
                Some(())
            } else {
                None
            }
        }
        PruneOperator::Equal
        | PruneOperator::NotEqual
        | PruneOperator::Less
        | PruneOperator::LessOrEqual
        | PruneOperator::Greater
        | PruneOperator::GreaterOrEqual => match literal {
            Some(Field::Integer(value)) if value.abs() <= F64_EXACT_INT_RANGE => Some(()),
            Some(Field::Bytes(_)) => Some(()),
            Some(Field::String(_)) if allow_text => Some(()),
            _ => None,
        },
    }
}

/// Borrowed typed views over one candidate span: dense integer arrays, raw
/// byte views for string/bytes columns, plus null bitmaps.
struct RangeViews<'a> {
    ints: HashMap<ColumnId, (&'a [i64], &'a [u8])>,
    raw: HashMap<ColumnId, RawView<'a>>,
    nulls: HashMap<ColumnId, &'a [u8]>,
}

/// Borrowed view of one string/bytes column over a span.
#[derive(Clone, Copy)]
struct RawView<'a> {
    physical: ColumnType,
    offsets: &'a [u32],
    lengths: &'a [u32],
    values: &'a [u8],
    nulls: &'a [u8],
}

impl<'a> RawView<'a> {
    /// Value bytes of `row`, or `None` for `NULL` / out-of-range rows.
    fn bytes_at(&self, row: usize) -> Option<&'a [u8]> {
        if self
            .nulls
            .get(row / 8)
            .is_some_and(|b| b & (1 << (row % 8)) != 0)
        {
            return None;
        }
        let offset = *self.offsets.get(row)? as usize;
        let length = *self.lengths.get(row)? as usize;
        let end = offset.checked_add(length)?;
        self.values.get(offset..end)
    }
}

/// One materialized column over a span, in the shape its consumers need.
enum OwnedColumn {
    /// Dense integer values plus null bitmap.
    Int { values: Vec<i64>, nulls: Vec<u8> },
    /// Raw offset/length/value buffers plus null bitmap and physical type.
    Raw {
        offsets: Vec<u32>,
        lengths: Vec<u32>,
        values: Vec<u8>,
        nulls: Vec<u8>,
        physical: ColumnType,
    },
    /// Null bitmap only (`COUNT(col)` over any type).
    Nulls { nulls: Vec<u8> },
}

/// Evaluates a normalized predicate over `count` rows, returning the
/// selection mask (1 = keep). Returns `None` when a leaf references a column
/// with no usable view — the caller declines the whole request.
fn eval_node(
    predicate: &PrunePredicate,
    views: &RangeViews<'_>,
    base: usize,
    count: usize,
) -> Option<Vec<u8>> {
    match predicate {
        PrunePredicate::Compare {
            column_id,
            operator,
            literal,
        } => eval_leaf_block(*column_id, *operator, literal.as_ref(), views, base, count),
        PrunePredicate::And(left, right) => {
            let mut out = eval_node(left, views, base, count)?;
            let right = eval_node(right, views, base, count)?;
            for (slot, keep) in out.iter_mut().zip(right.iter()) {
                *slot &= *keep;
            }
            Some(out)
        }
        PrunePredicate::Or(left, right) => {
            let mut out = eval_node(left, views, base, count)?;
            let right = eval_node(right, views, base, count)?;
            for (slot, keep) in out.iter_mut().zip(right.iter()) {
                *slot |= *keep;
            }
            Some(out)
        }
        // Normalized predicates never contain `Not`.
        PrunePredicate::Not(_) => None,
    }
}

/// Evaluates one normalized leaf over a block. Integer leaves require a
/// dense integer view (missing view = decline); null tests read bitmaps.
fn eval_leaf_block(
    column_id: ColumnId,
    operator: PruneOperator,
    literal: Option<&Field>,
    views: &RangeViews<'_>,
    base: usize,
    count: usize,
) -> Option<Vec<u8>> {
    match operator {
        PruneOperator::IsNull => {
            let bitmap = views.nulls.get(&column_id)?;
            Some(
                (0..count)
                    .map(|index| {
                        let row = base + index;
                        u8::from(
                            bitmap
                                .get(row / 8)
                                .is_some_and(|b| b & (1 << (row % 8)) != 0),
                        )
                    })
                    .collect(),
            )
        }
        PruneOperator::IsNotNull => {
            let bitmap = views.nulls.get(&column_id)?;
            Some(
                (0..count)
                    .map(|index| {
                        let row = base + index;
                        u8::from(
                            !bitmap
                                .get(row / 8)
                                .is_some_and(|b| b & (1 << (row % 8)) != 0),
                        )
                    })
                    .collect(),
            )
        }
        _ => match literal {
            Some(Field::Integer(bound)) => {
                let (values, bitmap) = views.ints.get(&column_id)?;
                let mut out = Vec::with_capacity(count);
                for index in 0..count {
                    let row = base + index;
                    if bitmap
                        .get(row / 8)
                        .is_some_and(|b| b & (1 << (row % 8)) != 0)
                    {
                        out.push(0);
                        continue;
                    }
                    let cell = *values.get(row)?;
                    out.push(u8::from(match operator {
                        PruneOperator::Equal => cell == *bound,
                        PruneOperator::NotEqual => cell != *bound,
                        PruneOperator::Less => cell < *bound,
                        PruneOperator::LessOrEqual => cell <= *bound,
                        PruneOperator::Greater => cell > *bound,
                        PruneOperator::GreaterOrEqual => cell >= *bound,
                        PruneOperator::IsNull | PruneOperator::IsNotNull => false,
                    }));
                }
                Some(out)
            }
            // Raw byte-order leaves: the physical type must match the literal
            // representation (Bytes literal over Bytes columns, String over
            // String — enforced by the metadata pre-check and re-verified
            // here). NULL rows never match, exactly like the scalar path.
            Some(Field::Bytes(bound)) => {
                let view = views.raw.get(&column_id)?;
                if view.physical != ColumnType::Bytes {
                    return None;
                }
                Some(compare_raw_block(view, operator, bound, base, count))
            }
            Some(Field::String(bound)) => {
                let view = views.raw.get(&column_id)?;
                if view.physical != ColumnType::String {
                    return None;
                }
                Some(compare_raw_block(
                    view,
                    operator,
                    bound.as_bytes(),
                    base,
                    count,
                ))
            }
            _ => None,
        },
    }
}

/// Compares one raw byte-order block against `bound` (byte-lexicographic,
/// matching the scalar `SortKey::Str` / `SortKey::Bytes` ordering for
/// same-representation values). `NULL` rows yield 0.
fn compare_raw_block(
    view: &RawView<'_>,
    operator: PruneOperator,
    bound: &[u8],
    base: usize,
    count: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let row = base + index;
        let Some(cell) = view.bytes_at(row) else {
            out.push(0);
            continue;
        };
        out.push(u8::from(match operator {
            PruneOperator::Equal => cell == bound,
            PruneOperator::NotEqual => cell != bound,
            PruneOperator::Less => cell < bound,
            PruneOperator::LessOrEqual => cell <= bound,
            PruneOperator::Greater => cell > bound,
            PruneOperator::GreaterOrEqual => cell >= bound,
            PruneOperator::IsNull | PruneOperator::IsNotNull => false,
        }));
    }
    out
}

/// Collects needed columns into typed sets: integer values (sums,
/// averages, integer predicate leaves, group keys), flexibly-typed values
/// (`MIN`/`MAX`/`COUNT DISTINCT` args, served as integers or raw bytes),
/// raw `Bytes` / `String` predicate leaves, and bitmap-only columns.
#[allow(clippy::type_complexity)]
fn collect_needed(
    request: &VectorRequest,
    filter: Option<&PrunePredicate>,
    agg_filters: &[Option<PrunePredicate>],
) -> (
    BTreeSet<ColumnId>,
    BTreeSet<ColumnId>,
    BTreeSet<ColumnId>,
    BTreeSet<ColumnId>,
    BTreeSet<ColumnId>,
) {
    let mut need_int: BTreeSet<ColumnId> = BTreeSet::new();
    let mut need_flex: BTreeSet<ColumnId> = BTreeSet::new();
    let mut need_bytes: BTreeSet<ColumnId> = BTreeSet::new();
    let mut need_string: BTreeSet<ColumnId> = BTreeSet::new();
    let mut need_nulls: BTreeSet<ColumnId> = BTreeSet::new();
    for aggregate in &request.aggregates {
        if let Some(column) = aggregate.column {
            match aggregate.kind {
                VectorAggKind::CountStar => {}
                VectorAggKind::Count => {
                    need_nulls.insert(column);
                }
                VectorAggKind::Sum | VectorAggKind::Avg => {
                    need_int.insert(column);
                }
                VectorAggKind::Min | VectorAggKind::Max | VectorAggKind::CountDistinct => {
                    need_flex.insert(column);
                }
            }
        }
    }
    if let Some(predicate) = filter {
        collect_eval_columns(
            predicate,
            &mut need_int,
            &mut need_bytes,
            &mut need_string,
            &mut need_nulls,
        );
    }
    for predicate in agg_filters.iter().flatten() {
        collect_eval_columns(
            predicate,
            &mut need_int,
            &mut need_bytes,
            &mut need_string,
            &mut need_nulls,
        );
    }
    for key in &request.group_by {
        need_int.insert(*key);
    }
    (need_int, need_flex, need_bytes, need_string, need_nulls)
}

/// Processes all pruned candidate ranges of one segment.
///
/// Each needed column is materialized ONCE over the candidates' span — never
/// once per range. Per-range reads re-decoded shared chunks every time
/// (measured ~9ms per 1024-row range versus ~100µs of actual compute), which
/// dominated the first vector implementation. The span bound is one segment's
/// requested columns; pruning still skips empty segments and ranges entirely.
#[allow(clippy::too_many_arguments)]
fn process_segment(
    reader: &SegmentReader,
    bytes: &[u8],
    candidates: &[CandidateRange],
    request: &VectorRequest,
    filter: Option<&PrunePredicate>,
    agg_filters: &[Option<PrunePredicate>],
    batch_rows: usize,
    need_int: &BTreeSet<ColumnId>,
    need_flex: &BTreeSet<ColumnId>,
    need_bytes: &BTreeSet<ColumnId>,
    need_string: &BTreeSet<ColumnId>,
    need_nulls: &BTreeSet<ColumnId>,
    global: &mut [VectorAcc],
    grouped: &mut HashMap<Vec<Option<i64>>, Vec<VectorAcc>>,
    result: &mut VectorResult,
) -> Result<bool> {
    if candidates.is_empty() {
        return Ok(true);
    }

    let span_start = candidates
        .iter()
        .map(|candidate| candidate.start_row)
        .min()
        .unwrap_or(0);
    let span_end = candidates
        .iter()
        .map(|candidate| candidate.end_row)
        .max()
        .unwrap_or(0);
    // Materialize each needed column once over the span. Integer views hold
    // dense i64 arrays (declining on ragged data); raw views borrow the
    // offset/length/value buffers for byte-ordered string/bytes work.
    let mut owned: HashMap<ColumnId, OwnedColumn> = HashMap::new();
    for column in need_int
        .iter()
        .chain(need_flex.iter())
        .chain(need_bytes.iter())
        .chain(need_string.iter())
        .chain(need_nulls.iter())
    {
        let want_int = need_int.contains(column);
        let want_flex = need_flex.contains(column);
        let want_bytes = need_bytes.contains(column);
        let want_string = need_string.contains(column);
        let Some(meta) = reader.column(*column) else {
            return Ok(false);
        };
        let physical = meta.column_type;
        // Cross-type leaves decline: byte/integer evaluation is only exact
        // when the literal and column representations agree.
        let wants_values = want_int || want_flex || want_bytes || want_string;
        let type_ok = match physical {
            ColumnType::Integer => !want_bytes && !want_string,
            ColumnType::String => !want_int && !want_bytes,
            ColumnType::Bytes => !want_int && !want_string,
            _ => !wants_values,
        };
        if !type_ok {
            return Ok(false);
        }
        let materialized = reader.read_column(bytes, meta, span_start, span_end)?;
        result.columns_read += 1;
        if want_int || (want_flex && physical == ColumnType::Integer) {
            let rows = materialized.row_count as usize;
            let dense = (0..rows).all(|row| {
                materialized.is_null(row)
                    || materialized
                        .lengths
                        .get(row)
                        .is_some_and(|length| *length == 8)
            });
            if !dense {
                return Ok(false);
            }
            // Explicitly-checked copy so alignment is never assumed.
            let mut values = Vec::with_capacity(rows);
            for row in 0..rows {
                if materialized.is_null(row) {
                    values.push(0);
                    continue;
                }
                let offset = materialized.offsets[row] as usize;
                let slice = materialized.values.get(offset..offset + 8).ok_or_else(|| {
                    plomid_core::PlomidError::new(
                        plomid_core::ErrorKind::Corruption,
                        "integer value lies outside the column buffer",
                    )
                })?;
                values.push(i64::from_le_bytes(slice.try_into().map_err(|_| {
                    plomid_core::PlomidError::new(
                        plomid_core::ErrorKind::Corruption,
                        "integer value is truncated",
                    )
                })?));
            }
            owned.insert(
                *column,
                OwnedColumn::Int {
                    values,
                    nulls: materialized.null_bitmap,
                },
            );
        } else if want_flex || want_bytes || want_string {
            owned.insert(
                *column,
                OwnedColumn::Raw {
                    offsets: materialized.offsets,
                    lengths: materialized.lengths,
                    values: materialized.values,
                    nulls: materialized.null_bitmap,
                    physical,
                },
            );
        } else {
            owned.insert(
                *column,
                OwnedColumn::Nulls {
                    nulls: materialized.null_bitmap,
                },
            );
        }
    }

    let mut views = RangeViews {
        ints: HashMap::new(),
        raw: HashMap::new(),
        nulls: HashMap::new(),
    };
    for (column, owned) in &owned {
        match owned {
            OwnedColumn::Int { values, nulls } => {
                views.nulls.insert(*column, nulls.as_slice());
                views
                    .ints
                    .insert(*column, (values.as_slice(), nulls.as_slice()));
            }
            OwnedColumn::Raw {
                offsets,
                lengths,
                values,
                nulls,
                physical,
            } => {
                views.nulls.insert(*column, nulls.as_slice());
                views.raw.insert(
                    *column,
                    RawView {
                        physical: *physical,
                        offsets: offsets.as_slice(),
                        lengths: lengths.as_slice(),
                        values: values.as_slice(),
                        nulls: nulls.as_slice(),
                    },
                );
            }
            OwnedColumn::Nulls { nulls } => {
                views.nulls.insert(*column, nulls.as_slice());
            }
        }
    }
    // Group keys must be dense integers (enforced via need_int).
    if !request.group_by.is_empty() {
        for key in &request.group_by {
            if !views.ints.contains_key(key) {
                return Ok(false);
            }
        }
    }
    // Every comparison leaf must have a matching view (cross-type leaves
    // decline here even though normalization accepted their shape).
    if let Some(predicate) = filter {
        if !leaves_have_views(predicate, &views) {
            return Ok(false);
        }
    }
    for predicate in agg_filters.iter().flatten() {
        if !leaves_have_views(predicate, &views) {
            return Ok(false);
        }
    }

    for candidate in candidates {
        let base = candidate.start_row.saturating_sub(span_start) as usize;
        let length = candidate.row_count() as usize;
        let mut start = 0_usize;
        while start < length {
            let count = (length - start).min(batch_rows);
            let block: Vec<u8> = match filter {
                Some(predicate) => match eval_node(predicate, &views, base + start, count) {
                    Some(mask) => mask,
                    None => return Ok(false),
                },
                None => vec![1; count],
            };
            if request.group_by.is_empty() {
                if !feed_global(
                    request,
                    agg_filters,
                    &views,
                    base + start,
                    count,
                    &block,
                    global,
                )? {
                    return Ok(false);
                }
            } else if !feed_grouped(
                request,
                agg_filters,
                &views,
                base + start,
                count,
                &block,
                grouped,
            )? {
                return Ok(false);
            }
            start += count;
        }
    }
    Ok(true)
}

/// True when every comparison leaf of a normalized predicate has a matching
/// view: integer leaves need dense integer views, `Bytes` / `String` leaves
/// need raw views of the same physical type (cross-type leaves decline even
/// though normalization accepted their shape).
fn leaves_have_views(predicate: &PrunePredicate, views: &RangeViews<'_>) -> bool {
    match predicate {
        PrunePredicate::Compare {
            column_id,
            operator,
            literal,
        } => {
            if matches!(operator, PruneOperator::IsNull | PruneOperator::IsNotNull) {
                return views.nulls.contains_key(column_id);
            }
            match literal {
                Some(Field::Integer(_)) => views.ints.contains_key(column_id),
                Some(Field::Bytes(_)) => views
                    .raw
                    .get(column_id)
                    .is_some_and(|view| view.physical == ColumnType::Bytes),
                Some(Field::String(_)) => views
                    .raw
                    .get(column_id)
                    .is_some_and(|view| view.physical == ColumnType::String),
                _ => false,
            }
        }
        PrunePredicate::And(left, right) | PrunePredicate::Or(left, right) => {
            leaves_have_views(left, views) && leaves_have_views(right, views)
        }
        PrunePredicate::Not(_) => false,
    }
}

/// Collects columns a normalized predicate reads, split by the physical
/// shape the evaluator needs: integer leaves need dense values, `Bytes` /
/// `String` leaves need raw values, `IS NULL` leaves need only bitmaps.
fn collect_eval_columns(
    predicate: &PrunePredicate,
    ints: &mut BTreeSet<ColumnId>,
    bytes: &mut BTreeSet<ColumnId>,
    strings: &mut BTreeSet<ColumnId>,
    nulls: &mut BTreeSet<ColumnId>,
) {
    match predicate {
        PrunePredicate::Compare {
            column_id,
            operator,
            literal,
        } => match operator {
            PruneOperator::IsNull | PruneOperator::IsNotNull => {
                nulls.insert(*column_id);
            }
            _ => match literal {
                Some(Field::Integer(_)) => {
                    ints.insert(*column_id);
                }
                Some(Field::Bytes(_)) => {
                    bytes.insert(*column_id);
                }
                Some(Field::String(_)) => {
                    strings.insert(*column_id);
                }
                _ => {
                    ints.insert(*column_id);
                }
            },
        },
        PrunePredicate::And(left, right) | PrunePredicate::Or(left, right) => {
            collect_eval_columns(left, ints, bytes, strings, nulls);
            collect_eval_columns(right, ints, bytes, strings, nulls);
        }
        PrunePredicate::Not(_) => {}
    }
}

/// Feeds one block into the single global accumulator row.
fn feed_global(
    request: &VectorRequest,
    agg_filters: &[Option<PrunePredicate>],
    views: &RangeViews<'_>,
    start: usize,
    count: usize,
    block: &[u8],
    global: &mut [VectorAcc],
) -> Result<bool> {
    for (index, aggregate) in request.aggregates.iter().enumerate() {
        let Some(acc) = global.get_mut(index) else {
            continue;
        };
        // Per-aggregate FILTER intersects the statement mask.
        let active: Vec<u8>;
        let mask: &[u8] = match agg_filters.get(index).and_then(|f| f.as_ref()) {
            Some(filter) => {
                let Some(filter_mask) = eval_node(filter, views, start, count) else {
                    return Ok(false);
                };
                active = block
                    .iter()
                    .zip(filter_mask.iter())
                    .map(|(keep, pass)| keep & pass)
                    .collect();
                &active
            }
            None => block,
        };
        accumulate(aggregate, views, start, count, mask, acc);
    }
    Ok(true)
}

/// Feeds one block into per-group accumulators.
fn feed_grouped(
    request: &VectorRequest,
    agg_filters: &[Option<PrunePredicate>],
    views: &RangeViews<'_>,
    start: usize,
    count: usize,
    block: &[u8],
    grouped: &mut HashMap<Vec<Option<i64>>, Vec<VectorAcc>>,
) -> Result<bool> {
    // Per-aggregate active masks (FILTER intersected with the statement mask).
    let mut masks: Vec<Option<Vec<u8>>> = Vec::with_capacity(request.aggregates.len());
    for (index, aggregate) in request.aggregates.iter().enumerate() {
        let _ = aggregate;
        match agg_filters.get(index).and_then(|f| f.as_ref()) {
            Some(filter) => {
                let Some(filter_mask) = eval_node(filter, views, start, count) else {
                    return Ok(false);
                };
                masks.push(Some(
                    block
                        .iter()
                        .zip(filter_mask.iter())
                        .map(|(keep, pass)| keep & pass)
                        .collect(),
                ));
            }
            None => masks.push(None),
        }
    }
    let aggregate_count = request.aggregates.len();
    // Single-column fast path avoids the per-row key allocation.
    let single_key = request.group_by.len() == 1;
    for row in 0..count {
        if block.get(row).is_some_and(|keep| *keep == 0) {
            continue;
        }
        let key: Vec<Option<i64>> = if single_key {
            vec![int_key_at(views, request.group_by[0], start + row)]
        } else {
            request
                .group_by
                .iter()
                .map(|column| int_key_at(views, *column, start + row))
                .collect()
        };
        let accs = grouped
            .entry(key)
            .or_insert_with(|| (0..aggregate_count).map(|_| VectorAcc::default()).collect());
        for (index, aggregate) in request.aggregates.iter().enumerate() {
            if masks[index]
                .as_ref()
                .is_some_and(|mask| mask.get(row).is_some_and(|keep| *keep == 0))
            {
                continue;
            }
            let Some(acc) = accs.get_mut(index) else {
                continue;
            };
            accumulate_single(aggregate, views, start + row, acc);
        }
    }
    Ok(true)
}

/// Reads one integer group key (`None` for SQL `NULL`).
fn int_key_at(views: &RangeViews<'_>, column: ColumnId, row: usize) -> Option<i64> {
    let (values, bitmap) = views.ints.get(&column)?;
    if bitmap
        .get(row / 8)
        .is_some_and(|b| b & (1 << (row % 8)) != 0)
    {
        return None;
    }
    values.get(row).copied()
}

/// Accumulates one block for a global aggregate from the active mask.
fn accumulate(
    aggregate: &VectorAggregate,
    views: &RangeViews<'_>,
    start: usize,
    count: usize,
    active: &[u8],
    acc: &mut VectorAcc,
) {
    match aggregate.kind {
        VectorAggKind::CountStar => {
            for row in 0..count {
                if active.get(row).is_some_and(|keep| *keep != 0) {
                    acc.count_star += 1;
                }
            }
        }
        VectorAggKind::Count => {
            let Some(column) = aggregate.column else {
                return;
            };
            let Some(bitmap) = views.nulls.get(&column) else {
                return;
            };
            for row in 0..count {
                if active.get(row).is_some_and(|keep| *keep != 0) {
                    let absolute = start + row;
                    if !bitmap
                        .get(absolute / 8)
                        .is_some_and(|b| b & (1 << (absolute % 8)) != 0)
                    {
                        acc.count += 1;
                    }
                }
            }
        }
        VectorAggKind::Sum => {
            sum_block(aggregate, views, start, count, active, acc, false);
        }
        VectorAggKind::Avg => {
            sum_block(aggregate, views, start, count, active, acc, true);
        }
        VectorAggKind::Min | VectorAggKind::Max => {
            minmax_block(aggregate, views, start, count, active, acc);
        }
        VectorAggKind::CountDistinct => {
            distinct_block(aggregate, views, start, count, active, acc);
        }
    }
}

/// Accumulates one row for a grouped aggregate (row already mask-qualified).
fn accumulate_single(
    aggregate: &VectorAggregate,
    views: &RangeViews<'_>,
    row: usize,
    acc: &mut VectorAcc,
) {
    match aggregate.kind {
        VectorAggKind::CountStar => {
            acc.count_star += 1;
        }
        VectorAggKind::Count => {
            let Some(column) = aggregate.column else {
                return;
            };
            let Some(bitmap) = views.nulls.get(&column) else {
                return;
            };
            if !bitmap
                .get(row / 8)
                .is_some_and(|b| b & (1 << (row % 8)) != 0)
            {
                acc.count += 1;
            }
        }
        VectorAggKind::Sum => {
            sum_single(aggregate, views, row, acc, false);
        }
        VectorAggKind::Avg => {
            sum_single(aggregate, views, row, acc, true);
        }
        VectorAggKind::Min | VectorAggKind::Max => {
            minmax_single(aggregate, views, row, acc);
        }
        VectorAggKind::CountDistinct => {
            distinct_single(aggregate, views, row, acc);
        }
    }
}

/// Shared `SUM` / `AVG` block accumulation (`avg` selects the AVG counters).
fn sum_block(
    aggregate: &VectorAggregate,
    views: &RangeViews<'_>,
    start: usize,
    count: usize,
    active: &[u8],
    acc: &mut VectorAcc,
    avg: bool,
) {
    let Some(column) = aggregate.column else {
        return;
    };
    let Some((values, bitmap)) = views.ints.get(&column) else {
        return;
    };
    for row in 0..count {
        if active.get(row).is_some_and(|keep| *keep == 0) {
            continue;
        }
        let absolute = start + row;
        if bitmap
            .get(absolute / 8)
            .is_some_and(|b| b & (1 << (absolute % 8)) != 0)
        {
            continue;
        }
        let Some(cell) = values.get(absolute) else {
            continue;
        };
        if avg {
            acc.avg_sum += *cell;
            acc.avg_count += 1;
        } else {
            // Identical overflow behavior to the scalar `SumN` accumulator
            // (`+=` panics in debug, wraps in release).
            acc.sum += *cell;
            acc.sum_has_any = true;
        }
    }
}

/// Shared `SUM` / `AVG` single-row accumulation.
fn sum_single(
    aggregate: &VectorAggregate,
    views: &RangeViews<'_>,
    row: usize,
    acc: &mut VectorAcc,
    avg: bool,
) {
    let Some(column) = aggregate.column else {
        return;
    };
    let Some((values, bitmap)) = views.ints.get(&column) else {
        return;
    };
    if bitmap
        .get(row / 8)
        .is_some_and(|b| b & (1 << (row % 8)) != 0)
    {
        return;
    }
    let Some(cell) = values.get(row) else {
        return;
    };
    if avg {
        acc.avg_sum += *cell;
        acc.avg_count += 1;
    } else {
        acc.sum += *cell;
        acc.sum_has_any = true;
    }
}

/// Shared `MIN` / `MAX` block accumulation.
fn minmax_block(
    aggregate: &VectorAggregate,
    views: &RangeViews<'_>,
    start: usize,
    count: usize,
    active: &[u8],
    acc: &mut VectorAcc,
) {
    let Some(column) = aggregate.column else {
        return;
    };
    let is_min = matches!(aggregate.kind, VectorAggKind::Min);
    if let Some((values, bitmap)) = views.ints.get(&column) {
        for row in 0..count {
            if active.get(row).is_some_and(|keep| *keep == 0) {
                continue;
            }
            let absolute = start + row;
            if bitmap
                .get(absolute / 8)
                .is_some_and(|b| b & (1 << (absolute % 8)) != 0)
            {
                continue;
            }
            let Some(cell) = values.get(absolute) else {
                continue;
            };
            if is_min {
                acc.min = Some(acc.min.map_or(*cell, |current| current.min(*cell)));
            } else {
                acc.max = Some(acc.max.map_or(*cell, |current| current.max(*cell)));
            }
        }
        return;
    }
    let Some(view) = views.raw.get(&column) else {
        return;
    };
    for row in 0..count {
        if active.get(row).is_some_and(|keep| *keep == 0) {
            continue;
        }
        let Some(cell) = view.bytes_at(start + row) else {
            continue;
        };
        if is_min {
            let replace = acc
                .min_raw
                .as_ref()
                .is_none_or(|current| cell < current.as_slice());
            if replace {
                acc.min_raw = Some(cell.to_vec());
            }
        } else {
            let replace = acc
                .max_raw
                .as_ref()
                .is_none_or(|current| cell > current.as_slice());
            if replace {
                acc.max_raw = Some(cell.to_vec());
            }
        }
    }
}

/// Shared `MIN` / `MAX` single-row accumulation.
fn minmax_single(
    aggregate: &VectorAggregate,
    views: &RangeViews<'_>,
    row: usize,
    acc: &mut VectorAcc,
) {
    let Some(column) = aggregate.column else {
        return;
    };
    let is_min = matches!(aggregate.kind, VectorAggKind::Min);
    if let Some((values, bitmap)) = views.ints.get(&column) {
        if bitmap
            .get(row / 8)
            .is_some_and(|b| b & (1 << (row % 8)) != 0)
        {
            return;
        }
        let Some(cell) = values.get(row) else {
            return;
        };
        if is_min {
            acc.min = Some(acc.min.map_or(*cell, |current| current.min(*cell)));
        } else {
            acc.max = Some(acc.max.map_or(*cell, |current| current.max(*cell)));
        }
        return;
    }
    let Some(view) = views.raw.get(&column) else {
        return;
    };
    let Some(cell) = view.bytes_at(row) else {
        return;
    };
    if is_min {
        let replace = acc
            .min_raw
            .as_ref()
            .is_none_or(|current| cell < current.as_slice());
        if replace {
            acc.min_raw = Some(cell.to_vec());
        }
    } else {
        let replace = acc
            .max_raw
            .as_ref()
            .is_none_or(|current| cell > current.as_slice());
        if replace {
            acc.max_raw = Some(cell.to_vec());
        }
    }
}

/// Shared `COUNT(DISTINCT ...)` block accumulation.
fn distinct_block(
    aggregate: &VectorAggregate,
    views: &RangeViews<'_>,
    start: usize,
    count: usize,
    active: &[u8],
    acc: &mut VectorAcc,
) {
    let Some(column) = aggregate.column else {
        return;
    };
    if let Some((values, bitmap)) = views.ints.get(&column) {
        for row in 0..count {
            if active.get(row).is_some_and(|keep| *keep == 0) {
                continue;
            }
            let absolute = start + row;
            if bitmap
                .get(absolute / 8)
                .is_some_and(|b| b & (1 << (absolute % 8)) != 0)
            {
                continue;
            }
            if let Some(cell) = values.get(absolute) {
                acc.distinct.insert(*cell);
            }
        }
        return;
    }
    let Some(view) = views.raw.get(&column) else {
        return;
    };
    for row in 0..count {
        if active.get(row).is_some_and(|keep| *keep == 0) {
            continue;
        }
        let Some(cell) = view.bytes_at(start + row) else {
            continue;
        };
        // Insert-on-miss: borrowed lookup first, so duplicate-heavy data
        // allocates only for genuinely new values.
        if !acc.distinct_raw.contains(cell) {
            acc.distinct_raw.insert(cell.to_vec());
        }
    }
}

/// Shared `COUNT(DISTINCT ...)` single-row accumulation.
fn distinct_single(
    aggregate: &VectorAggregate,
    views: &RangeViews<'_>,
    row: usize,
    acc: &mut VectorAcc,
) {
    let Some(column) = aggregate.column else {
        return;
    };
    if let Some((values, bitmap)) = views.ints.get(&column) {
        if bitmap
            .get(row / 8)
            .is_some_and(|b| b & (1 << (row % 8)) != 0)
        {
            return;
        }
        if let Some(cell) = values.get(row) {
            acc.distinct.insert(*cell);
        }
        return;
    }
    let Some(view) = views.raw.get(&column) else {
        return;
    };
    let Some(cell) = view.bytes_at(row) else {
        return;
    };
    if !acc.distinct_raw.contains(cell) {
        acc.distinct_raw.insert(cell.to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materialization::{materialize_columns, MaterializedColumn};
    use crate::pruning::row_matches;
    use crate::pruning::Tri;
    use plomid_storage::Row;

    /// Deterministic LCG (no rand dependency in this crate).
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) & 0x7FFF_FFFF
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    /// Builds a two-integer-column table with NULLs, as materialized columns.
    fn two_int_columns(n: usize) -> Vec<MaterializedColumn> {
        let rows: Vec<Row> = (0..n)
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
                ])
            })
            .collect();
        materialize_columns(&rows, &[ColumnType::Integer, ColumnType::Integer])
            .expect("materialize")
    }

    #[test]
    fn batch_masks_match_row_matches_oracle() {
        // Differential test: every batch mask must agree with the pruning
        // layer's three-valued ground truth on every row.
        let a = ColumnId::new(0);
        let b = ColumnId::new(1);
        let columns = two_int_columns(2000);
        // NOTE: owned buffers are leaked below so borrowed views stay
        // valid for the test body (test-only; production borrows live data).
        let rows = columns[0].row_count as usize;
        let mut int_values: Vec<Vec<i64>> = Vec::new();
        let mut null_maps: Vec<Vec<u8>> = Vec::new();
        for column in &columns {
            let mut values = Vec::with_capacity(rows);
            for row in 0..rows {
                match column.get_value(row) {
                    Some(bytes) => values.push(i64::from_le_bytes(bytes[..8].try_into().unwrap())),
                    None => values.push(0),
                }
            }
            int_values.push(values);
            null_maps.push(column.null_bitmap.clone());
        }
        // Leak so borrowed views stay valid for the test body.
        let int_leaked: &'static mut Vec<Vec<i64>> = Box::leak(Box::new(int_values));
        let null_leaked: &'static mut Vec<Vec<u8>> = Box::leak(Box::new(null_maps));
        let mut views = RangeViews {
            ints: HashMap::new(),
            raw: HashMap::new(),
            nulls: HashMap::new(),
        };
        for (index, id) in [a, b].iter().enumerate() {
            views.ints.insert(
                *id,
                (int_leaked[index].as_slice(), null_leaked[index].as_slice()),
            );
            views.nulls.insert(*id, null_leaked[index].as_slice());
        }

        let leaves: Vec<PrunePredicate> = vec![
            PrunePredicate::compare(a, PruneOperator::Greater, Field::Integer(100)),
            PrunePredicate::compare(a, PruneOperator::LessOrEqual, Field::Integer(-100)),
            PrunePredicate::compare(b, PruneOperator::Equal, Field::Integer(0)),
            PrunePredicate::compare(b, PruneOperator::NotEqual, Field::Integer(42)),
            PrunePredicate::is_null(a),
            PrunePredicate::is_not_null(b),
        ];
        let mut rng = Lcg(0x1234_5678);
        for round in 0..60 {
            // Random tree over 1-3 leaves with random connectives/negations.
            let mut predicate = leaves[rng.below(leaves.len() as u64) as usize].clone();
            for _ in 0..rng.below(3) {
                let leaf = leaves[rng.below(leaves.len() as u64) as usize].clone();
                predicate = if rng.below(2) == 0 {
                    predicate.and(leaf)
                } else {
                    predicate.or(leaf)
                };
            }
            if rng.below(4) == 0 {
                predicate = predicate.not();
            }
            let Some(normalized) = normalize_predicate(&predicate, false) else {
                continue;
            };
            let Some(mask) = eval_node(&normalized, &views, 0, rows) else {
                panic!("accepted predicate must evaluate: {predicate:?}");
            };
            for row in 0..rows {
                let expected = row_matches(&predicate, &|id| {
                    let index = if id == a { 0 } else { 1 };
                    let col = &columns[index];
                    if col.is_null(row) {
                        Some(Field::Null)
                    } else {
                        col.get_value(row).map(|bytes| {
                            Field::Integer(i64::from_le_bytes(bytes[..8].try_into().unwrap()))
                        })
                    }
                });
                assert_eq!(
                    (mask[row] != 0),
                    matches!(expected, Tri::True),
                    "round {round} row {row} predicate {predicate:?} (normalized {normalized:?})"
                );
            }
        }
    }

    #[test]
    fn not_pushdown_preserves_three_valued_logic() {
        // WHERE NOT (x = 1): NULL rows evaluate NULL and must be dropped.
        let a = ColumnId::new(0);
        let predicate = PrunePredicate::compare(a, PruneOperator::Equal, Field::Integer(1)).not();
        let normalized = normalize_predicate(&predicate, false).expect("normalizes");
        // NOT(=) pushes to !=, never a bare Not.
        assert!(matches!(
            normalized,
            PrunePredicate::Compare {
                operator: PruneOperator::NotEqual,
                ..
            }
        ));
        let columns = two_int_columns(64);
        let rows = columns[0].row_count as usize;
        let mut values = Vec::with_capacity(rows);
        let mut nulls = columns[0].null_bitmap.clone();
        for row in 0..rows {
            values.push(match columns[0].get_value(row) {
                Some(bytes) => i64::from_le_bytes(bytes[..8].try_into().unwrap()),
                None => 0,
            });
        }
        let views = RangeViews {
            ints: HashMap::from([(a, (values.as_slice(), nulls.as_slice()))]),
            raw: HashMap::new(),
            nulls: HashMap::from([(a, nulls.as_slice())]),
        };
        let mask = eval_node(&normalized, &views, 0, rows).expect("evaluates");
        for row in 0..rows {
            let expected = row_matches(&predicate, &|_| {
                if columns[0].is_null(row) {
                    Some(Field::Null)
                } else {
                    columns[0]
                        .get_value(row)
                        .map(|b| Field::Integer(i64::from_le_bytes(b[..8].try_into().unwrap())))
                }
            });
            assert_eq!(mask[row] != 0, matches!(expected, Tri::True));
        }
        let _ = &mut nulls;
    }

    #[test]
    fn unsupported_shapes_decline() {
        let a = ColumnId::new(0);
        // Text literal against an integer comparison: never exact (the scalar
        // executor resolves cross-type leaves through casts), so normalization
        // declines and the scalar path answers.
        let cross = PrunePredicate::compare(a, PruneOperator::Equal, Field::String("5".to_owned()));
        assert!(normalize_predicate(&cross, false).is_none());
        // Huge literal: normalization itself declines (f64 guard).
        let huge = PrunePredicate::compare(
            a,
            PruneOperator::Greater,
            Field::Integer(F64_EXACT_INT_RANGE + 1),
        );
        assert!(normalize_predicate(&huge, false).is_none());
        // Boundary literal is accepted.
        let edge = PrunePredicate::compare(
            a,
            PruneOperator::Greater,
            Field::Integer(F64_EXACT_INT_RANGE),
        );
        assert!(normalize_predicate(&edge, false).is_some());
    }

    #[test]
    fn global_accumulators_match_hand_computed() {
        // Values: [3, NULL, -2, 7, NULL]; mask keeps rows 0,2,3.
        let values = vec![3i64, 0, -2, 7, 0];
        let nulls = vec![0b0001_0010u8];
        let views = RangeViews {
            ints: HashMap::from([(ColumnId::new(0), (values.as_slice(), nulls.as_slice()))]),
            raw: HashMap::new(),
            nulls: HashMap::from([(ColumnId::new(0), nulls.as_slice())]),
        };
        let mask = vec![1u8, 0, 1, 1, 0];
        let kinds = [
            VectorAggKind::CountStar,
            VectorAggKind::Count,
            VectorAggKind::Sum,
            VectorAggKind::Min,
            VectorAggKind::Max,
            VectorAggKind::Avg,
            VectorAggKind::CountDistinct,
        ];
        let mut accs: Vec<VectorAcc> = kinds.iter().map(|_| VectorAcc::default()).collect();
        for (index, kind) in kinds.iter().enumerate() {
            accumulate(
                &VectorAggregate {
                    kind: *kind,
                    column: Some(ColumnId::new(0)),
                    filter: None,
                },
                &views,
                0,
                5,
                &mask,
                &mut accs[index],
            );
        }
        assert_eq!(accs[0].count_star, 3);
        assert_eq!(accs[1].count, 3);
        assert_eq!((accs[2].sum, accs[2].sum_has_any), (8, true));
        assert_eq!(accs[3].min, Some(-2));
        assert_eq!(accs[4].max, Some(7));
        assert_eq!((accs[5].avg_sum, accs[5].avg_count), (8, 3));
        assert_eq!(accs[6].distinct.len(), 3);

        // Empty mask: COUNTs are 0, everything else NULL-state.
        let mut empty: Vec<VectorAcc> = kinds.iter().map(|_| VectorAcc::default()).collect();
        let nomask = vec![0u8; 5];
        for (index, kind) in kinds.iter().enumerate() {
            accumulate(
                &VectorAggregate {
                    kind: *kind,
                    column: Some(ColumnId::new(0)),
                    filter: None,
                },
                &views,
                0,
                5,
                &nomask,
                &mut empty[index],
            );
        }
        assert_eq!(empty[0].count_star, 0);
        assert_eq!(empty[1].count, 0);
        assert!(!empty[2].sum_has_any);
        assert_eq!(empty[3].min, None);
        assert_eq!(empty[4].max, None);
        assert_eq!(empty[5].avg_count, 0);
        assert!(empty[6].distinct.is_empty());
    }
}
