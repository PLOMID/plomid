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
use plomid_columnar::{
    compact, read_generation_rows, ColumnarFailPoint, ColumnarPublish, ColumnarStore,
    CompactionFailPoint, CompactionRequest, FlushConfig,
};
use plomid_core::{
    CatalogVersion, DatabaseId, ErrorKind, GenerationId, ObjectId, PlomidError, RowId, SchemaId,
    TableId, TableIdentity,
};
use plomid_sql::{
    save_catalog, Catalog, ColumnType, CreateStatement, InMemoryCatalog, QueryResult, Statement,
    Value,
};
use plomid_storage::{DatabaseLayout, DatabaseMetadata, LayoutSchemaMetadata};
use plomid_txn::{StorageEngine, StorageEngineTransaction};

use crate::catalog_fn::sequence_key;
use crate::encoding::decode_row;
use crate::encoding::encode_row;
use crate::encoding::{sql_table_key_range, storage_row};
use crate::error::{SqlError, SqlResult};
use crate::index::{
    index_value_bytes, index_value_prefix, prefix_end, register_constraint_indexes,
};

/// Identity of the connection's current database.
///
/// The catalog owns the authoritative database name set, so the identity is the
/// name's position in that deterministic order (offset by one, because zero is
/// never a valid identity). No database number is special-cased, and a database
/// created later receives the next free identity.
pub(crate) fn current_database_id(catalog: &InMemoryCatalog, database: &str) -> Option<DatabaseId> {
    catalog
        .database_names()
        .iter()
        .position(|name| name.eq_ignore_ascii_case(database))
        .map(|index| DatabaseId::new(index as u64 + 1))
}

/// Materializes the logical object directory of a relation that now exists.
///
/// Name resolution stays with the catalog: this reads the catalog's
/// authoritative identities and creates the directory that represents the
/// object on disk, so the tree reflects real database state. A relation whose
/// schema or identity cannot be resolved from the catalog is reported instead of
/// being invented from a path.
fn materialize_relation(
    catalog: &InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    qualified_name: &str,
) -> plomid_core::Result<()> {
    let database_id = current_database_id(catalog, database).ok_or_else(|| {
        PlomidError::with_detail(
            ErrorKind::NotFound,
            "current database is not registered in the catalog",
            format!("database={database}"),
        )
    })?;
    let (schema_name, _) = qualified_name
        .split_once('.')
        .unwrap_or(("public", qualified_name));
    let schema_id = catalog.schema_id(schema_name).ok_or_else(|| {
        PlomidError::with_detail(
            ErrorKind::NotFound,
            format!("schema \"{schema_name}\" does not exist"),
            format!("schema={schema_name}"),
        )
    })?;
    let table_id = catalog
        .get_table(qualified_name)
        .map(|table| table.table_id)
        .map_err(|_| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("table \"{qualified_name}\" does not exist"),
                format!("table={qualified_name}"),
            )
        })?;
    // The schema's own identity record is materialized first, so a table never
    // appears inside a schema that has no durable directory of its own.
    layout.ensure_schema_dir(database_id, schema_id)?;
    let schema_path = layout.schema_meta_path(database_id, schema_id);
    if !schema_path.is_file() {
        LayoutSchemaMetadata::new(schema_id, database_id)?.publish(&schema_path)?;
    }
    layout.ensure_table_in_schema(database_id, schema_id, table_id)?;
    Ok(())
}

/// Removes a dropped table from the authoritative generation catalog, then
/// lets the existing GC pass reclaim its now-unreachable immutable files and
/// payload slices.
///
/// Slice cleanup follows the same snapshot discipline as compaction: segment
/// ownership is captured before the catalog edit, and only segments of
/// generations the GC pass reports reclaimed (and still unreferenced) are
/// deleted, in WAL-logged transactions. Failures only warn: files and slices
/// of a dropped table are invisible once dereferenced, so an incomplete pass
/// leaks space but never breaks reads or recovery.
fn remove_columnar_object<E: StorageEngine>(engine: &mut E, table_id: TableId) {
    let store = match ColumnarStore::open(engine.root()) {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(target: "sql::maintenance", "dropped table generation removal failed error={error}");
            return;
        }
    };
    let manager = store.generations();
    let snapshot = plomid_columnar::slice_ownership_snapshot(&manager);
    if let Err(error) = manager.remove_object(ObjectId::new(table_id.get())) {
        if error.kind() != ErrorKind::NotFound {
            tracing::warn!(
                target: "sql::maintenance",
                table_id = table_id.get(),
                "dropped table generation removal failed error={error}"
            );
            return;
        }
    }
    match manager.gc() {
        Ok(outcome) => {
            let (segments, bytes) = plomid_columnar::delete_unreferenced_slices(
                &store,
                engine,
                &manager,
                &outcome.reclaimed,
                &snapshot,
            );
            if !segments.is_empty() {
                tracing::info!(
                    target: "sql::maintenance",
                    table_id = table_id.get(),
                    segments = segments.len(),
                    bytes,
                    "dropped table slices reclaimed",
                );
            }
        }
        Err(error) => {
            tracing::warn!(target: "sql::maintenance", table_id = table_id.get(), "dropped table generation GC failed error={error}");
        }
    }
}

