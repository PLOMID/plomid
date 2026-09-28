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
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{
    Catalog, ColumnRule, Expression, InMemoryCatalog, IndexDefinition, InsertSource, InsertValue,
    OnConflict, OnConflictTarget, QueryResult, SelectTarget, TableSchema, Value,
};
use plomid_txn::{StorageEngine, StorageEngineTransaction};
use std::collections::HashSet;

use crate::encoding::{decode_row, encode_row, row_key};
use crate::error::{SqlError, SqlResult};
use crate::index::{
    index_value_bytes, index_value_prefix, prefix_end, stage_index_deletes, stage_index_puts,
    stage_index_update,
};

use crate::query::{evaluate_expression, evaluate_predicate};
use crate::row::{
    coerce_value_for_column, enforce_foreign_keys_txn, enforce_unique_values_txn,
    resolve_insert_values, validate_updated_row, values_equal,
};
use crate::util::{apply_subscripted_assignment, column_and_subscripts, unqualify};

fn apply_serial_defaults_txn<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    table: &str,
    schema: &TableSchema,
    columns: Option<&[String]>,
    mut values: Vec<Value>,
) -> SqlResult<(Option<Vec<String>>, Vec<Value>)> {
    let Some(columns) = columns else {
        return Ok((None, values));
    };
    let mut expanded_columns = columns.to_vec();
    for column in &schema.columns {
        if !column.col_type.serial || columns.iter().any(|name| unqualify(name) == column.name) {
            continue;
        }
        let sequence = format!("{table}_{}_seq", column.name);
        let key = crate::catalog_fn::sequence_key(&sequence);
        let mut end_key = key.clone();
        end_key.push(0xff);
        let current = txn
            .scan(Some(&key), Some(&end_key))
            .map_err(SqlError::Storage)?
            .into_iter()
            .find(|(stored_key, _)| stored_key == &key)
            .and_then(|(_, bytes)| bytes.as_slice().try_into().ok().map(i64::from_le_bytes))
            .unwrap_or(0);
        let next = current.checked_add(1).ok_or_else(|| {
            SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                "sequence value exhausted",
            ))
        })?;
        txn.put(&key, &next.to_le_bytes())
            .map_err(SqlError::Storage)?;
        expanded_columns.push(column.name.clone());
        values.push(Value::Int8(next));
    }
    Ok((Some(expanded_columns), values))
}

pub(crate) struct InsertContext<'a> {
    pub schema: &'a TableSchema,
    pub rules: Vec<ColumnRule>,
    pub columns: Option<Vec<String>>,
}

pub(crate) fn prepare_insert_context<'a>(
    catalog: &'a InMemoryCatalog,
    table: &str,
    columns: Option<Vec<String>>,
) -> SqlResult<InsertContext<'a>> {
    let schema = catalog.get_table(table)?;
    let rules = catalog.column_rules(table)?;
    Ok(InsertContext {
        schema,
        rules,
        columns,
    })
}

pub(crate) fn flatten_insert_value(item: InsertValue, schema: &TableSchema) -> SqlResult<Value> {
    Ok(match item {
        InsertValue::Literal(v) => v,
        InsertValue::Default => Value::Null,
        InsertValue::Expression(expr) => {
            // VALUES expressions evaluate in a dedicated INSERT-VALUES
            // statement context, not a SELECT projection context. Ensure the
            // statement execution context exists so CURRENT_* resolves from
            // the statement clock even when the caller has no SELECT.
            let _ctx = crate::context::StatementContext::enter_if_none();
            let empty_row = vec![];
            evaluate_expression(&empty_row, schema, &expr)?
        }
    })
}

pub(crate) fn flatten_insert_values(
    items: Vec<InsertValue>,
    schema: &TableSchema,
) -> SqlResult<Vec<Value>> {
    items
        .into_iter()
        .map(|item| flatten_insert_value(item, schema))
        .collect()
}

pub(crate) fn expand_insert_source(
    source: InsertSource,
    schema: &TableSchema,
) -> SqlResult<Vec<Vec<Value>>> {
    match source {
        InsertSource::DefaultValues => {
            let row = vec![Value::Null; schema.columns.len()];
            Ok(vec![row])
        }
        InsertSource::Values(rows) => {
            let mut out = Vec::with_capacity(rows.len());
            for (i, row_items) in rows.into_iter().enumerate() {
                let flat = flatten_insert_values(row_items, schema)
                    .map_err(|e| annotate_insert_error(e, i + 1))?;
                out.push(flat);
            }
            Ok(out)
        }
        InsertSource::Select(_stmt) => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            "INSERT ... SELECT is not yet implemented",
        ))),
    }
}

pub(crate) fn annotate_insert_error(err: SqlError, row_number: usize) -> SqlError {
    match err {
        SqlError::Storage(inner) => {
            let msg = format!("INSERT row {row_number}: {}", inner.message());
            let kind = inner.kind();
            let detail = inner.detail().map(|d| format!("{d}; row={row_number}"));
            let new_err = if let Some(d) = detail {
                PlomidError::with_detail(kind, msg, d)
            } else {
                PlomidError::new(kind, msg)
            };
            SqlError::Storage(new_err)
        }
        other => other,
    }
}

fn apply_select_target(
    row: &[Value],
    schema: &TableSchema,
    target: &SelectTarget,
) -> SqlResult<(String, Value)> {
    match target {
        SelectTarget::All => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Internal,
            "RETURNING * must be expanded by the caller",
        ))),
        SelectTarget::QualifiedStar { .. } => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            "RETURNING table.* is not yet implemented",
        ))),
        SelectTarget::Expr { expr, alias } => {
            let value = evaluate_expression(row, schema, expr)?;
            let name = alias.clone().unwrap_or_else(|| format!("{expr:?}"));
            Ok((name, value))
        }
        SelectTarget::FunctionCall { name, args } => {
            let expr = Expression::FunctionCall {
                name: name.clone(),
                args: args.clone(),
                distinct: false,
                filter: None,
                order_by: Vec::new(),
                returning: None,
                null_handling: None,
                unique_keys: None,
            };
            let value = evaluate_expression(row, schema, &expr)?;
            Ok((name.clone(), value))
        }
        SelectTarget::WindowFunction { .. } => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            "window functions in RETURNING are not supported",
        ))),
        SelectTarget::Aliased { target, alias } => {
            let (_, value) = apply_select_target(row, schema, target)?;
            Ok((alias.clone(), value))
        }
        SelectTarget::Function(name) => {
            let expr = Expression::FunctionCall {
                name: name.clone(),
                args: vec![],
                distinct: false,
                filter: None,
                order_by: Vec::new(),
                returning: None,
                null_handling: None,
                unique_keys: None,
            };
            let value = evaluate_expression(row, schema, &expr)?;
            Ok((name.clone(), value))
        }
    }
}

/// Expands `RETURNING` targets against the DELETE/UPDATE/INSERT target schema
/// so every projected row has one value per target.
///
/// `target_rel` carries the DML target's `(table, alias)` so a qualified
/// star (`RETURNING t.*`) whose qualifier matches the target table or its
/// alias can be expanded to all target columns. `None` (INSERT path, or an
/// unknown qualifier) preserves the previous behavior for `table.*` targets.
pub(crate) fn expand_returning_targets(
    schema: &TableSchema,
    returning: &[SelectTarget],
    target_rel: Option<(&str, Option<&str>)>,
) -> Vec<SelectTarget> {
    let mut expanded = Vec::new();
    for target in returning {
        match target {
            SelectTarget::All => {
                for col in &schema.columns {
                    expanded.push(SelectTarget::Expr {
                        expr: Expression::ColumnRef(col.name.clone()),
                        alias: Some(col.name.clone()),
                    });
                }
            }
            SelectTarget::QualifiedStar { qualifier } => {
                // `RETURNING d.*` / `RETURNING table.*`: expand to every
                // target column when the qualifier names the DML target
                // (its alias or its relation name).
                let known = target_rel.is_some_and(|(table, alias)| {
                    qualifier.eq_ignore_ascii_case(unqualify(table))
                        || alias.is_some_and(|a| qualifier.eq_ignore_ascii_case(a))
                });
                if !known {
                    // Unknown qualifier: left in place so the projection
                    // step reports the unsupported target as before.
                    expanded.push(target.clone());
                    continue;
                }
                for col in &schema.columns {
                    expanded.push(SelectTarget::Expr {
                        expr: Expression::ColumnRef(col.name.clone()),
                        alias: Some(col.name.clone()),
                    });
                }
            }
            SelectTarget::Expr { expr, alias } => {
                // Attach a deterministic display name to every expression so
                // the RETURNING result column metadata has exactly one entry
                // per projected value. Previously the `columns` derivation kept
                // only explicitly-aliased targets, so unaliased `RETURNING id`,
                // `RETURNING credit_limit`, and bare-expression targets were
                // silently dropped from the metadata, producing a `columns`
                // vector shorter than the returned rows and crashing the wire
                // protocol with "result column metadata does not match row
                // width". Naming mirrors the SELECT projection path: a bare
                // column yields its unqualified name, a function call its name,
                // and any other expression `?column?`.
                let display_name = match expr {
                    Expression::ColumnRef(col) => unqualify(col).to_string(),
                    Expression::FunctionCall { name, .. } => name.clone(),
                    _ => "?column?".to_string(),
                };
                expanded.push(SelectTarget::Expr {
                    expr: expr.clone(),
                    alias: Some(alias.clone().unwrap_or(display_name)),
                });
            }
            other => expanded.push(other.clone()),
        }
    }
    expanded
}

use plomid_core::ROWID_META_PREFIX;

fn next_rowid_for_table<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    table: &str,
) -> SqlResult<i64> {
    let meta_key = format!("{ROWID_META_PREFIX}{table}").into_bytes();
    let mut end_meta = meta_key.clone();
    end_meta.push(0xff);
    let stored = txn
        .scan(Some(meta_key.as_slice()), Some(end_meta.as_slice()))
        .map_err(SqlError::Storage)?;
    if let Some((_, v)) = stored
        .iter()
        .find(|(k, _)| k.as_slice() == meta_key.as_slice())
    {
        if v.len() == 8 {
            let current = i64::from_be_bytes(v.as_slice().try_into().unwrap_or([0u8; 8]));
            let next = current.saturating_add(1);
            txn.put(&meta_key, &next.to_be_bytes())
                .map_err(SqlError::Storage)?;
            return Ok(current);
        }
    }
    let table_prefix = format!("{table}:").into_bytes();
    let mut end_prefix = table_prefix.clone();
    end_prefix.push(0xff);
    let mut max_existing: i64 = 0;
    for (k, _) in txn
        .scan(Some(table_prefix.as_slice()), Some(end_prefix.as_slice()))
        .map_err(SqlError::Storage)?
    {
        if let Some(suffix) = k.strip_prefix(table_prefix.as_slice()) {
            if let Ok(s) = std::str::from_utf8(suffix) {
                if let Ok(n) = s.parse::<i64>() {
                    if n > max_existing {
                        max_existing = n;
                    }
                }
            }
        }
    }
    let next = max_existing.saturating_add(1);
    let persist = next.saturating_add(1);
    txn.put(&meta_key, &persist.to_be_bytes())
        .map_err(SqlError::Storage)?;
    Ok(next)
}

