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
//! STEP 28 — SQL end-to-end compaction test.
//!
//! This test drives the real SQL path for data (`CREATE DATABASE`, `USE`,
//! `CREATE SCHEMA`, `CREATE TABLE`, `INSERT`, `UPDATE`, `DELETE`, `SELECT`) and
//! then runs a real `VACUUM`, which materializes the table's committed rows
//! into an immutable generation and compacts it through the existing
//! columnar/generation machinery. The test injects nothing: `VACUUM` is the
//! only entry point it uses to reach compaction.
//!
//! It captures the before/after state required by STEP 29:
//!
//! ```text
//! published generations before   rows before   visible rows before
//! published generations after    rows after    visible rows after
//! ```
//!
//! and asserts that the *logical SQL result* is identical before compaction,
//! after compaction, and after a restart.

use plomid_columnar::ColumnarFailPoint;
use plomid_core::{DatabaseId, SchemaId, TableId};
use plomid_executor::{Executor, MaintenancePolicy};
use plomid_sql::{QueryResult, Value};
use plomid_storage::DatabaseLayout;
use plomid_txn::PlomidStorageEngine;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Returns an isolated scratch directory for one test.
fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-sql-compaction-{label}-{}-{id}",
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
fn open_engine(root: &Path) -> PlomidStorageEngine {
    let wal = root.with_extension("wal");
    PlomidStorageEngine::create(root, &wal, 32)
        .or_else(|_| PlomidStorageEngine::open(root, &wal, 32))
        .expect("create engine")
}

/// Opens a SQL session against `root`.
fn open_session(root: &Path) -> Executor<PlomidStorageEngine> {
    Executor::new(open_engine(root)).expect("executor")
}

/// Reopens an existing engine, recovering durable state instead of truncating.
fn reopen_session(root: &Path) -> Executor<PlomidStorageEngine> {
    let wal = root.with_extension("wal");
    let engine = PlomidStorageEngine::open(root, &wal, 32).expect("reopen engine");
    Executor::new(engine).expect("executor")
}

/// Executes one statement, failing the test on error.
fn run(session: &mut Executor<PlomidStorageEngine>, sql: &str) {
    session
        .execute(sql)
        .unwrap_or_else(|error| panic!("`{sql}` failed: {error}"));
}