fn remove_relation_directory(
    catalog: &InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    qualified_name: &str,
    table_id: TableId,
) {
    let Some(database_id) = current_database_id(catalog, database) else {
        return;
    };
    let (schema_name, _) = qualified_name
        .split_once('.')
        .unwrap_or(("public", qualified_name));
    let Some(schema_id) = catalog.schema_id(schema_name) else {
        return;
    };
    let dir = layout.table_dir_in_schema(database_id, schema_id, table_id);
    if dir.is_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn remove_schema_directory(
    catalog: &InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    schema_id: SchemaId,
) {
    let Some(database_id) = current_database_id(catalog, database) else {
        return;
    };
    let dir = layout.schema_dir(database_id, schema_id);
    if dir.is_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Materializes the logical object directory of a schema that now exists.
fn materialize_schema(
    catalog: &InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    schema_name: &str,
) -> plomid_core::Result<()> {
    let database_id = current_database_id(catalog, database).ok_or_else(|| {
        PlomidError::with_detail(
            ErrorKind::NotFound,
            "current database is not registered in the catalog",
            format!("database={database}"),
        )
    })?;
    let schema_id = catalog.schema_id(schema_name).ok_or_else(|| {
        PlomidError::with_detail(
            ErrorKind::NotFound,
            format!("schema \"{schema_name}\" does not exist"),
            format!("schema={schema_name}"),
        )
    })?;
    layout.ensure_schema_dir(database_id, schema_id)?;
    let record = LayoutSchemaMetadata::new(schema_id, database_id)?;
    let path = layout.schema_meta_path(database_id, schema_id);
    if !path.is_file() {
        record.publish(&path)?;
    }
    Ok(())
}

/// Materializes the logical object directory of a database that now exists.
///
/// The database identity record is published only when absent, so
/// re-materializing an existing database never rewrites durable metadata.
fn materialize_database(
    catalog: &InMemoryCatalog,
    layout: &DatabaseLayout,
    name: &str,
) -> plomid_core::Result<DatabaseId> {
    let database_id = current_database_id(catalog, name).ok_or_else(|| {
        PlomidError::with_detail(
            ErrorKind::NotFound,
            "database is not registered in the catalog",
            format!("database={name}"),
        )
    })?;
    layout.ensure_database_dir(database_id)?;
    let meta_path = layout.database_meta_path(database_id);
    if !meta_path.is_file() {
        DatabaseMetadata::new(database_id)?.publish(&meta_path)?;
    }
    Ok(database_id)
}

/// Materializes the logical object tree of a session's default database.
///
/// The catalog registers a default database and a default schema as real
/// logical objects, so a session that can already resolve `users` inside
/// `public` must observe both of them on disk. Materialization is idempotent
/// and never rewrites an existing record, so opening a session against an
/// existing database is a read-only no-op.
pub(crate) fn materialize_session_defaults(
    catalog: &InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
) -> plomid_core::Result<()> {
    materialize_database(catalog, layout, database)?;
    // The default schema is the search path's first real schema; the catalog
    // owns that list, so the session never invents a schema name.
    let default_schema = catalog
        .search_path()
        .iter()
        .find(|schema| catalog.has_schema(schema))
        .cloned();
    if let Some(schema) = default_schema {
        materialize_schema(catalog, layout, database, &schema)?;
    }
    Ok(())
}

/// Materializes one SQL table into an immutable generation and compacts it.
///
/// This is the SQL integration point for the existing columnar and compaction
/// machinery. Nothing new is introduced here: the table's committed rows are
/// read through the existing storage scan, mapped by the existing SQL decoder
/// bridge, and handed to the existing columnar flush, which publishes through
/// the existing `GenerationManager`. Compaction then runs over the generations
/// that exist for the table.
///
/// Identity comes from the catalog, never from a filesystem path, so the
/// published generation lands under the table's own
/// `objects/databases/DB-*/schemas/S-*/tables/T-*/generations/GEN-*` directory.
/// Builds the index-generation input of one table: every visible row's indexed
/// key paired with its RowId.
///
/// The bridge inverts `row_to_fields` on the indexed position so the generated
/// B+Tree carries exactly the key bytes the SQL index maintenance path writes
/// (`index_value_bytes`). Keys preserve row order — which is the same order the
/// storage snapshot produced — so the build is deterministic.
fn index_generation_rows(
    rows: &[plomid_storage::Row],
    column: usize,
) -> plomid_core::Result<Vec<(Vec<u8>, RowId)>> {
    use plomid_storage::Field;

    let mut out = Vec::with_capacity(rows.len());
    for (position, row) in rows.iter().enumerate() {
        let field = row.fields().get(column).ok_or_else(|| {
            PlomidError::new(ErrorKind::Corruption, "row has fewer columns than catalog")
        })?;
        let value = match field {
            Field::Null => Value::Null,
            Field::Integer(number) => Value::Int8(*number),
            Field::Bytes(bytes) => Value::Bytea(bytes.clone()),
            Field::String(text) => Value::Text(text.clone()),
        };
        out.push((index_value_bytes(&value), RowId::new(position as u64 + 1)));
    }
    Ok(out)
}

/// Refreshes the persistent index generations of one table when new data is
/// materialized.
///
/// This is the automatic maintenance boundary. It runs only at the explicit
/// VACUUM lifecycle event — never as part of DML — so ordinary writes remain
/// the normal WAL + MVCC + storage path with no per-write index rebuilding.
///
/// The engine converges both triggers into one lifecycle: this function calls
/// the idempotent automatic endpoint, so a repeated boundary over an unchanged
/// source data state reports `AlreadyCurrent` and writes nothing.
#[allow(clippy::too_many_arguments)]
fn maintain_index_generations(
    engine_root: &std::path::Path,
    catalog: &InMemoryCatalog,
    identity: TableIdentity,
    qualified_name: &str,
    source_data_generation: GenerationId,
    rows: &[plomid_storage::Row],
) -> plomid_core::Result<()> {
    use plomid_index::IndexGenerationStore;

    let table = catalog
        .get_table(qualified_name)
        .map_err(|_| {
            PlomidError::new(
                ErrorKind::NotFound,
                format!("table \"{qualified_name}\" not found"),
            )
        })?
        .clone();
    let definitions = catalog.indexes_for_table(qualified_name);
    if definitions.is_empty() {
        return Ok(());
    }

    let index_store = IndexGenerationStore::new(engine_root);
    for definition in &definitions {
        // The SQL write-time index path evaluates expression keys directly.
        // The compacted generation bridge currently accepts only a physical
        // column position; skipping expression indexes here prevents it from
        // publishing a generation for the wrong key semantics.
        if definition.expression.is_some() {
            continue;
        }
        // Composite tuple keys have no single-column encoding: bridging only
        // the first column would publish a generation the write path (tuple
        // keys) never reads. Skip until the bridge accepts column lists.
        if crate::index::index_columns(definition).len() != 1 {
            continue;
        }
        let column = match table.column_index(&definition.column) {
            Ok(position) => position,
            // An index over a dropped column is inert: it neither reads rows
            // nor claims a generation.
            Err(_) => continue,
        };
        let inputs = index_generation_rows(rows, column)?;
        let _ = index_store.request_automatic_generation(
            identity,
            definition.index_id,
            source_data_generation,
            &inputs,
        )?;
    }
    Ok(())
}

///
/// Work is skipped when the table is already minimal and current: a single
/// published generation holding exactly the visible rows. That is what keeps
/// repeated `VACUUM` from manufacturing generations without cause.
/// What one `vacuum_table` run established about a table's published state.
///
/// The freshness hook consumes this: only runs that prove the published
/// generation covers the current committed rows may mark the table clean.
/// An empty table keeps prior generations (which may hold deleted rows), so
/// that outcome must never mark anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VacuumOutcome {
    /// A fresh generation was published and is current.
    PublishedFresh(plomid_core::GenerationId),
    /// No publication was needed: the lone existing generation already holds
    /// exactly the visible rows (verified by content comparison).
    AlreadyCurrent(plomid_core::GenerationId),
    /// Nothing was published and currency was not proven (empty input keeps
    /// prior generations, which may still hold deleted rows).
    NoChange,
}

/// Phase accumulators for one table-maintenance pass (see `update_stmt`:
/// accumulators only, one DEBUG event per pass, so multi-minute builds don't
/// pay per-row log I/O).
#[derive(Default)]
struct MaintainPhaseUs {
    snapshot_us: u64,
    compare_us: u64,
    flush_us: u64,
    compact_us: u64,
    index_us: u64,
}

#[allow(clippy::too_many_arguments)]
fn vacuum_table<E: StorageEngine>(
    store: &ColumnarStore,
    engine: &mut E,
    catalog: &InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    qualified_name: &str,
    fail_at: ColumnarFailPoint,
    phases: &mut MaintainPhaseUs,
) -> plomid_core::Result<VacuumOutcome> {
    let database_id = current_database_id(catalog, database).ok_or_else(|| {
        PlomidError::with_detail(
            ErrorKind::NotFound,
            "current database is not registered in the catalog",
            format!("database={database}"),
        )
    })?;
    let (schema_name, _) = qualified_name
        .split_once('.')
        .unwrap_or(("public", qualified_name));
    let schema_id = catalog.schema_id(schema_name).ok_or_else(|| {
        PlomidError::with_detail(
            ErrorKind::NotFound,
            format!("schema \"{schema_name}\" does not exist"),
            format!("schema={schema_name}"),
        )
    })?;
    let table_id = catalog.get_table(qualified_name)?.table_id;
    let identity = TableIdentity::new(database_id, schema_id, table_id);
    let object_id = ObjectId::new(table_id.get());
    // Reconcile layout before publication. When a table was dropped and the
    // layout now hosts a newer table under the same storage identity, the
    // catalog's stale record must not claim generations that are not part of
    // this table's published state.
    let laid_out_object = layout
        .read_table_meta_in_schema(database_id, schema_id, table_id)
        .map(|meta| meta.object_id)
        .ok();
    let manager = store.generations();

    // Reader lifetime for the whole vacuum: pin the published catalog state
    // this run observes, so a concurrent release/reclamation pass retains
    // every generation this run may still read (inputs) or delete (slices of
    // reclaimed generations only after the proof). A missing publication
    // pointer (first generation not yet flushed) means there is nothing to
    // pin; any other acquisition failure aborts rather than running unpinned.
    let _vacuum_guard = match manager.reader() {
        Ok(reader) => Some(reader),
        Err(error) if error.kind() == plomid_core::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    // The table's committed rows, read over the same key range the SQL DML path
    // uses. The range is table-scoped, so another table's rows can never be
    // selected, and the scan observes the committed MVCC state.
    let (start, end) = sql_table_key_range(qualified_name);
    let phase_started = std::time::Instant::now();
    let rows = ColumnarStore::snapshot_rows_in_range(engine, &start, &end, |_key, value| {
        let values = decode_row(value)?;
        Ok(Some(storage_row(&values)))
    })?;
    phases.snapshot_us += phase_started.elapsed().as_micros() as u64;
    if rows.is_empty() {
        // An empty table keeps whatever generations it already has rather than
        // gaining an empty one. Currency is NOT proven here: retained
        // generations may still hold rows deleted since publication, so the
        // freshness hook must not mark this outcome clean.
        return Ok(VacuumOutcome::NoChange);
    }

    // Publication state is absent until the first generation lands; treat the
    // missing catalog as a table with no generations yet.
    //
    // Generation publication is globally unique, not per table, so the next
    // identity is the maximum over every owned generation plus one. A flush
    // for table B must never reuse or replace a generation that table A
    // published, which is exactly what per-table isolation requires.
    //
    // The maximum unions the published catalog with the on-disk inventory:
    // a kill between a generation's files landing and the catalog pointer
    // advancing orphans that identity on disk (see the publish guard, which
    // refuses to replace a durable generation). Sizing from the catalog
    // alone would re-offer the orphaned identity and fail every later
    // VACUUM permanently; sizing over both can only move forward, which the
    // guard still verifies. A scan failure degrades to the catalog maximum:
    // the worst case is the historical Conflict, never a replacement.
    let (existing, global_max, published_current): (Vec<GenerationId>, u64, Option<GenerationId>) =
        match manager.load() {
            Ok(published) => {
                let record = published.records().iter().find(|record| {
                    record.object_id == object_id && Some(record.object_id) == laid_out_object
                });
                let existing = record
                    .map(|record| record.generation_ids())
                    .unwrap_or_default();
                let published_current = record.map(|record| record.current_generation);
                let published_max = published
                    .records()
                    .iter()
                    .flat_map(|record| record.generation_ids())
                    .map(|generation| generation.get())
                    .max()
                    .unwrap_or(0);
                let durable_max = plomid_storage::discover_generation_ids(engine.root())
                    .map(|ids| ids.into_iter().map(|id| id.get()).max().unwrap_or(0))
                    .unwrap_or(0);
                (existing, published_max.max(durable_max), published_current)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => (Vec::new(), 0, None),
            Err(error) => return Err(error),
        };
    let column_types = ColumnarStore::infer_column_types(&rows);

    // Already minimal and current: one generation that already holds exactly
    // the visible rows. Nothing to materialize and nothing to merge.
    //
    // The catalog record is also the guard against unbalanced retained history:
    // the object must own at least one generation to be compactable, and a
    // lone retained holdover that is not the current generation is never the
    // minimal representation.
    let record_current_missing = published_current.is_none() && !existing.is_empty();
    if existing.len() == 1 && !record_current_missing {
        let phase_started = std::time::Instant::now();
        let current = read_generation_rows(store, engine, manager, object_id, existing[0])?;
        let rows_equal = current == rows;
        phases.compare_us += phase_started.elapsed().as_micros() as u64;
        if rows_equal {
            // The data state is already the current published generation, so
            // indexes must also be refreshed against that same stable state
            // before returning. This is the automatic boundary — not DML.
            let phase_started = std::time::Instant::now();
            maintain_index_generations(
                engine.root(),
                catalog,
                identity,
                qualified_name,
                existing[0],
                &rows,
            )?;
            phases.index_us += phase_started.elapsed().as_micros() as u64;
            return Ok(VacuumOutcome::AlreadyCurrent(existing[0]));
        }
    }

    // The publication pointer is absent until the first generation lands.
    // The first vacuum of a database seeds it from the same identity the
    // columnar tests use, so every later publication stays monotonic.
    let (pointer, _first_publication) = match manager.pointer() {
        Ok(pointer) => (pointer, false),
        Err(error) if error.kind() == ErrorKind::NotFound => (
            plomid_storage::PublicationPointer::new(
                GenerationId::new(1),
                CatalogVersion::new(1),
                GenerationId::new(1),
                plomid_core::Lsn::new(1),
            )?,
            true,
        ),
        Err(error) => return Err(error),
    };
    // Bring the visible rows into an immutable generation. The identity is
    // strictly greater than every generation any object owns, so a published
    // generation is never replaced or reused even across tables.
    let generation = GenerationId::new(global_max + 1);
    let publish = ColumnarPublish::new(
        object_id,
        ColumnarStore::columnar_schema(schema_id, CatalogVersion::new(1), &column_types)?,
        generation,
        pointer.storage_generation,
        pointer.checkpoint_lsn,
    )
    .with_identity(identity);
    let segment_id = store.allocate_segment_id();
    let phase_started = std::time::Instant::now();
    store.flush_rows_with_id(
        engine,
        &rows,
        &column_types,
        segment_id,
        &FlushConfig::default(),
        &publish,
        fail_at,
    )?;
    phases.flush_us += phase_started.elapsed().as_micros() as u64;

    // Merge the generations that now exist. `compact` selects its own inputs
    // from the published catalog and releases them only when live transaction
    // state proves no snapshot can still require them. The vacuum-level reader
    // guard ends here: it pinned the pre-flush catalog across this run's own
    // reads, but must not span the compaction's release+reclamation (same
    // self-pinning hazard as inside `compact`, which holds its own guard
    // across input verification and drops it before releasing).
    drop(_vacuum_guard);
    let (active, last_committed) = engine.mvcc_safety()?;
    let request = CompactionRequest {
        identity,
        object_id,
        column_types,
        config: FlushConfig::default(),
        generation: GenerationId::new(generation.get() + 1),
        storage_generation: pointer.storage_generation,
        checkpoint_lsn: pointer.checkpoint_lsn,
    };
    let phase_started = std::time::Instant::now();
    compact(
        store,
        engine,
        &request,
        &rows,
        &active,
        last_committed,
        CompactionFailPoint::None,
        true,
    )?;
    phases.compact_us += phase_started.elapsed().as_micros() as u64;
    // The compaction just established a new stable data state for this table.
    // Indexes rebuild against the generation the table now observes: the one
    // compaction published. Re-reading the authoritative record is what makes
    // the source generation stable rather than guessed.
    let outcome_generation = manager.load().ok().and_then(|published| {
        published
            .records()
            .iter()
            .find(|record| {
                record.object_id == object_id && Some(record.object_id) == laid_out_object
            })
            .map(|record| record.current_generation)
    });
    if let Some(source_data_generation) = outcome_generation {
        let phase_started = std::time::Instant::now();
        maintain_index_generations(
            engine.root(),
            catalog,
            identity,
            qualified_name,
            source_data_generation,
            &rows,
        )?;
        phases.index_us += phase_started.elapsed().as_micros() as u64;
        Ok(VacuumOutcome::PublishedFresh(source_data_generation))
    } else {
        // Publication succeeded but the authoritative pointer could not be
        // re-read: without the current generation identity nothing can be
        // marked clean, so this degrades to NoChange (row path until a later
        // vacuum proves currency).
        Ok(VacuumOutcome::NoChange)
    }
}

/// Materializes one table into its maintained generation.
///
/// This is the single table-scoped maintenance entry point. Both triggers
/// converge here:
///
/// ```text
/// explicit VACUUM ─┐
///                  ├─→ maintain_table ─→ data generation ─→ index generation
/// automatic pass ──┘
/// ```
///
/// The explicit `VACUUM` command and the automatic maintenance pass therefore
/// share one materialization path and, through it, the one authoritative
/// index-generation engine — there is no second builder for either trigger.
pub(crate) fn maintain_table<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    qualified_name: &str,
    fail_at: ColumnarFailPoint,
) -> plomid_core::Result<()> {
    let store = ColumnarStore::open(engine.root())?;
    // Segment payloads are durable while the store's counter is not, so the
    // store must never hand out an identity a persisted segment still uses.
    store.recover_segment_ids(engine)?;
    // Freshness hook: record the write count before the build scan. If no
    // commit lands inside the build window, the published generation is
    // proven to cover every committed write and all sessions may read it;
    // otherwise the table stays on the row path. Both explicit VACUUM and
    // automatic passes converge here, so both establish freshness identically.
    let key = crate::columnar_freshness::freshness_key(engine.root(), database, qualified_name);
    let writes_before = crate::columnar_freshness::writes_since_boot(&key);
    let perf = tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf");
    let pass_started = std::time::Instant::now();
    let mut phases = MaintainPhaseUs::default();
    let outcome = vacuum_table(
        &store,
        engine,
        catalog,
        layout,
        database,
        qualified_name,
        fail_at,
        &mut phases,
    )?;
    let outcome_label = match outcome {
        VacuumOutcome::PublishedFresh(_) => "published_fresh",
        VacuumOutcome::AlreadyCurrent(_) => "already_current",
        VacuumOutcome::NoChange => "no_change",
    };
    match outcome {
        VacuumOutcome::PublishedFresh(generation) | VacuumOutcome::AlreadyCurrent(generation) => {
            crate::columnar_freshness::try_mark_published(&key, generation, writes_before);
        }
        VacuumOutcome::NoChange => {}
    }
    if perf {
        tracing::debug!(
            target: "plomid::perf",
            event = "maintenance_pass",
            table = qualified_name,
            outcome = outcome_label,
            total_us = pass_started.elapsed().as_micros() as u64,
            snapshot_us = phases.snapshot_us,
            compare_us = phases.compare_us,
            flush_us = phases.flush_us,
            compact_us = phases.compact_us,
            index_us = phases.index_us,
        );
    }
    Ok(())
}

/// Resolves the tables a `VACUUM [table]` statement covers.
///
/// Targets are resolved through the catalog, which stays the logical authority:
/// a named relation is resolved by the catalog's own name resolution, and
/// `VACUUM` with no relation covers the tables the catalog knows. Tables are
/// never discovered by scanning the filesystem. Both the statement's execution
/// and any post-publication step (such as refreshing a derived runtime index)
/// use this one resolution, so they can never disagree about which tables the
/// statement addressed.
pub(crate) fn vacuum_targets(
    catalog: &InMemoryCatalog,
    table: Option<&str>,
) -> SqlResult<Vec<String>> {
    match table {
        Some(name) => Ok(vec![catalog.resolve_table_name(name)?]),
        None => Ok(catalog.table_names()),
    }
}

/// Bound for an explicit `VACUUM` waiting on a running automatic pass for
/// the same table. Passes complete in seconds at small scale and minutes at
/// multi-million-row scale, and always release (RAII, panic-safe); the bound
/// only fires on genuine pathology, where failing loudly beats running an
/// uncoordinated pass or hanging. After the wait the pass runs through the
/// normal minimality check, so work a just-finished pass already covered is
/// verified cheaply rather than rebuilt.
const VACUUM_CLAIM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Executes `VACUUM [table]`.
///
/// Targets are resolved through the catalog, which stays the logical authority:
/// a named relation is resolved by the catalog's own name resolution, and
/// `VACUUM` with no relation covers the tables the catalog knows. Tables are
/// never discovered by scanning the filesystem.
///
/// Before doing any work, the existing catalog and layout are reconciled: a
/// table the catalog knows must exist in the layout so generation publication
/// below never resurrects an identity that belongs to a dropped object.
fn execute_vacuum<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    table: Option<String>,
) -> SqlResult<QueryResult> {
    let targets = vacuum_targets(catalog, table.as_deref())?;
    for qualified_name in &targets {
        // Same-table maintenance has one owner: automatic passes (inline or
        // worker) hold the single-flight claim while they run, so an
        // explicit VACUUM waits for it (bounded) instead of racing it into
        // an allocator conflict. Waiting holds no engine locks, and holders
        // never wait, so this cannot deadlock; unrelated tables use
        // different claim keys and proceed concurrently. Explicit VACUUM
        // stays ungated (no burst check, no pass recording) — only the
        // mutual exclusion is shared with automatic maintenance.
        let claim_key =
            crate::maintenance::maintenance_claim_key(engine.root(), database, qualified_name);
        let _claim =
            crate::maintenance::MaintenanceClaim::acquire_wait(claim_key, VACUUM_CLAIM_TIMEOUT)
                .ok_or_else(|| {
                    SqlError::Storage(PlomidError::new(
                        ErrorKind::Conflict,
                        format!(
                            "VACUUM of \"{qualified_name}\" timed out waiting for a running maintenance pass"
                        ),
                    ))
                })?;
        maintain_table(
            engine,
            catalog,
            layout,
            database,
            qualified_name,
            ColumnarFailPoint::None,
        )?;
    }
    if !targets.is_empty() {
        tracing::info!(
            target: "sql::ddl",
            "vacuum tables={} database={}",
            targets.len(),
            database
        );
    }
    Ok(QueryResult::Created("VACUUM".into()))
}

pub fn execute_ddl<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    stmt: Statement,
) -> SqlResult<QueryResult> {
    match stmt {
        Statement::CreateTable {
            name,
            if_not_exists,
            temporary: _,
            columns,
            constraints,
        } => {
            tracing::info!(target: "sql::ddl", "create_table");
            // Resolve an unqualified table name against the session's
            // search_path so PostgreSQL's per-schema identity is honored:
            // an existing `public.customers` must not block the creation of
            // `plomid_test.customers`.
            let name = catalog.resolve_create_name(&name)?;
            if if_not_exists && catalog.has_table(&name) {
                return Ok(QueryResult::Created("CREATE TABLE".into()));
            }
            // SERIAL / BIGSERIAL / SMALLSERIAL columns auto-create a backing
            // sequence named `<table>_<column>_seq`, matching PostgreSQL. The
            // sequence uses the bare relation name so `nextval('seq')` resolves
            // it the same way as before schema-qualification.
            let base_name = name.rsplit('.').next().unwrap_or(&name);
            for col in &columns {
                if col.col_type.serial {
                    let seq_name = format!("{base_name}_{}_seq", col.name);
                    if !catalog.has_sequence(&seq_name) {
                        catalog.create_sequence(&seq_name)?;
                        let key = sequence_key(&seq_name);
                        let mut txn = engine.begin()?;
                        txn.put(&key, &0i64.to_le_bytes())?;
                        txn.commit()?;
                    }
                }
            }
            catalog.create_table(name.clone(), columns, constraints)?;
            // PRIMARY KEY / UNIQUE constraints get an authoritative index at
            // creation time, while the table is still provably empty, so no
            // backfill is needed. From then on the index answers point lookups
            // and uniqueness checks instead of a full table scan per statement.
            register_constraint_indexes(catalog, &name)?;
            // A table that exists must be visible on disk: materialize its
            // logical object directory through the identities the catalog just
            // assigned, so the tree reflects real database state.
            materialize_relation(catalog, layout, database, &name)?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("CREATE TABLE".into()))
        }
        Statement::CreateIndex {
            name,
            table,
            column,
            columns,
            expression,
            unique,
            if_not_exists,
            using,
            operator_class,
        } => {
            if if_not_exists && catalog.index(&name).is_some() {
                return Ok(QueryResult::Created("CREATE INDEX".into()));
            }
            execute_create_index(
                engine,
                catalog,
                name,
                table,
                column,
                columns,
                expression,
                unique,
                using,
                operator_class,
            )
        }
        Statement::CreateView {
            name,
            columns,
            query,
            or_replace,
        } => {
            // Resolve the view name through the session search_path so that
            // unqualified names land in the correct schema. This mirrors the
            // behavior of CREATE TABLE and honors PostgreSQL's per-schema
            // namespace isolation: an existing `public.customer_summary` must
            // not block the creation of `plomid_torture.customer_summary`.
            let name = catalog.resolve_create_name(&name)?;
            if or_replace && catalog.has_view(&name) {
                catalog.drop_view(&name)?;
            }
            catalog.create_view(name, columns, query)?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("CREATE VIEW".into()))
        }
        Statement::DropView {
            name,
            if_exists,
            cascade,
        } => {
            if !catalog.has_view(&name) {
                if if_exists {
                    return Ok(QueryResult::Created("DROP VIEW".into()));
                }
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::NotFound,
                    format!("view \"{name}\" does not exist"),
                )));
            }
            // RESTRICT (default) refuses when other objects depend on this view.
            if !cascade {
                if let Some(dependent) = catalog.find_view_dependency(&name) {
                    return Err(SqlError::Storage(PlomidError::with_detail(
                        ErrorKind::Catalog,
                        format!("cannot drop view \"{name}\" because other objects depend on it"),
                        format!("dependent={dependent}"),
                    )));
                }
            }
            catalog.drop_view(&name)?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("DROP VIEW".into()))
        }
        Statement::DropIndex { name, if_exists } => {
            execute_drop_index(engine, catalog, name, if_exists)
        }
        Statement::Create(create) => execute_create(engine, catalog, layout, database, create),
        Statement::DropTable {
            name,
            if_exists,
            cascade,
        } => execute_drop_table(engine, catalog, layout, database, name, if_exists, cascade),
        Statement::DropType {
            name,
            if_exists,
            cascade,
        } => execute_drop_type(engine, catalog, name, if_exists, cascade),
        Statement::DropDomain {
            name,
            if_exists,
            cascade,
        } => execute_drop_domain(engine, catalog, name, if_exists, cascade),
        Statement::DropFunction {
            name,
            args,
            if_exists,
            cascade,
        } => execute_drop_function(engine, catalog, name, args, if_exists, cascade),
        Statement::DropSchema {
            name,
            if_exists,
            cascade,
        } => execute_drop_schema(engine, catalog, layout, database, name, if_exists, cascade),
        Statement::DropSequence { name, if_exists } => {
            execute_drop_sequence(engine, catalog, name, if_exists)
        }
        Statement::AlterTableRename { table, new_name } => {
            let result = execute_alter_rename_table(engine, catalog, table.clone(), new_name);
            // The rename rewrote every row key through a direct engine
            // transaction (bypassing DML dirty-marking), so the old name's
            // claim must go; the new name starts dirty by absence.
            if result.is_ok() {
                forget_table_freshness(engine, catalog, database, &table);
            }
            result
        }
        Statement::AlterTableRenameColumn {
            table,
            old_name,
            new_name,
        } => {
            let result =
                execute_alter_rename_column(engine, catalog, table.clone(), old_name, new_name);
            if result.is_ok() {
                forget_table_freshness(engine, catalog, database, &table);
            }
            result
        }
        Statement::AlterTableAddColumn {
            table,
            column,
            if_not_exists,
        } => {
            if if_not_exists {
                let schema = catalog.get_table(&table)?;
                if schema.column_index(&column.name).is_ok() {
                    return Ok(QueryResult::Created("ALTER TABLE".into()));
                }
            }
            let result = execute_alter_add_column(engine, catalog, table.clone(), column);
            if result.is_ok() {
                forget_table_freshness(engine, catalog, database, &table);
            }
            result
        }
        Statement::AlterTableAddConstraint { table, constraint } => {
            let mut constraints = catalog.constraints(&table)?.to_vec();
            for column in &constraint.columns {
                catalog.get_table(&table)?.column_index(column)?;
            }
            constraints.push(constraint);
            catalog.set_constraints(&table, constraints)?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("ALTER TABLE".into()))
        }
        Statement::AlterTableDropColumn {
            table,
            column,
            cascade,
        } => {
            let result = execute_alter_drop_column(engine, catalog, table.clone(), column, cascade);
            if result.is_ok() {
                forget_table_freshness(engine, catalog, database, &table);
            }
            result
        }
        Statement::Set { name, value } => {
            if name.eq_ignore_ascii_case("search_path") {
                let path: Vec<String> = value
                    .split(',')
                    .map(|part| part.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|part| !part.is_empty())
                    .collect();
                catalog.set_search_path(path);
            }
            Ok(QueryResult::Set)
        }
        Statement::Show { name } => execute_show(catalog, name),
        Statement::Use { database } => execute_use(catalog, &database),
        Statement::GrantRole { role, member } => {
            catalog.grant_role(&role, &member)?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("GRANT ROLE".into()))
        }
        Statement::CommentOn { .. } => Ok(QueryResult::Created("COMMENT".into())),
        Statement::Vacuum { table } => execute_vacuum(engine, catalog, layout, database, table),
        Statement::Analyze { .. } => Ok(QueryResult::Created("ANALYZE".into())),
        Statement::Reindex => Ok(QueryResult::Created("REINDEX".into())),
        Statement::Lock => Ok(QueryResult::Created("LOCK TABLE".into())),
        Statement::Cluster => Ok(QueryResult::Created("CLUSTER".into())),
        Statement::RefreshMaterializedView { .. } => {
            Ok(QueryResult::Created("REFRESH MATERIALIZED VIEW".into()))
        }
        Statement::Truncate { .. } => Ok(QueryResult::Created("TRUNCATE TABLE".into())),
        _ => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            "unsupported statement",
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_create_index<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    name: String,
    table: String,
    column: String,
    columns: Vec<String>,
    expression: Option<plomid_sql::Expression>,
    unique: bool,
    _using: Option<String>,
    operator_class: Option<String>,
) -> SqlResult<QueryResult> {
    let table = catalog.resolve_table_name(&table)?;
    if catalog.index(&name).is_some() {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::AlreadyExists,
            format!("index \"{name}\" already exists"),
        )));
    }
    let schema = catalog.get_table(&table)?.clone();
    // Plain column indexes resolve keys by column position (one position per
    // indexed column for composite keys); expression indexes evaluate their
    // preserved AST against each row through the normal expression evaluator.
    let column_positions = if expression.is_none() {
        let ordered = if columns.is_empty() {
            vec![column.clone()]
        } else {
            columns.clone()
        };
        Some(
            ordered
                .iter()
                .map(|indexed| schema.column_index(indexed))
                .collect::<Result<Vec<_>, _>>()?,
        )
    } else {
        None
    };
    let entries = engine.scan(
        Some(format!("{table}:").as_bytes()),
        Some(format!("{table}:\u{10FFFF}").as_bytes()),
    )?;
    let mut txn = engine.begin()?;
    let mut seen: Vec<Vec<Value>> = Vec::new();
    for (row_key, bytes) in entries {
        let row = decode_row(&bytes)?;
        let values = if let Some(expr) = &expression {
            vec![crate::query::evaluate_expression(&row, &schema, expr)?]
        } else {
            column_positions
                .as_ref()
                .expect("plain column index")
                .iter()
                .map(|position| {
                    row.get(*position).cloned().ok_or_else(|| {
                        PlomidError::new(
                            ErrorKind::Corruption,
                            "row has fewer columns than catalog",
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        // Uniqueness is a tuple property: `(a, b)` conflicts as a whole, and
        // any NULL component voids the conflict (SQL NULL-not-equal),
        // mirroring the write-path tuple helpers.
        if unique
            && crate::index::tuple_conflicts_under_unique(&values)
            && seen
                .iter()
                .any(|existing| crate::index::tuples_equal(existing, &values))
        {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                "duplicate key violates unique index",
            )));
        }
        if unique && crate::index::tuple_conflicts_under_unique(&values) {
            seen.push(values.clone());
        }
        txn.put(
            &crate::index::index_entry_key_multi(&name, &values, &row_key),
            &row_key,
        )?;
    }
    txn.commit()?;
    drop(txn);
    catalog.create_index(
        name,
        table,
        column,
        columns,
        expression,
        unique,
        operator_class,
    )?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("CREATE INDEX".into()))
}

fn execute_drop_index<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    name: String,
    if_exists: bool,
) -> SqlResult<QueryResult> {
    let definition = catalog
        .indexes()
        .into_iter()
        .find(|index| index.name == name);
    let Some(definition) = definition else {
        if if_exists {
            return Ok(QueryResult::Created("DROP INDEX".into()));
        }
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::NotFound,
            format!("index \"{name}\" does not exist"),
        )));
    };
    let prefix = index_value_prefix(&name, None);
    let entries = engine.scan(Some(&prefix), Some(&prefix_end(&prefix)))?;
    let mut txn = engine.begin()?;
    for (key, _) in entries {
        txn.delete(&key)?;
    }
    txn.commit()?;
    drop(txn);
    catalog.drop_index(&definition.name)?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP INDEX".into()))
}

