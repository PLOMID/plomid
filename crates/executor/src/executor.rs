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
//! Executor that consumes SQL AST and executes it against a StorageEngine.
//!
//! For V1, the executor stores the engine by value. Explicit transactions
//! (BEGIN/COMMIT/ROLLBACK) must be contained within a single `execute` call.

use plomid_core::{DatabaseId, ErrorKind, PlomidError, TableIdentity};
use plomid_sql::{
    load_catalog, Catalog, ColumnType, FromClause, InMemoryCatalog, InsertSource, InsertValue,
    Lexer, Parser, QueryResult, Statement, Value,
};
use plomid_storage::DatabaseLayout;
use plomid_txn::StorageEngine;
use plomid_txn::{ConcurrentPlomidStorageEngine, PlomidStorageEngine};
use std::sync::{Arc, Mutex};

use crate::ddl::execute_ddl;
use crate::dml::{
    execute_delete, execute_insert_rows, execute_truncate, execute_update,
    indexed_dml_entries_engine,
};
use crate::encoding::decode_row;
use crate::error::{SqlError, SqlResult};
use crate::maintenance::{MaintenanceCoordinator, MaintenancePolicy};
use crate::query::evaluate_predicate;
use crate::query::execute_select;
use crate::transaction::execute_transaction_group;
use plomid_columnar::ColumnarFailPoint;

pub struct Executor<E: StorageEngine> {
    engine: E,
    catalog: InMemoryCatalog,
    current_database: String,
    current_user: String,
    database_names: Vec<String>,
    /// Filesystem layout of the engine's root. Database, schema, and table
    /// objects materialize their logical directories through it, while the
    /// catalog remains the single authority for name resolution.
    layout: DatabaseLayout,
    /// Committed-write accounting and the policy that decides when a table is
    /// due for a maintained generation. Ordinary writes only increment a
    /// counter here; the expensive materialization runs on a maintenance pass.
    maintenance: MaintenanceCoordinator,
    /// Background maintenance endpoint, installed by the server. When present,
    /// due tables are submitted to the worker instead of being maintained
    /// inline; a full queue (or stopped worker) falls back to inline passes,
    /// which is exactly the historical foreground behavior. Absent in tests
    /// and embedded use, where passes stay synchronous and deterministic.
    maintenance_link: Option<crate::maintenance_worker::WorkerLink>,
    /// Last automatic maintenance failure, kept for reporting without ever
    /// failing the committed statement that happened to trigger the pass.
    maintenance_error: Option<String>,
    /// Injection point for the maintenance pass's data-generation flush.
    ///
    /// Reuses the existing columnar fail-point infrastructure so a test can
    /// prove that an automatic maintenance failure never affects a
    /// already-committed DML or the previously published generation. Production
    /// code leaves this at [`ColumnarFailPoint::None`].
    maintenance_fail_at: ColumnarFailPoint,
    /// Derived runtime indexes: the in-memory ART for each index of the
    /// current database, keyed by the full logical path
    /// `(database, schema, table, index)` as raw identity counters.
    ///
    /// ART is never authoritative — the persistent B+Tree payload of an index's
    /// current generation is — so these entries are reconstructed from durable
    /// state on demand, and are never trusted across a state change. A
    /// failed reconstruction drops the entry instead of caching a partial tree.
    ///
    /// Construction is lazy: `Executor::new` does NOT build ART (a full index
    /// scan + rebuild per connection dominated ~80ms connection
    /// establishment while no production query path reads ART — it is only
    /// observed through `art_index`/`art_len`). The first explicit
    /// `rebuild_art_indexes` call builds it; publication-boundary refreshes
    /// are no-ops until then, and a later rebuild always derives from current
    /// durable state, so laziness cannot serve stale entries.
    art_indexes: std::collections::BTreeMap<(u64, u64, u64, u64), plomid_index::ArtIndex>,
    /// True once `art_indexes` has been explicitly built. Guards the lazy
    /// contract above: refreshes before the first build stay no-ops.
    art_built: bool,
    /// Why the most recent ART reconstruction failed, when one did.
    ///
    /// Recorded rather than returned so a recovery that cannot rebuild a
    /// runtime index stays observable: the failing index is absent from the
    /// cache instead of being exposed as valid.
    art_error: Option<String>,
    /// The single SQL statistics authority, loaded from durable records at
    /// session start and replaced atomically by ANALYZE.
    statistics: std::collections::BTreeMap<String, crate::statistics::TableStatistics>,
    columnar_store: plomid_columnar::ColumnarStore,
}

impl Executor<ConcurrentPlomidStorageEngine> {
    /// Creates an independent session executor over one shared authoritative
    /// database engine. Session/catalog/query state is not shared between
    /// connections; the concurrent storage facade owns the narrow write lock.
    pub fn new_shared(engine: Arc<Mutex<PlomidStorageEngine>>) -> SqlResult<Self> {
        let shared =
            ConcurrentPlomidStorageEngine::from_shared(engine).map_err(SqlError::Storage)?;
        Self::new(shared)
    }
}