/// Allocates row IDs from the transaction-local sequence after the first
/// lookup. The metadata write remains part of the same transaction, so abort
/// and recovery semantics are unchanged.
fn next_cached_rowid<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    table: &str,
    next: &mut Option<i64>,
) -> SqlResult<i64> {
    let row_id = match *next {
        Some(row_id) => row_id,
        None => {
            // Prefer the engine's atomic allocator (concurrent engines):
            // independent INSERTs on the same table no longer serialize on a
            // meta-key read/modify/write, and the returned id is unique
            // across ALL transactions. The meta key is still persisted below
            // (by `persist_next_rowid`) for compatibility with the
            // single-writer path, but no longer feeds the concurrent
            // allocator (the seed derives from committed rows, so a
            // transactional bump never re-arms a concurrent transaction).
            // Engines without an implementation (single-writer/test engines)
            // return the sentinel `Unsupported` error and keep the meta-key
            // protocol below.
            if let Ok(allocated) = txn.allocate_rowid(table) {
                if allocated > 0 {
                    *next = Some(allocated.saturating_add(1));
                    return Ok(allocated);
                }
            }
            let row_id = next_rowid_for_table(txn, table)?;
            *next = Some(row_id.saturating_add(1));
            return Ok(row_id);
        }
    };
    let next_id = row_id.saturating_add(1);
    *next = Some(next_id);
    Ok(row_id)
}

/// Computes the reservation keys a proposed row contributes: the canonical
/// normalized index-value bytes (`index_value_bytes`, the exact encoding the
/// B+Tree index entries use) per conflict domain, skipping NULLs (SQL NULL
/// never conflicts).
fn reservation_keys_for_row(
    targets: &[crate::index::ReservationTarget],
    row: &[Value],
    schema: &TableSchema,
) -> SqlResult<Vec<(String, Vec<u8>)>> {
    let mut keys = Vec::with_capacity(targets.len());
    for target in targets {
        if let Some(key) = crate::index::reservation_key_for_target(target, row, schema)? {
            keys.push(key);
        }
    }
    Ok(keys)
}

/// The single-row INSERT path for reservation-eligible tables: NO table-wide
/// write lane. Instead:
///
/// 1. the engine atomically allocates the internal row id (the meta-key RMW
///    that used to require the lane);
/// 2. the row's unique values reserve their `(index, value)` conflict
///    domains — same value in an active transaction blocks or fails a second
///    claimant; different values never interact (no table serialization);
/// 3. the durable-state probe (`stage_index_puts`'s index check) decides
///    duplicate-key AFTER the reservation is ours, so the probe observes the
///    winner of any race.
/// The reservations remain owned until commit/abort (released with the
/// transaction's write gates), so no later transaction can independently
/// conclude the value is free while this one is unresolved.
fn insert_one_row_reserved<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    targets: &[crate::index::ReservationTarget],
    row_values: &mut [Value],
    row_id: i64,
) -> SqlResult<(i64, Vec<u8>)> {
    let reservation_keys = reservation_keys_for_row(targets, row_values, schema)?;
    txn.lock_unique(&reservation_keys)
        .map_err(SqlError::Storage)?;
    let key = row_key(table, row_id);
    // Uniqueness: the per-value index probe inside `stage_index_puts` is the
    // durable-state authority. The row lock on our own row key is taken to
    // keep same-row coordination uniform with UPDATE/DELETE.
    txn.lock_rows(std::slice::from_ref(&key))
        .map_err(SqlError::Storage)?;
    enforce_foreign_keys_txn(txn, catalog, table, schema, row_values)?;
    let encoded = encode_row(row_values)?;
    txn.put(&key, &encoded).map_err(SqlError::Storage)?;
    stage_index_puts(txn, catalog, table, schema, row_values, &key, None)?;
    Ok((row_id, key))
}

/// The single DML access-path decision, shared by UPDATE, DELETE and the
/// statement-count preview: `WHERE column = literal` is serviced through the
/// existing authoritative single-column catalog index when one exists
/// (expression indexes are never used as plain-column indexes), otherwise
/// the caller falls back to its sequential scan. Returns the index-entry
/// prefix to scan.
fn indexed_dml_prefix(
    catalog: &InMemoryCatalog,
    table: &str,
    where_expr: Option<&Expression>,
) -> Option<Vec<u8>> {
    let Some(Expression::Equal(left, right)) = where_expr else {
        return None;
    };
    let (column, value) = match (&**left, &**right) {
        (Expression::ColumnRef(column), Expression::Literal(value)) => (unqualify(column), value),
        (Expression::Literal(value), Expression::ColumnRef(column)) => (unqualify(column), value),
        _ => return None,
    };
    let column = column.to_string();
    // Constraint backing indexes included: `WHERE pk = literal` and
    // `UPDATE ... WHERE pk = literal` must keep their index path.
    let indexes = catalog.all_indexes_for_table(table);
    // Exact single-column index: the encoded value plus its terminator is the
    // whole lookup key. The literal is spelled as the column stores values so
    // the prefix agrees with written entries (see `spell_like_column`).
    if let Some(index) = indexes.iter().find(|index| {
        index.expression.is_none() && crate::index::index_columns(index) == [column.clone()]
    }) {
        let spelled = crate::index::spell_like_column(catalog, table, &column, value);
        return Some(index_value_prefix(&index.name, Some(&spelled)));
    }
    // Otherwise a composite index whose *first* column is `column` still
    // serves a leading-component prefix scan. The terminator is deliberately
    // omitted: the scan must reach every tuple starting with this value, and
    // the caller rechecks the full predicate on each candidate.
    let index = indexes.into_iter().find(|index| {
        index.expression.is_none() && crate::index::index_columns(index).first() == Some(&column)
    })?;
    Some(crate::index::index_leading_prefix(
        &index.name,
        std::slice::from_ref(value),
    ))
}

/// Shared DML target discovery for transaction handles: resolves a
/// single-column-index equality predicate (`WHERE col = literal`) through the
/// existing authoritative catalog index instead of scanning the table.
/// Every discovered candidate row is returned with its current bytes; the
/// caller still applies the normal predicate recheck, MVCC/lock protocol,
/// and WAL-before-data write path.
#[allow(clippy::type_complexity)]
pub(crate) fn indexed_dml_entries<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    where_expr: Option<&Expression>,
) -> SqlResult<Option<Vec<(Vec<u8>, Vec<u8>)>>> {
    // Range-capable bounds first (equality, BETWEEN, </<=/>/>= on
    // order-preserving single-column indexes); the legacy equality-prefix
    // path below additionally serves composite-leading and text equality.
    if let Some((_, start, end)) =
        crate::query::index_bounds_for_predicate(catalog, table, where_expr)
    {
        let indexed_rows = txn.scan(Some(&start), Some(&end))?;
        if indexed_rows.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let keys: Vec<Vec<u8>> = indexed_rows
            .into_iter()
            .map(|(_, row_key)| row_key)
            .collect();
        let values = txn.get_many(&keys)?;
        let mut rows = Vec::with_capacity(keys.len());
        for (row_key, value) in keys.into_iter().zip(values) {
            if let Some(bytes) = value {
                rows.push((row_key, bytes));
            }
        }
        return Ok(Some(rows));
    }
    let Some(prefix) = indexed_dml_prefix(catalog, table, where_expr) else {
        return Ok(None);
    };
    let indexed_rows = txn.scan(Some(&prefix), Some(&prefix_end(&prefix)))?;
    let mut rows = Vec::with_capacity(indexed_rows.len());
    for (_, row_key) in indexed_rows {
        if let Some(bytes) = txn.get(&row_key)? {
            rows.push((row_key, bytes));
        }
    }
    Ok(Some(rows))
}

/// Same access path for callers holding a bare engine (committed-state
/// preview counting). Shares the one `indexed_dml_prefix` decision with the
/// transactional path.
#[allow(clippy::type_complexity)]
pub(crate) fn indexed_dml_entries_engine<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    table: &str,
    where_expr: Option<&Expression>,
) -> SqlResult<Option<Vec<(Vec<u8>, Vec<u8>)>>> {
    if let Some((_, start, end)) =
        crate::query::index_bounds_for_predicate(catalog, table, where_expr)
    {
        let indexed_rows = engine
            .scan(Some(&start), Some(&end))
            .map_err(SqlError::Storage)?;
        if indexed_rows.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let keys: Vec<Vec<u8>> = indexed_rows
            .into_iter()
            .map(|(_, row_key)| row_key)
            .collect();
        let values = engine.get_many(&keys).map_err(SqlError::Storage)?;
        let mut rows = Vec::with_capacity(keys.len());
        for (row_key, value) in keys.into_iter().zip(values) {
            if let Some(bytes) = value {
                rows.push((row_key, bytes));
            }
        }
        return Ok(Some(rows));
    }
    let Some(prefix) = indexed_dml_prefix(catalog, table, where_expr) else {
        return Ok(None);
    };
    let indexed_rows = engine
        .scan(Some(&prefix), Some(&prefix_end(&prefix)))
        .map_err(SqlError::Storage)?;
    let mut rows = Vec::with_capacity(indexed_rows.len());
    for (_, row_key) in indexed_rows {
        if let Some(bytes) = engine.get(&row_key).map_err(SqlError::Storage)? {
            rows.push((row_key, bytes));
        }
    }
    Ok(Some(rows))
}

#[allow(clippy::type_complexity)]
fn indexed_update_entries<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    where_expr: Option<&Expression>,
) -> SqlResult<Option<Vec<(Vec<u8>, Vec<u8>)>>> {
    indexed_dml_entries(txn, catalog, table, where_expr)
}