fn execute_create<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    create: CreateStatement,
) -> SqlResult<QueryResult> {
    match create {
        CreateStatement::Database { name } => {
            // The catalog's database name set is the authoritative record of
            // which databases exist for this engine, so creation is: register
            // the name, materialize its identity directory, and publish the
            // catalog. A name that already exists is refused rather than
            // silently re-created.
            if catalog
                .database_names()
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&name))
            {
                return Err(SqlError::Storage(PlomidError::with_detail(
                    ErrorKind::AlreadyExists,
                    format!("database \"{name}\" already exists"),
                    format!("database={name}"),
                )));
            }
            let mut names = catalog.database_names().to_vec();
            names.push(name.clone());
            catalog.set_database_names(names);
            let database_id = materialize_database(catalog, layout, &name)?;
            // A database resolves unqualified names against its default schema,
            // so the default schema is materialized as part of its creation.
            let default_schema = catalog
                .schema_names()
                .into_iter()
                .find(|schema| schema == "public");
            if default_schema.is_some() {
                materialize_schema(catalog, layout, &name, "public")?;
            }
            save_catalog(catalog, engine)?;
            tracing::info!(target: "sql::ddl", "create_database name={}", name);
            let _ = database_id;
            Ok(QueryResult::Created("CREATE DATABASE".into()))
        }
        CreateStatement::Schema {
            name,
            if_not_exists,
        } => {
            if if_not_exists && catalog.has_schema(&name) {
                return Ok(QueryResult::Created("CREATE SCHEMA".into()));
            }
            catalog.create_schema(name.clone())?;
            // A schema that exists must be visible on disk, bound to the
            // database that owns it.
            materialize_schema(catalog, layout, database, &name)?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("CREATE SCHEMA".into()))
        }
        CreateStatement::Role {
            name,
            login,
            password,
        } => {
            catalog.create_role(plomid_sql::RoleDefinition {
                name,
                superuser: false,
                inherit: true,
                create_role: false,
                create_database: false,
                can_login: login,
                replication: false,
                bypass_rls: false,
                connection_limit: -1,
                password,
                members: Vec::new(),
            })?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("CREATE ROLE".into()))
        }
        CreateStatement::View { name }
        | CreateStatement::MaterializedView { name }
        | CreateStatement::Index { name }
        | CreateStatement::Procedure { name }
        | CreateStatement::Trigger { name }
        | CreateStatement::Extension { name } => Err(SqlError::Storage(PlomidError::with_detail(
            ErrorKind::Unsupported,
            "CREATE object is not implemented yet",
            format!("object={name}"),
        ))),
        CreateStatement::Function {
            name,
            args,
            returns,
            language,
            body,
        } => {
            catalog.create_function(plomid_sql::FunctionDefinition {
                name,
                args: args
                    .into_iter()
                    .map(|a| plomid_sql::FunctionArgDefinition {
                        name: a.name,
                        data_type: a.data_type,
                    })
                    .collect(),
                returns,
                language,
                body,
            })?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("CREATE FUNCTION".into()))
        }
        CreateStatement::Type {
            name,
            labels,
            attributes,
        } => {
            catalog.create_type(plomid_sql::TypeDefinition {
                name,
                labels: labels.unwrap_or_default(),
                attributes: attributes.unwrap_or_default(),
            })?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("CREATE TYPE".into()))
        }
        CreateStatement::Domain {
            name,
            base_type,
            constraints,
        } => {
            catalog.create_domain(plomid_sql::DomainDefinition {
                name,
                base_type,
                constraints: constraints
                    .into_iter()
                    .map(|c| plomid_sql::DomainConstraintDefinition {
                        name: c.name,
                        check: c.check,
                    })
                    .collect(),
            })?;
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("CREATE DOMAIN".into()))
        }
        CreateStatement::Sequence { name } => {
            catalog.create_sequence(&name)?;
            let key = sequence_key(&name);
            if engine.get(&key)?.is_some() {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::AlreadyExists,
                    format!("sequence \"{name}\" already exists"),
                )));
            }
            let mut txn = engine.begin()?;
            txn.put(&key, &0i64.to_le_bytes())?;
            txn.commit()?;
            drop(txn);
            save_catalog(catalog, engine)?;
            Ok(QueryResult::Created("CREATE SEQUENCE".into()))
        }
        CreateStatement::Unsupported { object } => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            format!("unsupported CREATE object: {object}"),
        ))),
    }
}

