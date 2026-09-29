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
//! End-to-end SQL lifecycle tests for the logical database hierarchy.
//!
//! Each test drives the real SQL path (`CREATE DATABASE`, `USE`, `CREATE
//! SCHEMA`, `CREATE TABLE`, `INSERT`, `SELECT`) and then verifies that the
//! logical object tree reflects what the catalog recorded, that physical bytes
//! stay device-owned, and that identity survives a restart.

use plomid_core::{DatabaseId, SchemaId, TableId};
use plomid_executor::Executor;
use plomid_sql::{Catalog, QueryResult, Value};
use plomid_storage::DatabaseLayout;
use plomid_txn::PlomidStorageEngine;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Returns an isolated scratch directory for one test.
fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-sql-hierarchy-{label}-{}-{id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(path.with_extension("wal"));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root.with_extension("wal"));
}

/// Opens a storage engine rooted at `root`, creating it on first use.
fn create_engine(root: &Path) -> PlomidStorageEngine {
    let wal = root.with_extension("wal");
    PlomidStorageEngine::create(root, &wal, 32)
        .or_else(|_| PlomidStorageEngine::open(root, &wal, 32))
        .expect("create engine")
}

/// Opens a session against `root`.
fn open_session(root: &Path) -> Executor<PlomidStorageEngine> {
    Executor::new(create_engine(root)).expect("executor")
}

/// Reopens an existing engine through the recovery path.
///
/// Uses `open` (never `create`) so the test always exercises startup recovery
/// instead of an initialization path.
fn reopen_session(root: &Path) -> Executor<PlomidStorageEngine> {
    let wal = root.with_extension("wal");
    let engine = PlomidStorageEngine::open(root, &wal, 32).expect("reopen engine");
    Executor::new(engine).expect("executor")
}

/// Opens an existing database and returns its recovery report.
///
/// The report is produced by the engine's startup recovery, so asserting on it
/// proves the SQL startup path actually ran the authoritative recovery
/// pipeline rather than opening storage blindly.
fn open_recovery_report(root: &Path) -> plomid_wal::CheckpointReplayReport {
    let wal = root.with_extension("wal");
    let engine = PlomidStorageEngine::open(root, &wal, 32).expect("reopen engine");
    let report = engine
        .recovery_report()
        .expect("an opened engine carries a recovery report")
        .clone();
    drop(Executor::new(engine).expect("executor"));
    report
}

/// The `BIGINT` column values of every row, as integers.
fn ints(result: QueryResult) -> Vec<i64> {
    rows(result)
        .into_iter()
        .map(|row| match row.first() {
            Some(Value::Int2(number)) => i64::from(*number),
            Some(Value::Int4(number)) => i64::from(*number),
            Some(Value::Int8(number)) => *number,
            other => panic!("expected an integer column value, got {other:?}"),
        })
        .collect()
}

/// The second column of every row, as an optional integer (NULL preserved).
fn optional_ints(result: QueryResult) -> Vec<Option<i64>> {
    rows(result)
        .into_iter()
        .map(|row| match row.get(1) {
            Some(Value::Null) => None,
            Some(Value::Int2(number)) => Some(i64::from(*number)),
            Some(Value::Int4(number)) => Some(i64::from(*number)),
            Some(Value::Int8(number)) => Some(*number),
            other => panic!("expected an optional integer column, got {other:?}"),
        })
        .collect()
}

/// The text column of every row as a string, with NULL rendered as a marker.
fn texts(result: QueryResult) -> Vec<String> {
    rows(result)
        .into_iter()
        .map(|row| match row.first() {
            Some(Value::Null) => "<null>".to_string(),
            Some(Value::Text(text))
            | Some(Value::VarChar(text))
            | Some(Value::BpChar(text))
            | Some(Value::Name(text)) => text.clone(),
            other => panic!("expected a text column value, got {other:?}"),
        })
        .collect()
}

/// Creates the shared recovery fixture: database, table, and initial rows.
fn create_recovery_fixture(session: &mut Executor<PlomidStorageEngine>) {
    session
        .execute_all("CREATE DATABASE app")
        .expect("create database");
    session.execute_all("USE app").expect("use database");
    session
        .execute_all(
            "CREATE TABLE events (
                id BIGINT PRIMARY KEY,
                note TEXT,
                amount BIGINT
            )",
        )
        .expect("create table");
}