/// Returns the rows of a row-producing result, failing on anything else.
fn rows(result: QueryResult) -> Vec<Vec<Value>> {
    match result {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

/// Every materialized table, as (database, schema, table) identities.
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

/// Reads one SQL integer column value.
fn as_int(value: &Value) -> i64 {
    match value {
        Value::Int2(number) => i64::from(*number),
        Value::Int4(number) => i64::from(*number),
        Value::Int8(number) => *number,
        other => panic!("expected an integer column value, got {other:?}"),
    }
}

/// Runs a two-integer-column `SELECT` and returns its rows as pairs.
fn int_pairs(session: &mut Executor<PlomidStorageEngine>, sql: &str) -> Vec<(i64, i64)> {
    rows(
        session
            .execute(sql)
            .unwrap_or_else(|error| panic!("`{sql}` failed: {error}")),
    )
    .into_iter()
    .map(|row| {
        let mut columns = row.iter();
        (
            as_int(columns.next().expect("first column")),
            as_int(columns.next().expect("second column")),
        )
    })
    .collect()
}

/// Generation directories under every table of the logical hierarchy.
fn generation_dirs(layout: &DatabaseLayout) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for (database, schema, table) in discover_tables(layout) {
        let dir = layout.table_generations_dir_in_schema(database, schema, table);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Tables that own at least one generation directory.
fn tables_with_generations(layout: &DatabaseLayout) -> Vec<(DatabaseId, SchemaId, TableId)> {
    discover_tables(layout)
        .into_iter()
        .filter(|(database, schema, table)| {
            std::fs::read_dir(layout.table_generations_dir_in_schema(*database, *schema, *table))
                .map(|entries| entries.flatten().any(|entry| entry.path().is_dir()))
                .unwrap_or(false)
        })
        .collect()
}

/// Device directories (`devices/D-*`) under the engine root.
fn device_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("devices")) {
        for entry in entries.flatten() {
            let path = entry.path();
            let named = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("D-"));
            if named && path.is_dir() {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}
#[test]
fn sql_vacuum_materializes_and_compacts_an_update_delete_chain() {
    let dir = scratch("e2e");

    let (expected, stable_generations, devices) = {
        let mut session = open_session(&dir);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(&mut session, "CREATE SCHEMA analytics;");
        run(
            &mut session,
            "CREATE TABLE analytics.events (id BIGINT PRIMARY KEY, \
             \"timestamp\" BIGINT, value BIGINT);",
        );
        run(
            &mut session,
            "INSERT INTO analytics.events VALUES (1, 100, 42), (2, 200, 84);",
        );
        run(
            &mut session,
            "UPDATE analytics.events SET value = 50 WHERE id = 1;",
        );
        run(
            &mut session,
            "UPDATE analytics.events SET value = 60 WHERE id = 1;",
        );
        run(&mut session, "DELETE FROM analytics.events WHERE id = 2;");

        // STEP 29 — before compaction: only the surviving row is visible, and
        // the table has no immutable generation yet because nothing has
        // materialized the committed rows.
        let before = int_pairs(
            &mut session,
            "SELECT id, value FROM analytics.events ORDER BY id;",
        );
        assert_eq!(before, vec![(1, 60)]);
        assert!(
            generation_dirs(session.layout()).is_empty(),
            "no generation exists before VACUUM"
        );

        // The real statement under test.
        run(&mut session, "VACUUM analytics.events;");

        // After compaction the logical SQL result is identical.
        assert_eq!(
            int_pairs(
                &mut session,
                "SELECT id, value FROM analytics.events ORDER BY id;"
            ),
            before
        );

        // The compacted generation landed under the table hierarchy, and one
        // generation is all that remains because compaction merged the input.
        let generations_after_vacuum = generation_dirs(session.layout());
        assert_eq!(generations_after_vacuum.len(), 1);
        assert_eq!(tables_with_generations(session.layout()).len(), 1);

        // A write issued after compaction must survive the next VACUUM: the new
        // row joins the table's immutable state rather than being lost to it.
        run(
            &mut session,
            "INSERT INTO analytics.events VALUES (3, 300, 7);",
        );
        run(&mut session, "VACUUM analytics.events;");
        let expected = vec![(1, 60), (3, 7)];
        assert_eq!(
            int_pairs(
                &mut session,
                "SELECT id, value FROM analytics.events ORDER BY id;"
            ),
            expected
        );

        // Repeated VACUUM is a no-op, not unbounded generation churn.
        let stable_generations = generation_dirs(session.layout());
        run(&mut session, "VACUUM analytics.events;");
        run(&mut session, "VACUUM analytics.events;");
        assert_eq!(generation_dirs(session.layout()), stable_generations);

        // Physical bytes stay owned by the device hierarchy; no device id is
        // assumed anywhere in the path.
        let devices = device_dirs(&dir);
        (expected, stable_generations, devices)
    };

    assert!(
        !devices.is_empty(),
        "physical segments must live under devices/D-*"
    );

    // --- restart: recovery must preserve the logical result ---
    {
        let mut session = reopen_session(&dir);
        run(&mut session, "USE app;");
        assert_eq!(
            int_pairs(
                &mut session,
                "SELECT id, value FROM analytics.events ORDER BY id;"
            ),
            expected
        );
        // The published generation set survived the restart unchanged.
        assert_eq!(generation_dirs(session.layout()), stable_generations);
        // And VACUUM after recovery stays safe and idempotent.
        run(&mut session, "VACUUM analytics.events;");
        assert_eq!(generation_dirs(session.layout()), stable_generations);
    }

    cleanup(&dir);
}

#[test]
fn sql_vacuum_scopes_to_the_named_table_and_supports_the_wide_form() {
    let dir = scratch("scope");
    {
        let mut session = open_session(&dir);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE table_a (id BIGINT, value BIGINT);",
        );
        run(
            &mut session,
            "CREATE TABLE table_b (id BIGINT, value BIGINT);",
        );
        run(&mut session, "INSERT INTO table_a VALUES (1, 10);");
        run(&mut session, "INSERT INTO table_b VALUES (1, 20);");

        // A named VACUUM must touch exactly one table: selection is
        // table-scoped, so table_b can never contribute rows or generations.
        run(&mut session, "VACUUM table_a;");
        assert_eq!(
            tables_with_generations(session.layout()).len(),
            1,
            "only the named table is materialized"
        );
        assert_eq!(
            int_pairs(&mut session, "SELECT id, value FROM table_a;"),
            vec![(1, 10)]
        );
        assert_eq!(
            int_pairs(&mut session, "SELECT id, value FROM table_b;"),
            vec![(1, 20)]
        );

        // The database-wide form covers the tables the catalog knows, resolved
        // without scanning the filesystem.
        run(&mut session, "VACUUM;");
        assert_eq!(
            tables_with_generations(session.layout()).len(),
            2,
            "the wide form covers every catalog table"
        );

        // Running it again stays idempotent.
        let after = generation_dirs(session.layout());
        run(&mut session, "VACUUM;");
        assert_eq!(generation_dirs(session.layout()), after);

        // Both tables still report their own rows.
        assert_eq!(
            int_pairs(&mut session, "SELECT id, value FROM table_a;"),
            vec![(1, 10)]
        );
        assert_eq!(
            int_pairs(&mut session, "SELECT id, value FROM table_b;"),
            vec![(1, 20)]
        );
    }
    cleanup(&dir);
}

/// Returns the current index generations of one table under the layout.
///
/// Resolution stays catalog-first: the layout is read only for the generations
/// of index identities the catalog owns.
fn index_generations(
    layout: &DatabaseLayout,
    catalog: &plomid_sql::InMemoryCatalog,
    qualified_table: &str,
    database: &str,
) -> Vec<(String, Vec<plomid_core::GenerationId>)> {
    use plomid_sql::Catalog;

    let database_id = catalog
        .database_names()
        .iter()
        .position(|name| name.eq_ignore_ascii_case(database))
        .map(|index| DatabaseId::new(index as u64 + 1));
    let Some(database_id) = database_id else {
        return Vec::new();
    };
    let (schema_name, _) = qualified_table
        .split_once('.')
        .unwrap_or(("public", qualified_table));
    let Some(schema_id) = catalog.schema_id(schema_name) else {
        return Vec::new();
    };
    let Ok(schema) = catalog.get_table(qualified_table) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<plomid_core::GenerationId>)> = catalog
        .indexes_for_table(qualified_table)
        .into_iter()
        .map(|definition| {
            let records = layout
                .discover_index_generations_in_schema(
                    database_id,
                    schema_id,
                    schema.table_id,
                    definition.index_id,
                )
                .unwrap_or_default();
            let generations = records
                .into_iter()
                .map(|record| record.generation_id)
                .collect();
            (definition.name, generations)
        })
        .collect();
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

#[test]
fn vacuum_automatically_builds_index_generations_for_defined_indexes() {
    let dir = scratch("vacuum-index-auto");
    {
        let mut session = open_session(&dir);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE readings (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE INDEX readings_amount_idx ON readings (amount);",
        );
        run(
            &mut session,
            "INSERT INTO readings VALUES (1, 10), (2, 20), (3, 30);",
        );

        // DML writes hot rows only: no index generation is built per write.
        assert!(
            index_generations(
                session.layout(),
                session.catalog(),
                "public.readings",
                "app"
            )
            .iter()
            .all(|(_, generations)| generations.is_empty()),
            "an index generation must never be rebuilt on every write"
        );

        // VACUUM materializes the data generation, then the automatic boundary
        // builds one index generation for the defined index.
        run(&mut session, "VACUUM readings;");
        let mut with_generations = index_generations(
            session.layout(),
            session.catalog(),
            "public.readings",
            "app",
        );
        assert_eq!(with_generations.len(), 1, "the defined index is covered");
        let (_, generations) = with_generations.pop().expect("one index");
        assert_eq!(generations.len(), 1, "one generation, not one per row");

        // The visible logical state is unchanged by the automatic build.
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 10), (2, 20), (3, 30)]
        );

        // Re-running the boundary with no new data is a no-op: the automatic
        // endpoint coalesces against the stable source state.
        let before = index_generations(
            session.layout(),
            session.catalog(),
            "public.readings",
            "app",
        );
        run(&mut session, "VACUUM readings;");
        assert_eq!(
            index_generations(
                session.layout(),
                session.catalog(),
                "public.readings",
                "app"
            ),
            before,
            "repeated automatic triggers must not churn generations"
        );

        // A written row participates only after the next explicit boundary.
        run(&mut session, "INSERT INTO readings VALUES (4, 40);");
        run(&mut session, "VACUUM readings;");
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 10), (2, 20), (3, 30), (4, 40)]
        );
        let after = index_generations(
            session.layout(),
            session.catalog(),
            "public.readings",
            "app",
        );
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].1.len(),
            2,
            "a new source state earns a new generation"
        );
    }
    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// Automatic index maintenance reached through normal operation (never VACUUM)