/// Drops any columnar freshness claim for `table`.
///
/// Schema changes (rename, add/drop/rename column) alter the row layout or
/// key scope a published generation was built from, so the generation can no
/// longer serve the table even if its values look unchanged. Resolution
/// failure means the statement will fail anyway; there is nothing to forget.
fn forget_table_freshness<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    database: &str,
    table: &str,
) {
    if let Ok(qualified) = catalog.resolve_table_name(table) {
        crate::columnar_freshness::forget_table(&crate::columnar_freshness::freshness_key(
            engine.root(),
            database,
            &qualified,
        ));
    }
}

fn execute_drop_table<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    name: String,
    if_exists: bool,
    cascade: bool,
) -> SqlResult<QueryResult> {
    // Resolve the relation to its schema-qualified identity so storage scans
    // and view-dependency matching use the same fully-qualified name. When the
    // table is absent (for IF EXISTS) resolve to the intended namespace.
    let name = catalog
        .resolve_table_name(&name)
        .unwrap_or_else(|_| catalog.resolve_table_namespace(&name));
    if !catalog.has_table(&name) {
        if if_exists {
            return Ok(QueryResult::Created("DROP TABLE".into()));
        }
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::NotFound,
            format!("table \"{name}\" does not exist"),
        )));
    }
    // RESTRICT (default) refuses when other objects depend on this table.
    if !cascade {
        if let Some(dependent) = catalog.find_table_dependency(&name) {
            return Err(SqlError::Storage(PlomidError::with_detail(
                ErrorKind::Catalog,
                format!("cannot drop table \"{name}\" because other objects depend on it"),
                format!("dependent={dependent}"),
            )));
        }
    }
    // CASCADE: drop dependent views first.
    if cascade {
        let dependent_views: Vec<String> = catalog
            .view_names()
            .into_iter()
            .filter(|v| catalog.view_depends_on_table(v, &name))
            .collect();
        for view in &dependent_views {
            catalog.drop_view(view)?;
        }
    }
    // Every index's entries must go, including constraint backing indexes.
    let indexes = catalog.all_indexes_for_table(&name);
    let entries = engine.scan(
        Some(format!("{name}:").as_bytes()),
        Some(format!("{name}:\u{10FFFF}").as_bytes()),
    )?;
    let mut index_entries = Vec::new();
    for index in &indexes {
        let prefix = index_value_prefix(&index.name, None);
        index_entries.extend(
            engine
                .scan(Some(&prefix), Some(&prefix_end(&prefix)))?
                .into_iter()
                .map(|(key, _)| key),
        );
    }
    let mut txn = engine.begin()?;
    for (key, _) in entries {
        txn.delete(&key)?;
    }
    for key in index_entries {
        txn.delete(&key)?;
    }
    txn.commit()?;
    drop(txn);
    let table_id = catalog.get_table(&name)?.table_id;
    catalog.drop_table(&name)?;
    for index in indexes {
        let _ = catalog.drop_index(&index.name);
    }
    remove_columnar_object(engine, table_id);
    remove_relation_directory(catalog, layout, database, &name, table_id);
    // A recreated table must never inherit this table's freshness: drop the
    // entry by its already-resolved name (the catalog no longer resolves it
    // here), even though generation removal already forces a row-path fallback.
    crate::columnar_freshness::forget_table(&crate::columnar_freshness::freshness_key(
        engine.root(),
        database,
        &name,
    ));
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP TABLE".into()))
}