/// Returns the rows of a row-producing result, failing on anything else.
fn rows(result: QueryResult) -> Vec<Vec<Value>> {
    match result {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

/// Every materialized table, as (database, schema, table) identities.
///
/// Discovery walks the logical object tree, so a returned triple proves the
/// database and schema directories exist as real objects.
fn discover_tables(layout: &DatabaseLayout) -> Vec<(DatabaseId, SchemaId, TableId)> {
    let mut found = Vec::new();
    for database in layout.discover_database_ids().expect("databases") {
        for schema in layout.discover_schema_ids(database).expect("schemas") {
            for table in layout
                .discover_table_ids_in_schema(database, schema)
                .expect("tables")
            {
                found.push((database, schema, table));
            }
        }
    }
    found
}

/// Every regular file below `dir`, relative to `dir`.
fn files_below(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(dir) {
                found.push(relative.display().to_string());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn database_schema_table_survive_restart() {
    let root = scratch("lifecycle");
    let databases;
    let tables;

    // Phase 1: drive the real SQL lifecycle.
    {
        let mut session = open_session(&root);
        session
            .execute_all("CREATE DATABASE app")
            .expect("create database");
        session.execute_all("USE app").expect("use database");
        session
            .execute_all("CREATE SCHEMA analytics")
            .expect("create schema");
        session
            .execute_all(
                "CREATE TABLE analytics.events (
                    id BIGINT PRIMARY KEY,
                    \"timestamp\" BIGINT,
                    value BIGINT
                )",
            )
            .expect("create table");
        session
            .execute_all("INSERT INTO analytics.events VALUES (1, 100, 42), (2, 200, 84)")
            .expect("insert");

        let layout = session.layout().clone();
        databases = layout.discover_database_ids().expect("discover databases");
        tables = discover_tables(&layout);

        // The default database and the created database are both real objects.
        assert_eq!(
            databases,
            vec![DatabaseId::new(1), DatabaseId::new(2)],
            "the default database and the created database both exist"
        );
        for database in &databases {
            assert!(layout.database_dir(*database).is_dir());
            assert!(
                layout.database_meta_path(*database).is_file(),
                "database {database:?} has a durable identity record"
            );
        }

        // The table belongs to the schema the session named, not to `public`.
        assert_eq!(
            tables.len(),
            1,
            "exactly the created table exists: {tables:?}"
        );
        let (database_id, schema_id, table_id) = tables[0];
        assert_eq!(schema_id, SchemaId::new(2), "analytics owns the table");
        assert!(layout.schema_dir(database_id, schema_id).is_dir());
        assert!(layout.schema_meta_path(database_id, schema_id).is_file());
        assert!(layout
            .table_dir_in_schema(database_id, schema_id, table_id)
            .is_dir());
        assert!(layout
            .table_meta_path_in_schema(database_id, schema_id, table_id)
            .is_file());
        assert!(layout
            .table_hot_dir_in_schema(database_id, schema_id, table_id)
            .is_dir());
        assert!(layout
            .table_generations_dir_in_schema(database_id, schema_id, table_id)
            .is_dir());
        assert!(layout
            .table_indexes_dir_in_schema(database_id, schema_id, table_id)
            .is_dir());

        // Physical bytes never live under the logical table directory.
        let table_dir = layout.table_dir_in_schema(database_id, schema_id, table_id);
        assert_eq!(
            files_below(&table_dir),
            vec!["META.dat".to_string()],
            "the table directory holds only its identity record"
        );

        // No root-level legacy layout artifact is produced.
        for legacy in [
            "data",
            "database.db",
            "storage.db",
            "generation",
            "generations",
            "tables",
        ] {
            assert!(!root.join(legacy).exists(), "no root-level {legacy}");
        }

        // Physical storage stays device-owned.
        assert!(root.join("devices").is_dir());
        assert!(root.join("devices/D-00000000000000000001/packs").is_dir());
    }

    // Phase 2: restart and verify identity, data, and constraints.
    {
        let mut session = open_session(&root);
        session
            .execute_all("USE app")
            .expect("use database after restart");

        let reopened = session.layout().clone();
        assert_eq!(
            reopened.discover_database_ids().expect("databases"),
            databases,
            "database identities survive restart"
        );
        assert_eq!(
            discover_tables(&reopened),
            tables,
            "table identities survive restart"
        );

        let selected = rows(
            session
                .execute("SELECT id, value FROM analytics.events ORDER BY id")
                .expect("select"),
        );
        assert_eq!(
            selected,
            vec![
                vec![Value::Int8(1), Value::Int8(42)],
                vec![Value::Int8(2), Value::Int8(84)],
            ]
        );

        // The primary key is still enforced after restart.
        assert!(
            session
                .execute_all("INSERT INTO analytics.events VALUES (1, 999, 999)")
                .is_err(),
            "a duplicate primary key is rejected"
        );
    }

    cleanup(&root);
}
#[test]
fn table_directory_appears_only_after_table_creation() {
    let root = scratch("table-later");

    let mut session = open_session(&root);
    let layout = session.layout().clone();

    // A database with no tables has no table directories at all.
    assert_eq!(
        discover_tables(&layout),
        Vec::new(),
        "an empty database materializes no table directories"
    );
    let schema_tables = layout.schema_tables_dir(DatabaseId::new(1), SchemaId::new(1));
    assert!(
        schema_tables.is_dir(),
        "the default schema is a real logical object"
    );
    assert_eq!(
        std::fs::read_dir(&schema_tables).expect("read").count(),
        0,
        "the default schema holds no table directory yet"
    );

    session
        .execute_all("CREATE TABLE users (id BIGINT PRIMARY KEY, name TEXT)")
        .expect("create table");

    let tables = discover_tables(&layout);
    assert_eq!(tables.len(), 1, "creating a table materializes exactly one");
    let (database_id, schema_id, _table_id) = tables[0];
    assert_eq!(database_id, DatabaseId::new(1));
    assert_eq!(schema_id, SchemaId::new(1), "unqualified names use public");

    cleanup(&root);
}

#[test]
fn table_names_are_schema_scoped() {
    let root = scratch("scoped");
    let mut session = open_session(&root);

    session
        .execute_all("CREATE TABLE users (id BIGINT PRIMARY KEY)")
        .expect("create public.users");
    session
        .execute_all("CREATE SCHEMA analytics")
        .expect("create schema");
    session
        .execute_all("CREATE TABLE analytics.users (id BIGINT PRIMARY KEY, score BIGINT)")
        .expect("create analytics.users");

    let layout = session.layout().clone();
    let tables = discover_tables(&layout);
    assert_eq!(
        tables.len(),
        2,
        "the same name lives in two schemas: {tables:?}"
    );

    // Each table has its own identity and its own directory, and the two
    // directories are distinct even though both tables are named `users`.
    let public = tables
        .iter()
        .find(|(_, schema, _)| *schema == SchemaId::new(1))
        .expect("public.users");
    let analytics = tables
        .iter()
        .find(|(_, schema, _)| *schema == SchemaId::new(2))
        .expect("analytics.users");
    assert_ne!(public.2, analytics.2, "each table has its own identity");
    assert_ne!(
        layout.table_dir_in_schema(public.0, public.1, public.2),
        layout.table_dir_in_schema(analytics.0, analytics.1, analytics.2),
        "each table has its own directory"
    );
    assert_eq!(
        public.0, analytics.0,
        "both tables belong to the session's database"
    );

    // Qualified resolution picks the named schema, so the two tables really are
    // separate relations.
    let rows = rows(
        session
            .execute("SELECT count(*) FROM analytics.users")
            .expect("count analytics.users"),
    );
    assert_eq!(
        rows.len(),
        1,
        "analytics.users resolves after qualification"
    );

    cleanup(&root);
}

// ---------------------------------------------------------------------------
// Full SQL recovery: startup → recovery → SQL-ready
// ---------------------------------------------------------------------------

#[test]
fn committed_insert_update_delete_survive_recovery() {
    let root = scratch("recovery-dml");

    {
        let mut session = open_session(&root);
        create_recovery_fixture(&mut session);
        session
            .execute_all(
                "INSERT INTO events VALUES (1, 'one', 10), (2, NULL, 20), (3, 'three', 30)",
            )
            .expect("insert");
        session
            .execute_all("UPDATE events SET amount = 11, note = 'one-updated' WHERE id = 1")
            .expect("update");
        session
            .execute_all("DELETE FROM events WHERE id = 3")
            .expect("delete");
        // The pre-crash view, to compare against the recovered view exactly.
        assert_eq!(
            ints(
                session
                    .execute("SELECT id FROM events ORDER BY id")
                    .expect("ids")
            ),
            vec![1, 2]
        );
    }

    // Recovery: the same logical state must come back.
    let mut session = reopen_session(&root);
    session.execute_all("USE app").expect("use database");
    assert_eq!(
        ints(
            session
                .execute("SELECT id FROM events ORDER BY id")
                .expect("ids")
        ),
        vec![1, 2],
        "the deleted row must not reappear and no row may be lost"
    );
    assert_eq!(
        optional_ints(
            session
                .execute("SELECT id, amount FROM events ORDER BY id")
                .expect("amounts")
        ),
        vec![Some(11), Some(20)],
        "the updated value survives and the untouched value is unchanged"
    );
    assert_eq!(
        texts(
            session
                .execute("SELECT note FROM events ORDER BY id")
                .expect("notes")
        ),
        vec!["one-updated".to_string(), "<null>".to_string()],
        "variable-width text and NULL survive recovery"
    );

    cleanup(&root);
}

#[test]
fn startup_recovery_produces_a_report_and_validates_physical_state() {
    let root = scratch("recovery-report");

    {
        let mut session = open_session(&root);
        create_recovery_fixture(&mut session);
        session
            .execute_all("INSERT INTO events VALUES (1, 'one', 10)")
            .expect("insert");
    }

    let report = open_recovery_report(&root);
    assert_eq!(
        report.state,
        plomid_wal::RecoveryState::Ready,
        "startup recovery reached the terminal Ready state"
    );
    assert!(
        !report.storage_generation.is_zero(),
        "recovery validated a durable storage generation"
    );
    assert!(
        report.applied_records > 0,
        "the committed WAL after the boundary was replayed"
    );
    assert!(
        report.applied_transactions > 0,
        "at least the committed transaction was applied"
    );
    assert_eq!(
        report.crash_tail_boundary, None,
        "a cleanly closed log has no incomplete crash tail"
    );

    cleanup(&root);
}

#[test]
fn uncommitted_transaction_does_not_become_committed_by_recovery() {
    let root = scratch("recovery-uncommitted");

    {
        let mut session = open_session(&root);
        create_recovery_fixture(&mut session);
        session
            .execute_all("INSERT INTO events VALUES (1, 'kept', 10)")
            .expect("committed insert");
        // A rolled-back transaction must leave no durable trace. BEGIN and its
        // terminator must share one batch, which is the executor's transaction
        // contract: an explicit transaction is contained in a single call.
        session
            .execute_all("BEGIN; INSERT INTO events VALUES (2, 'rolled-back', 20); ROLLBACK")
            .expect("rolled-back transaction");
        assert_eq!(
            ints(
                session
                    .execute("SELECT id FROM events ORDER BY id")
                    .expect("ids")
            ),
            vec![1],
            "the rolled-back row is not visible before restart"
        );
    }

    let mut session = reopen_session(&root);
    session.execute_all("USE app").expect("use database");
    assert_eq!(
        ints(
            session
                .execute("SELECT id FROM events ORDER BY id")
                .expect("ids")
        ),
        vec![1],
        "an uncommitted transaction must never become committed through recovery"
    );

    cleanup(&root);
}

#[test]
fn checkpoint_then_more_wal_recovers_both_segments_of_history() {
    let root = scratch("recovery-checkpoint");

    {
        let mut session = open_session(&root);
        create_recovery_fixture(&mut session);
        session
            .execute_all("INSERT INTO events VALUES (1, 'before', 10)")
            .expect("insert before checkpoint");
        // A real checkpoint through the engine's own durability path, so the
        // recovered prefix is a published checkpoint rather than LSN 0.
        session
            .engine_mut()
            .checkpoint()
            .expect("checkpoint the engine");
        session
            .execute_all("INSERT INTO events VALUES (2, 'after', 20)")
            .expect("insert after checkpoint");
    }

    let wal = root.with_extension("wal");
    let engine = PlomidStorageEngine::open(&root, &wal, 32).expect("reopen engine");
    let report = engine.recovery_report().expect("recovery report").clone();
    assert_eq!(report.state, plomid_wal::RecoveryState::Ready);

    let mut session = Executor::new(engine).expect("executor");
    session.execute_all("USE app").expect("use database");
    // The checkpointed write and the write after the checkpoint are both present.
    assert_eq!(
        ints(
            session
                .execute("SELECT id FROM events ORDER BY id")
                .expect("ids")
        ),
        vec![1, 2],
        "recovery must rejoin the checkpointed prefix with the WAL after it"
    );
    assert_eq!(
        optional_ints(
            session
                .execute("SELECT id, amount FROM events ORDER BY id")
                .expect("amounts")
        ),
        vec![Some(10), Some(20)],
        "no committed write may be lost across the checkpoint boundary"
    );

    cleanup(&root);
}

#[test]
fn repeated_reopen_cycles_preserve_catalog_and_row_identity() {
    let root = scratch("recovery-cycles");

    // Cycle 1: create the hierarchy and a first row.
    let table_identity = {
        let mut session = open_session(&root);
        create_recovery_fixture(&mut session);
        session
            .execute_all("INSERT INTO events VALUES (1, 'one', 10)")
            .expect("insert");
        let tables = discover_tables(session.layout());
        assert_eq!(tables.len(), 1, "one materialized table");
        tables[0]
    };

    // Cycles 2 and 3: reopen, write, and confirm identity never shifts and no
    // generation identity is reused. Every reopen runs full recovery.
    for cycle in 2..=3u64 {
        let mut session = reopen_session(&root);
        session.execute_all("USE app").expect("use database");
        assert_eq!(
            discover_tables(session.layout()),
            vec![table_identity],
            "the catalog-resolved table identity is stable across reopen cycle {cycle}"
        );
        let next_id = cycle;
        session
            .execute_all(&format!(
                "INSERT INTO events VALUES ({next_id}, 'row-{next_id}', {})",
                next_id * 10
            ))
            .unwrap_or_else(|error| panic!("insert in cycle {cycle}: {error}"));
        let expected: Vec<i64> = (1..=cycle).map(|value| value as i64).collect();
        assert_eq!(
            ints(
                session
                    .execute("SELECT id FROM events ORDER BY id")
                    .expect("ids")
            ),
            expected,
            "every committed row survives reopen cycle {cycle}"
        );
    }

    // Final open: the full history is present exactly once.
    let mut session = reopen_session(&root);
    session.execute_all("USE app").expect("use database");
    assert_eq!(
        ints(
            session
                .execute("SELECT id FROM events ORDER BY id")
                .expect("ids")
        ),
        vec![1, 2, 3]
    );
    assert_eq!(discover_tables(session.layout()), vec![table_identity]);

    cleanup(&root);
}

#[test]
fn corrupted_wal_makes_startup_recovery_fail_instead_of_losing_state() {
    let root = scratch("recovery-corrupt-wal");

    {
        let mut session = open_session(&root);
        create_recovery_fixture(&mut session);
        session
            .execute_all("INSERT INTO events VALUES (1, 'one', 10)")
            .expect("insert");
    }

    // Corrupt the newest WAL segment deterministically: overwrite a byte inside
    // the record framing so the record checksum can no longer verify.
    let wal_dir = root.join("wal");
    let mut segments: Vec<PathBuf> = std::fs::read_dir(&wal_dir)
        .expect("wal dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("checkpoint") | Some("checkpoint.tmp")
            )
        })
        .collect();
    segments.sort();
    let newest = segments.pop().expect("a WAL segment");
    let mut bytes = std::fs::read(&newest).expect("read segment");
    assert!(bytes.len() > 40, "segment holds at least one framed record");
    // Flip a payload byte (offset 40 is inside the first record body).
    bytes[40] ^= 0xFF;
    std::fs::write(&newest, &bytes).expect("write corrupted segment");

    // Opening must report corruption rather than silently dropping the record.
    let wal = root.with_extension("wal");
    let error = PlomidStorageEngine::open(&root, &wal, 32)
        .err()
        .expect("corrupt WAL must fail recovery");
    assert_eq!(
        error.kind(),
        plomid_core::ErrorKind::Corruption,
        "corruption is reported as such, not as a missing database"
    );

    cleanup(&root);
}

/// Looks up the whole derived ART as sorted (key, row-id) pairs.
fn art_entries(art: &plomid_index::ArtIndex) -> Vec<(Vec<u8>, u64)> {
    let mut found: Vec<(Vec<u8>, u64)> = art
        .entries()
        .into_iter()
        .map(|entry| (entry.key().to_vec(), entry.row_id().get()))
        .collect();
    found.sort_unstable();
    found
}

/// Creates the ART restart fixture: two tables, three indexes across them.
fn create_art_fixture(session: &mut Executor<PlomidStorageEngine>) {
    session
        .execute_all("CREATE DATABASE app")
        .expect("create database");
    session.execute_all("USE app").expect("use database");
    session
        .execute_all("CREATE SCHEMA analytics")
        .expect("create schema");
    session
        .execute_all(
            "CREATE TABLE analytics.events (
                id BIGINT PRIMARY KEY,
                kind TEXT,
                amount BIGINT
            )",
        )
        .expect("create events table");
    session
        .execute_all("CREATE INDEX events_amount_idx ON analytics.events (amount)")
        .expect("create amount index");
    session
        .execute_all("CREATE INDEX events_kind_idx ON analytics.events (kind)")
        .expect("create kind index");
    session
        .execute_all(
            "CREATE TABLE analytics.orders (
                id BIGINT PRIMARY KEY,
                amount BIGINT
            )",
        )
        .expect("create orders table");
    session
        .execute_all("CREATE INDEX orders_amount_idx ON analytics.orders (amount)")
        .expect("create orders index");
    session
        .execute_all("INSERT INTO analytics.events VALUES (1, 'click', 10), (2, 'view', 20)")
        .expect("insert events");
    session
        .execute_all("INSERT INTO analytics.orders VALUES (7, 70)")
        .expect("insert orders");
    // The materialization boundary publishes durable index generations, which
    // is the authority every derived ART is rebuilt from.
    session
        .execute_all("VACUUM analytics.events")
        .expect("vacuum events");
    session
        .execute_all("VACUUM analytics.orders")
        .expect("vacuum orders");
    // ART is a lazy derived cache: build it explicitly now that the durable
    // index generations exist. Connection establishment never builds it.
    session.rebuild_art_indexes();
}