fn persist_next_rowid<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    table: &str,
    next: Option<i64>,
) -> SqlResult<()> {
    let Some(next) = next else {
        return Ok(());
    };
    let meta_key = format!("{ROWID_META_PREFIX}{table}").into_bytes();
    txn.put(&meta_key, &next.to_be_bytes())
        .map_err(SqlError::Storage)
}
/// Builds the statement-local uniqueness state for a multi-row statement.
///
/// Index-backed constraints are answered by the authoritative index itself:
/// every proposed row is checked against the index by `stage_index_puts` (an
/// exact index probe on the proposed value), and the transaction's read
/// overlay makes staged index entries visible to those probes, so a
/// multi-row statement cannot duplicate a value it inserted earlier in the
/// same statement. Building hash sets of every existing value here — by
/// scanning the index (or worse, the whole table for non-index-backed
/// constraints) — made every INSERT cost O(existing rows) even for a
/// single-row insert into a million-row table.
///
/// Returns sets only for the constrained columns that have NO backing index
/// (so a scan is still required to answer them), plus `None` covering sets —
/// the index probe path verifies the rest.
#[allow(clippy::type_complexity)]
fn load_unique_rows<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    rules: &[ColumnRule],
    schema: &TableSchema,
) -> SqlResult<(
    Option<Vec<HashSet<Vec<u8>>>>,
    Option<std::collections::BTreeSet<String>>,
)> {
    if !rules.iter().any(|rule| rule.unique || rule.primary_key) {
        return Ok((None, None));
    }
    // Columns whose unique/primary-key constraint has no backing unique
    // index. Only these need the scan-derived set; every index-backed column
    // is verified per-row by `stage_index_puts` (or `enforce_unique_values_txn`
    // when a rule mix leaves gaps).
    let uncovered: Vec<usize> = rules
        .iter()
        .enumerate()
        .filter(|(index, rule)| {
            (rule.unique || rule.primary_key)
                && schema.columns.get(*index).is_some_and(|column| {
                    crate::index::constraint_index_for_column(catalog, table, &column.name)
                        .is_none()
                })
        })
        .map(|(index, _)| index)
        .collect();
    if uncovered.is_empty() {
        // Every constraint is index-backed: the per-row index probe is the
        // authority, so no preloaded state is needed at all.
        return Ok((None, None));
    }
    let mut keys = (0..rules.len()).map(|_| HashSet::new()).collect::<Vec<_>>();
    let start = format!("{table}:").into_bytes();
    let end = format!("{table}:\u{10ffff}").into_bytes();
    let existing = txn
        .scan(Some(&start), Some(&end))?
        .into_iter()
        .map(|(_, bytes)| decode_row(&bytes).map_err(SqlError::Storage))
        .collect::<SqlResult<Vec<_>>>()?;
    for row in existing {
        for &index in &uncovered {
            if !matches!(row.get(index), Some(Value::Null)) {
                if let Some(value) = row.get(index) {
                    keys[index].insert(index_value_bytes(value));
                }
            }
        }
    }
    Ok((Some(keys), None))
}

fn check_unique_rows(
    keys: &mut [HashSet<Vec<u8>>],
    schema: &TableSchema,
    rules: &[ColumnRule],
    row: &[Value],
) -> SqlResult<()> {
    for (index, rule) in rules.iter().enumerate() {
        if !(rule.unique || rule.primary_key) {
            continue;
        }
        let Some(value) = row.get(index) else {
            continue;
        };
        if matches!(value, Value::Null) {
            continue;
        }
        if !keys[index].insert(index_value_bytes(value)) {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                format!(
                    "duplicate key value violates unique constraint on \"{}\"",
                    schema.columns[index].name
                ),
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_one_row_txn<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    rules: &[ColumnRule],
    row_values: &mut [Value],
    row_id: Option<i64>,
    unique_rows: Option<&mut UniqueRows>,
) -> SqlResult<(i64, Vec<u8>)> {
    let row_id = row_id.unwrap_or(next_rowid_for_table(txn, table)?);
    let key = row_key(table, row_id);
    let unique_checked = match unique_rows {
        Some(rows) => {
            check_unique_rows(&mut rows.keys, schema, rules, row_values)?;
            rows.covering.clone()
        }
        None => {
            if rules.iter().any(|rule| rule.unique || rule.primary_key) {
                enforce_unique_values_txn(txn, catalog, table, schema, rules, row_values, None)?;
            }
            None
        }
    };
    enforce_foreign_keys_txn(txn, catalog, table, schema, row_values)?;
    let encoded = encode_row(row_values)?;
    txn.put(&key, &encoded).map_err(SqlError::Storage)?;
    stage_index_puts(
        txn,
        catalog,
        table,
        schema,
        row_values,
        &key,
        unique_checked.as_ref(),
    )?;
    Ok((row_id, key))
}

/// Statement-local uniqueness state for a multi-row statement: the value sets
/// built once, plus the indexes whose uniqueness those sets already answer.
struct UniqueRows {
    keys: Vec<HashSet<Vec<u8>>>,
    covering: Option<std::collections::BTreeSet<String>>,
}

/// Per-row result of `INSERT ... ON CONFLICT`.
enum UpsertOutcome {
    /// A new row was inserted. Carries the final (coerced) row for RETURNING.
    Inserted(Vec<Value>),
    /// An existing conflicting row was updated in place. Carries the
    /// post-update row (PostgreSQL exposes the new tuple to RETURNING).
    Updated(Vec<Value>),
    /// A conflict occurred and the action was `DO NOTHING` (or a `WHERE` guard
    /// rejected the target update). No insert/update happened.
    NotHing,
}

/// Resolve the arbiter column indices for an `ON CONFLICT` target.
///
/// An explicit `(col, ...)` list maps each name to its schema index; an omitted
/// target (`ON CONFLICT DO ...`) falls back to every unique/primary-key column,
/// mirroring PostgreSQL's inferred-arbiter inference. Returns `None` when no
/// uniqueness constraint is available (nothing can conflict).
fn conflict_target_indices(
    schema: &TableSchema,
    rules: &[ColumnRule],
    target: &OnConflictTarget,
) -> Option<Vec<usize>> {
    let indices: Vec<usize> = match target {
        OnConflictTarget::Columns(cols) => cols
            .iter()
            .filter_map(|c| schema.column_index(unqualify(c)).ok())
            .collect(),
        OnConflictTarget::NoTarget => rules
            .iter()
            .enumerate()
            .filter(|(_, r)| r.primary_key || r.unique)
            .map(|(i, _)| i)
            .collect(),
    };
    if indices.is_empty() {
        None
    } else {
        Some(indices)
    }
}

/// Scan the table for a row whose arbiter columns equal the proposed row's,
/// returning its storage key and decoded value. NULL arbiter values never
/// conflict (PostgreSQL NULL-not-equal semantics).
fn find_conflicting_row<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    table: &str,
    target_indices: &[usize],
    proposed: &[Value],
) -> SqlResult<Option<(Vec<u8>, Vec<Value>)>> {
    for (key, bytes) in txn
        .scan(
            Some(format!("{table}:").as_bytes()),
            Some(format!("{table}:\u{10FFFF}").as_bytes()),
        )
        .map_err(SqlError::Storage)?
    {
        let row = decode_row(&bytes)?;
        let conflict = target_indices.iter().all(|&idx| {
            let p = &proposed[idx];
            !p.is_null() && values_equal(&row[idx], p)
        });
        if conflict {
            return Ok(Some((key, row)));
        }
    }
    Ok(None)
}

/// The unique index that authoritatively answers an `ON CONFLICT` arbiter.
///
/// Only an arbiter that is exactly one plain, single-column, index-backed
/// `UNIQUE`/`PRIMARY KEY` column is supported: that index's key *is* the
/// arbiter's conflict domain, so a lookup on it is equivalent to the scan. Any
/// other shape (composite arbiter, expression arbiter, an arbiter with no
/// backing index, or several inferred arbiters) returns `None`, and the caller
/// keeps the conservative full-table scan.
fn conflict_arbiter_index(
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    rules: &[ColumnRule],
    target: &OnConflictTarget,
) -> Option<(IndexDefinition, usize)> {
    let indices = conflict_target_indices(schema, rules, target)?;
    if indices.len() != 1 {
        return None;
    }
    let position = indices[0];
    let column = schema.columns.get(position)?.name.clone();
    let index = crate::index::constraint_index_for_column(catalog, table, &column)?;
    Some((index, position))
}

/// Finds the row conflicting on an arbiter through the arbiter's unique index
/// instead of a full table scan.
///
/// The index is the authoritative conflict domain: uniqueness is enforced on
/// exactly these encoded keys, so a probe over the same encoding makes the
/// arbiter decision O(log n) instead of O(n). NULL never conflicts.
fn find_conflicting_row_indexed<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    index: &IndexDefinition,
    position: usize,
    proposed: &[Value],
) -> SqlResult<Option<(Vec<u8>, Vec<Value>)>> {
    let Some(value) = proposed.get(position) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let prefix = index_value_prefix(&index.name, Some(value));
    for (_, row_key) in txn.scan(Some(&prefix), Some(&prefix_end(&prefix)))? {
        if let Some(bytes) = txn.get(&row_key).map_err(SqlError::Storage)? {
            return Ok(Some((row_key, decode_row(&bytes)?)));
        }
    }
    Ok(None)
}

/// Finds a row that conflicts with `proposed` on *any* unique conflict domain
/// of `table`.
///
/// `ON CONFLICT DO NOTHING` without an explicit target infers "any unique
/// violation is handled". Probing each domain independently is required: the
/// all-arbiter-columns-match scan only detects a conflict when *every* unique
/// column matches, so a conflict on one of several unique indexes slipped
/// through and the insert then reached the index probe, which raised the very
/// error `DO NOTHING` exists to suppress.
fn find_any_conflicting_row<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    proposed: &[Value],
) -> SqlResult<Option<(Vec<u8>, Vec<Value>)>> {
    for index in catalog.all_indexes_for_table(table) {
        if !index.unique {
            continue;
        }
        let values = crate::index::index_values_for_row(&index, proposed, schema)?;
        if !crate::index::tuple_conflicts_under_unique(&values) {
            continue;
        }
        // The tuple prefix is exactly the key uniqueness is enforced on, so any
        // entry under it is a conflict for an inserting row.
        let prefix = crate::index::index_tuple_prefix(&index.name, &values);
        for (_, row_key) in txn.scan(Some(&prefix), Some(&prefix_end(&prefix)))? {
            if let Some(bytes) = txn.get(&row_key).map_err(SqlError::Storage)? {
                return Ok(Some((row_key, decode_row(&bytes)?)));
            }
        }
    }
    Ok(None)
}