// ---------------------------------------------------------------------------

/// Opens a session whose automatic-maintenance policy uses `bound` committed
/// mutations, so a test drives the policy deterministically by executing
/// statements rather than by waiting.
fn session_with_policy(root: &Path, bound: u64) -> Executor<PlomidStorageEngine> {
    let mut session = open_session(root);
    session.set_maintenance_policy(MaintenancePolicy::new(bound));
    session
}

/// Total index generations across every defined index of one table.
fn index_generation_total(
    session: &Executor<PlomidStorageEngine>,
    qualified_table: &str,
    database: &str,
) -> usize {
    index_generations(
        session.layout(),
        session.catalog(),
        qualified_table,
        database,
    )
    .iter()
    .map(|(_, generations)| generations.len())
    .sum()
}

#[test]
fn writes_do_not_build_index_generations_before_the_policy_is_due() {
    let dir = scratch("auto-not-per-write");
    {
        let mut session = session_with_policy(&dir, 64);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE readings (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE INDEX readings_amount_idx ON readings (amount);",
        );

        // Five separate committed writes, well below the policy bound.
        run(&mut session, "INSERT INTO readings VALUES (1, 10);");
        run(&mut session, "INSERT INTO readings VALUES (2, 20);");
        run(
            &mut session,
            "UPDATE readings SET amount = 11 WHERE id = 1;",
        );
        run(
            &mut session,
            "UPDATE readings SET amount = 12 WHERE id = 1;",
        );
        run(&mut session, "DELETE FROM readings WHERE id = 2;");

        assert_eq!(
            session.committed_mutations("public.readings"),
            5,
            "every committed write is accounted exactly once"
        );
        assert_eq!(
            session.maintenance_passes("public.readings"),
            0,
            "no pass may run before the policy bound is reached"
        );
        assert!(
            session.tables_due_for_maintenance().is_empty(),
            "the policy bound has not been reached"
        );
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            0,
            "an index generation must never be built per write"
        );
        assert!(
            generation_dirs(session.layout()).is_empty(),
            "a data generation must never be built per write"
        );
        assert!(
            session.last_maintenance_error().is_none(),
            "nothing failed, so nothing is reported"
        );
    }
    cleanup(&dir);
}

