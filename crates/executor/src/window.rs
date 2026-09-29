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
//! Window function evaluation.
//!
//! One module for `OVER`-clause execution: partition matching, peer groups and
//! frame bounds, the ranking/value/lag-lead families, and windowed aggregates.
//! It sits beside the join engine because a window is evaluated over the joined
//! row set, and reuses [`JoinEval`] for argument evaluation so window arguments
//! resolve exactly like any other expression.

use std::collections::HashMap;

use crate::error::{SqlError, SqlResult};
use crate::join::{as_f64, unsupported, JoinEval, JoinScope, OuterContext};
use crate::query::value_cmp;
use crate::row::values_equal;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{
    Expression, FrameBound, FrameBounds, FrameSpec, InMemoryCatalog, OrderByItem, Value, WindowSpec,
};
use plomid_txn::StorageEngine;

/// Computes a window function across the result rows, returning one value per
/// input row in original order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_window<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    scopes: &[JoinScope],
    rows: &[Vec<Value>],
    fname: &str,
    args: &[Expression],
    over: &WindowSpec,
    outer: Option<&OuterContext>,
    depth: usize,
    current_database: &str,
    current_user: &str,
) -> SqlResult<Vec<Value>> {
    let name = fname.to_ascii_lowercase();
    if !matches!(
        name.as_str(),
        "row_number"
            | "rank"
            | "dense_rank"
            | "percent_rank"
            | "cume_dist"
            | "ntile"
            | "lag"
            | "lead"
            | "first_value"
            | "last_value"
            | "nth_value"
            | "sum"
            | "avg"
            | "count"
            | "min"
            | "max"
    ) {
        return Err(unsupported(format!(
            "window function \"{fname}\" is not supported"
        )));
    }
    if matches!(name.as_str(), "lag" | "lead") && args.is_empty() {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("window function {name} requires at least one argument"),
        )));
    }
    if matches!(name.as_str(), "first_value" | "last_value" | "nth_value") && args.is_empty() {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("window function {name} requires an argument"),
        )));
    }
    if matches!(name.as_str(), "ntile") && args.is_empty() {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("window function {name} requires an argument"),
        )));
    }
    if name == "nth_value" && args.len() < 2 {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("window function {name} requires two arguments"),
        )));
    }
    let mut eval = JoinEval::new(
        engine,
        catalog,
        scopes,
        outer,
        depth,
        current_database,
        current_user,
    );
    // Partition and ordering keys per row.
    let n = rows.len();
    let mut partition_keys: Vec<String> = Vec::with_capacity(n);
    for row in rows {
        let mut parts = Vec::with_capacity(over.partition_by.len());
        for expr in &over.partition_by {
            let v = eval.eval(row, expr)?;
            parts.push(v.to_sql_text());
        }
        partition_keys.push(parts.join("\u{1}"));
    }
    let mut order_keys: Vec<Vec<Value>> = Vec::with_capacity(n);
    for row in rows {
        let mut keys = Vec::with_capacity(over.order_by.len());
        for item in &over.order_by {
            keys.push(eval.eval(row, &item.expr)?);
        }
        order_keys.push(keys);
    }

    // Group row indices by partition key (preserve first-seen order).
    let mut partitions: Vec<Vec<usize>> = Vec::new();
    let mut partition_index: HashMap<String, usize> = HashMap::new();
    for (i, key) in partition_keys.iter().enumerate() {
        let index = *partition_index.entry(key.clone()).or_insert_with(|| {
            partitions.push(Vec::new());
            partitions.len() - 1
        });
        partitions[index].push(i);
    }

    let mut result = vec![Value::Null; n];
    for indices in &partitions {
        // Sort each partition once per window specification, then precompute
        // peer-group metadata so ranking/default frames never split a group.
        let mut ordered = indices.clone();
        ordered.sort_by(|&a, &b| compare_order_keys(a, b, &over.order_by, &order_keys));
        let peers = build_peer_groups(&ordered, &order_keys);
        let rows_in_partition = ordered.len();
        for (pos, &row_index) in ordered.iter().enumerate() {
            let (fstart, fend) = frame_bounds(
                over,
                &mut eval,
                &rows[row_index],
                pos,
                rows_in_partition,
                &peers,
            )?;
            match name.as_str() {
                "row_number" => result[row_index] = Value::Int8(pos as i64 + 1),
                "rank" => result[row_index] = Value::Int8(peers.group_rank(pos) as i64),
                "dense_rank" => result[row_index] = Value::Int8(peers.dense_rank(pos) as i64),
                "percent_rank" => {
                    result[row_index] =
                        Value::Float8(percent_rank(peers.group_rank(pos), rows_in_partition))
                }
                "cume_dist" => {
                    result[row_index] =
                        Value::Float8((peers.group_end(pos) + 1) as f64 / rows_in_partition as f64)
                }
                "ntile" => {
                    let buckets = match args.get(0) {
                        Some(expr) => match eval.eval(&rows[row_index], expr)? {
                            Value::Int2(v) => usize::try_from(i64::from(v)).unwrap_or(0),
                            Value::Int4(v) => usize::try_from(i64::from(v)).unwrap_or(0),
                            Value::Int8(v) => usize::try_from(v).unwrap_or(0),
                            _ => 0,
                        },
                        None => 0,
                    };
                    result[row_index] = ntile_value(pos, rows_in_partition, buckets);
                }
                "lag" | "lead" => {
                    result[row_index] = lag_lead(
                        &mut eval,
                        rows,
                        &ordered,
                        args,
                        &rows[row_index],
                        pos,
                        name == "lag",
                    )?;
                }
                "first_value" | "last_value" | "nth_value" => {
                    result[row_index] = value_window_function(
                        &mut eval,
                        rows,
                        &ordered,
                        args,
                        name.as_str(),
                        pos,
                        fstart,
                        fend,
                    )?;
                }
                "sum" | "avg" | "count" | "min" | "max" => {
                    result[row_index] = aggregate_window(
                        &mut eval,
                        rows,
                        &ordered,
                        args,
                        name.as_str(),
                        fstart,
                        fend,
                    )?;
                }
                _ => unreachable!("supported window functions are matched above"),
            }
        }
    }
    Ok(result)
}