/// Rewrite `expr`, replacing every `EXCLUDED.<col>` reference with the literal
/// value taken from the proposed (excluded) row. Bare column references are
/// left untouched so the ordinary evaluator resolves them against the existing
/// row, giving PostgreSQL's ON CONFLICT UPDATE scoping. Subqueries are copied
/// without descending (their columns belong to a nested scope).
fn substitute_excluded(
    expr: &Expression,
    excluded_row: &[Value],
    schema: &TableSchema,
) -> SqlResult<Expression> {
    macro_rules! sub2 {
        ($V:ident, $a:expr, $b:expr) => {
            Expression::$V(
                Box::new(substitute_excluded(&**$a, excluded_row, schema)?),
                Box::new(substitute_excluded(&**$b, excluded_row, schema)?),
            )
        };
    }
    macro_rules! sub1 {
        ($V:ident, $a:expr) => {
            Expression::$V(Box::new(substitute_excluded(&**$a, excluded_row, schema)?))
        };
    }
    match expr {
        Expression::ColumnRef(name) => {
            if name
                .rsplit_once('.')
                .is_some_and(|(q, _)| q.eq_ignore_ascii_case("excluded"))
            {
                let idx = schema.column_index(unqualify(name))?;
                Ok(Expression::Literal(excluded_row[idx].clone()))
            } else {
                Ok(Expression::ColumnRef(name.clone()))
            }
        }
        Expression::Literal(v) => Ok(Expression::Literal(v.clone())),
        Expression::Star => Ok(Expression::Star),
        Expression::Equal(a, b) => Ok(sub2!(Equal, a, b)),
        Expression::NotEqual(a, b) => Ok(sub2!(NotEqual, a, b)),
        Expression::Less(a, b) => Ok(sub2!(Less, a, b)),
        Expression::LessOrEqual(a, b) => Ok(sub2!(LessOrEqual, a, b)),
        Expression::Greater(a, b) => Ok(sub2!(Greater, a, b)),
        Expression::GreaterOrEqual(a, b) => Ok(sub2!(GreaterOrEqual, a, b)),
        Expression::And(a, b) => Ok(sub2!(And, a, b)),
        Expression::Or(a, b) => Ok(sub2!(Or, a, b)),
        Expression::Add(a, b) => Ok(sub2!(Add, a, b)),
        Expression::Subtract(a, b) => Ok(sub2!(Subtract, a, b)),
        Expression::Multiply(a, b) => Ok(sub2!(Multiply, a, b)),
        Expression::Divide(a, b) => Ok(sub2!(Divide, a, b)),
        Expression::Modulo(a, b) => Ok(sub2!(Modulo, a, b)),
        Expression::Concat(a, b) => Ok(sub2!(Concat, a, b)),
        Expression::Power(a, b) => Ok(sub2!(Power, a, b)),
        Expression::BitAnd(a, b) => Ok(sub2!(BitAnd, a, b)),
        Expression::BitOr(a, b) => Ok(sub2!(BitOr, a, b)),
        Expression::BitXor(a, b) => Ok(sub2!(BitXor, a, b)),
        Expression::ShiftLeft(a, b) => Ok(sub2!(ShiftLeft, a, b)),
        Expression::ShiftRight(a, b) => Ok(sub2!(ShiftRight, a, b)),
        Expression::IsDistinctFrom(a, b) => Ok(sub2!(IsDistinctFrom, a, b)),
        Expression::NullIf(a, b) => Ok(sub2!(NullIf, a, b)),
        Expression::IsNull(inner) => Ok(sub1!(IsNull, inner)),
        Expression::IsNotNull(inner) => Ok(sub1!(IsNotNull, inner)),
        Expression::Not(inner) => Ok(sub1!(Not, inner)),
        Expression::Negate(inner) => Ok(sub1!(Negate, inner)),
        Expression::In {
            expr,
            list,
            subquery,
            negated,
        } => Ok(Expression::In {
            expr: Box::new(substitute_excluded(expr, excluded_row, schema)?),
            list: list
                .iter()
                .map(|e| substitute_excluded(e, excluded_row, schema))
                .collect::<SqlResult<Vec<_>>>()?,
            subquery: subquery.as_ref().map(|s| (**s).clone()).map(Box::new),
            negated: *negated,
        }),
        Expression::Between {
            expr,
            low,
            high,
            negated,
        } => Ok(Expression::Between {
            expr: Box::new(substitute_excluded(expr, excluded_row, schema)?),
            low: Box::new(substitute_excluded(low, excluded_row, schema)?),
            high: Box::new(substitute_excluded(high, excluded_row, schema)?),
            negated: *negated,
        }),
        Expression::Like {
            expr,
            pattern,
            escape,
            negated,
        } => Ok(Expression::Like {
            expr: Box::new(substitute_excluded(expr, excluded_row, schema)?),
            pattern: Box::new(substitute_excluded(pattern, excluded_row, schema)?),
            escape: *escape,
            negated: *negated,
        }),
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            let operand = match operand.as_deref() {
                Some(o) => Some(Box::new(substitute_excluded(o, excluded_row, schema)?)),
                None => None,
            };
            let mut new_whens = Vec::with_capacity(whens.len());
            for (c, v) in whens {
                new_whens.push((
                    substitute_excluded(c, excluded_row, schema)?,
                    substitute_excluded(v, excluded_row, schema)?,
                ));
            }
            let default = match default.as_deref() {
                Some(d) => Some(Box::new(substitute_excluded(d, excluded_row, schema)?)),
                None => None,
            };
            Ok(Expression::Case {
                operand,
                whens: new_whens,
                default,
            })
        }
        Expression::Coalesce(args) => Ok(Expression::Coalesce(
            args.iter()
                .map(|a| substitute_excluded(a, excluded_row, schema))
                .collect::<SqlResult<Vec<_>>>()?,
        )),
        Expression::Exists(stmt) => Ok(Expression::Exists(Box::new((**stmt).clone()))),
        Expression::ScalarSubquery(stmt) => {
            Ok(Expression::ScalarSubquery(Box::new((**stmt).clone())))
        }
        Expression::IsJson {
            expr,
            kind,
            negated,
        } => Ok(Expression::IsJson {
            expr: Box::new(substitute_excluded(expr, excluded_row, schema)?),
            kind: *kind,
            negated: *negated,
        }),
        Expression::IsBoolean {
            expr,
            kind,
            negated,
        } => Ok(Expression::IsBoolean {
            expr: Box::new(substitute_excluded(expr, excluded_row, schema)?),
            kind: *kind,
            negated: *negated,
        }),
        Expression::QuantifiedComparison {
            left,
            operator,
            quantifier,
            subquery,
        } => Ok(Expression::QuantifiedComparison {
            left: Box::new(substitute_excluded(left, excluded_row, schema)?),
            operator: *operator,
            quantifier: *quantifier,
            subquery: subquery.clone(),
        }),
        Expression::WindowFunction { name, args, over } => Ok(Expression::WindowFunction {
            name: name.clone(),
            args: args
                .iter()
                .map(|a| substitute_excluded(a, excluded_row, schema))
                .collect::<SqlResult<Vec<_>>>()?,
            over: over.clone(),
        }),
        Expression::Cast { expr, type_name } => Ok(Expression::Cast {
            expr: Box::new(substitute_excluded(expr, excluded_row, schema)?),
            type_name: type_name.clone(),
        }),
        Expression::Extract { field, expr } => Ok(Expression::Extract {
            field: field.clone(),
            expr: Box::new(substitute_excluded(expr, excluded_row, schema)?),
        }),
        Expression::RowField { expr, field } => Ok(Expression::RowField {
            expr: Box::new(substitute_excluded(expr, excluded_row, schema)?),
            field: field.clone(),
        }),
        Expression::TypeCast { expr, type_name } => Ok(Expression::TypeCast {
            expr: Box::new(substitute_excluded(expr, excluded_row, schema)?),
            type_name: type_name.clone(),
        }),
        Expression::FunctionCall {
            name,
            args,
            distinct,
            filter,
            order_by,
            returning,
            null_handling,
            unique_keys,
        } => {
            let filter = match filter.as_deref() {
                Some(f) => Some(Box::new(substitute_excluded(f, excluded_row, schema)?)),
                None => None,
            };
            Ok(Expression::FunctionCall {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|a| substitute_excluded(a, excluded_row, schema))
                    .collect::<SqlResult<Vec<_>>>()?,
                distinct: *distinct,
                filter,
                order_by: order_by.clone(),
                returning: returning.clone(),
                null_handling: *null_handling,
                unique_keys: *unique_keys,
            })
        }
        Expression::JsonArrow {
            left,
            right,
            as_text,
        } => Ok(Expression::JsonArrow {
            left: Box::new(substitute_excluded(left, excluded_row, schema)?),
            right: Box::new(substitute_excluded(right, excluded_row, schema)?),
            as_text: *as_text,
        }),
        Expression::ArrayIndex { array, index } => Ok(Expression::ArrayIndex {
            array: Box::new(substitute_excluded(array, excluded_row, schema)?),
            index: Box::new(substitute_excluded(index, excluded_row, schema)?),
        }),
        Expression::JsonSubscript { array, index } => Ok(Expression::JsonSubscript {
            array: Box::new(substitute_excluded(array, excluded_row, schema)?),
            index: Box::new(substitute_excluded(index, excluded_row, schema)?),
        }),
        Expression::DateLiteral(s) => Ok(Expression::DateLiteral(s.clone())),
        Expression::TimestampLiteral(s) => Ok(Expression::TimestampLiteral(s.clone())),
        Expression::TimestamptzLiteral(s) => Ok(Expression::TimestamptzLiteral(s.clone())),
        Expression::TimeLiteral(s) => Ok(Expression::TimeLiteral(s.clone())),
        Expression::TypedTimestampLiteral { text, precision } => {
            Ok(Expression::TypedTimestampLiteral {
                text: text.clone(),
                precision: *precision,
            })
        }
        Expression::TypedTimestamptzLiteral { text, precision } => {
            Ok(Expression::TypedTimestamptzLiteral {
                text: text.clone(),
                precision: *precision,
            })
        }
        Expression::TypedTimeLiteral { text, precision } => Ok(Expression::TypedTimeLiteral {
            text: text.clone(),
            precision: *precision,
        }),
    }
}

