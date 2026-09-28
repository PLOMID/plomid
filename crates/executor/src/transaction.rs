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
use plomid_sql::{Catalog, InMemoryCatalog, QueryResult, Statement};
use plomid_txn::{StorageEngine, StorageEngineTransaction};

use crate::dml::{execute_delete_txn, execute_insert_rows_txn, execute_update_txn};
use crate::error::{SqlError, SqlResult};
use crate::index::register_constraint_indexes;

/// Executes DDL statements inside an explicit transaction.
///
/// These modify the in-memory catalog but do NOT persist to storage
/// until after the transaction commits. This allows CREATE TABLE etc.
/// to be used inside BEGIN...COMMIT blocks.
fn execute_ddl_in_txn<'a, E: StorageEngineTransaction<'a>>(
    _txn: &mut E,
    catalog: &mut InMemoryCatalog,
    stmt: Statement,
) -> SqlResult<()> {
    match stmt {
        Statement::CreateTable {
            name,
            if_not_exists,
            temporary: _,
            columns,
            constraints,
        } => {
            // Resolve an unqualified table name against the session's
            // search_path so PostgreSQL's per-schema identity is honored.
            let name = catalog.resolve_create_name(&name)?;
            if if_not_exists && catalog.has_table(&name) {
                return Ok(());
            }
            // SERIAL / BIGSERIAL / SMALLSERIAL columns auto-create a backing
            // sequence named `<table>_<column>_seq`, matching PostgreSQL.
            // The sequence uses the bare relation name so `nextval('seq')`
            // resolves it the same way as before schema-qualification.
            let base_name = name.rsplit('.').next().unwrap_or(&name);
            for col in &columns {
                if col.col_type.serial {
                    let seq_name = format!("{base_name}_{}_seq", col.name);
                    if !catalog.has_sequence(&seq_name) {
                        catalog.create_sequence(&seq_name)?;
                    }
                }
            }
            catalog.create_table(name.clone(), columns, constraints)?;
            // A table created inside an explicit transaction is also empty, so
            // its constraint indexes need no backfill; inserts later in the
            // same transaction maintain them through `stage_index_puts`.
            register_constraint_indexes(catalog, &name)?;
            Ok(())
        }
        Statement::CreateView {
            name,
            columns,
            query,
            or_replace,
        } => {
            // Resolve the view name through the session search_path so that
            // unqualified names land in the correct schema.
            let name = catalog.resolve_create_name(&name)?;
            if or_replace && catalog.has_view(&name) {
                catalog.drop_view(&name)?;
            }
            catalog.create_view(name, columns, query)?;
            Ok(())
        }
        _ => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Unsupported,
                "statement inside explicit transaction not supported",
            )));
        }
    }
}

pub fn execute_transaction_group<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    stmts: Vec<Statement>,
) -> SqlResult<QueryResult> {
    execute_transaction_group_mode(engine, catalog, stmts)
}