/// Compares two order-key vectors following the window `ORDER BY` direction
/// and NULL ordering. `a`/`b` are **global** row indexes into `order_keys`.
fn compare_order_keys(
    a: usize,
    b: usize,
    order_by: &[OrderByItem],
    order_keys: &[Vec<Value>],
) -> std::cmp::Ordering {
    for (key_index, item) in order_by.iter().enumerate() {
        let va = &order_keys[a][key_index];
        let vb = &order_keys[b][key_index];
        let cmp = match (va.is_null(), vb.is_null()) {
            (true, true) => std::cmp::Ordering::Equal,
            // PostgreSQL default: NULLS LAST for ASC, first for DESC.
            (true, false) if item.descending => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) if item.descending => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => value_cmp(va, vb).unwrap_or(std::cmp::Ordering::Equal),
        };
        let cmp = if item.descending { cmp.reverse() } else { cmp };
        if cmp != std::cmp::Ordering::Equal {
            return cmp;
        }
    }
    std::cmp::Ordering::Equal
}

/// Tracks peer-group boundaries over a partition's ordered row positions.
///
/// Rows are peers when their full window `ORDER BY` key vector compares equal.
/// Positions are 0-based indexes into the sorted `ordered` buffer.
#[derive(Debug, Clone)]
struct PeerGroups {
    /// For each ordered position, the first ordered position of its group.
    starts: Vec<usize>,
    /// For each ordered position, the last ordered position of its group.
    ends: Vec<usize>,
}