#[test]
fn automatic_maintenance_runs_during_normal_operation_without_vacuum() {
    let dir = scratch("auto-normal");
    {
        let mut session = session_with_policy(&dir, 3);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE readings (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE INDEX readings_amount_idx ON readings (amount);",
        );

        run(&mut session, "INSERT INTO readings VALUES (1, 10);");
        run(&mut session, "INSERT INTO readings VALUES (2, 20);");
        assert_eq!(
            session.maintenance_passes("public.readings"),
            0,
            "two writes are below the bound"
        );

        // The third committed write reaches the policy bound, so the automatic
        // pass runs as part of ordinary statement execution. The user never
        // issues VACUUM, and the statement itself is unaffected.
        run(
            &mut session,
            "UPDATE readings SET amount = 30 WHERE id = 2;",
        );

        assert_eq!(
            session.maintenance_passes("public.readings"),
            1,
            "normal operation reached the automatic maintenance path"
        );
        assert_eq!(
            session.committed_mutations("public.readings"),
            0,
            "a successful pass consumes the accumulated work"
        );
        assert!(
            session.last_maintenance_error().is_none(),
            "the automatic pass completed"
        );
        assert_eq!(
            generation_dirs(session.layout()).len(),
            1,
            "the pass published one data generation"
        );
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            1,
            "the pass refreshed the defined index without VACUUM"
        );

        // The visible logical state is exactly what the writes produced.
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 10), (2, 30)]
        );
    }
    cleanup(&dir);
}