fn execute_transaction_group_mode<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    stmts: Vec<Statement>,
) -> SqlResult<QueryResult> {
    let stmts = coalesce_insert_statements(stmts);
    tracing::debug!(
        target: "sql::txn",
        "transaction_start stmt_count={}",
        stmts.len()
    );
    let mut txn = engine.begin()?;
    let mut last = QueryResult::Created(String::new());
    let result = (|| -> SqlResult<()> {
        for stmt in stmts.into_iter().skip(1) {
            // Each statement in the transaction gets its own statement
            // execution context (PostgreSQL: CURRENT_TIMESTAMP is the
            // transaction/statement start time; statements in one txn still
            // each observe a stable timestamp within themselves).
            let _ctx = crate::context::StatementContext::enter_if_none();
            match stmt {
                Statement::Commit => {
                    tracing::debug!(target: "sql::txn", "commit");
                    txn.commit()?;
                    last = QueryResult::Committed;
                }
                Statement::Rollback => {
                    tracing::debug!(target: "sql::txn", "rollback");
                    txn.abort()?;
                    last = QueryResult::RolledBack;
                }
                Statement::Insert {
                    table,
                    columns,
                    source,
                    returning,
                    on_conflict,
                } => {
                    tracing::trace!(
                        target: "sql::txn",
                        "execute_dml kind=insert"
                    );
                    last = execute_insert_rows_txn(
                        &mut txn,
                        catalog,
                        table,
                        columns,
                        source,
                        returning,
                        on_conflict,
                    )?;
                }
                Statement::Copy { .. } => {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::Unsupported,
                        "COPY inside explicit transactions requires the PostgreSQL wire-level \
                     CopyData/CopyDone/CopyFail frame protocol",
                    )));
                }
                Statement::Update {
                    table,
                    alias,
                    from,
                    assignments,
                    where_expr,
                    returning,
                } => {
                    if from.is_some() {
                        // `UPDATE ... FROM` needs the shared SELECT/join
                        // machinery (engine access) to materialize its source
                        // relation; that is unavailable inside an explicit
                        // transaction (same limitation as INSERT ... SELECT).
                        return Err(SqlError::Storage(PlomidError::new(
                            ErrorKind::Unsupported,
                            "UPDATE ... FROM inside explicit transactions is not yet supported",
                        )));
                    }
                    // Forward `UPDATE ... RETURNING` targets into the transactional
                    // DML path. `returning: None` keeps the legacy row-count
                    // result; `Some(targets)` projects each post-update row
                    // (PostgreSQL exposes the *new* tuple to RETURNING).
                    last = execute_update_txn(
                        &mut txn,
                        catalog,
                        table,
                        alias.clone(),
                        assignments,
                        None,
                        where_expr,
                        returning,
                    )?;
                }
                Statement::Delete {
                    table,
                    alias,
                    using,
                    where_expr,
                    returning,
                } => {
                    // Materialize `DELETE ... USING` source relations. Plain
                    // table relations are read through the transaction's own
                    // scan API, so uncommitted changes from earlier statements
                    // in the same transaction are visible. Subquery/join/table
                    // function sources need the shared SELECT/join machinery
                    // (engine access) and report an unsupported error.
                    let using_source = match &using {
                        Some(clause) => Some(crate::update_from::materialize_using_in_txn(
                            &mut txn, catalog, clause,
                        )?),
                        None => None,
                    };
                    // Same contract as UPDATE: `returning: None` keeps the
                    // legacy deleted-count result; `Some(targets)` returns the
                    // deleted rows as `Rows`, evaluated against the pre-delete
                    // row (PostgreSQL exposes the deleted tuple).
                    last = execute_delete_txn(
                        &mut txn,
                        catalog,
                        table,
                        alias.clone(),
                        using_source,
                        where_expr,
                        returning,
                    )?;
                }
                _ => {
                    // DDL statements (CREATE TABLE, CREATE VIEW, etc.) modify the
                    // in-memory catalog. We execute them here to update the catalog
                    // state, and defer catalog persistence until after commit.
                    execute_ddl_in_txn(&mut txn, catalog, stmt)?;
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        // A failed statement must release the transaction's storage locks and
        // staging state so a later connection cannot be stranded.
        let _ = txn.abort();
        return Err(error);
    }
    tracing::debug!(
        target: "sql::txn",
        "transaction_end result={:?}",
        std::mem::discriminant(&last)
    );
    Ok(last)
}

/// Combines adjacent plain `INSERT ... VALUES` statements into the existing
/// multi-row INSERT path. Richer forms remain separate to preserve their
/// result and conflict semantics.
fn coalesce_insert_statements(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        let Statement::Insert {
            table,
            columns,
            source,
            returning,
            on_conflict,
        } = stmt
        else {
            out.push(stmt);
            continue;
        };
        let mut source = Some(source);
        let mut merged = false;
        if let Some(Statement::Insert {
            table: previous_table,
            columns: previous_columns,
            source: previous_source,
            returning: previous_returning,
            on_conflict: previous_conflict,
        }) = out.last_mut()
        {
            if *previous_table == table
                && *previous_columns == columns
                && previous_returning.is_none()
                && returning.is_none()
                && previous_conflict.is_none()
                && on_conflict.is_none()
            {
                if let (
                    plomid_sql::InsertSource::Values(previous_rows),
                    Some(plomid_sql::InsertSource::Values(rows)),
                ) = (previous_source, source.as_mut())
                {
                    previous_rows.append(rows);
                    merged = true;
                }
            }
        }
        if !merged {
            out.push(Statement::Insert {
                table,
                columns,
                source: source.take().expect("insert source present"),
                returning,
                on_conflict,
            });
        }
    }
    out
}