impl<E: StorageEngine> Executor<E> {
    /// Materialize an INSERT ... SELECT source through the normal relational
    /// executor.  Keeping the conversion here means VALUES and SELECT
    /// inserts share the same constraint, default, index, and transaction
    /// insertion path in `dml.rs`.
    fn materialize_insert_source(&mut self, source: InsertSource) -> SqlResult<InsertSource> {
        let InsertSource::Select(query) = source else {
            return Ok(source);
        };
        let result = crate::join::execute_statement(
            &mut self.engine,
            &self.catalog,
            &query,
            &self.current_database,
            &self.current_user,
            None,
            0,
        )?;
        let QueryResult::Rows { rows, .. } = result else {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "INSERT ... SELECT source did not produce rows",
            )));
        };
        Ok(InsertSource::Values(
            rows.into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(InsertValue::Literal)
                        .collect::<Vec<_>>()
                })
                .collect(),
        ))
    }

    pub fn new(mut engine: E) -> SqlResult<Self> {
        let catalog = load_catalog(&mut engine)?;
        let statistics = crate::statistics::load_all(&mut engine)?;
        let columnar_store = plomid_columnar::ColumnarStore::open(engine.root())?;
        let layout = DatabaseLayout::new(engine.root());
        let current_database = "plomid".to_string();
        // The catalog registers a default database and a default schema as real
        // logical objects, so their identity directories are made durable when
        // the session opens. Both steps are idempotent: an existing database or
        // schema is never rewritten.
        crate::ddl::materialize_session_defaults(&catalog, &layout, &current_database)?;
        // ART stays lazy (see the `art_indexes` field docs): no derived index
        // is built here, so session open pays only catalog/statistics loads.
        let session = Self {
            engine,
            catalog,
            current_database,
            current_user: "plomid".to_string(),
            database_names: vec!["plomid".to_string()],
            layout,
            maintenance: MaintenanceCoordinator::default(),
            maintenance_link: None,
            maintenance_error: None,
            maintenance_fail_at: ColumnarFailPoint::None,
            art_indexes: std::collections::BTreeMap::new(),
            art_built: false,
            art_error: None,
            statistics,
            columnar_store,
        };
        Ok(session)
    }

    /// Reconstructs the runtime ART for every index of the session's current
    /// database from its recovered durable index state.
    ///
    /// This is the on-demand builder for the lazy ART cache (see the
    /// `art_indexes` field docs): it runs when the operator or a test
    /// explicitly asks for runtime indexes, after the engine's recovery has
    /// completed and the catalog is loaded, so ART is always derived from
    /// recovered state and never from pre-recovery state. Ordinary DML and
    /// connection establishment never invoke it.
    ///
    /// The persistent B+Tree payload of each index's current generation is the
    /// sole authority; the ART is a derived runtime structure. An index that
    /// cannot be reconstructed is dropped from the cache and its reason is
    /// recorded ([`Self::art_error`]) instead of being exposed as valid. The
    /// number of cached indexes is returned.
    pub fn rebuild_art_indexes(&mut self) -> usize {
        self.art_built = true;
        self.refresh_art_indexes_for(&self.catalog.table_names())
    }

    /// Returns the last persisted statistics for a table, when ANALYZE has
    /// completed for it. This is intentionally read-only; ANALYZE is the only
    /// writer and the statistics record is the only authority.
    #[allow(clippy::type_complexity)]
    pub fn analyzed_statistics(
        &self,
        table: &str,
    ) -> Option<(
        u64,
        Vec<(String, u64, u64, u64, Option<String>, Option<String>)>,
    )> {
        let qualified = self.catalog.resolve_table_name(table).ok()?;
        let stats = self.statistics.get(&qualified)?;
        Some((
            stats.row_count,
            stats
                .columns
                .iter()
                .map(|column| {
                    (
                        column.column.clone(),
                        column.row_count,
                        column.null_count,
                        column.distinct_count,
                        column.min.as_ref().map(Value::to_sql_text),
                        column.max.as_ref().map(Value::to_sql_text),
                    )
                })
                .collect(),
        ))
    }

    fn analyze(&mut self, table: Option<String>) -> SqlResult<QueryResult> {
        let tables = match table {
            Some(name) => vec![self.catalog.resolve_table_name(&name)?],
            None => self.catalog.table_names(),
        };
        for table in tables {
            let schema = self.catalog.get_table(&table)?.clone();
            let stats = crate::statistics::collect(&mut self.engine, &table, &schema)?;
            crate::statistics::save(&mut self.engine, &stats)?;
            tracing::debug!(
                target: "sql::statistics",
                table = %table,
                row_count = stats.row_count,
                "analyze complete"
            );
            self.statistics.insert(table, stats);
        }
        Ok(QueryResult::Created("ANALYZE".into()))
    }

    /// Refreshes the runtime ART for the indexes of the named tables.
    ///
    /// This is the publication-boundary hook: it runs after a table's index
    /// generation is newly published so the derived runtime structure can never
    /// observe stale state. Ordinary DML never invokes it.
    ///
    /// Lazy-cache guard: before the first explicit `rebuild_art_indexes`
    /// there is nothing cached to refresh, so this is a no-op returning 0.
    /// The later rebuild derives from current durable state, which includes
    /// every publication this skipped.
    ///
    /// Resolution is catalog-only: table names resolve to identities through the
    /// same catalog the writers use, so a table refreshes exactly the ART of
    /// its own indexes and never another table's. A table whose index cannot be
    /// reconstructed keeps no runtime entry and its reason is recorded. The
    /// number of cached indexes is returned.
    pub(crate) fn refresh_art_indexes_for(&mut self, tables: &[String]) -> usize {
        use plomid_index::IndexGenerationStore;

        if !self.art_built {
            return 0;
        }

        let Some(database_id) =
            crate::ddl::current_database_id(&self.catalog, &self.current_database)
        else {
            return 0;
        };
        let engine_root = self.engine.root().to_path_buf();
        let index_store = IndexGenerationStore::new(&engine_root);
        for qualified in tables {
            let Ok(table) = self.catalog.get_table(qualified) else {
                continue;
            };
            let (schema_name, _) = qualified
                .split_once('.')
                .unwrap_or(("public", qualified.as_str()));
            let Some(schema_id) = self.catalog.schema_id(schema_name) else {
                continue;
            };
            let table_identity = TableIdentity::new(database_id, schema_id, table.table_id);
            for definition in self.catalog.indexes_for_table(qualified) {
                let key = (
                    database_id.get(),
                    schema_id.get(),
                    table.table_id.get(),
                    definition.index_id.get(),
                );
                match index_store.derive_art(table_identity, definition.index_id) {
                    Ok(Some(art)) => {
                        self.art_indexes.insert(key, art);
                    }
                    Ok(None) => {
                        self.art_indexes.remove(&key);
                    }
                    Err(error) => {
                        self.art_indexes.remove(&key);
                        self.art_error = Some(error.to_string());
                    }
                }
            }
        }
        self.art_indexes.len()
    }

    /// The derived runtime ART for one catalog index.
    ///
    /// Resolved through the catalog like every other SQL name: the table name
    /// may be qualified (`schema.table`) or resolved from the search path, and
    /// the named index must belong to that table. Returns `None` when the index
    /// has no published generation or when its last reconstruction failed (see
    /// [`Self::art_error`]).
    #[must_use]
    pub fn art_index(&self, table: &str, index_name: &str) -> Option<&plomid_index::ArtIndex> {
        let definition = self.catalog.index(index_name)?;
        let qualified = self.catalog.resolve_table_name(table).ok()?;
        if definition.table != qualified {
            return None;
        }
        let table = self.catalog.get_table(&qualified).ok()?;
        let (schema_name, _) = qualified
            .split_once('.')
            .unwrap_or(("public", qualified.as_str()));
        let schema_id = self.catalog.schema_id(schema_name)?;
        let database_id = crate::ddl::current_database_id(&self.catalog, &self.current_database)?;
        self.art_indexes.get(&(
            database_id.get(),
            schema_id.get(),
            table.table_id.get(),
            definition.index_id.get(),
        ))
    }

    /// Why the most recent ART reconstruction failed, when one did.
    ///
    /// A failed reconstruction removes the entry from [`Self::art_index`]
    /// instead of exposing a partial tree, so an operator can distinguish "no
    /// published generation" (an absence) from "the durable source could not be
    /// trusted" (a recorded corruption).
    #[must_use]
    pub fn art_error(&self) -> Option<&str> {
        self.art_error.as_deref()
    }

    /// Number of derived runtime indexes currently cached.
    #[must_use]
    pub fn art_len(&self) -> usize {
        self.art_indexes.len()
    }

    /// Filesystem layout of this executor's storage root.
    ///
    /// Exposed so tests and administrative callers can inspect the logical
    /// object tree without re-deriving the root path.
    #[must_use]
    pub fn layout(&self) -> &DatabaseLayout {
        &self.layout
    }

    /// The storage engine this session runs against.
    ///
    /// Exposed so the durability boundary stays reachable from the SQL layer:
    /// an operator or test can flush and checkpoint through the engine's own
    /// implementation instead of a second durability path existing here.
    pub fn engine_mut(&mut self) -> &mut E {
        &mut self.engine
    }

    /// Authoritative SQL catalog of this session.
    ///
    /// Exposed so tests can resolve index identities through the same catalog
    /// the executor itself uses, without inventing a second lookup path.
    #[must_use]
    pub fn catalog(&self) -> &InMemoryCatalog {
        &self.catalog
    }

    /// The active automatic-maintenance policy.
    #[must_use]
    pub fn maintenance_policy(&self) -> &MaintenancePolicy {
        self.maintenance.policy()
    }

    /// Replaces the automatic-maintenance policy.
    ///
    /// Committed-write accounting already recorded is kept, so a session can
    /// tighten or relax the bound at any point without losing work. This is the
    /// configuration point for the generation rate: a bound of `1` materializes
    /// a generation after every committed write, while the default
    /// ([`MaintenancePolicy::DEFAULT_COMMITTED_MUTATIONS_PER_GENERATION`])
    /// accumulates many writes into one generation.
    pub fn set_maintenance_policy(&mut self, policy: MaintenancePolicy) {
        self.maintenance.set_policy(policy);
    }

    /// Committed mutations accumulated for `table` since its last maintained
    /// generation.
    ///
    /// `table` is matched against the catalog-resolved name, which is the same
    /// identity the maintenance pass materializes.
    #[must_use]
    pub fn committed_mutations(&self, table: &str) -> u64 {
        self.maintenance.committed_mutations(table)
    }

    /// Completed maintenance passes for `table`.
    #[must_use]
    pub fn maintenance_passes(&self, table: &str) -> u64 {
        self.maintenance.maintenance_passes(table)
    }

    /// Tables whose committed mutations have reached the policy bound.
    #[must_use]
    pub fn tables_due_for_maintenance(&self) -> Vec<String> {
        self.maintenance.due_tables()
    }

    /// The last automatic maintenance failure, when one occurred.
    ///
    /// Automatic maintenance never fails the statement that committed before
    /// it, so a failure is reported here instead of being raised to the caller.
    #[must_use]
    pub fn last_maintenance_error(&self) -> Option<&str> {
        self.maintenance_error.as_deref()
    }

    /// Injects a failure into the maintenance pass's data-generation flush.
    ///
    /// Reuses the existing columnar fail-point infrastructure. A test sets this
    /// to prove that an automatic maintenance failure leaves already-committed
    /// DML durable, keeps the previously published generation usable, and is
    /// retried by a later pass. Production code never calls this.
    pub fn set_maintenance_fail_at(&mut self, fail_at: ColumnarFailPoint) {
        self.maintenance_fail_at = fail_at;
    }

    /// Runs one deterministic maintenance pass over every due table.
    ///
    /// This is the single maintenance entry point. The production path invokes
    /// it automatically once a table's committed mutations reach the policy
    /// bound; tests and administrative callers may also invoke it explicitly to
    /// drive the pass without depending on wall-clock time.
    ///
    /// Each due table is materialized into its maintained data generation and
    /// its indexes are refreshed through the existing one-authority
    /// index-generation engine. A table whose pass succeeds has its accumulated
    /// mutations consumed; a table whose pass fails keeps them, so a later pass
    /// retries. The number of tables maintained is returned.
    ///
    /// The pass reads only committed state, so it can never observe an
    /// uncommitted write.
    pub fn run_maintenance(&mut self) -> SqlResult<usize> {
        let due = self.maintenance.due_tables();
        let mut maintained = 0usize;
        for table in due {
            if self.maintain_one(&table)? {
                maintained += 1;
            }
        }
        if maintained > 0 {
            tracing::info!(
                target: "sql::maintenance",
                "maintenance pass tables={} database={}",
                maintained,
                self.current_database
            );
        }
        Ok(maintained)
    }

    /// Runs one maintenance pass for a single due table.
    ///
    /// Materializing a table publishes database-wide state (a data generation
    /// and its index generations), so exactly one session may run it at a
    /// time — enforced by the process-wide single-flight claim, which a
    /// session that cannot take simply skips (keeping its accumulated
    /// mutations for a later retry). Returns true when a pass ran to success.
    fn maintain_one(&mut self, table: &str) -> SqlResult<bool> {
        // Keyed by the storage root (not just the table name): the claim
        // must be shared by every session over the *same* database
        // directory, and must NOT couple sessions in different
        // directories (independent tests, or distinct databases).
        let claim_key = crate::maintenance::maintenance_claim_key(
            self.engine.root(),
            &self.current_database,
            table,
        );
        // Burst gate: a table maintained within the policy interval is
        // skipped without claiming (mutations stay accumulated for a
        // later pass). With a zero interval this check always passes, so
        // deterministic tests and explicit administrative drives are
        // unaffected.
        if !crate::maintenance::auto_pass_allowed(&claim_key, self.maintenance.policy()) {
            return Ok(false);
        }
        let Some(_claim) = crate::maintenance::MaintenanceClaim::try_acquire(claim_key.clone())
        else {
            return Ok(false);
        };
        let pass_started = std::time::Instant::now();
        crate::ddl::maintain_table(
            &mut self.engine,
            &self.catalog,
            &self.layout,
            &self.current_database,
            table,
            self.maintenance_fail_at,
        )?;
        crate::maintenance::record_auto_pass(&claim_key, pass_started.elapsed());
        self.maintenance.record_maintenance_pass(table);
        // A maintained table's index generation may have been newly
        // published, so the derived runtime structure for it is refreshed
        // here — the publication boundary, not per DML.
        self.refresh_art_indexes_for(std::slice::from_ref(&table.to_owned()));
        Ok(true)
    }

    /// Runs the automatic maintenance pass when the policy says a table is due.
    ///
    /// Errors are recorded ([`Self::last_maintenance_error`]) rather than
    /// returned: a committed statement's durability must never depend on
    /// maintenance, and the table's accumulated mutations stay recorded so a
    /// later pass retries.
    fn maybe_run_maintenance(&mut self) {
        if !self.maintenance.any_due() {
            return;
        }
        if let Err(error) = self.run_maintenance() {
            tracing::warn!(
                target: "sql::maintenance",
                "automatic maintenance failed error={}",
                error
            );
            self.maintenance_error = Some(error.to_string());
        }
    }

    /// Installs the background maintenance endpoint (server use).
    ///
    /// Sessions without an installed link run passes inline, preserving
    /// deterministic test behavior.
    pub fn set_background_maintenance(&mut self, link: crate::maintenance_worker::WorkerLink) {
        self.maintenance_link = Some(link);
    }

    /// Records one committed write to each of `write_targets` and runs a
    /// maintenance pass when the policy says a table is due.
    ///
    /// Called only after the statement's transaction committed, so maintenance
    /// always observes committed state and never precedes the durability
    /// boundary. With a background link installed, due tables are submitted
    /// to the worker (coalesced there); a declined submission runs the pass
    /// inline, exactly as without a worker.
    fn after_commit(&mut self, write_targets: Vec<(String, u64)>) {
        if write_targets.is_empty() {
            return;
        }
        for (table, rows) in &write_targets {
            self.maintenance.record_committed_mutations(table, *rows);
        }
        let Some(link) = self.maintenance_link.clone() else {
            self.maybe_run_maintenance();
            return;
        };
        for table in self.maintenance.due_tables() {
            if !link.submit(self.engine.root(), &self.current_database, &table) {
                // Bounded queue is full (or the worker is gone): degrade to
                // the historical inline pass rather than accumulating debt.
                // Errors are recorded, never raised: committed durability
                // must not depend on maintenance.
                if let Err(error) = self.maintain_one(&table) {
                    tracing::warn!(
                        target: "sql::maintenance",
                        "inline fallback maintenance failed error={}",
                        error
                    );
                    self.maintenance_error = Some(error.to_string());
                }
            }
        }
    }

    /// Catalog-resolved tables one statement writes to.
    ///
    /// Resolution uses the same catalog lookup the writers use, so a table
    /// accumulates under one identity regardless of how the statement
    /// qualified it. A statement that does not modify rows reports nothing and
    /// therefore never registers committed maintenance work.
    fn committed_write_targets(&self, statements: &[Statement]) -> Vec<String> {
        let resolve = |name: &str| {
            self.catalog
                .resolve_table_name(name)
                .unwrap_or_else(|_| name.to_string())
        };
        let mut targets = Vec::new();
        for stmt in statements {
            match stmt {
                Statement::Insert { table, .. }
                | Statement::Update { table, .. }
                | Statement::Delete { table, .. }
                | Statement::Copy { table, .. } => targets.push(resolve(table)),
                Statement::Truncate { tables } => {
                    targets.extend(tables.iter().map(|table| resolve(table)));
                }
                _ => {}
            }
        }
        targets
    }

    pub fn set_session_context(&mut self, database: impl Into<String>, user: impl Into<String>) {
        self.current_database = database.into();
        self.current_user = user.into();
        self.catalog.ensure_role(&self.current_user);
    }

    /// Switches the session's current database.
    ///
    /// Name resolution stays with the catalog: the requested database must be
    /// registered there, and the session adopts the catalog's spelling of the
    /// name. The database's identity directory is materialized so that a
    /// database the session resolves names inside is also a real logical object
    /// on disk; the step is idempotent and never rewrites an existing record.
    fn execute_use(&mut self, database: &str) -> SqlResult<QueryResult> {
        let names = self.catalog.database_names().to_vec();
        let Some(index) = names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(database))
        else {
            return Err(SqlError::Storage(PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("database \"{database}\" does not exist"),
                format!("database={database}"),
            )));
        };
        // Identities are the catalog's deterministic order, offset by one
        // because zero is never a valid identity.
        let database_id = DatabaseId::new(index as u64 + 1);
        self.layout.ensure_database_dir(database_id)?;
        self.current_database = names[index].clone();
        // A database switch is a generation boundary for the lazy ART cache:
        // cached entries of the previous database must not survive. Drop them
        // and mark the cache unbuilt; the next explicit rebuild derives from
        // the newly selected database's durable state.
        if self.art_built {
            self.art_indexes.clear();
            self.art_built = false;
        }
        Ok(QueryResult::Set)
    }

    pub fn set_database_names(&mut self, mut names: Vec<String>) {
        names.sort_unstable_by_key(|name| name.to_ascii_lowercase());
        names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        if names.is_empty() {
            names.push(self.current_database.clone());
        }
        self.catalog.set_database_names(names.clone());
        self.database_names = names;
    }

    pub fn execute(&mut self, sql: &str) -> SqlResult<QueryResult> {
        let mut results = self.execute_all(sql)?;
        Ok(results
            .pop()
            .unwrap_or_else(|| QueryResult::Created(String::new())))
    }

    /// Executes every statement in a simple-query batch and preserves each
    /// statement's result for PostgreSQL's one-response-sequence-per-
    /// statement wire behavior.
    pub fn execute_all(&mut self, sql: &str) -> SqlResult<Vec<QueryResult>> {
        tracing::trace!(target: "sql::execute", "start sql_len={}", sql.len());
        let mut tokens = Lexer::new(sql).lex()?;
        // Parse one statement at a time and execute it before parsing the next,
        // so `SET search_path` updates the session state before later
        // statements in the same batch are resolved (PostgreSQL semantics).
        // The lexer emits exactly one trailing `Eof`; keep it on the tail of
        // the remaining token stream instead of splitting it into groups.
        let mut results = Vec::new();
        let mut pending: Vec<Statement> = Vec::new();
        loop {
            // Drop leading statement separators from prior iterations.
            while tokens
                .first()
                .is_some_and(|token| matches!(token.kind, plomid_sql::TokenKind::SemiColon))
            {
                tokens.remove(0);
            }
            if tokens.iter().all(|token| {
                matches!(
                    token.kind,
                    plomid_sql::TokenKind::Eof | plomid_sql::TokenKind::SemiColon
                )
            }) {
                break;
            }
            // Split off the next top-level statement (depth-aware so
            // semicolons inside parens/brackets/strings stay intact).
            let mut depth: usize = 0;
            let mut split_at: Option<usize> = None;
            for (index, token) in tokens.iter().enumerate() {
                match token.kind {
                    plomid_sql::TokenKind::LParen | plomid_sql::TokenKind::LBracket => {
                        depth = depth.saturating_add(1);
                    }
                    plomid_sql::TokenKind::RParen | plomid_sql::TokenKind::RBracket => {
                        depth = depth.saturating_sub(1);
                    }
                    plomid_sql::TokenKind::SemiColon if depth == 0 => {
                        split_at = Some(index);
                        break;
                    }
                    plomid_sql::TokenKind::Eof => {
                        split_at = Some(index);
                        break;
                    }
                    _ => {}
                }
            }
            let end = split_at.unwrap_or(tokens.len());
            let mut group: Vec<plomid_sql::Token> = tokens.drain(..end).collect();
            // Consume the separator itself (the trailing Eof stays for the last
            // statement's parser, which expects it as terminator).
            if tokens
                .first()
                .is_some_and(|token| matches!(token.kind, plomid_sql::TokenKind::SemiColon))
            {
                tokens.remove(0);
            }
            let has_eof = tokens
                .first()
                .is_some_and(|token| matches!(token.kind, plomid_sql::TokenKind::Eof))
                && tokens.len() == 1;
            if has_eof {
                group.push(tokens.remove(0));
            }
            if group.iter().all(|token| {
                matches!(
                    token.kind,
                    plomid_sql::TokenKind::Eof | plomid_sql::TokenKind::SemiColon
                )
            }) {
                continue;
            }
            // Each split group is parsed standalone, so terminate it with a
            // synthetic `Eof` when the real trailing `Eof` belongs to a later
            // statement in the batch.
            if !group
                .iter()
                .any(|token| matches!(token.kind, plomid_sql::TokenKind::Eof))
            {
                group.push(plomid_sql::Token::new(plomid_sql::TokenKind::Eof, "", 0, 0));
            }
            let parser = Parser::new(group, &mut self.catalog);
            let mut statements: Vec<Statement> =
                parser.parse_statements().map_err(SqlError::from)?;
            if statements.is_empty() {
                continue;
            }
            // A split group holds exactly one statement, but keep the loop
            // for safety.
            for stmt in statements.drain(..) {
                if matches!(stmt, Statement::Begin) {
                    pending.push(stmt);
                    continue;
                }
                if !pending.is_empty() {
                    pending.push(stmt.clone());
                    if matches!(stmt, Statement::Commit | Statement::Rollback) {
                        // Only a COMMIT persists the group's writes, so only a
                        // COMMIT registers committed maintenance work. The
                        // targets are captured before the group is consumed.
                        let committed_targets = matches!(stmt, Statement::Commit)
                            .then(|| self.committed_write_targets(&pending));
                        // Dirty-mark before replay: the group's writes become
                        // visible at commit, strictly after this mark, so no
                        // concurrent reader can observe a stale generation.
                        // (ROLLBACK computes no targets and marks nothing.)
                        if let Some(targets) = committed_targets.as_ref() {
                            self.mark_write_targets_dirty(targets);
                        }
                        let group = std::mem::take(&mut pending);
                        let mut group = group;
                        for statement in group.iter_mut() {
                            if let Statement::Insert { source, .. } = statement {
                                let original =
                                    std::mem::replace(source, InsertSource::DefaultValues);
                                *source = self.materialize_insert_source(original)?;
                            }
                        }
                        results.push(execute_transaction_group(
                            &mut self.engine,
                            &mut self.catalog,
                            group,
                        )?);
                        // The group is durable: record it and evaluate the
                        // maintenance policy. This is strictly after commit.
                        // Row counts are unavailable at this layer (one
                        // result per group), so each table accrues a single
                        // conservative unit — the historical behavior.
                        if let Some(targets) = committed_targets {
                            let targets = targets.into_iter().map(|table| (table, 1)).collect();
                            self.after_commit(targets);
                        }
                    }
                    continue;
                }
                if matches!(stmt, Statement::Commit | Statement::Rollback) {
                    results.push(execute_transaction_group(
                        &mut self.engine,
                        &mut self.catalog,
                        vec![Statement::Begin, stmt, Statement::Rollback],
                    )?);
                    continue;
                }
                // Capture the tables this statement will write to before it is
                // consumed, so a successful commit can register its work.
                // The freshness dirty-mark also happens here, before
                // execution: it must precede visibility, not follow commit.
                let write_targets = self.committed_write_targets(std::slice::from_ref(&stmt));
                self.mark_write_targets_dirty(&write_targets);
                let result = if let Statement::Show { ref name } = stmt {
                    self.execute_show(name)
                } else if let Statement::Describe { ref name } = stmt {
                    self.execute_describe(name)
                } else {
                    self.execute_autocommit(stmt)
                }?;
                results.push(result);
                // The statement committed inside the call above; only now does
                // its work become maintenance accounting. Debt is counted in
                // rows written (from the statement's own affected-row count),
                // so a bulk commit trips the policy while single-row OLTP
                // commits accrue exactly as before. Multi-table statements
                // attribute the full count to each table (conservative:
                // earlier maintenance, never later).
                let rows_changed = match results.last() {
                    Some(
                        plomid_sql::QueryResult::Inserted(n)
                        | plomid_sql::QueryResult::Updated(n)
                        | plomid_sql::QueryResult::Deleted(n),
                    ) => *n,
                    _ => 1,
                };
                let write_targets = write_targets
                    .into_iter()
                    .map(|table| (table, rows_changed))
                    .collect();
                self.after_commit(write_targets);
            }
        }
        if !pending.is_empty() {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                "BEGIN without COMMIT or ROLLBACK",
            )));
        }

        if results.is_empty() {
            return Ok(vec![QueryResult::Created(String::new())]);
        }

        tracing::trace!(
            target: "sql::execute",
            "complete result_count={}",
            results.len()
        );
        Ok(results)
    }

    /// Execute using the connection's PostgreSQL search_path. The executor is
    /// shared by wire connections, so the path is installed only for the
    /// duration of this serialized execution and restored afterward -- unless
    /// the batch itself ran `SET search_path`, in which case the updated
    /// session value is returned so the caller can persist it.
    pub fn execute_all_with_search_path(
        &mut self,
        sql: &str,
        search_path: &[String],
    ) -> SqlResult<(Vec<QueryResult>, Option<Vec<String>>)> {
        let previous = self.catalog.search_path().to_vec();
        self.catalog.set_search_path(search_path.to_vec());
        let result = self.execute_all(sql);
        let updated = self.catalog.search_path().to_vec();
        self.catalog.set_search_path(previous);
        let changed = result
            .is_ok()
            .then(|| updated)
            .filter(|next| next != search_path);
        result.map(|results| (results, changed))
    }

    fn execute_show(&mut self, name: &str) -> SqlResult<QueryResult> {
        let normalized = name.to_ascii_lowercase();
        if matches!(normalized.as_str(), "database" | "databases") {
            return Ok(QueryResult::Rows {
                columns: vec!["database".to_string()],
                column_types: vec![Some(plomid_sql::ColumnType::text())],
                rows: self
                    .database_names
                    .iter()
                    .map(|database| vec![plomid_sql::Value::Text(database.clone())])
                    .collect(),
            });
        }
        if normalized == "schema_name" {
            return Ok(QueryResult::Rows {
                columns: vec!["schema_name".to_string()],
                column_types: vec![Some(plomid_sql::ColumnType::text())],
                rows: vec![vec![plomid_sql::Value::Text("public".into())]],
            });
        }
        if matches!(normalized.as_str(), "schema" | "schemas") {
            let mut schemas = self.catalog.schema_names();
            for table in self.catalog.table_names() {
                schemas.push(
                    table
                        .split_once('.')
                        .map_or("public", |(schema, _)| schema)
                        .into(),
                );
            }
            schemas.sort_unstable();
            schemas.dedup();
            return Ok(QueryResult::Rows {
                columns: vec!["schema".to_string()],
                column_types: vec![Some(plomid_sql::ColumnType::text())],
                rows: schemas
                    .into_iter()
                    .map(|schema| vec![plomid_sql::Value::Text(schema)])
                    .collect(),
            });
        }
        execute_ddl(
            &mut self.engine,
            &mut self.catalog,
            &self.layout,
            &self.current_database,
            Statement::Show {
                name: name.to_string(),
            },
        )
    }

    fn execute_describe(&self, name: &str) -> SqlResult<QueryResult> {
        let table = self.catalog.get_table(name).map_err(SqlError::Storage)?;
        let rows = table
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                vec![
                    Value::Text(column.name.clone()),
                    Value::Text(
                        plomid_types::PgType::by_oid(column.col_type.type_oid)
                            .map(|ty| ty.name().to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                    ),
                    Value::Int4((index + 1) as i32),
                    Value::Text(
                        if table.is_not_null(index) {
                            "NO"
                        } else {
                            "YES"
                        }
                        .into(),
                    ),
                ]
            })
            .collect();
        Ok(QueryResult::Rows {
            columns: vec![
                "column_name".into(),
                "data_type".into(),
                "ordinal_position".into(),
                "is_nullable".into(),
            ],
            column_types: vec![
                Some(ColumnType::text()),
                Some(ColumnType::text()),
                Some(ColumnType::new(
                    plomid_types::TypeOid::INT4,
                    plomid_types::NO_TYPEMOD,
                )),
                Some(ColumnType::text()),
            ],
            rows,
        })
    }

    pub fn validate(&mut self, sql: &str) -> SqlResult<()> {
        let tokens = Lexer::new(sql).lex()?;
        let parser = Parser::new(tokens, &mut self.catalog);
        parser.parse_statements()?;
        Ok(())
    }

    /// Counts the rows a standalone DML statement would affect without
    /// applying it. The network protocol uses this while staging explicit
    /// transactions, where the real write is deferred until COMMIT.
    pub fn preview_dml_count(&mut self, sql: &str) -> SqlResult<u64> {
        let tokens = Lexer::new(sql).lex()?;
        let parser = Parser::new(tokens, &mut self.catalog);
        let statements = parser.parse_statements()?;
        let Some(stmt) = statements.into_iter().next() else {
            return Ok(0);
        };
        match stmt {
            Statement::Insert { source, .. } => Ok(match source {
                plomid_sql::InsertSource::Values(v) => v.len() as u64,
                plomid_sql::InsertSource::DefaultValues => 1,
                plomid_sql::InsertSource::Select(_) => 0,
            }),
            Statement::Update {
                table, where_expr, ..
            } => {
                let schema = self.catalog.get_table(&table)?;
                // Same access path as the real statement: resolve a
                // single-column-index equality predicate through the index
                // (O(1)-ish) instead of scanning every row of the table. The
                // full scan — required by PostgreSQL semantics here, because
                // this counts the rows an UPDATE would affect — remains the
                // fallback when no index equality applies.
                let entries = match indexed_dml_entries_engine(
                    &mut self.engine,
                    &self.catalog,
                    &table,
                    where_expr.as_ref(),
                )? {
                    Some(entries) => entries,
                    None => self.engine.scan(
                        Some(format!("{table}:").as_bytes()),
                        Some(format!("{table}:\u{10FFFF}").as_bytes()),
                    )?,
                };
                let mut count = 0;
                for (_, bytes) in entries {
                    let row = decode_row(&bytes)?;
                    let matches = match where_expr.as_ref() {
                        Some(expr) => evaluate_predicate(&row, schema, expr)?,
                        None => true,
                    };
                    if matches {
                        count += 1;
                    }
                }
                Ok(count)
            }
            Statement::Delete {
                table,
                alias,
                using,
                where_expr,
                returning: _,
            } => {
                let schema = self.catalog.get_table(&table)?;
                let target_quals = crate::update_from::target_qualifiers(&table, alias.as_deref());
                // Materialize `DELETE ... USING` source relations so the
                // staged count honors the join (same machinery as the real
                // execution path).
                let using_source = match &using {
                    Some(clause) => Some(crate::update_from::materialize_from(
                        &mut self.engine,
                        &self.catalog,
                        clause,
                        &self.current_database,
                        &self.current_user,
                    )?),
                    None => None,
                };
                // Same access path as the real statement: indexed equality
                // discovery first, full scan only as the fallback (and never
                // for USING, whose row set depends on the join).
                let entries = if using.is_none() {
                    match indexed_dml_entries_engine(
                        &mut self.engine,
                        &self.catalog,
                        &table,
                        where_expr.as_ref(),
                    )? {
                        Some(entries) => entries,
                        None => self.engine.scan(
                            Some(format!("{table}:").as_bytes()),
                            Some(format!("{table}:\u{10FFFF}").as_bytes()),
                        )?,
                    }
                } else {
                    self.engine.scan(
                        Some(format!("{table}:").as_bytes()),
                        Some(format!("{table}:\u{10FFFF}").as_bytes()),
                    )?
                };
                // A target row counts at most once even when several USING
                // rows match it (PostgreSQL deletes the row once).
                let empty_row: Vec<plomid_sql::Value> = Vec::new();
                let using_rows: &[Vec<plomid_sql::Value>] = match &using_source {
                    Some(src) => &src.rows,
                    None => std::slice::from_ref(&empty_row),
                };
                let mut count = 0;
                for (_, bytes) in entries {
                    let row = decode_row(&bytes)?;
                    for from_row in using_rows {
                        let bound = match where_expr.as_ref() {
                            Some(expr) => Some(crate::update_from::bind_from_refs(
                                expr,
                                using_source.as_ref(),
                                &target_quals,
                                from_row,
                                schema,
                            )?),
                            None => None,
                        };
                        let matches = match &bound {
                            Some(bound) => evaluate_predicate(&row, schema, bound)?,
                            None => true,
                        };
                        if matches {
                            count += 1;
                            break;
                        }
                    }
                }
                Ok(count)
            }
            _ => Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "preview_dml_count requires INSERT, UPDATE, or DELETE",
            ))),
        }
    }

    /// Validates a staged DML statement without executing it, and returns the
    /// row count it would affect. The real mutation happens exactly once, at
    /// COMMIT, when the network layer replays the staged batch.
    ///
    /// [`Self::validate_dml_strict`] with the connection's PostgreSQL
    /// `search_path` installed for the duration of validation, mirroring
    /// [`Self::execute_all_with_search_path`]. Without this, unqualified table
    /// names in transactional DML would resolve against the executor's default
    /// path (`public`) instead of the session path.
    pub fn validate_dml_strict_with_search_path(
        &mut self,
        sql: &str,
        search_path: &[String],
    ) -> SqlResult<u64> {
        let previous = self.catalog.search_path().to_vec();
        self.catalog.set_search_path(search_path.to_vec());
        let result = self.validate_dml_strict(sql);
        self.catalog.set_search_path(previous);
        result
    }

    /// Validates a staged DML statement **without executing it**.
    ///
    /// The statement is parsed (grammar) and bound (the target relation is
    /// resolved now, using the caller-installed `search_path`), and validated
    /// against committed state through read-only paths. It is then executed
    /// exactly once — in the real transaction, when COMMIT replays the staged
    /// batch. Run as a throwaway trial transaction here, this step would
    /// execute every explicit-transaction DML twice (two sets of write gates,
    /// unique reservations, MVCC versions, and storage/WAL work).
    ///
    /// Statement-time errors are still reported for the checks that can be
    /// answered from committed state: for INSERT that is the full
    /// binding/constraint funnel (type coercion, DEFAULT substitution,
    /// NOT NULL, CHECK, generated columns) plus a uniqueness probe. Nothing
    /// here mutates state; the constraint checks that depend on
    /// transaction-local state run during the single real execution.
    pub fn validate_dml_strict(&mut self, sql: &str) -> SqlResult<u64> {
        let tokens = Lexer::new(sql).lex()?;
        let parser = Parser::new(tokens, &mut self.catalog);
        let statements = parser.parse_statements()?;
        let Some(stmt) = statements.into_iter().next() else {
            return Ok(0);
        };
        match stmt {
            Statement::Insert {
                table,
                columns,
                source,
                on_conflict,
                ..
            } => self.validate_insert_readonly(&table, columns, &source, on_conflict.as_ref()),
            // UPDATE/DELETE bind their relation while counting affected rows
            // in `preview_dml_count` below.
            Statement::Update { .. } | Statement::Delete { .. } => self.preview_dml_count(sql),
            _ => Ok(0),
        }
    }

    /// Statement-time validation for an INSERT staged by an explicit
    /// transaction, using only committed state.
    ///
    /// This runs the same binding and constraint funnel as the real statement
    /// — serial defaults, [`crate::row::resolve_insert_values`] (type coercion,
    /// DEFAULT substitution, NOT NULL, CHECK, generated columns) and the
    /// unique-index probe — so errors are reported at statement time exactly as
    /// they were when this step ran a throwaway trial transaction. The
    /// difference is that it only ever *reads*: no write gate, no unique
    /// reservation, no MVCC version, no storage mutation, no WAL record. The
    /// mutation itself still runs exactly once, at COMMIT.
    fn validate_insert_readonly(
        &mut self,
        table: &str,
        columns: Option<Vec<String>>,
        source: &InsertSource,
        on_conflict: Option<&plomid_sql::OnConflict>,
    ) -> SqlResult<u64> {
        let schema = self.catalog.get_table(table)?.clone();
        let rules = self.catalog.column_rules(table)?;
        let rows = crate::dml::expand_insert_source(source.clone(), &schema)?;
        // Rows already staged by *this* statement. The real execution sees them
        // through the transaction's own index writes; here an O(1) key set
        // reproduces that view without writing anything.
        let mut statement_keys = std::collections::HashSet::new();
        let mut count = 0u64;
        for (position, flat) in rows.into_iter().enumerate() {
            let row_number = position + 1;
            let (columns, flat) =
                self.readonly_serial_defaults(table, &schema, columns.as_deref(), flat)?;
            let row = crate::row::resolve_insert_values(&schema, &rules, columns, flat)
                .map_err(|error| crate::dml::annotate_insert_error(error, row_number))?;
            // `ON CONFLICT` accepts a conflicting row by design, so only the
            // plain INSERT path probes for one.
            if on_conflict.is_none() {
                self.check_unique_conflicts_readonly(
                    table,
                    &schema,
                    &rules,
                    &row,
                    &mut statement_keys,
                )
                .map_err(|error| crate::dml::annotate_insert_error(error, row_number))?;
            }
            count += 1;
        }
        Ok(count)
    }

    /// Read-only counterpart of `apply_serial_defaults_txn`.
    ///
    /// Fills serial columns omitted from the target column list from the
    /// sequence's committed value so a `SERIAL NOT NULL` column is not
    /// rejected as missing. The value is used for validation only: the real
    /// execution allocates the value it actually stores.
    fn readonly_serial_defaults(
        &mut self,
        table: &str,
        schema: &plomid_sql::TableSchema,
        columns: Option<&[String]>,
        mut values: Vec<Value>,
    ) -> SqlResult<(Option<Vec<String>>, Vec<Value>)> {
        let Some(columns) = columns else {
            return Ok((None, values));
        };
        let mut expanded = columns.to_vec();
        for column in &schema.columns {
            if !column.col_type.serial
                || columns
                    .iter()
                    .any(|name| crate::util::unqualify(name) == column.name)
            {
                continue;
            }
            let sequence = format!("{table}_{}_seq", column.name);
            let key = crate::catalog_fn::sequence_key(&sequence);
            let mut end_key = key.clone();
            end_key.push(0xff);
            let current = self
                .engine
                .scan(Some(&key), Some(&end_key))?
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
            expanded.push(column.name.clone());
            values.push(Value::Int8(next));
        }
        Ok((Some(expanded), values))
    }

    /// Read-only uniqueness probe against committed state.
    ///
    /// Mirrors `enforce_unique_values_txn` plus the authoritative index probe:
    /// index-backed constraints use an exact index probe and constraints with
    /// no index fall back to a table scan. `statement_keys` carries the keys
    /// already seen inside the current statement so a multi-row INSERT still
    /// rejects a duplicate of its own earlier row, as the real execution does
    /// through the transaction's own index writes.
    fn check_unique_conflicts_readonly(
        &mut self,
        table: &str,
        schema: &plomid_sql::TableSchema,
        rules: &[plomid_sql::ColumnRule],
        row: &[Value],
        statement_keys: &mut std::collections::HashSet<(String, Vec<u8>)>,
    ) -> SqlResult<()> {
        for index in self.catalog.all_indexes_for_table(table) {
            if !index.unique {
                continue;
            }
            let values = crate::index::index_values_for_row(&index, row, schema)?;
            // SQL NULL-not-equal: a tuple with any NULL component never
            // conflicts (same rule as the authoritative probe).
            if !crate::index::tuple_conflicts_under_unique(&values) {
                continue;
            }
            let prefix = crate::index::index_tuple_prefix(&index.name, &values);
            if !statement_keys.insert((index.name.clone(), prefix.clone())) {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Conflict,
                    format!(
                        "duplicate key value violates unique index \"{}\"",
                        index.name
                    ),
                )));
            }
            let end = crate::index::prefix_end(&prefix);
            if !self.engine.scan(Some(&prefix), Some(&end))?.is_empty() {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Conflict,
                    format!(
                        "duplicate key value violates unique index \"{}\"",
                        index.name
                    ),
                )));
            }
        }
        // Constraints whose domain is not backed by an authoritative index
        // still need the table scan (`enforce_unique_values_txn`'s fallback).
        let uncovered: Vec<usize> = rules
            .iter()
            .enumerate()
            .filter(|(position, rule)| {
                (rule.unique || rule.primary_key)
                    && schema.columns.get(*position).is_some_and(|column| {
                        crate::index::constraint_index_for_column(
                            &self.catalog,
                            table,
                            &column.name,
                        )
                        .is_none()
                    })
            })
            .map(|(position, _)| position)
            .collect();
        if uncovered.is_empty() {
            return Ok(());
        }
        let entries = self.engine.scan(
            Some(format!("{table}:").as_bytes()),
            Some(format!("{table}:\u{10FFFF}").as_bytes()),
        )?;
        for (_, bytes) in entries {
            let existing = decode_row(&bytes)?;
            for position in &uncovered {
                if existing.get(*position) == row.get(*position)
                    && !matches!(row.get(*position), Some(Value::Null))
                {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::Conflict,
                        format!(
                            "duplicate key value violates unique constraint on \"{}\"",
                            schema.columns[*position].name
                        ),
                    )));
                }
            }
        }
        Ok(())
    }

    /// Marks the freshness map dirty for every table `targets` may write.
    ///
    /// Called BEFORE the statement executes (or the staged group replays),
    /// so a concurrent reader can never observe a published generation that
    /// misses a committed write: the dirty mark precedes visibility. Failed
    /// or rolled-back statements stay dirty conservatively until the next
    /// vacuum re-proves the generation.
    fn mark_write_targets_dirty(&self, targets: &[String]) {
        let root = self.engine.root();
        for table in targets {
            let key = crate::columnar_freshness::freshness_key(root, &self.current_database, table);
            crate::columnar_freshness::mark_table_dirty(&key);
        }
    }

    fn execute_autocommit(&mut self, stmt: Statement) -> SqlResult<QueryResult> {
        // One statement execution context per top-level statement: every
        // CURRENT_* reference inside this statement (SELECT list, VALUES,
        // UPDATE assignments, WHERE, subqueries) observes the same logical
        // statement timestamp.
        let search_path = self.catalog.search_path().to_vec();
        let current_schema = search_path
            .iter()
            .find(|schema| {
                !schema.is_empty() && *schema != "$user" && self.catalog.has_schema(schema)
            })
            .cloned();
        let _ctx = crate::context::StatementContext::enter_with_search_path(
            &self.current_database,
            &self.current_user,
            search_path,
            current_schema,
        );
        match stmt {
            Statement::Insert {
                table,
                columns,
                source,
                returning,
                on_conflict,
            } => {
                let source = self.materialize_insert_source(source)?;
                execute_insert_rows(
                    &mut self.engine,
                    &mut self.catalog,
                    table,
                    columns,
                    source,
                    returning,
                    on_conflict,
                )
            }
            Statement::Copy {
                table: _,
                columns: _,
                direction: _,
            } => Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Unsupported,
                "COPY FROM STDIN/COPY TO STDOUT requires PostgreSQL wire-level \
                 CopyData/CopyDone/CopyFail frames; issue INSERT statements for now, or use the \
                 bulk_insert_from_rows API directly in executor/dml.rs",
            ))),
            Statement::Select {
                targets,
                distinct,
                distinct_on,
                from,
                where_expr,
                group_by,
                having,
                order_by,
                limit,
                offset,
            } => {
                if let Some(FromClause::Table { name, .. }) = from.as_ref() {
                    if let Some((row_count, _)) = self.analyzed_statistics(name) {
                        tracing::trace!(
                            target: "sql::planner",
                            table = %name,
                            row_count,
                            "using persisted table statistics"
                        );
                    }
                }
                execute_select(
                    &mut self.engine,
                    &self.catalog,
                    targets,
                    distinct,
                    distinct_on.clone().unwrap_or_default(),
                    from,
                    where_expr,
                    group_by,
                    having,
                    order_by,
                    limit,
                    offset,
                    &self.current_database,
                    &self.current_user,
                    Some(&self.columnar_store),
                )
            }
            Statement::Values(_) => crate::join::execute_statement(
                &mut self.engine,
                &self.catalog,
                &stmt,
                &self.current_database,
                &self.current_user,
                None,
                0,
            ),
            Statement::Update {
                table,
                alias,
                from,
                assignments,
                where_expr,
                returning,
            } => execute_update(
                &mut self.engine,
                &mut self.catalog,
                table,
                alias.clone(),
                assignments,
                from.clone(),
                where_expr,
                // Forward the parsed `RETURNING` targets (None = plain UPDATE
                // row-count path). `execute_update` expands `*` against the
                // schema and evaluates each target per updated row.
                returning,
                &self.current_database,
                &self.current_user,
            ),
            Statement::Delete {
                table,
                alias,
                using,
                where_expr,
                returning,
            } => execute_delete(
                &mut self.engine,
                &self.catalog,
                table,
                alias.clone(),
                using.clone(),
                where_expr,
                // Same contract as UPDATE: None keeps the legacy deleted-count
                // result; Some(targets) returns the deleted rows as `Rows`.
                returning,
                &self.current_database,
                &self.current_user,
            ),
            Statement::Truncate { tables } => {
                execute_truncate(&mut self.engine, &self.catalog, tables)
            }
            Statement::SetOperation { .. } => crate::join::execute_statement(
                &mut self.engine,
                &self.catalog,
                &stmt,
                &self.current_database,
                &self.current_user,
                None,
                0,
            ),
            Statement::With { .. } => crate::join::execute_with(
                &mut self.engine,
                &self.catalog,
                stmt,
                &self.current_database,
                &self.current_user,
            ),
            Statement::Explain {
                statement,
                analyze,
                format,
            } => crate::query::explain_statement(
                &mut self.engine,
                &self.catalog,
                &statement,
                analyze,
                format.clone(),
                &self.current_database,
                &self.current_user,
            ),
            Statement::Do { body } => execute_do_block(self, &body),
            Statement::Use { database } => self.execute_use(&database),
            Statement::Vacuum { table } => {
                let targets = crate::ddl::vacuum_targets(&self.catalog, table.as_deref())?;
                let result = execute_ddl(
                    &mut self.engine,
                    &mut self.catalog,
                    &self.layout,
                    &self.current_database,
                    Statement::Vacuum { table },
                )?;
                // The vacuum's data/index generations are newly published, so
                // the derived runtime structure for exactly those tables is
                // refreshed here — never by scanning, always by catalog
                // resolution, so it covers the statement's own targets.
                // Columnar freshness itself is recorded inside maintain_table
                // (shared across sessions), not in session-local state.
                self.refresh_art_indexes_for(&targets);
                self.columnar_store = plomid_columnar::ColumnarStore::open(self.engine.root())?;
                Ok(result)
            }
            Statement::Analyze { table } => self.analyze(table),
            ddl => execute_ddl(
                &mut self.engine,
                &mut self.catalog,
                &self.layout,
                &self.current_database,
                ddl,
            ),
        }
    }
}