#[test]
fn automatic_maintenance_survives_drop_and_recreate_without_missing_metadata() {
    let dir = scratch("auto-drop-recreate");
    {
        let mut session = session_with_policy(&dir, 2);
        run(&mut session, "CREATE SCHEMA bench;");
        run(
            &mut session,
            "CREATE TABLE bench.events (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(&mut session, "INSERT INTO bench.events VALUES (1, 10);");
        run(&mut session, "INSERT INTO bench.events VALUES (2, 20);");
        assert_eq!(session.maintenance_passes("bench.events"), 1);
        assert!(session.last_maintenance_error().is_none());

        // Dropping SQL objects must not remove generation metadata that the
        // durable generation catalog still retains for recovery/GC.
        run(&mut session, "DROP SCHEMA bench CASCADE;");
        run(&mut session, "CREATE SCHEMA bench;");
        run(
            &mut session,
            "CREATE TABLE bench.events (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(&mut session, "INSERT INTO bench.events VALUES (3, 30);");
        run(&mut session, "INSERT INTO bench.events VALUES (4, 40);");

        assert_eq!(session.maintenance_passes("bench.events"), 2);
        assert!(
            session.last_maintenance_error().is_none(),
            "drop/recreate must not leave maintenance with missing generation metadata"
        );
        assert_eq!(
            int_pairs(
                &mut session,
                "SELECT id, amount FROM bench.events ORDER BY id;"
            ),
            vec![(3, 30), (4, 40)]
        );
    }
    cleanup(&dir);
}

#[test]
fn automatic_maintenance_coalesces_many_writes_into_one_generation() {
    let dir = scratch("auto-coalesce");
    {
        let mut session = session_with_policy(&dir, 6);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE readings (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE INDEX readings_amount_idx ON readings (amount);",
        );

        // Six distinct committed writes. The policy makes the sixth commit the
        // single due boundary, so all six collapse into one generation.
        run(&mut session, "INSERT INTO readings VALUES (1, 10);");
        run(&mut session, "INSERT INTO readings VALUES (2, 20);");
        run(
            &mut session,
            "UPDATE readings SET amount = 11 WHERE id = 1;",
        );
        run(
            &mut session,
            "UPDATE readings SET amount = 12 WHERE id = 1;",
        );
        run(&mut session, "DELETE FROM readings WHERE id = 2;");
        run(&mut session, "INSERT INTO readings VALUES (3, 30);");

        assert_eq!(
            session.maintenance_passes("public.readings"),
            1,
            "six writes produce exactly one maintenance boundary"
        );
        assert_eq!(
            generation_dirs(session.layout()).len(),
            1,
            "the boundary consumed all six writes into one generation"
        );
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            1,
            "one source generation earns one index generation"
        );
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 12), (3, 30)]
        );
    }
    cleanup(&dir);
}

#[test]
fn repeated_automatic_maintenance_over_one_source_is_idempotent() {
    let dir = scratch("auto-idempotent");
    {
        let mut session = session_with_policy(&dir, 2);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE readings (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE INDEX readings_amount_idx ON readings (amount);",
        );
        run(&mut session, "INSERT INTO readings VALUES (1, 10);");
        run(&mut session, "INSERT INTO readings VALUES (2, 20);");

        let after_first = generation_dirs(session.layout());
        assert_eq!(after_first.len(), 1);
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            1
        );

        // A write that changes no visible value, then an eager policy, so the
        // pass runs again against a byte-identical source state.
        run(
            &mut session,
            "UPDATE readings SET amount = 10 WHERE id = 1;",
        );
        session.set_maintenance_policy(MaintenancePolicy::new(1));
        let maintained = session.run_maintenance().expect("maintenance");
        assert_eq!(maintained, 1, "the table is due and receives a pass");
        assert_eq!(
            generation_dirs(session.layout()),
            after_first,
            "an unchanged source state must not earn a duplicate data generation"
        );
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            1,
            "an unchanged source state must not earn a duplicate index generation"
        );
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 10), (2, 20)]
        );
    }
    cleanup(&dir);
}