fn execute_drop_schema<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    layout: &DatabaseLayout,
    database: &str,
    name: String,
    if_exists: bool,
    cascade: bool,
) -> SqlResult<QueryResult> {
    // Catalog object names use dots (`schema.table`), so a schema-prefixed
    // object always starts with `<schema>.`.  (This is the separator that the
    // old `:`-based prefix missed, which is what left tables/indexes behind.)
    let prefix = format!("{name}.");
    let schema_id = catalog.schema_id(&name);

    if !catalog.has_schema(&name) {
        if if_exists {
            return Ok(QueryResult::Created("DROP SCHEMA".into()));
        }
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::NotFound,
            format!("schema \"{name}\" does not exist"),
        )));
    }

    // Gather every object contained in the schema.
    let tables: Vec<String> = catalog
        .table_names()
        .into_iter()
        .filter(|table| table.starts_with(&prefix))
        .collect();
    let table_ids: Vec<TableId> = tables
        .iter()
        .filter_map(|table| catalog.get_table(table).ok().map(|stored| stored.table_id))
        .collect();
    let views: Vec<String> = catalog
        .view_names()
        .into_iter()
        .filter(|view| view.starts_with(&prefix))
        .collect();
    let sequences: Vec<String> = catalog
        .sequence_names()
        .into_iter()
        .filter(|sequence| sequence.starts_with(&prefix))
        .collect();
    // Any index whose underlying table lives in this schema.
    let indexes: Vec<String> = catalog
        .indexes()
        .into_iter()
        .filter(|index| index.table.starts_with(&prefix))
        .map(|index| index.name)
        .collect();

    let has_objects =
        !tables.is_empty() || !views.is_empty() || !sequences.is_empty() || !indexes.is_empty();

    // PostgreSQL RESTRICT semantics: refuse to drop a schema that still owns
    // objects (tables, views, sequences, indexes depending on them).
    if has_objects && !cascade {
        return Err(SqlError::Storage(PlomidError::with_detail(
            ErrorKind::Catalog,
            format!("cannot drop schema \"{name}\" because other objects depend on it"),
            format!("schema={name}"),
        )));
    }

    // Collect every storage row owned by the dropped objects so they can be
    // deleted atomically in one transaction.
    let mut keys_to_delete: Vec<Vec<u8>> = Vec::new();

    // Table data rows are stored under `{table}:` for each `schema.table`.
    for table in &tables {
        let start = format!("{table}:").into_bytes();
        let end = format!("{table}:\u{10FFFF}").into_bytes();
        let rows = engine.scan(Some(&start), Some(&end))?;
        for (key, _) in rows {
            keys_to_delete.push(key);
        }
    }

    // Secondary index entries live under `__plomid_index:{index_name}:`.
    for index in &indexes {
        let prefix = index_value_prefix(index, None);
        let end = prefix_end(&prefix);
        let entries = engine.scan(Some(&prefix), Some(&end))?;
        for (key, _) in entries {
            keys_to_delete.push(key);
        }
    }

    // Sequence values live under `__plomid_sequence:{name}`.
    for sequence in &sequences {
        keys_to_delete.push(sequence_key(sequence));
    }

    if !keys_to_delete.is_empty() {
        let mut txn = engine.begin()?;
        for key in keys_to_delete {
            txn.delete(&key)?;
        }
        txn.commit()?;
        drop(txn);
    }

    // Update in-memory metadata consistently. All objects were gathered from
    // the live catalog, so these removals are expected to succeed.
    for table in &tables {
        catalog.drop_table(table)?;
        // Freshness must not survive the table: use the already-resolved
        // name directly (the catalog no longer resolves it here).
        crate::columnar_freshness::forget_table(&crate::columnar_freshness::freshness_key(
            engine.root(),
            database,
            table,
        ));
    }
    for index in &indexes {
        let _ = catalog.drop_index(index);
    }
    for view in &views {
        let _ = catalog.drop_view(view);
    }
    for sequence in &sequences {
        let _ = catalog.drop_sequence(sequence);
    }
    // Drop types and domains that belong to this schema (CASCADE only).
    let type_prefix = format!("{}.", name);
    let types_to_drop: Vec<String> = catalog
        .type_names()
        .into_iter()
        .filter(|t| t.starts_with(&type_prefix))
        .collect();
    let domains_to_drop: Vec<String> = catalog
        .domain_names()
        .into_iter()
        .filter(|d| d.starts_with(&type_prefix))
        .collect();
    for ty in &types_to_drop {
        let _ = catalog.drop_type(ty);
    }
    for domain in &domains_to_drop {
        let _ = catalog.drop_domain(domain);
    }
    catalog.drop_schema(&name)?;
    for table_id in table_ids {
        remove_columnar_object(engine, table_id);
    }
    if let Some(schema_id) = schema_id {
        remove_schema_directory(catalog, layout, database, schema_id);
    }
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP SCHEMA".into()))
}