/// Executes a `DO $$ ... $$` anonymous code block.
fn execute_do_block<E: StorageEngine>(
    executor: &mut Executor<E>,
    body: &str,
) -> SqlResult<QueryResult> {
    let normalized = body.trim();
    let lower = normalized.to_ascii_lowercase();
    let inner = if lower.starts_with("begin") && lower.ends_with("end;") {
        normalized[5..normalized.len() - 4].trim()
    } else if lower.starts_with("begin") && lower.ends_with("end") {
        normalized[5..normalized.len() - 3].trim()
    } else {
        normalized
    };
    if inner.is_empty() {
        return Ok(QueryResult::Created("DO".into()));
    }
    let Some((main_body, exception_handler)) = split_at_exception(inner) else {
        return execute_do_body(executor, inner).map(|_| QueryResult::Created("DO".into()));
    };
    match execute_do_body(executor, main_body.trim()) {
        Ok(_) => Ok(QueryResult::Created("DO".into())),
        Err(_) => {
            let _ = execute_do_body(executor, exception_handler.trim());
            Ok(QueryResult::Created("DO".into()))
        }
    }
}

/// Splits a PL/pgSQL block body at the top-level `EXCEPTION` keyword.
fn split_at_exception(body: &str) -> Option<(&str, &str)> {
    let lower = body.to_ascii_lowercase();
    let mut in_string = false;
    let mut prev_char = ' ';
    for (i, ch) in body.char_indices() {
        if ch == '\'' && prev_char != '\\' {
            in_string = !in_string;
        }
        if !in_string {
            let remaining = &lower[i..];
            if remaining.starts_with("exception")
                && (i == 0 || !body.as_bytes()[i - 1].is_ascii_alphanumeric())
                && remaining
                    .as_bytes()
                    .get(9)
                    .map_or(true, |c| !c.is_ascii_alphanumeric())
            {
                return Some((&body[..i], &body[i + 9..]));
            }
        }
        prev_char = ch;
    }
    None
}