/// Asserts that every cached ART of a session satisfies the ART invariants.
fn assert_art_valid(session: &Executor<PlomidStorageEngine>) {
    assert!(
        session.art_error().is_none(),
        "no reconstruction failure is recorded"
    );
    for qualified in session.catalog().table_names() {
        for definition in session.catalog().indexes_for_table(qualified.as_str()) {
            if let Some(art) = session.art_index(qualified.as_str(), &definition.name) {
                art.validate().expect("derived ART satisfies invariants");
            }
        }
    }
}

/// Runs one explicit maintenance boundary for a single table.
fn run_vacuum(session: &mut Executor<PlomidStorageEngine>, statement: &str) {
    session.execute_all(statement).expect("vacuum");
}

/// Forces the next index generation of `analytics.events` to a strictly newer
/// source state through a maintenance boundary.
///
/// The boundary publishes a new generation on the same single engine the
/// runtime ART derives from, so the ART must track a newest — not stale —
/// source afterwards. This exercises the generation-tracking property (§13)
/// without inventing an administrative SQL command the repository does not
/// have.
fn advance_to_a_newer_source_generation(session: &mut Executor<PlomidStorageEngine>) {
    session
        .execute_all("UPDATE analytics.events SET amount = amount + 1000 WHERE id = 3")
        .expect("update before the newer source generation");
    session
        .execute_all("VACUUM analytics.events")
        .expect("boundary that publishes the newer source generation");
}