#[test]
fn automatic_maintenance_survives_restart_with_unique_generation_ids() {
    let dir = scratch("auto-restart");
    let first = {
        let mut session = session_with_policy(&dir, 2);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE readings (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE INDEX readings_amount_idx ON readings (amount);",
        );
        run(&mut session, "INSERT INTO readings VALUES (1, 10);");
        run(&mut session, "INSERT INTO readings VALUES (2, 20);");
        assert_eq!(session.maintenance_passes("public.readings"), 1);
        let dirs = generation_dirs(session.layout());
        assert_eq!(dirs.len(), 1);
        dirs
    };

    // Restart: the published data generation and its index generation survive,
    // and the logical result is unchanged.
    let second = {
        let mut session = reopen_session(&dir);
        run(&mut session, "USE app;");
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 10), (2, 20)]
        );
        assert_eq!(generation_dirs(session.layout()), first);
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            1
        );

        // A new source state after restart earns a fresh generation whose
        // identity does not collide with the recovered one.
        session.set_maintenance_policy(MaintenancePolicy::new(1));
        run(&mut session, "INSERT INTO readings VALUES (3, 30);");
        assert_eq!(session.maintenance_passes("public.readings"), 1);
        let dirs = generation_dirs(session.layout());
        assert_ne!(
            dirs, first,
            "the generation published after reopen is a new identity"
        );
        assert_ne!(
            index_generation_total(&session, "public.readings", "app"),
            0,
            "the index generation set is populated after reopen"
        );
        dirs
    };

    // Third open: the newest generation and index generation are current, and
    // the logical result still matches exactly.
    {
        let mut session = reopen_session(&dir);
        run(&mut session, "USE app;");
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 10), (2, 20), (3, 30)]
        );
        assert_eq!(generation_dirs(session.layout()), second);
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            2,
            "each source state keeps its own index generation"
        );
    }
    cleanup(&dir);
}

#[test]
fn automatic_maintenance_isolates_tables_and_indexes() {
    let dir = scratch("auto-isolation");
    {
        let mut session = session_with_policy(&dir, 1);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE table_a (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE TABLE table_b (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE INDEX table_a_amount_idx ON table_a (amount);",
        );
        run(&mut session, "CREATE INDEX table_a_id_idx ON table_a (id);");
        run(
            &mut session,
            "CREATE INDEX table_b_amount_idx ON table_b (amount);",
        );

        // One committed write reaches a bound of one, so table_a alone is
        // maintained.
        run(&mut session, "INSERT INTO table_a VALUES (1, 10);");

        assert_eq!(
            session.maintenance_passes("public.table_a"),
            1,
            "the written table is maintained"
        );
        assert_eq!(
            session.maintenance_passes("public.table_b"),
            0,
            "an unwritten table is never maintained"
        );
        assert_eq!(
            index_generation_total(&session, "public.table_a", "app"),
            2,
            "both indexes of the written table are refreshed"
        );
        assert_eq!(
            index_generation_total(&session, "public.table_b", "app"),
            0,
            "an unwritten table's indexes must never be rebuilt"
        );

        // Both tables still report only their own rows.
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM table_a;"),
            vec![(1, 10)]
        );
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM table_b;"),
            Vec::new()
        );
    }
    cleanup(&dir);
}

#[test]
fn automatic_maintenance_failure_does_not_roll_back_committed_dml() {
    let dir = scratch("auto-failure");
    {
        let mut session = session_with_policy(&dir, 2);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE readings (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE INDEX readings_amount_idx ON readings (amount);",
        );

        // The automatic pass will fail before staging anything.
        session.set_maintenance_fail_at(ColumnarFailPoint::AfterBuild);
        run(&mut session, "INSERT INTO readings VALUES (1, 10);");
        run(&mut session, "INSERT INTO readings VALUES (2, 20);");

        // Committed DML is untouched by the maintenance failure.
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 10), (2, 20)],
            "committed rows survive a maintenance failure"
        );
        assert!(
            session.last_maintenance_error().is_some(),
            "the automatic maintenance failure is reported"
        );
        assert_eq!(
            session.maintenance_passes("public.readings"),
            0,
            "a failed pass is not counted as a maintenance boundary"
        );
        assert_eq!(
            session.committed_mutations("public.readings"),
            2,
            "the accumulated work is kept so a later pass retries"
        );
        assert!(
            generation_dirs(session.layout()).is_empty(),
            "a failed pass publishes nothing"
        );

        // The retry succeeds and consumes the accumulated work.
        session.set_maintenance_fail_at(ColumnarFailPoint::None);
        let maintained = session.run_maintenance().expect("retry succeeds");
        assert_eq!(maintained, 1, "the retry maintains the pending table");
        assert_eq!(session.maintenance_passes("public.readings"), 1);
        assert_eq!(generation_dirs(session.layout()).len(), 1);
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            1,
            "the retry produced the index generation"
        );
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 10), (2, 20)]
        );
    }
    cleanup(&dir);
}

