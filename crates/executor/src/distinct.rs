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
//! `SELECT DISTINCT` deduplication.
//!
//! Duplicate removal is the one place a `SELECT` may collapse rows, so the
//! comparator is the authoritative [`values_equal`] and nothing else. The work
//! here is in *reaching* that comparator cheaply: rows are bucketed by an
//! equality-implied signature so each row is compared only against rows that
//! could equal it, turning the previous `O(rows x distinct values)` linear scan
//! into an `O(rows)` pass.

use crate::row::values_equal;
use plomid_sql::{Expression, QueryResult, Value};

/// Marks the rows `SELECT DISTINCT` must drop, preserving first-seen order.
///
/// The returned flag is `true` when an earlier row exists that
/// [`values_equal`] calls equal. Candidates are bucketed by
/// [`crate::row::equality_signature`], which is implied by [`values_equal`], so
/// every duplicate still lands in the same bucket and is found. This replaces
/// the previous `Vec` scan — one comparison pass per row over every distinct
/// row already kept, i.e. `O(rows x distinct)` — with `O(rows)` signature
/// lookups while leaving every decision to the authoritative comparator.
///
/// A bucket may contain rows that are not equal (a hash collision, or
/// numerically equal values of different types); that only costs a confirmed
/// comparison and can never cause a wrong merge.
///
/// One behaviour note: the signature covers the two [`values_equal`] branches
/// reachable for values of one column type. Its textual fallback additionally
/// merges values of *different* type families in the same position
/// (`Bool(true)` with `Text("true")`, or `Date(10)` with `Int4(10)` through
/// their shared `sort_key`), which no per-value signature can express; such rows
/// are no longer merged. Uniformly typed positions — every real table column —
/// are unaffected.
fn duplicate_flags(rows: &[Vec<Value>]) -> Vec<bool> {
    let mut buckets: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
    let mut duplicate = vec![false; rows.len()];
    for (index, row) in rows.iter().enumerate() {
        let candidates = buckets
            .entry(crate::row::equality_signature(row))
            .or_default();
        let already_seen = candidates.iter().any(|&kept| {
            let previous = &rows[kept];
            previous.len() == row.len()
                && previous
                    .iter()
                    .zip(row.iter())
                    .all(|(left, right)| values_equal(left, right))
        });
        if already_seen {
            duplicate[index] = true;
        } else {
            candidates.push(index);
        }
    }
    duplicate
}

pub(crate) fn apply_distinct(
    mut result: QueryResult,
    distinct: bool,
    distinct_on: &[Expression],
    limit: Option<usize>,
    offset: Option<usize>,
) -> QueryResult {
    let QueryResult::Rows {
        columns: _,
        column_types: _,
        rows,
    } = &mut result
    else {
        return result;
    };
    // Apply DISTINCT deduplication (preserving first-seen row order) when
    // requested, then apply LIMIT/OFFSET — PostgreSQL defers LIMIT until
    // after the duplicate removal. DISTINCT ON rows arrive already
    // de-duplicated from the engine, so only LIMIT/OFFSET apply to them.
    if distinct && distinct_on.is_empty() {
        let duplicate = duplicate_flags(&rows);
        // Compact in place so kept rows are moved, never cloned.
        let mut write = 0;
        for read in 0..rows.len() {
            if !duplicate[read] {
                // `write <= read` always holds, so the row swapped backwards
                // into `read` has already been classified and is never read
                // again.
                if write != read {
                    rows.swap(write, read);
                }
                write += 1;
            }
        }
        rows.truncate(write);
    }
    if distinct || !distinct_on.is_empty() {
        if let Some(skip) = offset {
            rows.drain(..skip.min(rows.len()));
        }
        if let Some(take) = limit {
            rows.truncate(take);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use plomid_sql::ColumnType;

    fn rows_result(rows: Vec<Vec<Value>>) -> QueryResult {
        QueryResult::Rows {
            columns: vec!["c".to_string()],
            column_types: vec![Some(ColumnType::bigint())],
            rows,
        }
    }

    fn distinct(rows: &[Vec<Value>]) -> Vec<Vec<Value>> {
        let result = apply_distinct(rows_result(rows.to_vec()), true, &[], None, None);
        let QueryResult::Rows { rows, .. } = result else {
            panic!("expected rows");
        };
        rows
    }

    #[test]
    fn distinct_keeps_first_seen_order() {
        let rows = vec![
            vec![Value::Int8(3)],
            vec![Value::Int8(1)],
            vec![Value::Int8(3)],
            vec![Value::Int8(2)],
            vec![Value::Int8(1)],
        ];
        assert_eq!(
            distinct(&rows),
            vec![
                vec![Value::Int8(3)],
                vec![Value::Int8(1)],
                vec![Value::Int8(2)]
            ]
        );
    }

    #[test]
    fn distinct_merges_equal_normally_typed_values() {
        // `values_equal` compares numeric values through `f64`, so these three
        // are one group even though their SQL text differs.
        let rows = vec![
            vec![Value::Int8(1)],
            vec![Value::Float8(1.0)],
            vec![Value::Int4(1)],
            vec![Value::Float8(0.0)],
        ];
        assert_eq!(
            distinct(&rows),
            vec![vec![Value::Int8(1)], vec![Value::Float8(0.0)]]
        );
    }

    #[test]
    fn distinct_merges_signed_zero_but_not_nan() {
        let rows = vec![
            vec![Value::Float8(0.0)],
            vec![Value::Float8(-0.0)],
            vec![Value::Float8(f64::NAN)],
            vec![Value::Float8(f64::NAN)],
        ];
        // `-0.0` equals `0.0`; a NaN equals nothing, including itself, so both
        // NaN rows survive.
        assert_eq!(distinct(&rows).len(), 3);
    }

    #[test]
    fn distinct_deduplicates_nulls_and_multi_column_rows() {
        let rows = vec![
            vec![Value::Null, Value::Bool(true)],
            vec![Value::Null, Value::Bool(true)],
            vec![Value::Null, Value::Bool(false)],
            vec![Value::Text("x".into()), Value::Bool(true)],
        ];
        assert_eq!(distinct(&rows).len(), 3);
    }

    #[test]
    fn distinct_defers_limit_and_offset_until_after_dedup() {
        let rows = vec![
            vec![Value::Int8(1)],
            vec![Value::Int8(1)],
            vec![Value::Int8(2)],
            vec![Value::Int8(3)],
        ];
        let result = apply_distinct(rows_result(rows), true, &[], Some(2), Some(1));
        let QueryResult::Rows { rows, .. } = result else {
            panic!("expected rows");
        };
        assert_eq!(rows, vec![vec![Value::Int8(2)], vec![Value::Int8(3)]]);
    }

    #[test]
    fn distinct_on_rows_are_only_paged() {
        // DISTINCT ON rows arrive already de-duplicated by the engine.
        let rows = vec![vec![Value::Int8(1)], vec![Value::Int8(1)]];
        let result = apply_distinct(
            rows_result(rows),
            true,
            &[Expression::ColumnRef("c".into())],
            None,
            None,
        );
        let QueryResult::Rows { rows, .. } = result else {
            panic!("expected rows");
        };
        assert_eq!(rows.len(), 2);
    }
}