/// Executes the statements inside a DO block body.
fn execute_do_body<E: StorageEngine>(executor: &mut Executor<E>, body: &str) -> SqlResult<()> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    for stmt in split_do_statements(trimmed) {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        let lower = stmt.to_ascii_lowercase();
        if lower.starts_with("perform ") {
            let expr_text = stmt[8..].trim();
            let select_sql = format!("SELECT {expr_text}");
            // PERFORM errors propagate to the EXCEPTION handler via `?`.
            executor.execute(&select_sql)?;
        } else if lower.starts_with("raise notice") {
            let notice_text = stmt[12..].trim();
            if let Some(msg) = parse_notice_message(notice_text) {
                tracing::info!(target: "plomid::do", "NOTICE: {msg}");
            }
        } else if lower.starts_with("when others then") {
            let handler_body = stmt[16..].trim();
            if !handler_body.is_empty() {
                return execute_do_body(executor, handler_body);
            }
        } else if lower == "end" || lower == "end;" {
            continue;
        }
    }
    Ok(())
}

/// Splits DO block body into individual statements by semicolons.
fn split_do_statements(body: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut in_string = false;
    let mut prev_char = ' ';
    for (i, ch) in body.char_indices() {
        if ch == '\'' && prev_char != '\\' {
            in_string = !in_string;
        }
        if ch == ';' && !in_string {
            result.push(&body[start..i]);
            start = i + 1;
        }
        prev_char = ch;
    }
    if start < body.len() {
        result.push(&body[start..]);
    }
    result
}