#[test]
fn automatic_boundary_stays_idempotent_while_manual_rebuild_forces_a_generation() {
    let dir = scratch("auto-then-manual");
    {
        let mut session = session_with_policy(&dir, 2);
        run(&mut session, "CREATE DATABASE app;");
        run(&mut session, "USE app;");
        run(
            &mut session,
            "CREATE TABLE readings (id BIGINT PRIMARY KEY, amount BIGINT);",
        );
        run(
            &mut session,
            "CREATE INDEX readings_amount_idx ON readings (amount);",
        );
        run(&mut session, "INSERT INTO readings VALUES (1, 10);");
        run(&mut session, "INSERT INTO readings VALUES (2, 20);");

        // The automatic boundary published exactly one generation.
        assert_eq!(generation_dirs(session.layout()).len(), 1);
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            1
        );

        // `VACUUM` is an explicit maintenance boundary that still routes through
        // the idempotent automatic endpoint, so over an unchanged source state it
        // must not churn generations.
        run(&mut session, "VACUUM readings;");
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            1,
            "the explicit boundary is idempotent for an unchanged source state"
        );

        // A manual rebuild is the deliberate exception: over that same source
        // state it must publish a new generation and retain the superseded one.
        use plomid_core::{GenerationId, IndexId};
        use plomid_index::IndexGenerationStore;
        use plomid_sql::Catalog;

        let catalog = session.catalog();
        let database_id = catalog
            .database_names()
            .iter()
            .position(|name| name.eq_ignore_ascii_case("app"))
            .map(|index| DatabaseId::new(index as u64 + 1))
            .expect("app database");
        let schema_id = catalog.schema_id("public").expect("public schema");
        let table_id = catalog
            .get_table("public.readings")
            .expect("readings table")
            .table_id;
        let identity = plomid_core::TableIdentity::new(database_id, schema_id, table_id);
        let definition = catalog
            .indexes_for_table("public.readings")
            .into_iter()
            .next()
            .expect("one defined index");
        let index_id: IndexId = definition.index_id;

        let store = IndexGenerationStore::new(&dir);
        let current = store
            .current_generation(identity, index_id)
            .expect("current query")
            .expect("a current index generation");
        // Read back exactly the keys the automatic build produced, so the manual
        // rebuild is over the same logical index content.
        let payload = store.layout().index_payload_path_in_schema(
            database_id,
            schema_id,
            table_id,
            index_id,
            current.generation_id,
        );
        let entries = plomid_index::btree::BTreeIndex::open(&payload, 64)
            .expect("open built payload")
            .scan_all()
            .expect("scan built payload");
        let rows: Vec<(Vec<u8>, plomid_core::RowId)> = {
            let mut collected = Vec::new();
            for entry in entries {
                for row_id in entry.row_ids {
                    collected.push((entry.key.clone(), row_id));
                }
            }
            collected
        };
        assert_eq!(rows.len(), 2, "both indexed rows are present");

        let manual = store
            .rebuild_generation_manually(
                identity,
                index_id,
                GenerationId::new(current.source_data_generation_id.get()),
                &rows,
            )
            .expect("manual rebuild");
        assert_eq!(
            manual.trigger,
            plomid_storage::IndexGenerationTrigger::Manual
        );
        assert_ne!(manual.generation_id, current.generation_id);
        assert_eq!(
            index_generation_total(&session, "public.readings", "app"),
            2,
            "a manual rebuild forces a new index generation"
        );

        // The logical result is unchanged by either trigger.
        assert_eq!(
            int_pairs(&mut session, "SELECT id, amount FROM readings ORDER BY id;"),
            vec![(1, 10), (2, 20)]
        );
    }
    cleanup(&dir);
}