fn execute_drop_sequence<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    name: String,
    if_exists: bool,
) -> SqlResult<QueryResult> {
    if !engine.get(&sequence_key(&name))?.is_some() {
        if if_exists {
            return Ok(QueryResult::Created("DROP SEQUENCE".into()));
        }
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::NotFound,
            format!("sequence \"{name}\" does not exist"),
        )));
    }
    let mut txn = engine.begin()?;
    txn.delete(&sequence_key(&name))?;
    txn.commit()?;
    drop(txn);
    catalog.drop_sequence(&name)?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP SEQUENCE".into()))
}

fn execute_drop_type<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    name: String,
    if_exists: bool,
    cascade: bool,
) -> SqlResult<QueryResult> {
    if !catalog.has_type(&name) {
        if if_exists {
            return Ok(QueryResult::Created("DROP TYPE".into()));
        }
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::NotFound,
            format!("type \"{name}\" does not exist"),
        )));
    }
    // RESTRICT (default) refuses when other objects depend on this type.
    if !cascade {
        if let Some(dependent) = catalog.find_type_dependency(&name) {
            return Err(SqlError::Storage(PlomidError::with_detail(
                ErrorKind::Catalog,
                format!("cannot drop type \"{name}\" because other objects depend on it"),
                format!("dependent={dependent}"),
            )));
        }
    }
    // CASCADE drops the table columns that use this type (PostgreSQL drops
    // each dependent column, removing a table only when its last column goes).
    if cascade {
        let oid = plomid_sql::custom_type_oid(&name);
        for (table_name, column_name) in dependent_columns_of_type(catalog, oid) {
            catalog.drop_column(&table_name, &column_name)?;
        }
    }
    catalog.drop_type(&name)?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP TYPE".into()))
}