impl PeerGroups {
    /// Standard RANK: 1-based position of the first row of this row's group.
    fn group_rank(&self, pos: usize) -> usize {
        self.starts[pos] + 1
    }

    /// DENSE_RANK: number of distinct peer groups up to and including `pos`.
    fn dense_rank(&self, pos: usize) -> usize {
        let mut dense = 0;
        for p in 0..=pos {
            if p == 0 || self.starts[p] != self.starts[p - 1] {
                dense += 1;
            }
        }
        dense
    }

    fn group_start(&self, pos: usize) -> usize {
        self.starts[pos]
    }

    fn group_end(&self, pos: usize) -> usize {
        self.ends[pos]
    }
}

/// Builds peer-group boundaries for an ordered partition.
fn build_peer_groups(ordered: &[usize], order_keys: &[Vec<Value>]) -> PeerGroups {
    let m = ordered.len();
    let mut starts = vec![0; m];
    let mut ends = vec![0; m];
    let mut current = 0;
    while current < m {
        let group_start = current;
        let key = &order_keys[ordered[group_start]];
        let mut group_end = group_start;
        while group_end + 1 < m && order_keys_equal(&order_keys[ordered[group_end + 1]], key) {
            group_end += 1;
        }
        for p in group_start..=group_end {
            starts[p] = group_start;
            ends[p] = group_end;
        }
        current = group_end + 1;
    }
    PeerGroups { starts, ends }
}

fn order_keys_equal(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(l, r)| values_equal(l, r))
}

/// Which frame mode a `ROWS`/`RANGE`/`GROUPS` clause selects (executor-local copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Rows,
    Range,
    Groups,
}

/// Computes the inclusive `[start, end]` row window for the given ordered
/// position, applying frame clipping and peer-group snapping as required by
/// `ROWS`/`RANGE`/`GROUPS`. Offsets are evaluated against the current row.
fn frame_bounds<'a, E: StorageEngine>(
    over: &WindowSpec,
    eval: &mut JoinEval<'a, E>,
    current: &[Value],
    pos: usize,
    total: usize,
    peers: &PeerGroups,
) -> SqlResult<(usize, usize)> {
    // Derive the effective frame. When none is given, follow the PostgreSQL
    // default: with ORDER BY the frame is RANGE UNBOUNDED PRECEDING ..
    // CURRENT ROW (peer-aware running frame); without ORDER BY it is the whole
    // partition.
    let frame = over.frame.clone();
    let (kind, bounds) = match frame {
        Some(FrameSpec::Rows { bounds }) => (FrameKind::Rows, bounds),
        Some(FrameSpec::Range { bounds }) => (FrameKind::Range, bounds),
        Some(FrameSpec::Groups { bounds }) => (FrameKind::Groups, bounds),
        None if !over.order_by.is_empty() => (
            FrameKind::Range,
            FrameBounds {
                start: FrameBound::UnboundedPreceding,
                end: FrameBound::CurrentRow,
            },
        ),
        None => (
            FrameKind::Range,
            FrameBounds {
                start: FrameBound::UnboundedPreceding,
                end: FrameBound::UnboundedFollowing,
            },
        ),
    };
    let start = resolve_bound(eval, current, pos, total, peers, kind, &bounds.start, true)?;
    let end = resolve_bound(eval, current, pos, total, peers, kind, &bounds.end, false)?;
    Ok((start.min(end), end.max(start)))
}

