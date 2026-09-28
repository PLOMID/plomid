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
//! Join scopes: the relations a row is evaluated against.

use crate::error::{SqlError, SqlResult};
use plomid_core::{ColumnId, ErrorKind, PlomidError, TableId};
use plomid_sql::{ColumnDef, ColumnType, TableSchema, Value};

/// combined row.
#[derive(Clone, Debug)]
pub(crate) struct JoinScope {
    pub alias: String,
    pub schema: TableSchema,
    pub offset: usize,
    /// True when this scope was produced by a set-returning function
    /// (`generate_series`, `unnest`, `json_each`, ...).  A bare alias of a
    /// single-column SRF relation resolves as the scalar column value rather
    /// than a whole-row composite (PostgreSQL SRF semantics).
    pub is_srf: bool,
}

impl JoinScope {
    pub(super) fn width(&self) -> usize {
        self.schema.columns.len()
    }
}

/// Row context of an enclosing query, visible to correlated subqueries.
#[derive(Clone, Debug)]
pub(crate) struct OuterContext {
    pub scopes: Vec<JoinScope>,
    pub row: Vec<Value>,
    /// Any context enclosing this one, for nested correlated subqueries.
    pub parent: Option<Box<OuterContext>>,
}

/// Builds a synthetic schema for a subquery result so its columns resolve
/// like ordinary table columns under `alias`.
pub(super) fn synthetic_schema(
    alias: &str,
    columns: &[String],
    types: &[Option<ColumnType>],
) -> TableSchema {
    let cols: Vec<ColumnDef> = columns
        .iter()
        .enumerate()
        .map(|(i, name)| ColumnDef {
            name: name.clone(),
            col_type: types
                .get(i)
                .copied()
                .flatten()
                .unwrap_or_else(ColumnType::text),
            constraints: Vec::new(),
        })
        .collect();
    TableSchema {
        name: alias.to_string(),
        table_id: TableId::new(0),
        column_ids: cols.iter().map(|_| ColumnId::new(0)).collect(),
        columns: cols,
        constraints: Vec::new(),
    }
}

pub(super) fn qualifier_of(name: &str) -> (Option<&str>, &str) {
    match name.rsplit_once('.') {
        Some((q, c)) => (Some(q), c),
        None => (None, name),
    }
}

/// Resolves a (possibly qualified) column reference against the join scopes,
/// then the outer context for correlated references. Returns the absolute
/// index into the flattened row.
pub(super) fn resolve_column(
    scopes: &[JoinScope],
    outer: Option<&OuterContext>,
    name: &str,
) -> SqlResult<usize> {
    let (qualifier, column) = qualifier_of(name);
    let mut matches: Vec<usize> = Vec::new();
    for scope in scopes {
        if let Some(q) = qualifier {
            if !scope.alias.eq_ignore_ascii_case(q) {
                continue;
            }
        }
        for (i, col) in scope.schema.columns.iter().enumerate() {
            if col.name == column || col.name.eq_ignore_ascii_case(column) {
                matches.push(scope.offset + i);
            }
        }
    }
    match matches.len() {
        1 => return Ok(matches[0]),
        n if n > 1 => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("column reference \"{name}\" is ambiguous"),
            )));
        }
        _ => {}
    }
    if let Some(ctx) = outer {
        if let Ok(index) = resolve_column(&ctx.scopes, None, name) {
            return Ok(index);
        }
    }
    let hint = qualifier
        .map(|q| format!(" (no relation has alias \"{q}\")"))
        .unwrap_or_default();
    Err(SqlError::Storage(PlomidError::new(
        ErrorKind::NotFound,
        format!("column \"{name}\" does not exist{hint}"),
    )))
}

/// Looks up a column and returns its value from the row belonging to the
/// matching scope. Unlike `resolve_column`, this preserves which row the
/// reference came from; an outer-column index must never be applied to the
/// inner row.
pub(super) fn lookup_column_value(
    scopes: &[JoinScope],
    row: &[Value],
    name: &str,
) -> SqlResult<Option<Value>> {
    let (qualifier, column) = qualifier_of(name);
    let mut matches = Vec::new();
    for scope in scopes {
        if qualifier.is_some_and(|q| !scope.alias.eq_ignore_ascii_case(q)) {
            continue;
        }
        for (i, col) in scope.schema.columns.iter().enumerate() {
            if col.name == column || col.name.eq_ignore_ascii_case(column) {
                matches.push(scope.offset + i);
            }
        }
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(row.get(matches[0]).cloned().unwrap_or(Value::Null))),
        _ => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("column reference \"{name}\" is ambiguous"),
        ))),
    }
}

pub(super) fn lookup_outer_value(ctx: &OuterContext, name: &str) -> SqlResult<Option<Value>> {
    if let Some(value) = lookup_column_value(&ctx.scopes, &ctx.row, name)? {
        return Ok(Some(value));
    }
    match &ctx.parent {
        Some(parent) => lookup_outer_value(parent, name),
        None => Ok(None),
    }
}

/// Splits a table name into its effective alias. `sales.orders` aliases as
/// `orders`, matching PostgreSQL's behaviour for schema-qualified tables.
pub(super) fn effective_alias(name: &str, alias: &Option<String>) -> String {
    alias
        .clone()
        .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name).to_string())
}
