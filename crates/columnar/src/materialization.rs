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
//! Materialization: convert Hot Row Store rows into columnar representation.

use crate::column::ColumnType;
use crate::layout::invalid;
use plomid_core::{ColumnId, ErrorKind, PlomidError, Result};
use plomid_mvcc::{Snapshot, VersionStore};
use plomid_storage::{Field, Row};

/// A materialized column in memory.
#[derive(Debug, Clone)]
pub struct MaterializedColumn {
    pub column_id: ColumnId,
    pub column_type: ColumnType,
    pub row_count: u64,
    pub null_bitmap: Vec<u8>,
    pub values: Vec<u8>,
    pub offsets: Vec<u32>,
    pub lengths: Vec<u32>,
}

/// Builder for materializing columns.
pub struct ColumnMaterializer {
    pub row_count: u64,
    pub columns: Vec<MaterializedColumn>,
}

impl ColumnMaterializer {
    pub fn new(row_count: u64, column_count: usize) -> Self {
        let null_bitmap = vec![0u8; (row_count as usize).div_ceil(8)];
        Self {
            row_count,
            columns: (0..column_count)
                .map(|i| MaterializedColumn {
                    column_id: ColumnId::new(i as u64),
                    column_type: ColumnType::Null,
                    row_count,
                    null_bitmap: null_bitmap.clone(),
                    values: Vec::new(),
                    offsets: vec![0; row_count as usize],
                    lengths: vec![0; row_count as usize],
                })
                .collect(),
        }
    }

    /// Declares the logical type of column `col_idx`.
    pub fn set_column_type(&mut self, col_idx: usize, col_type: ColumnType) {
        if col_idx < self.columns.len() {
            self.columns[col_idx].column_type = col_type;
        }
    }

    /// Marks row `row_idx` of column `col_idx` as NULL.
    pub fn set_null(&mut self, row_idx: usize, col_idx: usize) {
        if col_idx < self.columns.len() && row_idx < self.row_count as usize {
            let byte_idx = row_idx / 8;
            let bit_idx = row_idx % 8;
            self.columns[col_idx].null_bitmap[byte_idx] |= 1 << bit_idx;
        }
    }

    /// Appends the value bytes of row `row_idx` of column `col_idx`.
    pub fn append_value(&mut self, row_idx: usize, col_idx: usize, value: &[u8]) {
        if col_idx >= self.columns.len() || row_idx >= self.row_count as usize {
            return;
        }
        let col = &mut self.columns[col_idx];
        let offset = col.values.len() as u32;
        let length = value.len() as u32;
        col.values.extend_from_slice(value);
        col.offsets[row_idx] = offset;
        col.lengths[row_idx] = length;
    }

    pub fn finalize(mut self) -> Result<Vec<MaterializedColumn>> {
        for col in &mut self.columns {
            col.row_count = self.row_count;
            for i in 0..self.row_count as usize {
                if col.is_null(i) {
                    if col.offsets[i] != 0 || col.lengths[i] != 0 {
                        return Err(PlomidError::new(
                            plomid_core::ErrorKind::Corruption,
                            format!("NULL row {} has non-zero offset/length", i),
                        ));
                    }
                }
            }
        }
        Ok(self.columns)
    }
}

impl MaterializedColumn {
    /// Returns true when `row_idx` is marked NULL.
    #[must_use]
    pub fn is_null(&self, row_idx: usize) -> bool {
        crate::format::null_bitmap_is_null(&self.null_bitmap, row_idx)
    }

    /// Returns the stored bytes for `row_idx`, or `None` when NULL.
    #[must_use]
    pub fn get_value(&self, row_idx: usize) -> Option<&[u8]> {
        if self.is_null(row_idx) {
            return None;
        }
        let offset = *self.offsets.get(row_idx)? as usize;
        let length = *self.lengths.get(row_idx)? as usize;
        let end = offset.checked_add(length)?;
        self.values.get(offset..end)
    }