#[allow(clippy::too_many_arguments)]
fn resolve_bound<'a, E: StorageEngine>(
    eval: &mut JoinEval<'a, E>,
    current: &[Value],
    pos: usize,
    total: usize,
    peers: &PeerGroups,
    kind: FrameKind,
    bound: &FrameBound,
    is_start: bool,
) -> SqlResult<usize> {
    let m = total;
    match (kind, bound) {
        (_, FrameBound::UnboundedPreceding) => Ok(0),
        (_, FrameBound::UnboundedFollowing) => Ok(m.saturating_sub(1)),
        (FrameKind::Rows, FrameBound::CurrentRow) => Ok(pos),
        (FrameKind::Rows, FrameBound::Preceding { offset }) => {
            let n = eval_offset(eval, current, Some(offset.as_ref()))?;
            Ok(pos.saturating_sub(n))
        }
        (FrameKind::Rows, FrameBound::Following { offset }) => {
            let n = eval_offset(eval, current, Some(offset.as_ref()))?;
            Ok(pos.saturating_add(n).min(m.saturating_sub(1)))
        }
        (FrameKind::Range | FrameKind::Groups, FrameBound::CurrentRow) => Ok(if is_start {
            peers.group_start(pos)
        } else {
            peers.group_end(pos)
        }),
        (FrameKind::Groups, FrameBound::Preceding { offset }) => {
            let n = eval_offset(eval, current, Some(offset.as_ref()))?;
            Ok(group_start_n_before(peers, pos, n))
        }
        (FrameKind::Groups, FrameBound::Following { offset }) => {
            let n = eval_offset(eval, current, Some(offset.as_ref()))?;
            Ok(group_end_n_after(peers, pos, n))
        }
        (FrameKind::Range, FrameBound::Preceding { .. }) => {
            // PostgreSQL RANGE offsets are VALUE offsets against every ORDER BY
            // key; approximating them with row/peer offsets would silently
            // return wrong frames, so they are refused explicitly.
            Err(unsupported(
                "RANGE with an offset is not supported; use ROWS or GROUPS for offset frames",
            ))
        }
        (FrameKind::Range, FrameBound::Following { .. }) => Err(unsupported(
            "RANGE with an offset is not supported; use ROWS or GROUPS for offset frames",
        )),
    }
}

/// The 0-based peer-group ordinal containing ordered position `pos`.
fn group_ordinal(peers: &PeerGroups, pos: usize) -> usize {
    let mut ordinal: usize = 0;
    for i in 0..=pos {
        if i == 0 || peers.starts[i] != peers.starts[i - 1] {
            ordinal = ordinal.saturating_add(1);
        }
    }
    ordinal.saturating_sub(1)
}

/// Start positions of each peer group, in order.
fn group_starts(peers: &PeerGroups) -> Vec<usize> {
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..peers.starts.len() {
        if i == 0 || peers.starts[i] != peers.starts[i - 1] {
            starts.push(i);
        }
    }
    starts
}

/// Start position of the group `n` groups before the group containing `pos`.
fn group_start_n_before(peers: &PeerGroups, pos: usize, n: usize) -> usize {
    let starts = group_starts(peers);
    let ordinal = group_ordinal(peers, pos);
    let target = ordinal.saturating_sub(n);
    starts.get(target).copied().unwrap_or(0)
}

/// End position of the group `n` groups after the group containing `pos`.
fn group_end_n_after(peers: &PeerGroups, pos: usize, n: usize) -> usize {
    let starts = group_starts(peers);
    let ordinal = group_ordinal(peers, pos);
    let target = ordinal.saturating_add(n);
    let start = starts
        .get(target)
        .copied()
        .unwrap_or(starts.last().copied().unwrap_or(0));
    peers.ends[start]
}

/// Reads an integer offset expression used by frame offsets and NTILE.
fn eval_offset<'a, E: StorageEngine>(
    eval: &mut JoinEval<'a, E>,
    row: &[Value],
    expr: Option<&Expression>,
) -> SqlResult<usize> {
    if let Some(expr) = expr {
        match eval.eval(row, expr)? {
            Value::Int2(v) => Ok(usize::try_from(i64::from(v)).unwrap_or(0)),
            Value::Int4(v) => Ok(usize::try_from(i64::from(v)).unwrap_or(0)),
            Value::Int8(v) => Ok(usize::try_from(v).unwrap_or(0)),
            _ => Ok(0),
        }
    } else {
        Ok(1)
    }
}