/// Parses a RAISE NOTICE message string.
fn parse_notice_message(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut in_string = false;
    let mut prev_char = ' ';
    let mut string_start = None;
    let mut string_end = None;
    for (i, ch) in text.char_indices() {
        if ch == '\'' && prev_char != '\\' {
            if !in_string {
                string_start = Some(i);
                in_string = true;
            } else {
                string_end = Some(i);
                break;
            }
        }
        prev_char = ch;
    }
    let start = string_start?;
    let end = string_end?;
    let format_str = &text[start + 1..end];
    let after = &text[end + 1..];
    let args: Vec<&str> = after
        .split(',')
        .skip(1)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if args.is_empty() {
        Some(format_str.to_string())
    } else {
        let mut result = String::new();
        let mut arg_iter = args.iter();
        for ch in format_str.chars() {
            if ch == '%' {
                if let Some(arg) = arg_iter.next() {
                    result.push_str(arg);
                } else {
                    result.push('%');
                }
            } else {
                result.push(ch);
            }
        }
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plomid_core::{ErrorKind, PlomidError};
    use plomid_sql::{QueryResult, Value};
    use plomid_txn::PlomidStorageEngine;

    #[test]
    fn end_to_end_sql_with_restart() {
        let storage_path = std::path::PathBuf::from("/tmp/plomid-e2e-test");
        let wal_path = std::path::PathBuf::from("/tmp/plomid-e2e-wal-test");
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        let result: SqlResult<()> = (|| {
            let engine = PlomidStorageEngine::create(&storage_path, &wal_path, 32)?;
            let mut executor = Executor::new(engine)?;
            executor.execute("CREATE TABLE users (id INTEGER, name TEXT);")?;
            executor.execute("INSERT INTO users (id, name) VALUES (1, 'Alice');")?;
            executor.execute("BEGIN; INSERT INTO users (id, name) VALUES (2, 'Bob'); COMMIT;")?;
            executor.execute("UPDATE users SET name = 'Alice2' WHERE id = 1;")?;
            executor.execute("DELETE FROM users WHERE id = 2;")?;
            drop(executor);

            let engine = PlomidStorageEngine::open(&storage_path, &wal_path, 32)?;
            let mut executor = Executor::new(engine)?;
            let QueryResult::Rows { rows, .. } = executor.execute("SELECT * FROM users;")? else {
                panic!("expected rows after restart");
            };
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][1], Value::Text("Alice2".to_string()));
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        assert!(
            result.is_ok(),
            "end_to_end_sql_with_restart failed: {result:?}"
        );
    }

    #[test]
    fn secondary_index_builds_persists_and_filters_reads() {
        let storage_path = std::path::PathBuf::from("/tmp/plomid-index-test");
        let wal_path = std::path::PathBuf::from("/tmp/plomid-index-wal-test");
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        let result: SqlResult<()> = (|| {
            let engine = PlomidStorageEngine::create(&storage_path, &wal_path, 32)?;
            let mut executor = Executor::new(engine)?;
            executor.execute("CREATE TABLE users (id INTEGER, name TEXT);")?;
            executor.execute("INSERT INTO users VALUES (1, 'Alice');")?;
            executor.execute("INSERT INTO users VALUES (2, 'Bob');")?;
            executor.execute("CREATE INDEX users_name_idx ON users(name);")?;
            executor.execute("CREATE UNIQUE INDEX users_id_idx ON users(id);")?;
            assert!(executor
                .execute("INSERT INTO users VALUES (1, 'Duplicate');")
                .is_err());
            let QueryResult::Rows { rows, .. } =
                executor.execute("SELECT * FROM users WHERE name = 'Bob';")?
            else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Corruption,
                    "expected rows",
                )));
            };
            assert_eq!(
                rows,
                vec![vec![Value::Int4(2), Value::Text("Bob".to_string())]]
            );
            executor.execute("UPDATE users SET name = 'Robert' WHERE id = 2;")?;
            let QueryResult::Rows { rows, .. } =
                executor.execute("SELECT * FROM users WHERE name = 'Robert';")?
            else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Corruption,
                    "expected updated rows",
                )));
            };
            assert_eq!(rows.len(), 1);
            executor.execute("DELETE FROM users WHERE id = 1;")?;
            drop(executor);
            let engine = PlomidStorageEngine::open(&storage_path, &wal_path, 32)?;
            let mut executor = Executor::new(engine)?;
            let QueryResult::Rows { rows, .. } =
                executor.execute("SELECT * FROM users WHERE name = 'Robert';")?
            else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Corruption,
                    "expected persisted rows",
                )));
            };
            assert_eq!(rows.len(), 1);
            executor.execute("DROP INDEX users_name_idx;")?;
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn schema_qualified_tables_use_isolated_key_namespaces() {
        let storage_path = std::path::PathBuf::from("/tmp/plomid-schema-test");
        let wal_path = std::path::PathBuf::from("/tmp/plomid-schema-wal-test");
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        let result: SqlResult<()> = (|| {
            let engine = PlomidStorageEngine::create(&storage_path, &wal_path, 32)?;
            let mut executor = Executor::new(engine)?;
            executor.execute("CREATE SCHEMA sales;")?;
            executor.execute("CREATE TABLE sales.orders (id INTEGER, customer TEXT);")?;
            executor.execute("CREATE TABLE orders (id INTEGER, customer TEXT);")?;
            executor.execute("INSERT INTO sales.orders VALUES (1, 'sales');")?;
            executor.execute("INSERT INTO orders VALUES (1, 'public');")?;
            let QueryResult::Rows { rows, .. } = executor.execute("SELECT * FROM sales.orders;")?
            else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Internal,
                    "expected schema-qualified rows",
                )));
            };
            assert_eq!(rows[0][1], Value::Text("sales".to_string()));
            let QueryResult::Rows { rows, .. } = executor.execute("SELECT * FROM orders;")? else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Internal,
                    "expected public rows",
                )));
            };
            assert_eq!(rows[0][1], Value::Text("public".to_string()));
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        assert!(result.is_ok(), "schema isolation test failed: {result:?}");
    }

    #[test]
    fn alter_table_columns_rewrite_persisted_rows() {
        let storage_path = std::path::PathBuf::from("/tmp/plomid-alter-test");
        let wal_path = std::path::PathBuf::from("/tmp/plomid-alter-wal-test");
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        let result: SqlResult<()> = (|| {
            let engine = PlomidStorageEngine::create(&storage_path, &wal_path, 32)?;
            let mut executor = Executor::new(engine)?;
            executor.execute("CREATE TABLE users (id INTEGER, name TEXT);")?;
            executor.execute("INSERT INTO users VALUES (1, 'Alice');")?;
            executor.execute("ALTER TABLE users ADD COLUMN age INTEGER;")?;
            let QueryResult::Rows { rows, .. } = executor.execute("SELECT * FROM users;")? else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Internal,
                    "expected rows",
                )));
            };
            assert_eq!(rows[0].len(), 3);
            assert_eq!(rows[0][2], Value::Null);
            executor.execute("ALTER TABLE users DROP COLUMN age;")?;
            executor.execute("ALTER TABLE users RENAME COLUMN name TO display_name;")?;
            let QueryResult::Rows { columns, rows, .. } =
                executor.execute("SELECT * FROM users;")?
            else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Internal,
                    "expected rows",
                )));
            };
            assert_eq!(columns, vec!["id", "display_name"]);
            assert_eq!(rows[0][1], Value::Text("Alice".into()));
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        assert!(result.is_ok(), "alter test failed: {result:?}");
    }

    #[test]
    fn constraints_defaults_and_unique_values_are_enforced() {
        let storage_path = std::path::PathBuf::from("/tmp/plomid-constraints-test");
        let wal_path = std::path::PathBuf::from("/tmp/plomid-constraints-wal-test");
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        let result: SqlResult<()> = (|| {
            let engine = PlomidStorageEngine::create(&storage_path, &wal_path, 32)?;
            let mut executor = Executor::new(engine)?;
            executor.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'unknown');")?;
            assert!(executor.catalog.column_rules("users")?[1].not_null);
            executor.execute("INSERT INTO users (id) VALUES (1);")?;
            let duplicate = executor.execute("INSERT INTO users (id) VALUES (1);");
            assert!(
                matches!(duplicate, Err(SqlError::Storage(error)) if error.kind() == ErrorKind::Conflict)
            );
            let null_name = executor.execute("INSERT INTO users VALUES (2, NULL);");
            assert!(
                null_name.is_err(),
                "expected NOT NULL failure: {null_name:?}"
            );
            let QueryResult::Rows { rows, .. } = executor.execute("SELECT * FROM users;")? else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Internal,
                    "expected rows",
                )));
            };
            assert_eq!(rows[0][1], Value::Text("unknown".into()));
            executor.execute("CREATE TABLE ages (id INTEGER, age INTEGER CHECK (age >= 0));")?;
            let invalid = executor.execute("INSERT INTO ages VALUES (1, -1);");
            assert!(invalid.is_err(), "negative CHECK value must be rejected");
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        assert!(result.is_ok(), "constraint test failed: {result:?}");
    }

    #[test]
    fn sequence_state_is_transactionally_persistent() {
        let storage_path = std::path::PathBuf::from("/tmp/plomid-sequence-test");
        let wal_path = std::path::PathBuf::from("/tmp/plomid-sequence-wal-test");
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        let result: SqlResult<()> = (|| {
            let engine = PlomidStorageEngine::create(&storage_path, &wal_path, 32)?;
            let mut executor = Executor::new(engine)?;
            executor.execute("CREATE SEQUENCE users_id_seq;")?;
            let QueryResult::Rows { rows, .. } =
                executor.execute("SELECT nextval('users_id_seq');")?
            else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Internal,
                    "expected nextval row",
                )));
            };
            assert_eq!(rows[0][0], Value::Int8(1));
            drop(executor);
            let engine = PlomidStorageEngine::open(&storage_path, &wal_path, 32)?;
            let mut executor = Executor::new(engine)?;
            let QueryResult::Rows { rows, .. } =
                executor.execute("SELECT nextval('users_id_seq');")?
            else {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Internal,
                    "expected nextval row",
                )));
            };
            assert_eq!(rows[0][0], Value::Int8(2));
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        assert!(result.is_ok(), "sequence test failed: {result:?}");
    }
}

