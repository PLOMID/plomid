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
//! The SQL statistics authority.
//!
//! Statistics are derived from the committed SQL row path and persisted as a
//! small storage-engine record.  They are deliberately separate from the
//! immutable columnar segment metadata: that metadata describes a published
//! generation, while these statistics describe the current SQL table state.
//! No planner or storage engine owns a second copy.

use crate::encoding::{decode_row, sql_table_key_range};
use crate::error::SqlResult;
use crate::query::value_cmp;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{value_pg_type, TableSchema, Value};
use plomid_txn::{StorageEngine, StorageEngineTransaction};
use std::collections::{BTreeMap, HashSet};

const PREFIX: &[u8] = b"__plomid_stats:";
const VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColumnStatistics {
    pub column: String,
    pub row_count: u64,
    pub null_count: u64,
    pub distinct_count: u64,
    pub min: Option<Value>,
    pub max: Option<Value>,
}

impl ColumnStatistics {
    fn new(column: String) -> Self {
        Self {
            column,
            row_count: 0,
            null_count: 0,
            distinct_count: 0,
            min: None,
            max: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TableStatistics {
    pub table: String,
    pub row_count: u64,
    pub columns: Vec<ColumnStatistics>,
}

/// Collects statistics from the same committed table range used by SQL reads.
pub(crate) fn collect<E: StorageEngine>(
    engine: &mut E,
    table: &str,
    schema: &TableSchema,
) -> SqlResult<TableStatistics> {
    let mut columns = schema
        .columns
        .iter()
        .map(|column| ColumnStatistics::new(column.name.clone()))
        .collect::<Vec<_>>();
    let mut distinct = vec![HashSet::<String>::new(); columns.len()];
    let (start, end) = sql_table_key_range(table);
    let rows = engine.scan(Some(&start), Some(&end))?;
    let mut row_count = 0_u64;
    for (_, bytes) in rows {
        let row = decode_row(&bytes)?;
        if row.len() != columns.len() {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                format!(
                    "table {table} row has {} columns, expected {}",
                    row.len(),
                    columns.len()
                ),
            )
            .into());
        }
        row_count += 1;
        for (index, value) in row.iter().enumerate() {
            let stats = &mut columns[index];
            stats.row_count += 1;
            if value.is_null() {
                stats.null_count += 1;
                continue;
            }
            distinct[index].insert(value.to_sql_text());
            if stats
                .min
                .as_ref()
                .and_then(|current| value_cmp(value, current))
                .is_none_or(|ordering| ordering.is_lt())
            {
                stats.min = Some(value.clone());
            }
            if stats
                .max
                .as_ref()
                .and_then(|current| value_cmp(value, current))
                .is_none_or(|ordering| ordering.is_gt())
            {
                stats.max = Some(value.clone());
            }
        }
    }
    for (stats, values) in columns.iter_mut().zip(distinct) {
        stats.distinct_count = values.len() as u64;
    }
    Ok(TableStatistics {
        table: table.to_owned(),
        row_count,
        columns,
    })
}

pub(crate) fn key(table: &str) -> Vec<u8> {
    let mut key = PREFIX.to_vec();
    key.extend_from_slice(table.as_bytes());
    key
}

pub(crate) fn load_all<E: StorageEngine>(
    engine: &mut E,
) -> SqlResult<BTreeMap<String, TableStatistics>> {
    let end = {
        let mut end = PREFIX.to_vec();
        end.push(0xff);
        end
    };
    let mut loaded = BTreeMap::new();
    for (_, bytes) in engine.scan(Some(PREFIX), Some(&end))? {
        let stats = decode(&bytes)?;
        loaded.insert(stats.table.clone(), stats);
    }
    Ok(loaded)
}

pub(crate) fn save<E: StorageEngine>(engine: &mut E, stats: &TableStatistics) -> SqlResult<()> {
    let mut txn = engine.begin()?;
    txn.put(&key(&stats.table), &encode(stats))?;
    txn.commit()?;
    Ok(())
}

fn encode(stats: &TableStatistics) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(VERSION);
    put_string(&mut out, &stats.table);
    out.extend_from_slice(&stats.row_count.to_le_bytes());
    out.extend_from_slice(&(stats.columns.len() as u16).to_le_bytes());
    for column in &stats.columns {
        put_string(&mut out, &column.column);
        out.extend_from_slice(&column.row_count.to_le_bytes());
        out.extend_from_slice(&column.null_count.to_le_bytes());
        out.extend_from_slice(&column.distinct_count.to_le_bytes());
        put_value(&mut out, column.min.as_ref());
        put_value(&mut out, column.max.as_ref());
    }
    out
}

fn decode(bytes: &[u8]) -> SqlResult<TableStatistics> {
    let mut cursor = Cursor { bytes, pos: 0 };
    if cursor.byte()? != VERSION {
        return Err(
            PlomidError::new(ErrorKind::Corruption, "unsupported statistics version").into(),
        );
    }
    let table = cursor.string()?;
    let row_count = cursor.u64()?;
    let column_count = cursor.u16()? as usize;
    let mut columns = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        columns.push(ColumnStatistics {
            column: cursor.string()?,
            row_count: cursor.u64()?,
            null_count: cursor.u64()?,
            distinct_count: cursor.u64()?,
            min: cursor.value()?,
            max: cursor.value()?,
        });
    }
    if cursor.pos != bytes.len() {
        return Err(PlomidError::new(
            ErrorKind::Corruption,
            "statistics record has trailing bytes",
        )
        .into());
    }
    Ok(TableStatistics {
        table,
        row_count,
        columns,
    })
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn put_value(out: &mut Vec<u8>, value: Option<&Value>) {
    let Some(value) = value else {
        out.push(0);
        return;
    };
    out.push(1);
    let oid = value_pg_type(value).map_or(0, |ty| ty.oid().0);
    out.extend_from_slice(&oid.to_le_bytes());
    put_string(out, &value.to_sql_text());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> SqlResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| PlomidError::new(ErrorKind::Corruption, "statistics length overflow"))?;
        let bytes = self.bytes.get(self.pos..end).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "truncated statistics record")
        })?;
        self.pos = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> SqlResult<u8> {
        Ok(*self.take(1)?.first().expect("one byte"))
    }
    fn u16(&mut self) -> SqlResult<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }
    fn u64(&mut self) -> SqlResult<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }
    fn string(&mut self) -> SqlResult<String> {
        let len = u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")) as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| {
            PlomidError::new(ErrorKind::Corruption, "statistics string is not UTF-8").into()
        })
    }
    fn value(&mut self) -> SqlResult<Option<Value>> {
        if self.byte()? == 0 {
            return Ok(None);
        }
        let oid = plomid_types::TypeOid(self.u32()?);
        let text = self.string()?;
        plomid_types::text::parse_value_oid(&text, oid)
            .map(Some)
            .map_err(|error| PlomidError::new(ErrorKind::Corruption, error).into())
    }
    fn u32(&mut self) -> SqlResult<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }
}