    /// Returns the stored bytes for `row_idx`, reporting layout corruption.
    ///
    /// Unlike [`Self::get_value`], a NULL row and an inconsistent offset/length
    /// pair are distinguished, so a writer can refuse to persist a column whose
    /// in-memory layout does not add up.
    pub fn value_bytes(&self, row_idx: usize) -> Result<&[u8]> {
        if self.is_null(row_idx) {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                format!("row {row_idx} is NULL and has no value bytes"),
            ));
        }
        self.get_value(row_idx).ok_or_else(|| {
            PlomidError::new(
                ErrorKind::Corruption,
                format!("row {row_idx} value range is outside the column buffer"),
            )
        })
    }

    /// Encodes this column's values in the persisted segment layout.
    ///
    /// Every row contributes a little-endian `u32` length followed by exactly
    /// that many value bytes; a NULL row contributes a zero length. The NULL
    /// bitmap stays authoritative, so a zero length on a non-NULL row is an
    /// empty value rather than a NULL.
    pub fn encoded_values(&self) -> Result<Vec<u8>> {
        Ok(self.encode_values()?.0)
    }

    /// Encodes this column's values and returns the per-row entry boundaries.
    ///
    /// The returned offsets have `row_count + 1` entries: entry `i` occupies
    /// `offsets[i]..offsets[i + 1]`. Deciding chunk boundaries from these
    /// offsets is what guarantees a chunk never splits a value entry.
    pub fn encode_values(&self) -> Result<(Vec<u8>, Vec<usize>)> {
        let row_count = self.row_count as usize;
        let mut encoded = Vec::with_capacity(self.values.len() + row_count * 4);
        let mut offsets = Vec::with_capacity(row_count + 1);
        for row_idx in 0..row_count {
            offsets.push(encoded.len());
            let value = if self.is_null(row_idx) {
                &[][..]
            } else {
                self.value_bytes(row_idx)?
            };
            let length = u32::try_from(value.len()).map_err(|_| {
                invalid(format!(
                    "row {row_idx} value exceeds the maximum encodable length"
                ))
            })?;
            encoded.extend_from_slice(&length.to_le_bytes());
            encoded.extend_from_slice(value);
        }
        offsets.push(encoded.len());
        Ok((encoded, offsets))
    }

    /// Returns the number of rows marked NULL.
    #[must_use]
    pub fn null_count(&self) -> u64 {
        self.null_bitmap
            .iter()
            .map(|byte| u64::from(byte.count_ones()))
            .sum()
    }

    /// Rebuilds the logical value of `row_idx`.
    ///
    /// Returns [`Field::Null`] for NULL rows and for values that cannot be
    /// interpreted through the column's logical type.
    #[must_use]
    pub fn get_field(&self, row_idx: usize) -> Field {
        if self.is_null(row_idx) {
            return Field::Null;
        }
        let Some(bytes) = self.get_value(row_idx) else {
            return Field::Null;
        };
        match self.column_type {
            ColumnType::Integer => match <[u8; 8]>::try_from(bytes) {
                Ok(array) => Field::Integer(i64::from_le_bytes(array)),
                Err(_) => Field::Null,
            },
            ColumnType::String => match std::str::from_utf8(bytes) {
                Ok(text) => Field::String(text.to_owned()),
                Err(_) => Field::Null,
            },
            ColumnType::Bytes => Field::Bytes(bytes.to_vec()),
            ColumnType::Null
            | ColumnType::Bool
            | ColumnType::I16
            | ColumnType::I32
            | ColumnType::F32
            | ColumnType::F64 => Field::Null,
        }
    }
}

/// Materializes columns from a set of rows.
///
/// Every row must carry exactly one field per declared column. A ragged row is
/// refused rather than silently truncated, because a segment must be able to
/// state the arity of the rows it holds.
pub fn materialize_columns(
    rows: &[Row],
    column_types: &[ColumnType],
) -> Result<Vec<MaterializedColumn>> {
    let row_count = rows.len() as u64;
    let column_count = column_types.len();
    let mut materializer = ColumnMaterializer::new(row_count, column_count);
    for (i, ty) in column_types.iter().enumerate() {
        materializer.set_column_type(i, *ty);
    }
    for (row_idx, row) in rows.iter().enumerate() {
        if row.fields().len() != column_count {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!(
                    "row {row_idx} has {} fields but the segment declares {column_count} columns",
                    row.fields().len()
                ),
            ));
        }
        for (col_idx, field) in row.fields().iter().enumerate() {
            match field {
                Field::Null => materializer.set_null(row_idx, col_idx),
                Field::Integer(val) => {
                    materializer.append_value(row_idx, col_idx, &val.to_le_bytes());
                }
                Field::Bytes(bytes) => materializer.append_value(row_idx, col_idx, bytes),
                Field::String(text) => {
                    materializer.append_value(row_idx, col_idx, text.as_bytes());
                }
            }
        }
    }
    materializer.finalize()
}

/// Materializes columns from an MVCC version store scan.
pub fn materialize_from_version_store(
    version_store: &VersionStore,
    snapshot: &Snapshot,
    start_key: Option<&[u8]>,
    end_key: Option<&[u8]>,
    column_types: &[ColumnType],
) -> Result<Vec<MaterializedColumn>> {
    let scan_results = version_store.scan_visible(start_key, end_key, snapshot);
    let mut rows = Vec::with_capacity(scan_results.len());
    for (_key, payload) in scan_results {
        if let Ok(row) = Row::decode(&payload) {
            rows.push(row);
        }
    }
    materialize_columns(&rows, column_types)
}

/// Estimates the memory size of materialized columns.
pub fn estimate_materialized_size(columns: &[MaterializedColumn]) -> usize {
    columns
        .iter()
        .map(|col| {
            col.null_bitmap.len() + col.values.len() + col.offsets.len() * 4 + col.lengths.len() * 4
        })
        .sum()
}