#[cfg(test)]
mod vacuum_claim_tests {
    use super::*;
    use crate::maintenance::{maintenance_claim_key, MaintenanceClaim};
    use plomid_txn::PlomidStorageEngine;
    use std::sync::{Arc, Mutex};

    #[test]
    fn explicit_vacuum_waits_for_running_pass_instead_of_racing_it() {
        let storage_path = std::path::PathBuf::from("/tmp/plomid-vacuum-claim-test");
        let wal_path = std::path::PathBuf::from("/tmp/plomid-vacuum-claim-wal-test");
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        let result: SqlResult<()> = (|| {
            let engine = PlomidStorageEngine::create(&storage_path, &wal_path, 32)?;
            let shared = Arc::new(Mutex::new(engine));
            let mut setup = Executor::new_shared(Arc::clone(&shared))?;
            setup.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")?;
            setup.execute("INSERT INTO t VALUES (1, 10), (2, 20);")?;
            let root = shared.lock().expect("engine").root().to_path_buf();
            let key = maintenance_claim_key(&root, "plomid", "public.t");
            // Hold the single-flight claim as a running automatic pass would.
            let claim = MaintenanceClaim::try_acquire(key).expect("claim free");
            let vacuumed = Arc::new(Mutex::new(None));
            let vacuumed_thread = Arc::clone(&vacuumed);
            let shared_thread = Arc::clone(&shared);
            let started = std::time::Instant::now();
            let handle = std::thread::spawn(move || {
                let mut session = Executor::new_shared(shared_thread).expect("session");
                let outcome = session.execute("VACUUM t;");
                *vacuumed_thread.lock().expect("flag") = Some((outcome.is_ok(), started.elapsed()));
            });
            // Let the VACUUM arrive while the claim is held, then release:
            // it must have waited (not skipped, not conflicted) and then
            // succeeded on the freed slot.
            std::thread::sleep(std::time::Duration::from_millis(500));
            drop(claim);
            handle.join().expect("vacuum thread");
            let (ok, waited) = vacuumed.lock().expect("flag").expect("vacuum ran");
            assert!(ok, "VACUUM after claim release must succeed");
            assert!(
                waited >= std::time::Duration::from_millis(400),
                "VACUUM must wait for the running pass, waited={waited:?}"
            );
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&storage_path);
        let _ = std::fs::remove_dir_all(&wal_path);
        assert!(
            result.is_ok(),
            "explicit_vacuum_waits_for_running_pass failed: {result:?}"
        );
    }
}