fn execute_drop_domain<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    name: String,
    if_exists: bool,
    cascade: bool,
) -> SqlResult<QueryResult> {
    if !catalog.has_domain(&name) {
        if if_exists {
            return Ok(QueryResult::Created("DROP DOMAIN".into()));
        }
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::NotFound,
            format!("domain \"{name}\" does not exist"),
        )));
    }
    // RESTRICT (default) refuses when other objects depend on this domain.
    if !cascade {
        if let Some(dependent) = catalog.find_domain_dependency(&name) {
            return Err(SqlError::Storage(PlomidError::with_detail(
                ErrorKind::Catalog,
                format!("cannot drop domain \"{name}\" because other objects depend on it"),
                format!("dependent={dependent}"),
            )));
        }
    }
    // CASCADE drops the table columns that use this domain.
    if cascade {
        let oid = plomid_sql::custom_type_oid(&name);
        for (table_name, column_name) in dependent_columns_of_type(catalog, oid) {
            catalog.drop_column(&table_name, &column_name)?;
        }
    }
    catalog.drop_domain(&name)?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP DOMAIN".into()))
}

/// Collects `(table, column)` pairs whose column type resolves to the given
/// synthetic user-defined type OID.
fn dependent_columns_of_type(catalog: &InMemoryCatalog, oid: u32) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for table in catalog.tables() {
        for column in &table.columns {
            if column.col_type.type_oid.raw() == oid {
                pairs.push((table.name.clone(), column.name.clone()));
            }
        }
    }
    pairs
}