fn percent_rank(rank: usize, total: usize) -> f64 {
    if total <= 1 {
        return 0.0;
    }
    (rank - 1) as f64 / (total - 1) as f64
}

fn ntile_value(pos: usize, total: usize, buckets: usize) -> Value {
    if buckets == 0 || total == 0 {
        return Value::Null;
    }
    // Standard even-distribution bucket assignment, 1-based.
    let v = pos as i128 + 1;
    let n = buckets as i128;
    let m = total as i128;
    let bucket = ((v * n - 1) / m).min(n - 1) + 1;
    Value::Int8(bucket as i64)
}

/// Evaluates LAG/LEAD against the ordered partition using position offsets.
#[allow(clippy::too_many_arguments)]
fn lag_lead<'a, E: StorageEngine>(
    eval: &mut JoinEval<'a, E>,
    rows: &[Vec<Value>],
    ordered: &[usize],
    args: &[Expression],
    current: &[Value],
    pos: usize,
    is_lag: bool,
) -> SqlResult<Value> {
    let offset = match args.get(1) {
        Some(expr) => match eval.eval(current, expr)? {
            Value::Int2(v) => usize::try_from(i64::from(v)).unwrap_or(0),
            Value::Int4(v) => usize::try_from(i64::from(v)).unwrap_or(0),
            Value::Int8(v) => usize::try_from(v).unwrap_or(0),
            _ => 1,
        },
        None => 1,
    };
    let source = if is_lag {
        pos.checked_sub(offset)
    } else {
        pos.checked_add(offset).filter(|p| *p < ordered.len())
    };
    match source {
        Some(source) => eval.eval(&rows[ordered[source]], &args[0]),
        None => match args.get(2) {
            Some(default) => eval.eval(current, default),
            None => Ok(Value::Null),
        },
    }
}

/// Evaluates FIRST_VALUE, LAST_VALUE and NTH_VALUE against the current frame.
#[allow(clippy::too_many_arguments)]
fn value_window_function<'a, E: StorageEngine>(
    eval: &mut JoinEval<'a, E>,
    rows: &[Vec<Value>],
    ordered: &[usize],
    args: &[Expression],
    name: &str,
    pos: usize,
    fstart: usize,
    fend: usize,
) -> SqlResult<Value> {
    if fend < fstart {
        return Ok(Value::Null);
    }
    if name == "first_value" {
        let value = eval.eval(&rows[ordered[fstart]], &args[0])?;
        return Ok(if value.is_null() { Value::Null } else { value });
    }
    if name == "last_value" {
        let value = eval.eval(&rows[ordered[fend]], &args[0])?;
        return Ok(if value.is_null() { Value::Null } else { value });
    }
    // NTH_VALUE(expr, n) — the n-th value from the start of the frame.
    let nth = match args.get(1) {
        Some(expr) => match eval.eval(&rows[ordered[pos]], expr)? {
            Value::Int2(v) => usize::try_from(i64::from(v)).unwrap_or(0),
            Value::Int4(v) => usize::try_from(i64::from(v)).unwrap_or(0),
            Value::Int8(v) => usize::try_from(v).unwrap_or(0),
            _ => 1,
        },
        None => 1,
    };
    if nth == 0 || nth > fend - fstart + 1 {
        return Ok(Value::Null);
    }
    let value = eval.eval(&rows[ordered[fstart + nth - 1]], &args[0])?;
    Ok(if value.is_null() { Value::Null } else { value })
}