#[test]
fn art_is_absent_before_any_index_generation_exists() {
    let root = scratch("art-absent");
    let mut session = open_session(&root);
    session
        .execute_all("CREATE DATABASE app")
        .expect("create database");
    session.execute_all("USE app").expect("use database");
    session
        .execute_all("CREATE TABLE events (id BIGINT PRIMARY KEY)")
        .expect("create table");
    assert_eq!(
        session.art_len(),
        0,
        "no index exists, so no derived ART may be cached"
    );
    assert!(session.art_error().is_none());
    cleanup(&root);
}

#[test]
fn art_reconstruction_survives_a_restart_with_identical_lookups() {
    let root = scratch("art-restart");
    let before = {
        let mut session = open_session(&root);
        create_art_fixture(&mut session);
        assert_art_valid(&session);
        assert_eq!(session.art_len(), 3, "one derived ART per defined index");
        let amount = session
            .art_index("analytics.events", "events_amount_idx")
            .expect("events amount ART");
        let kind = session
            .art_index("analytics.events", "events_kind_idx")
            .expect("events kind ART");
        let orders = session
            .art_index("analytics.orders", "orders_amount_idx")
            .expect("orders amount ART");
        (art_entries(amount), art_entries(kind), art_entries(orders))
    };

    // Restart through the real recovery path: a fresh engine and session over
    // the same root. ART is lazy, so rebuild explicitly after recovery + USE;
    // production connections never pay this cost.
    let mut session = reopen_session(&root);
    session.execute_all("USE app").expect("use database");
    session.rebuild_art_indexes();
    assert_art_valid(&session);
    assert_eq!(session.art_len(), 3);
    let amount_after = art_entries(
        session
            .art_index("analytics.events", "events_amount_idx")
            .expect("events amount ART after restart"),
    );
    let kind_after = art_entries(
        session
            .art_index("analytics.events", "events_kind_idx")
            .expect("events kind ART after restart"),
    );
    let orders_after = art_entries(
        session
            .art_index("analytics.orders", "orders_amount_idx")
            .expect("orders amount ART after restart"),
    );
    assert_eq!(
        (amount_after, kind_after, orders_after),
        before,
        "restart must reconstruct byte-identical runtime lookups"
    );

    cleanup(&root);
}

