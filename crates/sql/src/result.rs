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
//! Structured query execution results for the PLOMID V1 SQL subset.

use crate::ColumnType;
use crate::Value;

// "What type is this value?" is answered by the type registry.
pub use plomid_types::value_pg_type;

/// A structured result returned by the executor.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    Rows {
        columns: Vec<String>,
        column_types: Vec<Option<ColumnType>>,
        rows: Vec<Vec<Value>>,
    },
    Inserted(u64),
    Updated(u64),
    Deleted(u64),
    Set,
    /// A DDL/DCL statement succeeded; the payload is the PostgreSQL command
    /// tag reported on the wire (e.g. `CREATE TABLE`, `CREATE SCHEMA`,
    /// `DROP SEQUENCE`, `ALTER TABLE`).
    Created(String),
    Committed,
    RolledBack,
}

impl QueryResult {
    pub fn rows(columns: Vec<String>, rows: Vec<Vec<Value>>) -> Self {
        let column_types = rows
            .first()
            .map(|row| row.iter().map(value_type).collect())
            .unwrap_or_else(|| vec![None; columns.len()]);
        Self::Rows {
            columns,
            column_types,
            rows,
        }
    }

    pub fn inserted(count: u64) -> Self {
        Self::Inserted(count)
    }

    pub fn updated(count: u64) -> Self {
        Self::Updated(count)
    }

    pub fn deleted(count: u64) -> Self {
        Self::Deleted(count)
    }

    pub fn committed() -> Self {
        Self::Committed
    }

    pub fn rolled_back() -> Self {
        Self::RolledBack
    }
}

/// Maps a runtime value to its authoritative PostgreSQL column type`.
fn value_type(value: &Value) -> Option<ColumnType> {
    if let Value::Array { element_oid, .. } = value {
        if let Some(element) = plomid_types::PgType::by_oid(*element_oid) {
            if let Some(array_oid) = element.array_oid() {
                return Some(ColumnType::new(array_oid, plomid_types::NO_TYPEMOD));
            }
        }
    }
    value_pg_type(value).map(|pg_type| ColumnType::new(pg_type.oid(), plomid_types::NO_TYPEMOD))
}