/// Evaluates an aggregate window function (SUM/AVG/COUNT/MIN/MAX) over the
/// computed `[fstart, fend]` frame rather than the whole partition.
fn aggregate_window<'a, E: StorageEngine>(
    eval: &mut JoinEval<'a, E>,
    rows: &[Vec<Value>],
    ordered: &[usize],
    args: &[Expression],
    name: &str,
    fstart: usize,
    fend: usize,
) -> SqlResult<Value> {
    if args.is_empty() {
        return Err(unsupported(format!(
            "window function {name} requires an argument"
        )));
    }
    if fend < fstart {
        // Empty frame: PostgreSQL yields COUNT 0, and NULL for SUM/AVG/MIN/MAX.
        if name == "count" {
            return Ok(Value::Int8(0));
        }
        return Ok(Value::Null);
    }
    let mut values: Vec<Value> = Vec::new();
    let mut count_all = 0;
    let count_star = matches!(args[0], Expression::Star);
    for p in fstart..=fend {
        if count_star {
            count_all += 1;
            continue;
        }
        let value = eval.eval(&rows[ordered[p]], &args[0])?;
        if !value.is_null() {
            values.push(value);
        }
    }
    if name == "count" {
        return Ok(Value::Int8(
            (if count_star { count_all } else { values.len() }) as i64,
        ));
    }
    if name == "min" {
        return Ok(values
            .iter()
            .min_by(|a, b| value_cmp(a, b).unwrap_or(std::cmp::Ordering::Equal))
            .cloned()
            .unwrap_or(Value::Null));
    }
    if name == "max" {
        return Ok(values
            .iter()
            .max_by(|a, b| value_cmp(a, b).unwrap_or(std::cmp::Ordering::Equal))
            .cloned()
            .unwrap_or(Value::Null));
    }
    Ok(sum_avg_value(name, &values))
}

fn sum_avg_value(name: &str, values: &[Value]) -> Value {
    // Mirrors the ordinary aggregate typing rules: integer inputs keep an
    // Int8 sum, a numeric input promotes to exact Numeric accumulation
    // (preserving scale), and float inputs degrade to an f64 accumulation.
    let mut int_sum: i64 = 0;
    let mut numeric_sum: Option<plomid_types::Numeric> = None;
    let mut float_sum: f64 = 0.0;
    let mut has_numeric = false;
    let mut has_float = false;
    for value in values {
        match value {
            Value::Int2(v) => int_sum += i64::from(*v),
            Value::Int4(v) => int_sum += i64::from(*v),
            Value::Int8(v) => int_sum += *v,
            Value::Numeric(n) => {
                has_numeric = true;
                let base = numeric_sum
                    .take()
                    .unwrap_or_else(|| plomid_types::Numeric::from_i64(int_sum));
                int_sum = 0;
                numeric_sum = base.add(n.clone()).ok();
            }
            other => {
                if let Some(f) = as_f64(other) {
                    has_float = true;
                    float_sum += f;
                }
            }
        }
    }
    let count = values.len();
    if name == "avg" {
        if count == 0 {
            return Value::Null;
        }
        if has_numeric {
            let total = numeric_sum.unwrap_or_else(|| plomid_types::Numeric::from_i64(int_sum));
            let divided = total
                .clone()
                .div(plomid_types::Numeric::from_i64(count as i64));
            return match divided {
                Ok(value) => Value::Numeric(value.normalize()),
                Err(_) => Value::Numeric(total),
            };
        }
        if has_float {
            let avg = (int_sum as f64 + float_sum) / count as f64;
            return Value::Numeric(plomid_types::Numeric::from_f64(avg).normalize());
        }
        // PostgreSQL AVG of integers returns an exact numeric mean (normalized
        // so integral means render as "21", fractional as "266.6666...").
        return plomid_types::Numeric::from_i64(int_sum)
            .div(plomid_types::Numeric::from_i64(count as i64))
            .map(|v| Value::Numeric(v.normalize()))
            .unwrap_or(Value::Null);
    }
    // SUM
    if count == 0 {
        return Value::Null;
    }
    if has_numeric {
        return Value::Numeric(
            numeric_sum.unwrap_or_else(|| plomid_types::Numeric::from_i64(int_sum)),
        );
    }
    if has_float {
        return Value::Numeric(plomid_types::Numeric::from_f64(int_sum as f64 + float_sum));
    }
    Value::Int8(int_sum)
}