fn execute_drop_function<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    name: String,
    args: Vec<String>,
    if_exists: bool,
    cascade: bool,
) -> SqlResult<QueryResult> {
    if !catalog.has_function(&name, &args) {
        if if_exists {
            return Ok(QueryResult::Created("DROP FUNCTION".into()));
        }
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::NotFound,
            format!("function \"{name}\" does not exist"),
        )));
    }
    // RESTRICT (default) refuses when other objects depend on this function.
    if !cascade {
        if let Some(dependent) = catalog.find_function_dependency(&name, &args) {
            return Err(SqlError::Storage(PlomidError::with_detail(
                ErrorKind::Catalog,
                format!("cannot drop function \"{name}\" because other objects depend on it"),
                format!("dependent={dependent}"),
            )));
        }
    }
    catalog.drop_function(&name, &args)?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP FUNCTION".into()))
}

fn execute_alter_rename_table<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    table: String,
    new_name: String,
) -> SqlResult<QueryResult> {
    // Resolve the source to its schema-qualified key and keep the renamed table
    // in the same schema, matching `Catalog::rename_table`.
    let table = catalog.resolve_table_name(&table)?;
    let schema = table
        .split_once('.')
        .map(|(s, _)| s.to_string())
        .unwrap_or_else(|| "public".to_string());
    let resolved_new = if new_name.contains('.') {
        new_name.clone()
    } else {
        format!("{schema}.{new_name}")
    };
    let entries = engine.scan(
        Some(format!("{table}:").as_bytes()),
        Some(format!("{table}:\u{10FFFF}").as_bytes()),
    )?;
    let mut txn = engine.begin()?;
    for (key, value) in entries {
        let suffix = key
            .strip_prefix(format!("{table}:").as_bytes())
            .ok_or_else(|| PlomidError::new(ErrorKind::Corruption, "invalid table key"))?;
        let mut new_key = format!("{resolved_new}:").into_bytes();
        new_key.extend_from_slice(suffix);
        txn.put(&new_key, &value)?;
        txn.delete(&key)?;
    }
    txn.commit()?;
    drop(txn);
    catalog.rename_table(&table, resolved_new)?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP INDEX".into()))
}

fn execute_alter_rename_column<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    table: String,
    old_name: String,
    new_name: String,
) -> SqlResult<QueryResult> {
    catalog.rename_column(&table, &old_name, new_name)?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("ALTER TABLE".into()))
}

fn execute_alter_add_column<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    table: String,
    column: plomid_sql::ColumnDef,
) -> SqlResult<QueryResult> {
    let schema = catalog.get_table(&table)?;
    let entries = engine.scan(
        Some(format!("{table}:").as_bytes()),
        Some(format!("{table}:\u{10FFFF}").as_bytes()),
    )?;
    for (key, bytes) in entries {
        let mut row = decode_row(&bytes)?;
        row.push(Value::Null);
        let encoded = encode_row(&row)?;
        let mut txn = engine.begin()?;
        txn.put(&key, &encoded)?;
        txn.commit()?;
    }
    let _ = schema;
    catalog.add_column(&table, column)?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP INDEX".into()))
}

fn execute_alter_drop_column<E: StorageEngine>(
    engine: &mut E,
    catalog: &mut InMemoryCatalog,
    table: String,
    column: String,
    cascade: bool,
) -> SqlResult<QueryResult> {
    let schema = catalog.get_table(&table)?;
    let index = schema.column_index(&column)?;
    if index == 0 {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Conflict,
            "the row identity column cannot be dropped",
        )));
    }
    // RESTRICT (default) refuses when other objects depend on this column.
    if !cascade {
        if let Some(dependent) = catalog.find_column_dependency(&table, &column) {
            return Err(SqlError::Storage(PlomidError::with_detail(
                ErrorKind::Catalog,
                format!(
                    "cannot drop column \"{column}\" of table \"{table}\" because other objects depend on it"
                ),
                format!("dependent={dependent}"),
            )));
        }
    }
    if cascade {
        let dependent_views = catalog.find_views_referencing_column(&table, &column);
        for view in &dependent_views {
            catalog.drop_view(view)?;
        }
    }
    let entries = engine.scan(
        Some(format!("{table}:").as_bytes()),
        Some(format!("{table}:\u{10FFFF}").as_bytes()),
    )?;
    for (key, bytes) in entries {
        let mut row = decode_row(&bytes)?;
        row.remove(index);
        let encoded = encode_row(&row)?;
        let mut txn = engine.begin()?;
        txn.put(&key, &encoded)?;
        txn.commit()?;
    }
    catalog.drop_column(&table, &column)?;
    save_catalog(catalog, engine)?;
    Ok(QueryResult::Created("DROP TABLE".into()))
}

fn execute_show(catalog: &InMemoryCatalog, name: String) -> SqlResult<QueryResult> {
    let show_name = name.to_ascii_lowercase();
    if show_name == "schema_name" {
        return Ok(QueryResult::Rows {
            columns: vec!["schema_name".to_string()],
            column_types: vec![Some(ColumnType::text())],
            rows: vec![vec![Value::Text("public".into())]],
        });
    }
    if matches!(show_name.as_str(), "tables" | "table") {
        let rows = catalog
            .table_names()
            .into_iter()
            .map(|table| {
                let bare = table.rsplit('.').next().unwrap_or(&table).to_string();
                vec![Value::Text(bare)]
            })
            .collect();
        return Ok(QueryResult::Rows {
            columns: vec!["table_name".to_string()],
            column_types: vec![Some(ColumnType::text())],
            rows,
        });
    }
    if matches!(show_name.as_str(), "schema" | "schemas") {
        let mut schemas = catalog.schema_names();
        for table in catalog.table_names() {
            schemas.push(
                table
                    .split_once('.')
                    .map_or("public", |(schema, _)| schema)
                    .into(),
            );
        }
        schemas.sort_unstable();
        schemas.dedup();
        let rows = schemas
            .into_iter()
            .map(|schema| vec![Value::Text(schema)])
            .collect();
        return Ok(QueryResult::Rows {
            columns: vec!["schema".to_string()],
            column_types: vec![Some(ColumnType::text())],
            rows,
        });
    }
    if name.eq_ignore_ascii_case("search_path") {
        let value = catalog
            .search_path()
            .iter()
            .map(|schema| {
                if schema == "$user" {
                    "\"$user\"".to_string()
                } else {
                    schema.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(QueryResult::Rows {
            columns: vec![name],
            column_types: vec![Some(ColumnType::text())],
            rows: vec![vec![Value::Text(value)]],
        });
    }
    let value = match name.to_ascii_lowercase().as_str() {
        "transaction_read_only" => "off",
        "transaction_isolation" => "read committed",
        "standard_conforming_strings" => "on",
        "integer_datetimes" => "on",
        "timezone" => "UTC",
        "server_version" => "14.0",
        "application_name" => "plomid",
        "extra_float_digits" => "3",
        _ => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Unsupported,
                format!("unsupported SHOW parameter: {name}"),
            )))
        }
    };
    Ok(QueryResult::Rows {
        columns: vec![name],
        column_types: vec![Some(ColumnType::text())],
        rows: vec![vec![Value::Text(value.to_string())]],
    })
}

/// Validates a `USE` that reaches the statement dispatcher.
///
/// The catalog owns the authoritative database name set, so a name that is not
/// registered is refused instead of being invented. Switching the session's
/// current database is done by the executor, which owns session state; this
/// path exists so a `USE` executed inside a transaction is still validated
/// against the same authority.
fn execute_use(catalog: &InMemoryCatalog, database: &str) -> SqlResult<QueryResult> {
    if !catalog
        .database_names()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(database))
    {
        return Err(SqlError::Storage(PlomidError::with_detail(
            ErrorKind::NotFound,
            format!("database \"{database}\" does not exist"),
            format!("database={database}"),
        )));
    }
    Ok(QueryResult::Set)
}
