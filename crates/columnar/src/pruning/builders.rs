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
//! Metadata construction from real materialized column data.
//!
//! Everything here runs inside the flush pipeline, over the same
//! [`MaterializedColumn`] values the chunk payloads are encoded from — the
//! metadata cannot drift from the data because it is derived from the very
//! same in-memory structures, in the same pass.

use super::brin::BrinIndex;
use super::zonemap::{ZoneMap, ZoneMapBuilder};
use crate::column::ColumnType;
use plomid_core::ColumnId;
use plomid_storage::Field;

/// Builds one zone map per chunk extent from materialized columns.
///
/// `chunks` are `(column_id, first_row, row_count)` physical extents — the
/// same boundaries the writer cuts chunk payloads at. Each returned zone map
/// covers exactly one such extent and summarizes the materialized values
/// that become that chunk's payload. Rows outside a column's materialized
/// range are treated as NULL; for well-formed flush input this cannot
/// happen, but the builder stays total instead of panicking on ragged data.
#[must_use]
pub fn build_zones_for_chunks(
    columns: &[crate::materialization::MaterializedColumn],
    chunks: &[(ColumnId, u64, u64)],
) -> Vec<ZoneMap> {
    let by_column: std::collections::BTreeMap<
        ColumnId,
        &crate::materialization::MaterializedColumn,
    > = columns
        .iter()
        .map(|column| (column.column_id, column))
        .collect();
    let mut out = Vec::with_capacity(chunks.len());
    for (column_id, first_row, row_count) in chunks {
        let column = by_column.get(column_id);
        let column_type = column.map_or(ColumnType::Null, |column| column.column_type);
        let mut builder = ZoneMapBuilder::new(*column_id, column_type);
        for row in *first_row..first_row.saturating_add(*row_count) {
            let value = column.and_then(|column| {
                if (row as usize) < column.row_count as usize {
                    Some(column.get_field(row as usize))
                } else {
                    None
                }
            });
            match value {
                None | Some(Field::Null) => builder.observe(row, None),
                Some(field) => builder.observe(row, Some(field)),
            }
        }
        out.push(builder.finish());
    }
    out
}

/// Builds a BRIN index over `rows_per_range`-row intervals from materialized
/// columns.
///
/// This is the metadata the flush pipeline persists: every range summarizes
/// the actual rows that fall inside its interval, per column.
#[must_use]
pub fn build_brin_from_materialized(
    columns: &[crate::materialization::MaterializedColumn],
    row_count: u64,
    rows_per_range: u64,
) -> BrinIndex {
    let mut builder = super::brin::BrinBuilder::new(row_count).with_rows_per_range(rows_per_range);
    for column in columns {
        builder = builder.add_column(column.column_id, column.column_type);
    }
    for column in columns {
        for row in 0..row_count {
            let value = column.get_field(row as usize);
            builder.observe(
                row,
                column.column_id,
                if matches!(value, Field::Null) {
                    None
                } else {
                    Some(value)
                },
            );
        }
    }
    builder.finish()
}

/// Builds the full pruning metadata a flush persists, from materialized
/// columns: a BRIN index over fixed row intervals (each carrying per-column
/// zone maps) plus per-chunk zone maps kept in memory for callers that want
/// chunk-granular pruning.
#[must_use]
pub fn build_segment_pruning(
    columns: &[crate::materialization::MaterializedColumn],
    row_count: u64,
    rows_per_range: u64,
) -> super::scan_plan::SegmentPruning {
    super::scan_plan::SegmentPruning::from_brin(build_brin_from_materialized(
        columns,
        row_count,
        rows_per_range,
    ))
}