#[test]
fn art_tracks_a_new_index_generation_after_maintenance_boundaries() {
    let root = scratch("art-generations");
    let mut session = open_session(&root);
    create_art_fixture(&mut session);
    let first = art_entries(
        session
            .art_index("analytics.events", "events_amount_idx")
            .expect("amount ART"),
    );
    assert_eq!(first.len(), 2);

    // Another committed write followed by the automatic pass publishes a new
    // data generation and a new index generation; the runtime ART for that
    // index must observe the new state immediately, without a restart.
    session
        .execute_all("INSERT INTO analytics.events VALUES (3, 'buy', 30)")
        .expect("insert third event");
    run_vacuum(&mut session, "VACUUM analytics.events;");
    let second = art_entries(
        session
            .art_index("analytics.events", "events_amount_idx")
            .expect("amount ART after maintenance"),
    );
    assert_eq!(second.len(), 3, "the refresh covers the new generation");
    assert!(
        second
            .iter()
            .all(|pair| first.contains(pair) || pair.1 == 3),
        "only the new row is added; no old row is lost or duplicated"
    );
    assert_art_valid(&session);

    // A new source generation also publishes a newer durable generation; a
    // restart then proves the runtime ART reconstructs from the newest state,
    // not a stale one.
    advance_to_a_newer_source_generation(&mut session);
    let mut restarted = reopen_session(&root);
    restarted.execute_all("USE app").expect("use database");
    restarted.rebuild_art_indexes();
    assert_art_valid(&restarted);
    let after_restart = art_entries(
        restarted
            .art_index("analytics.events", "events_amount_idx")
            .expect("amount ART after restart"),
    );
    assert_eq!(
        after_restart.len(),
        3,
        "a restart reconstructs from the newest generation"
    );

    cleanup(&root);
}