/// Execute a single `INSERT ... ON CONFLICT` row against an open transaction.
///
/// `row_values` is the fully resolved/coerced proposed row. When a conflict is
/// detected on the arbiter columns the action is applied (skip or update);
/// otherwise the row is inserted through the normal insertion path.
#[allow(clippy::too_many_arguments)]
fn upsert_one_row_txn<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    rules: &[ColumnRule],
    row_values: &mut [Value],
    on_conflict: &OnConflict,
    arbiter: Option<&(IndexDefinition, usize)>,
) -> SqlResult<UpsertOutcome> {
    let target = match on_conflict {
        OnConflict::DoNothing { target } | OnConflict::DoUpdate { target, .. } => target,
    };
    // A single-column index-backed arbiter is answered by its unique index
    // (O(log n)); every other arbiter shape keeps the conservative scan.
    let existing = match arbiter {
        Some((index, position)) => find_conflicting_row_indexed(txn, index, *position, row_values)?,
        None => match &target {
            // No explicit arbiter: any unique violation is a conflict.
            OnConflictTarget::NoTarget => {
                let found = find_any_conflicting_row(txn, catalog, table, schema, row_values)?;
                let index_backed = catalog
                    .all_indexes_for_table(table)
                    .iter()
                    .any(|index| index.unique);
                if found.is_some() || index_backed {
                    found
                } else {
                    // No unique index exists at all: uniqueness is scan-derived,
                    // so fall back to the per-column sets.
                    match conflict_target_indices(schema, rules, &target) {
                        Some(indices) => {
                            find_conflicting_row(txn, table, indices.as_slice(), row_values)?
                        }
                        None => None,
                    }
                }
            }
            OnConflictTarget::Columns(_) => match conflict_target_indices(schema, rules, &target) {
                Some(indices) => find_conflicting_row(txn, table, indices.as_slice(), row_values)?,
                None => None,
            },
        },
    };

    match on_conflict {
        OnConflict::DoNothing { .. } => {
            if existing.is_some() {
                return Ok(UpsertOutcome::NotHing);
            }
            let (_row_id, _key) =
                insert_one_row_txn(txn, catalog, table, schema, rules, row_values, None, None)?;
            Ok(UpsertOutcome::Inserted(row_values.to_vec()))
        }
        OnConflict::DoUpdate {
            assignments,
            where_expr,
            ..
        } => {
            if let Some((conflict_key, conflict_row)) = existing {
                if let Some(w) = where_expr {
                    if !evaluate_predicate(&conflict_row, schema, w)? {
                        return Ok(UpsertOutcome::NotHing);
                    }
                }
                let mut new_row = conflict_row.clone();
                for (col_expr, val_expr) in assignments {
                    let (col_name, subscripts) =
                        column_and_subscripts(col_expr).ok_or_else(|| {
                            SqlError::Storage(PlomidError::new(
                                ErrorKind::Syntax,
                                "invalid assignment target in ON CONFLICT DO UPDATE SET",
                            ))
                        })?;
                    let idx = schema.column_index(unqualify(col_name))?;
                    let substituted = substitute_excluded(val_expr, row_values, schema)?;
                    let rhs = evaluate_expression(&conflict_row, schema, &substituted)?;
                    let cell = if subscripts.is_empty() {
                        rhs
                    } else {
                        let indices = subscripts
                            .iter()
                            .map(|e| crate::query::evaluate_expression(&conflict_row, schema, e))
                            .collect::<SqlResult<Vec<_>>>()?;
                        apply_subscripted_assignment(&new_row[idx], &indices, &rhs)?
                    };
                    new_row[idx] = cell;
                    coerce_value_for_column(schema, idx, &mut new_row[idx])?;
                }
                crate::row::apply_generated_columns(schema, rules, &mut new_row)?;
                validate_updated_row(schema, rules, &new_row)?;
                enforce_unique_values_txn(
                    txn,
                    catalog,
                    table,
                    schema,
                    rules,
                    &new_row,
                    Some(&conflict_key),
                )?;
                let encoded = encode_row(&new_row)?;
                stage_index_update(
                    txn,
                    catalog,
                    table,
                    schema,
                    &conflict_row,
                    &new_row,
                    &conflict_key,
                )?;
                txn.put(&conflict_key, &encoded)
                    .map_err(SqlError::Storage)?;
                Ok(UpsertOutcome::Updated(new_row))
            } else {
                let (_row_id, _key) =
                    insert_one_row_txn(txn, catalog, table, schema, rules, row_values, None, None)?;
                Ok(UpsertOutcome::Inserted(row_values.to_vec()))
            }
        }
    }
}
pub fn execute_insert_rows_txn<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &mut InMemoryCatalog,
    table: String,
    columns: Option<Vec<String>>,
    source: InsertSource,
    returning: Option<Vec<SelectTarget>>,
    on_conflict: Option<plomid_sql::OnConflict>,
) -> SqlResult<QueryResult> {
    tracing::trace!(
        target: "sql::dml",
        "insert_rows_txn table={}",
        table
    );
    let ctx = prepare_insert_context(catalog, &table, columns)?;
    // The `ON CONFLICT` arbiter, resolved once per statement. `Some` means the
    // arbiter is a single index-backed unique column, so every row's conflict
    // decision is an index probe rather than a full-table scan.
    let upsert_arbiter = on_conflict.as_ref().and_then(|on_conflict| {
        let target = match on_conflict {
            OnConflict::DoNothing { target } | OnConflict::DoUpdate { target, .. } => target,
        };
        conflict_arbiter_index(catalog, &table, ctx.schema, &ctx.rules, target)
    });
    // Reservation path eligibility: every unique conflict domain is backed by
    // its own unique index (plain column, expression, or user-created), no FK,
    // no composite unique index, no SERIAL default, no ON CONFLICT. Everything
    // else keeps the conservative table-wide write lane (serialized
    // read/modify/write semantics).
    let supported_unique =
        crate::index::unique_reservation_targets(catalog, &table, ctx.schema, &ctx.rules);
    let has_serial = ctx
        .schema
        .columns
        .iter()
        .any(|column| column.col_type.serial);
    // Probe once whether the engine supports atomic row-id allocation: the
    // reservation path needs it (it replaces the meta-key RMW the table lane
    // used to serialize). Engines without it (single-writer/test engines)
    // keep the conservative table-lane path for every INSERT.
    let engine_allocates = txn.allocate_rowid(&table).map(|id| id > 0).unwrap_or(false);
    // `unique_reservation_targets` returns `None` for a composite unique index
    // (its domain is the whole tuple, which a single-column reservation cannot
    // represent), so such tables keep the table-wide lane.
    let reservation_mode = on_conflict.is_none()
        && !has_serial
        && engine_allocates
        && supported_unique.as_ref().is_some_and(|s| !s.is_empty());
    if reservation_mode {
        // Shared lane: concurrent with other reservation holders; excludes
        // only a conservative exclusive writer on the same table.
        txn.lock_shared(table.as_bytes())
            .map_err(SqlError::Storage)?;
    } else {
        txn.lock_for_write(table.as_bytes())
            .map_err(SqlError::Storage)?;
    }
    // `INSERT ... DEFAULT VALUES` provides no column list but must still let
    // every SERIAL column allocate: an empty explicit list is exactly that
    // statement shape (every cell omitted, defaults applied per column).
    let insert_columns = match &source {
        InsertSource::DefaultValues => Some(Vec::new()),
        _ => ctx.columns.clone(),
    };
    let rows = expand_insert_source(source, ctx.schema)?;
    let col_count = ctx.schema.columns.len();
    // Statement phase timers (see `update_stmt`): accumulators only, one
    // DEBUG event per statement, so bulk loads don't pay per-row log I/O.
    let perf = tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf");
    let mut resolve_us: u64 = 0;
    let mut insert_row_us: u64 = 0;

    let returning_targets = returning
        .as_deref()
        .map(|r| expand_returning_targets(ctx.schema, r, None));

    let mut returning_rows: Vec<Vec<Value>> = Vec::new();
    let mut returning_cols: Vec<String> = Vec::new();
    if let Some(ref targets) = returning_targets {
        returning_cols = targets
            .iter()
            .filter_map(|t| match t {
                SelectTarget::Expr { alias: Some(a), .. } => Some(a.clone()),
                SelectTarget::Function(n) => Some(n.clone()),
                SelectTarget::FunctionCall { name, .. } => Some(name.clone()),
                SelectTarget::Aliased { alias, .. } => Some(alias.clone()),
                _ => None,
            })
            .collect();
    }

    let mut count = 0u64;
    let mut next_row_id = None;
    // Unique preload sets are only needed on the conservative path (they
    // answer non-index-backed constraints and multi-row statement-local
    // checks); the reservation path is fully index-probe based.
    let mut unique_rows = if reservation_mode {
        None
    } else {
        let (unique_keys, unique_covering) =
            load_unique_rows(txn, catalog, &table, &ctx.rules, ctx.schema)?;
        unique_keys.map(|keys| UniqueRows {
            keys,
            covering: unique_covering,
        })
    };
    for (i, flat_row) in rows.into_iter().enumerate() {
        let row_i = i + 1;
        let phase_started = std::time::Instant::now();
        let (columns, flat_row) = apply_serial_defaults_txn(
            txn,
            &table,
            ctx.schema,
            insert_columns.as_deref(),
            flat_row,
        )?;
        let mut row_values = resolve_insert_values(ctx.schema, &ctx.rules, columns, flat_row)
            .map_err(|e| annotate_insert_error(e, row_i))?;
        resolve_us += phase_started.elapsed().as_micros() as u64;
        let phase_started = std::time::Instant::now();
        if row_values.len() != col_count {
            return Err(annotate_insert_error(
                SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    format!(
                        "INSERT has more expressions than target columns: found {} values for {} columns",
                        row_values.len(),
                        col_count
                    ),
                )),
                row_i,
            ));
        }
        let result_row = if let Some(ref oc) = on_conflict {
            let outcome = upsert_one_row_txn(
                txn,
                catalog,
                &table,
                ctx.schema,
                &ctx.rules,
                &mut row_values,
                oc,
                upsert_arbiter.as_ref(),
            )
            .map_err(|e| annotate_insert_error(e, row_i))?;
            insert_row_us += phase_started.elapsed().as_micros() as u64;
            match outcome {
                UpsertOutcome::Inserted(row) | UpsertOutcome::Updated(row) => {
                    count += 1;
                    Some(row)
                }
                UpsertOutcome::NotHing => None,
            }
        } else if reservation_mode {
            let supported = supported_unique
                .as_deref()
                .expect("reservation mode implies supported constraints");
            let row_id = txn.allocate_rowid(&table).map_err(SqlError::Storage)?;
            if row_id <= 0 {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Internal,
                    "concurrent engine returned no row id for reservation-mode INSERT",
                )));
            }
            // Statement-local duplicate detection for multi-row statements:
            // our own earlier staged values are visible through the read
            // overlay, so the index probe below still errors on a same-
            // statement duplicate. Reservations are per (index, value), so a
            // multi-row statement with two identical values re-enters its own
            // reservation (filtered as already-owned) and the second probe
            // sees its own staged index entry — the duplicate is rejected.
            let inserted = insert_one_row_reserved(
                txn,
                catalog,
                &table,
                ctx.schema,
                supported,
                &mut row_values,
                row_id,
            )
            .map_err(|e| annotate_insert_error(e, row_i))?;
            count += 1;
            let _ = inserted;
            insert_row_us += phase_started.elapsed().as_micros() as u64;
            next_row_id = next_row_id.max(Some(row_id.saturating_add(1)));
            Some(row_values.clone())
        } else {
            let row_id = next_cached_rowid(txn, &table, &mut next_row_id)?;
            insert_one_row_txn(
                txn,
                catalog,
                &table,
                ctx.schema,
                &ctx.rules,
                &mut row_values,
                Some(row_id),
                unique_rows.as_mut(),
            )
            .map_err(|e| annotate_insert_error(e, row_i))?;
            count += 1;
            insert_row_us += phase_started.elapsed().as_micros() as u64;
            Some(row_values.clone())
        };
        if let Some(ref targets) = returning_targets {
            if let Some(ref rv) = result_row {
                let mut out_row = Vec::with_capacity(targets.len());
                for t in targets {
                    let (_, v) = apply_select_target(rv, ctx.schema, t)
                        .map_err(|e| annotate_insert_error(e, row_i))?;
                    out_row.push(v);
                }
                returning_rows.push(out_row);
            }
        }
    }
    persist_next_rowid(txn, &table, next_row_id)?;

    if perf {
        tracing::debug!(
            target: "plomid::perf",
            event = "insert_stmt",
            table = table.as_str(),
            rows = count,
            resolve_us,
            insert_row_us,
        );
    }
    tracing::trace!(
        target: "sql::dml",
        "insert_rows_txn complete table={} row_count={}",
        table,
        count
    );

    if let Some(_targets) = returning_targets {
        let column_types = returning_rows
            .first()
            .map(|row| {
                row.iter()
                    .map(|v| {
                        plomid_sql::value_pg_type(v).map(|ty| {
                            plomid_sql::ColumnType::new(ty.oid(), plomid_types::NO_TYPEMOD)
                        })
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec![None; returning_cols.len()]);
        Ok(QueryResult::Rows {
            columns: returning_cols,
            column_types,
            rows: returning_rows,
        })
    } else {
        Ok(QueryResult::Inserted(count))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_insert_rows<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    table: String,
    columns: Option<Vec<String>>,
    source: InsertSource,
    returning: Option<Vec<SelectTarget>>,
    on_conflict: Option<plomid_sql::OnConflict>,
) -> SqlResult<QueryResult> {
    tracing::trace!(target: "sql::dml", "insert_rows table={}", table);
    // Single execution path: the autocommit statement runs as one
    // transaction through `execute_insert_rows_txn` (same gate/reservation
    // semantics as the explicit-transaction path — no duplicated INSERT
    // body). Statement failure aborts the transaction here, mirroring the
    // previous autocommit behavior.
    let mut txn = engine.begin().map_err(SqlError::Storage)?;
    let result = execute_insert_rows_txn(
        &mut txn,
        catalog,
        table,
        columns,
        source,
        returning,
        on_conflict,
    );
    match result {
        Ok(r) => {
            txn.commit().map_err(SqlError::Storage)?;
            Ok(r)
        }
        Err(e) => {
            let _ = txn.abort();
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_update_txn<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &mut InMemoryCatalog,
    table: String,
    // Optional target alias (`UPDATE products p SET ...`). Kept for AST
    // fidelity; the evaluator already resolves `alias.col` against the target
    // schema by unqualifying, so no separate alias table is needed here.
    alias: Option<String>,
    assignments: Vec<(Expression, Expression)>,
    // Optional materialized `UPDATE ... FROM` relation. `None` = plain UPDATE.
    from_source: Option<crate::update_from::UpdateFromSource>,
    where_expr: Option<plomid_sql::Expression>,
    // Optional `RETURNING` targets parsed from `UPDATE ... RETURNING ...`.
    // `None` preserves the legacy row-count path; `Some(targets)` projects
    // each post-update row (PostgreSQL exposes the *new* tuple to RETURNING).
    returning: Option<Vec<SelectTarget>>,
) -> SqlResult<QueryResult> {
    // Qualifiers that legally prefix a column reference to the target
    // relation. A reference qualified by anything else is an error rather
    // than a silent fallback to the target row (see `bind_from_refs`).
    let target_quals = crate::update_from::target_qualifiers(&table, alias.as_deref());
    // Measurement only: every timer below only observes the existing path.
    let perf = tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf");
    let txn_started = std::time::Instant::now();
    let mut phase_started = std::time::Instant::now();
    let schema = catalog.get_table(&table)?;
    let rules = catalog.column_rules(&table)?;
    // Assignments that touch a UNIQUE/PK column can create cross-row unique
    // conflicts that row locks cannot cover (two transactions setting two
    // different rows to the same unique value); per-key reservations cover
    // those below when every domain is reservable, and the conservative
    // table-wide write lane covers the rest. Every other UPDATE takes
    // row-level locks, so independent rows update concurrently.
    let unique_modifying = assignments.iter().any(|(column, _)| {
        column_and_subscripts(column).is_some_and(|(name, _)| {
            schema
                .column_index(&unqualify(&name))
                .is_ok_and(|index| schema.column_is_unique(index))
        })
    });
    // The unqualified names this statement assigns.
    let assigned_columns: Vec<String> = assignments
        .iter()
        .filter_map(|(column, _)| {
            column_and_subscripts(column).map(|(name, _)| unqualify(name).to_string())
        })
        .collect();
    // `unique_modifying` also covers domains the column rules cannot see: an
    // expression unique index, a unique index created directly by the user,
    // and a composite index whose participating column is assigned.
    let unique_modifying = unique_modifying
        || crate::index::assignment_moves_unique_domain(catalog, &table, &assigned_columns);
    // Reservation-mode eligibility for unique-modifying UPDATE: every unique
    // conflict domain on the table is coverable by its own single-column
    // reservation (plain column or expression). Anything else (composite
    // uniqueness, an uncovered constraint, FK) keeps the conservative
    // table-wide write lane. This used to require `from_source.is_none()`;
    // with indexed joined discovery the row set is just as deterministic as
    // a plain UPDATE's, so a FROM clause no longer forces the lane — the
    // per-key reservations give the same cross-row protection here that they
    // give plain unique-modifying UPDATE.
    let reservation_targets = if unique_modifying {
        crate::index::unique_reservation_targets(catalog, &table, schema, &rules)
    } else {
        None
    };
    let unique_reservation_mode = reservation_targets.is_some();
    // Row locks are safe when uniqueness coordination does not need the
    // table-wide lane (no unique assignment, or every domain is covered by a
    // reservation). Re-computed per discovery branch below.
    let uniqueness_allows_row_locks = !unique_modifying || unique_reservation_mode;
    let catalog_us = phase_started.elapsed().as_micros() as u64;
    phase_started = std::time::Instant::now();
    // UPDATE ... FROM target discovery. When the WHERE holds a provably safe
    // equality atom against an indexed target column, discover candidates by
    // probing that index once per source row (O(source · log target)) and take
    // row locks like plain UPDATE — independent rows stay concurrent and the
    // table-wide lane is not needed, because the same row-set determinism and
    // predicate recheck guarantees the nested loop provided hold here. The
    // full original WHERE is still evaluated per candidate below, so the
    // predicate remains authoritative regardless of the discovery path.
    // Without a safe atom (OR shapes, expression sources, cross-family keys,
    // no usable index, or uniqueness that needs the lane) the statement keeps
    // the conservative table-lane scan.
    // Per-candidate producing source-row indices from indexed joined
    // discovery (`joined_dml_entries`). Absent key or `None` value = every
    // source row participates in the per-row WHERE recheck (plain-UPDATE and
    // fallback paths) — which matches the historical all-rows pairing. An
    // empty `Some` set = no source row can match this row (skip it).
    let mut joined_provenance: Option<std::collections::HashMap<Vec<u8>, Option<Vec<usize>>>> =
        None;
    let (entries, indexed_update, use_row_locks) = if from_source.is_none() {
        if !uniqueness_allows_row_locks {
            txn.lock_for_write(table.as_bytes())
                .map_err(SqlError::Storage)?;
        }
        match indexed_update_entries(txn, catalog, &table, where_expr.as_ref())? {
            Some(entries) => (entries, true, uniqueness_allows_row_locks),
            None => (
                txn.scan(
                    Some(format!("{table}:").as_bytes()),
                    Some(format!("{table}:\u{10FFFF}").as_bytes()),
                )?,
                false,
                uniqueness_allows_row_locks,
            ),
        }
    } else if uniqueness_allows_row_locks {
        // `from_source` is `Some` here (the `is_none()` branch above did not
        // run); the `else if` chain shape keeps the three-way split readable.
        #[allow(clippy::unnecessary_unwrap)]
        let outcome = crate::update_from::indexed_join_candidates(
            txn,
            catalog,
            &table,
            schema,
            from_source.as_ref().expect("from_source checked above"),
            where_expr.as_ref(),
            &target_quals,
        )?;
        let joined = if outcome.index_used {
            joined_provenance = Some(
                outcome
                    .candidates
                    .iter()
                    .map(|c| (c.row_key.clone(), c.producing.clone()))
                    .collect(),
            );
            Some(
                outcome
                    .candidates
                    .into_iter()
                    .map(|c| (c.row_key, c.row_bytes))
                    .collect(),
            )
        } else {
            None
        };
        match joined {
            Some(entries) => (entries, true, true),
            None => (
                txn.scan(
                    Some(format!("{table}:").as_bytes()),
                    Some(format!("{table}:\u{10FFFF}").as_bytes()),
                )?,
                false,
                false,
            ),
        }
    } else {
        txn.lock_for_write(table.as_bytes())
            .map_err(SqlError::Storage)?;
        (
            txn.scan(
                Some(format!("{table}:").as_bytes()),
                Some(format!("{table}:\u{10FFFF}").as_bytes()),
            )?,
            false,
            false,
        )
    };
    // Row-level write locks: acquire the affected keys, then re-read each
    // row's current committed bytes. Another transaction may have committed a
    // change (or deleted the row) between our snapshot and our lock
    // acquisition; re-reading under the lock gives the statement
    // read-committed semantics (PostgreSQL EvalPlanQual) and prevents lost
    // updates. The per-row WHERE re-check in the update loop then decides
    // which fresh rows still qualify.
    let discover_us = phase_started.elapsed().as_micros() as u64;
    phase_started = std::time::Instant::now();
    let entries = if use_row_locks {
        let row_keys: Vec<Vec<u8>> = entries.iter().map(|(key, _)| key.clone()).collect();
        txn.lock_rows(&row_keys).map_err(SqlError::Storage)?;
        let lock_us = phase_started.elapsed().as_micros() as u64;
        phase_started = std::time::Instant::now();
        let mut current = Vec::with_capacity(entries.len());
        for (key, _bytes) in entries {
            if let Some(fresh) = txn.get(&key).map_err(SqlError::Storage)? {
                current.push((key, fresh));
            }
        }
        let reread_us = phase_started.elapsed().as_micros() as u64;
        if perf {
            tracing::debug!(
                target: "plomid::perf",
                event = "update_stmt",
                stage = "lock_and_reread",
                catalog_us,
                discover_us,
                lock_us,
                reread_us,
            );
        }
        current
    } else {
        if perf {
            tracing::debug!(
                target: "plomid::perf",
                event = "update_stmt",
                stage = "discover",
                catalog_us,
                discover_us,
            );
        }
        entries
    };
    if tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf") {
        tracing::debug!(
            target: "plomid::perf",
            event = "update_access_path",
            table = %table,
            indexed = indexed_update,
            rows_examined = entries.len(),
        );
    }
    // Expand `RETURNING *` once against the table schema so every updated row
    // projects the same column list; explicit lists pass through unchanged.
    let returning_targets = returning
        .as_deref()
        .map(|r| expand_returning_targets(schema, r, Some((table.as_str(), alias.as_deref()))));
    // Column names for the `Rows` result, derived from the expanded targets
    // (aliases preserved, mirroring the INSERT ... RETURNING path).
    let mut returning_cols: Vec<String> = Vec::new();
    if let Some(ref targets) = returning_targets {
        returning_cols = targets
            .iter()
            .filter_map(|t| match t {
                SelectTarget::Expr { alias: Some(a), .. } => Some(a.clone()),
                SelectTarget::Function(n) => Some(n.clone()),
                SelectTarget::FunctionCall { name, .. } => Some(name.clone()),
                SelectTarget::Aliased { alias, .. } => Some(alias.clone()),
                _ => None,
            })
            .collect();
    }
    // One projected output row per updated tuple, in scan order.
    let mut returning_rows: Vec<Vec<Value>> = Vec::new();
    let mut affected = 0;
    // With no FROM clause, a single empty binding row makes every target row
    // evaluated exactly once — identical to the plain UPDATE path.
    let empty_row: Vec<Value> = Vec::new();
    let from_rows: &[Vec<Value>] = match &from_source {
        Some(f) => &f.rows,
        None => std::slice::from_ref(&empty_row),
    };
    let mut decode_us = 0_u64;
    let mut eval_us = 0_u64;
    let mut index_us = 0_u64;
    let mut put_us = 0_u64;
    for (key, bytes) in entries {
        let decode_started = std::time::Instant::now();
        let old_row = decode_row(&bytes)?;
        if perf {
            decode_us += decode_started.elapsed().as_micros() as u64;
        }
        let mut row = old_row.clone();
        // Pair the target row with FROM rows. PostgreSQL applies at most one
        // update per target row even when multiple FROM rows match (which of
        // them is used is unspecified); we take the first matching FROM row
        // in materialized order. Indexed joined discovery narrows the pairing
        // to the source rows whose probe key could equal this row's indexed
        // column (`joined_provenance`) — the equality conjunct cannot hold
        // for any other source row, so removing those pairings is semantics-
        // preserving. `None` keeps the historical all-rows pairing.
        let producing = joined_provenance
            .as_ref()
            .and_then(|m| m.get(&key))
            .and_then(|o| o.as_ref());
        let paired: Vec<&Vec<Value>> = match producing {
            Some(idx) => idx.iter().map(|&i| &from_rows[i]).collect(),
            None => from_rows.iter().collect(),
        };
        for from_row in paired {
            let eval_started = std::time::Instant::now();
            let bound_where = match &where_expr {
                Some(e) => Some(crate::update_from::bind_from_refs(
                    e,
                    from_source.as_ref(),
                    &target_quals,
                    from_row,
                    schema,
                )?),
                None => None,
            };
            if let Some(expr) = &bound_where {
                if !evaluate_predicate(&row, schema, expr)? {
                    if perf {
                        eval_us += eval_started.elapsed().as_micros() as u64;
                    }
                    continue;
                }
            }
            let bound_assignments: Vec<(Expression, Expression)> = assignments
                .iter()
                .map(|(c, v)| {
                    Ok((
                        crate::update_from::bind_from_refs(
                            c,
                            from_source.as_ref(),
                            &target_quals,
                            from_row,
                            schema,
                        )?,
                        crate::update_from::bind_from_refs(
                            v,
                            from_source.as_ref(),
                            &target_quals,
                            from_row,
                            schema,
                        )?,
                    ))
                })
                .collect::<SqlResult<Vec<_>>>()?;
            for (column, value) in &bound_assignments {
                let (col_name, subscripts) = column_and_subscripts(column).ok_or_else(|| {
                    SqlError::Storage(PlomidError::new(
                        ErrorKind::Syntax,
                        "invalid assignment target in UPDATE SET",
                    ))
                })?;
                let index = schema.column_index(unqualify(col_name))?;
                let rhs = crate::query::evaluate_expression(&old_row, schema, value)?;
                let new_cell = if subscripts.is_empty() {
                    rhs
                } else {
                    let indices = subscripts
                        .iter()
                        .map(|e| crate::query::evaluate_expression(&old_row, schema, e))
                        .collect::<SqlResult<Vec<_>>>()?;
                    apply_subscripted_assignment(&old_row[index], &indices, &rhs)?
                };
                row[index] = new_cell;
                coerce_value_for_column(schema, index, &mut row[index])?;
            }
            // GENERATED ALWAYS AS (...) STORED columns recompute from the final
            // updated row (the generation expression may reference updated cells).
            crate::row::apply_generated_columns(schema, &rules, &mut row)?;
            validate_updated_row(schema, &rules, &row)?;
            if perf {
                eval_us += eval_started.elapsed().as_micros() as u64;
            }
            let index_started = std::time::Instant::now();
            // Reservation mode: acquire the NEW-value conflict domains BEFORE
            // the durable-state uniqueness probe so the probe is race-free
            // against active transactions without a table-wide lane. Only the
            // columns this statement actually changes are reserved, and only
            // when their value really changes (old-value release needs no
            // reservation; the row lock on the updated row covers it).
            if unique_reservation_mode {
                let targets = reservation_targets
                    .as_deref()
                    .expect("reservation mode implies reservation targets");
                let reservation_keys =
                    crate::index::changed_reservation_keys(targets, &old_row, &row, schema)?;
                txn.lock_unique(&reservation_keys)
                    .map_err(SqlError::Storage)?;
            }
            enforce_unique_values_txn(txn, catalog, &table, schema, &rules, &row, Some(&key))?;
            stage_index_update(txn, catalog, &table, schema, &old_row, &row, &key)?;
            if perf {
                index_us += index_started.elapsed().as_micros() as u64;
            }
            let put_started = std::time::Instant::now();
            let encoded = encode_row(&row)?;
            txn.put(&key, &encoded)?;
            if perf {
                put_us += put_started.elapsed().as_micros() as u64;
            }
            // Evaluate RETURNING against the post-update row (PostgreSQL semantics).
            if let Some(ref targets) = returning_targets {
                let bound_targets = crate::update_from::bind_returning_targets(
                    targets,
                    from_source.as_ref(),
                    &target_quals,
                    from_row,
                    schema,
                )?;
                let mut out_row = Vec::with_capacity(targets.len());
                for t in &bound_targets {
                    let (_, v) = apply_select_target(&row, schema, t)?;
                    out_row.push(v);
                }
                returning_rows.push(out_row);
            }
            affected += 1;
            // First matching FROM row wins: never update one target row twice.
            break;
        }
    }
    if perf {
        tracing::debug!(
            target: "plomid::perf",
            event = "update_loop",
            rows = affected,
            decode_us,
            eval_us,
            index_us,
            put_us,
            total_us = txn_started.elapsed().as_micros() as u64,
        );
    }
    // When RETURNING was requested, return the projected rows with per-column
    // type metadata (same shape as INSERT ... RETURNING); otherwise return the
    // classic `UPDATE <count>` row-count result.
    if returning_targets.is_some() {
        let column_types = returning_rows
            .first()
            .map(|row| {
                row.iter()
                    .map(|v| {
                        plomid_sql::value_pg_type(v).map(|ty| {
                            plomid_sql::ColumnType::new(ty.oid(), plomid_types::NO_TYPEMOD)
                        })
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec![None; returning_cols.len()]);
        return Ok(QueryResult::Rows {
            columns: returning_cols,
            column_types,
            rows: returning_rows,
        });
    }
    Ok(QueryResult::updated(affected))
}

#[allow(clippy::too_many_arguments)]
pub fn execute_update<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    table: String,
    // Optional target alias (`UPDATE products p SET ...` / `AS p`).
    alias: Option<String>,
    assignments: Vec<(Expression, Expression)>,
    // Optional `FROM <relation>` clause (`UPDATE ... FROM`). Materialized
    // through the shared SELECT/join machinery before the update loop runs.
    from: Option<plomid_sql::FromClause>,
    where_expr: Option<plomid_sql::Expression>,
    // Optional `RETURNING` targets from `UPDATE ... RETURNING ...`.
    // `None` keeps the legacy row-count path; `Some(targets)` projects each
    // post-update row (PostgreSQL exposes the *new* tuple to RETURNING).
    returning: Option<Vec<SelectTarget>>,
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    tracing::trace!(target: "sql::dml", "update table={}", table);
    // Materialize the FROM relation once (a source of rows only, never a
    // write target). When the WHERE holds a provably safe source-side equality
    // over an indexed source column, the source scan is pruned through the
    // source's own durable B+Tree before the shared SELECT/join executor is
    // invoked; every other shape takes the full materialization unchanged.
    let from_source = match &from {
        Some(clause) => Some(crate::update_from::materialize_from_pruned(
            engine,
            catalog,
            clause,
            where_expr.as_ref(),
            current_database,
            current_user,
        )?),
        None => None,
    };
    let mut txn = engine.begin()?;
    let result = execute_update_txn(
        &mut txn,
        catalog,
        table,
        alias,
        assignments,
        from_source,
        where_expr,
        returning,
    );
    match result {
        Ok(r) => {
            txn.commit()?;
            Ok(r)
        }
        Err(e) => {
            let _ = txn.abort();
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_delete_txn<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: String,
    // Optional target alias (`DELETE FROM t d`). Kept for AST fidelity and
    // `RETURNING d.*` expansion; the evaluator already resolves `alias.col`
    // against the target schema by unqualifying.
    alias: Option<String>,
    // Optional materialized `DELETE ... USING` relation(s). `None` = plain
    // DELETE. USING relations are a source of rows for WHERE/RETURNING
    // expression evaluation only — they are never themselves deleted.
    using: Option<crate::update_from::UpdateFromSource>,
    where_expr: Option<plomid_sql::Expression>,
    // Optional `RETURNING` targets parsed from `DELETE ... RETURNING ...`.
    // `None` preserves the legacy deleted-count path; `Some(targets)`
    // projects each deleted row (PostgreSQL exposes the *old* tuple to
    // RETURNING on DELETE, unlike UPDATE which exposes the new tuple).
    returning: Option<Vec<SelectTarget>>,
) -> SqlResult<QueryResult> {
    let schema = catalog.get_table(&table)?;
    // See `execute_update_txn`: qualifiers that legally prefix a target column
    // reference. Unknown qualifiers are rejected instead of silently binding
    // to the target row.
    let target_quals = crate::update_from::target_qualifiers(&table, alias.as_deref());
    // Target discovery: plain DELETE resolves a single-column-index equality
    // predicate through the shared authoritative index access path (the same
    // one UPDATE uses) instead of scanning the table. `DELETE ... USING` does
    // the same when its WHERE holds a provably safe equality atom against an
    // indexed target column (`joined_dml_entries`, one probe per materialized
    // source row); the full original WHERE is still re-checked per candidate
    // below, so the predicate remains authoritative. Without a usable atom —
    // OR shapes, expression sources, cross-family keys, no index — the
    // statement keeps the sequential scan + table lane.
    // Per-candidate producing source-row indices from indexed joined
    // discovery. Absent key or `None` value = every USING row participates in
    // the per-row WHERE recheck (plain-DELETE and fallback paths — historical
    // pairing). An empty `Some` set = no source row can match (skip it).
    let mut joined_provenance: Option<std::collections::HashMap<Vec<u8>, Option<Vec<usize>>>> =
        None;
    let (entries, indexed_delete) = if using.is_none() {
        match indexed_dml_entries(txn, catalog, &table, where_expr.as_ref())? {
            Some(entries) => (entries, true),
            None => (
                txn.scan(
                    Some(format!("{table}:").as_bytes()),
                    Some(format!("{table}:\u{10FFFF}").as_bytes()),
                )?,
                false,
            ),
        }
    } else {
        // `using` is `Some` here (the `is_none()` branch above did not run).
        #[allow(clippy::unnecessary_unwrap)]
        let outcome = crate::update_from::indexed_join_candidates(
            txn,
            catalog,
            &table,
            schema,
            using.as_ref().expect("using checked above"),
            where_expr.as_ref(),
            &target_quals,
        )?;
        if outcome.index_used {
            joined_provenance = Some(
                outcome
                    .candidates
                    .iter()
                    .map(|c| (c.row_key.clone(), c.producing.clone()))
                    .collect(),
            );
            (
                outcome
                    .candidates
                    .into_iter()
                    .map(|c| (c.row_key, c.row_bytes))
                    .collect(),
                true,
            )
        } else {
            (
                txn.scan(
                    Some(format!("{table}:").as_bytes()),
                    Some(format!("{table}:\u{10FFFF}").as_bytes()),
                )?,
                false,
            )
        }
    };
    if tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf") {
        tracing::debug!(
            target: "plomid::perf",
            event = "delete_access_path",
            table = %table,
            indexed = indexed_delete,
            rows_examined = entries.len(),
        );
    }
    // Row-level write locks: acquire the affected keys, then re-read each
    // row's current committed bytes — another transaction may have deleted
    // the row between our snapshot and our lock acquisition. This covers
    // plain DELETE *and* indexed `DELETE ... USING` (its row set is exactly
    // as deterministic as a plain DELETE's); only the conservative fallback
    // keeps the table-wide lane.
    let entries = if indexed_delete || using.is_none() {
        let row_keys: Vec<Vec<u8>> = entries.iter().map(|(key, _)| key.clone()).collect();
        txn.lock_rows(&row_keys).map_err(SqlError::Storage)?;
        let mut current = Vec::with_capacity(entries.len());
        for (key, _bytes) in entries {
            if let Some(fresh) = txn.get(&key).map_err(SqlError::Storage)? {
                current.push((key, fresh));
            }
        }
        current
    } else {
        txn.lock_for_write(table.as_bytes())
            .map_err(SqlError::Storage)?;
        entries
    };
    // Expand `RETURNING *` once against the table schema so every deleted row
    // projects the same column list; explicit lists pass through unchanged.
    let returning_targets = returning
        .as_deref()
        .map(|r| expand_returning_targets(schema, r, Some((table.as_str(), alias.as_deref()))));
    // Column names for the `Rows` result, derived from the expanded targets
    // (aliases preserved, mirroring the INSERT ... RETURNING path).
    let mut returning_cols: Vec<String> = Vec::new();
    if let Some(ref targets) = returning_targets {
        returning_cols = targets
            .iter()
            .filter_map(|t| match t {
                SelectTarget::Expr { alias: Some(a), .. } => Some(a.clone()),
                SelectTarget::Function(n) => Some(n.clone()),
                SelectTarget::FunctionCall { name, .. } => Some(name.clone()),
                SelectTarget::Aliased { alias, .. } => Some(alias.clone()),
                _ => None,
            })
            .collect();
    }
    // One projected output row per deleted tuple, in scan order.
    let mut returning_rows: Vec<Vec<Value>> = Vec::new();
    let mut affected = 0;
    // USING rows are read-only sources of values for WHERE/RETURNING. A
    // target row is deleted (and emitted by RETURNING) at most once even
    // when several USING rows match it — PostgreSQL deletes the row on the
    // first join match and skips the rest.
    let empty_row: Vec<Value> = Vec::new();
    let using_rows: &[Vec<Value>] = match &using {
        Some(src) => &src.rows,
        None => std::slice::from_ref(&empty_row),
    };
    for (key, bytes) in entries {
        let row = decode_row(&bytes)?;
        // A target row qualifies when the WHERE predicate holds for at least
        // one USING row; the predicate is bound to each source row's values
        // before evaluation (same resolution rules as `UPDATE FROM`). With no
        // WHERE clause every visible target row qualifies: `DELETE FROM t`
        // deletes all rows, and `DELETE ... USING` with no predicate
        // qualifies every row. (Treating a missing predicate as "no match"
        // made `DELETE FROM t` a silent no-op.)
        let mut matching_from_row: Option<&Vec<Value>> = None;
        // Indexed joined discovery narrows the pairing to the USING rows
        // whose probe key could equal this row's indexed column
        // (`joined_provenance`); the equality conjunct cannot hold for any
        // other source row. `None` keeps the historical all-rows pairing.
        let producing = joined_provenance
            .as_ref()
            .and_then(|m| m.get(&key))
            .and_then(|o| o.as_ref());
        let paired: Vec<&Vec<Value>> = match producing {
            Some(idx) => idx.iter().map(|&i| &using_rows[i]).collect(),
            None => using_rows.iter().collect(),
        };
        for from_row in paired {
            let qualifies = match &where_expr {
                Some(expr) => {
                    let bound = crate::update_from::bind_from_refs(
                        expr,
                        using.as_ref(),
                        &target_quals,
                        from_row,
                        schema,
                    )?;
                    evaluate_predicate(&row, schema, &bound)?
                }
                None => true,
            };
            if qualifies {
                matching_from_row = Some(from_row);
                break;
            }
        }
        if matching_from_row.is_none() {
            continue;
        }
        // Evaluate RETURNING against the pre-delete row (PostgreSQL exposes
        // the deleted tuple) before the row and its index entries vanish.
        if let Some(ref targets) = returning_targets {
            let bound_targets = crate::update_from::bind_returning_targets(
                targets,
                using.as_ref(),
                &target_quals,
                matching_from_row.unwrap(),
                schema,
            )?;
            let mut out_row = Vec::with_capacity(bound_targets.len());
            for t in &bound_targets {
                let (_, v) = apply_select_target(&row, schema, t)?;
                out_row.push(v);
            }
            returning_rows.push(out_row);
        }
        stage_index_deletes(txn, catalog, &table, schema, &row, &key)?;
        txn.delete(&key)?;
        affected += 1;
    }
    // When RETURNING was requested, return the projected rows with per-column
    // type metadata (same shape as INSERT ... RETURNING); otherwise return
    // the classic `DELETE <count>` row-count result.
    if returning_targets.is_some() {
        let column_types = returning_rows
            .first()
            .map(|row| {
                row.iter()
                    .map(|v| {
                        plomid_sql::value_pg_type(v).map(|ty| {
                            plomid_sql::ColumnType::new(ty.oid(), plomid_types::NO_TYPEMOD)
                        })
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec![None; returning_cols.len()]);
        return Ok(QueryResult::Rows {
            columns: returning_cols,
            column_types,
            rows: returning_rows,
        });
    }
    Ok(QueryResult::deleted(affected))
}

#[allow(clippy::too_many_arguments)]
pub fn execute_delete<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    table: String,
    // Optional target alias (`DELETE FROM t d` / `DELETE FROM t AS d`).
    alias: Option<String>,
    // Optional `DELETE ... USING <relation list>` source relations. Materialized
    // once through the shared SELECT/join machinery (a source of rows only,
    // never a write target), reusing the `UPDATE ... FROM` path.
    using: Option<plomid_sql::FromClause>,
    where_expr: Option<plomid_sql::Expression>,
    // Optional `RETURNING` targets from `DELETE ... RETURNING ...`.
    // `None` keeps the legacy deleted-count path; `Some(targets)` returns
    // the deleted rows as `Rows` (evaluated against the pre-delete row).
    returning: Option<Vec<SelectTarget>>,
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    tracing::trace!(target: "sql::dml", "delete table={}", table);
    // Materialize the USING relation(s) once before opening the write
    // transaction (a source of rows only, never a write target). Source-side
    // index pruning applies on the same terms as UPDATE ... FROM.
    let using_source = match &using {
        Some(clause) => Some(crate::update_from::materialize_from_pruned(
            engine,
            catalog,
            clause,
            where_expr.as_ref(),
            current_database,
            current_user,
        )?),
        None => None,
    };
    // One shared transactional path (same target discovery — including the
    // indexed equality fast path — locking, and index maintenance as the
    // explicit-transaction DELETE).
    let mut txn = engine.begin()?;
    let result = execute_delete_txn(
        &mut txn,
        catalog,
        table,
        alias,
        using_source,
        where_expr,
        returning,
    );
    match result {
        Ok(r) => {
            txn.commit()?;
            tracing::trace!(target: "sql::dml", "delete complete");
            Ok(r)
        }
        Err(e) => {
            let _ = txn.abort();
            Err(e)
        }
    }
}

/// Empties every named table of all rows (PostgreSQL `TRUNCATE` semantics —
/// a DDL-style equivalent of `DELETE` with no predicate). Row and index
/// entries are removed transactionally so repeated scans and lookups stay
/// consistent. Returns the standard `TRUNCATE TABLE` command tag.
pub fn execute_truncate<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    tables: Vec<String>,
) -> SqlResult<QueryResult> {
    let mut affected = 0u64;
    let mut txn = engine.begin()?;
    for table in tables {
        let schema = catalog.get_table(&table)?;
        let entries = txn.scan(
            Some(format!("{table}:").as_bytes()),
            Some(format!("{table}:\u{10FFFF}").as_bytes()),
        )?;
        for (key, value_bytes) in entries {
            let row = decode_row(&value_bytes)?;
            stage_index_deletes(&mut txn, catalog, &table, schema, &row, &key)?;
            txn.delete(&key)?;
            affected += 1;
        }
    }
    txn.commit()?;
    let _ = affected;
    Ok(QueryResult::Created("TRUNCATE TABLE".into()))
}
