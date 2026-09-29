# PLOMID Release Baseline

## Repository State
- **Git Status**: Clean working tree on branch `RLS`
- **Current Commit**: 9587f97e6c168e1c0477bcccee9b8baf496c26b0
- **Branch**: `RLS`
- **Workspace Crates**: `cli`, `columnar`, `core`, `crc32c`, `cron`, `executor`, `filters`, `index`, `lifecycle`, `mvcc`, `network`, `optimizer`, `ring`, `security`, `server`, `sql`, `storage`, `txn`, `types`, `wal`
- **Binaries**: `plomid-cli`, `plomid-server`
- **Public Server Entry Points**: 
  - `plomid-server`: Main TCP server process providing PGWire compatibility.
  - `plomid-cli`: Command-line tool.

## Architecture Summaries
- **SQL Feature Surface**: Complete CRUD with Transactions, Subqueries, CTEs, Aggregates, GROUP BY, Window Functions, JSON/JSONB functions, array operations, time operations (`date_trunc`), and basic DDL (CREATE/DROP tables, indexes, views).
- **Transaction Architecture**: MVCC via a robust `StorageEngine` abstraction. Includes explicit `BEGIN`/`COMMIT`/`ROLLBACK` boundaries, conflict detection (`ON CONFLICT DO UPDATE/NOTHING`), and sequence generation.
- **Storage Architecture**: Manifest-based segment system with 16 KiB pages and CRC32C checks. Data is organized into tables, schemas, and databases on the local filesystem.
- **Index Architecture**: B+Tree for durable state combined with Adaptive Radix Tree (ART) for runtime reconstruction, scaling lookups independent of table size.
- **JSON Implementation**: `JSON` and `JSONB` data types. Support for JSON paths (`->`, `->>`), array elements, and JSON aggregation (`json_agg`, `json_object_agg`).
- **Time-series Implementation**: Native `TIMESTAMP`, `TIMESTAMPTZ`, `TIME`, and `INTERVAL` types. Built-in functions for time aggregation (`date_trunc`).
- **Recovery Architecture**: Segmented Write-Ahead Log (WAL) with durability watermarks and deterministic checkpoint recovery. 

## Tests and Benchmarks
- **Current Tests**: Unit tests within crates, integration tests via python (`tests/python/*.py`), and comprehensive SQL qualification scripts (`tests/*.sql`).
- **Current Benchmarks**: Targeted `criterion` benchmarks at the crate level (`storage`, `txn`, `index`, `columnar`, `crc32c`, `filters`).

## Release Capability Matrix

| Capability | Current Implementation | SQL Exposed? | Actually Wired? | Tested? | Benchmarked? | Production-Ready? | Remaining Work |
|------------|------------------------|--------------|-----------------|---------|--------------|-------------------|----------------|
| SELECT | Full query engine | Yes | Yes | Yes | Yes | Yes | None |
| INSERT | Values, Subquery, Upsert | Yes | Yes | Yes | Yes | Yes | None |
| UPDATE | Update, Update From | Yes | Yes | Yes | Yes | Yes | None |
| DELETE | Delete, Delete Using | Yes | Yes | Yes | Yes | Yes | None |
| transactions | MVCC / StorageEngine | Yes | Yes | Yes | Yes | Yes | None |
| rollback | Transaction abort | Yes | Yes | Yes | Yes | Yes | None |
| PRIMARY KEY | BTree unique index | Yes | Yes | Yes | Yes | Yes | None |
| UNIQUE | BTree unique index | Yes | Yes | Yes | Yes | Yes | None |
| secondary indexes | BTree + ART runtime | Yes | Yes | Yes | Yes | Yes | None |
| range scans | Storage index scan | Yes | Yes | Yes | Yes | Yes | None |
| ORDER BY | Memory sort / Pushdown | Yes | Yes | Yes | Yes | Yes | None |
| LIMIT | Execution node | Yes | Yes | Yes | Yes | Yes | None |
| aggregates | Accumulators (SUM, AVG) | Yes | Yes | Yes | Yes | Yes | None |
| GROUP BY | Grouping / Aggregation | Yes | Yes | Yes | Yes | Yes | None |
| JOIN | Inner, Outer, Cross | Yes | Yes | Yes | Yes | Yes | None |
| JSON | JSON data type | Yes | Yes | Yes | Yes | Yes | None |
| JSONB | Binary JSON | Yes | Yes | Yes | Yes | Yes | None |
| nested JSON paths | `->` and `->>` operators | Yes | Yes | Yes | Yes | Yes | None |
| arrays | Native Arrays | Yes | Yes | Yes | Yes | Yes | None |
| NULL semantics | IS NULL, COALESCE | Yes | Yes | Yes | Yes | Yes | None |
| timestamp | Native Type | Yes | Yes | Yes | Yes | Yes | None |
| timestamptz | Native Type | Yes | Yes | Yes | Yes | Yes | None |
| time predicates | Comparisons / Between | Yes | Yes | Yes | Yes | Yes | None |
| time aggregation | `date_trunc` function | Yes | Yes | Yes | Yes | Yes | None |
| VACUUM | Maintenance coordinator | Yes | Yes | Yes | Yes | Yes | None |
| ANALYZE | Dummy no-op | Yes | No | No | No | No | Implement statistics collection |
| prepared statements | PGWire Extended Proto | Yes | Yes | Yes | Yes | Yes | None |
| PGWire | TCP Server / Auth | N/A | Yes | Yes | Yes | Yes | None |
| WAL | Segmented append-only | N/A | Yes | Yes | Yes | Yes | None |
| checkpoint | Background process | N/A | Yes | Yes | Yes | Yes | None |
| recovery | WAL Replay | N/A | Yes | Yes | Yes | Yes | None |
| restart | Idempotent load | N/A | Yes | Yes | Yes | Yes | None |
| observability | Tracing / EXPLAIN | Yes | Yes | Yes | Yes | Yes | None |
| security/network boundary | SCRAM/MD5/Cleartext | N/A | Yes | Yes | Yes | Yes | TLS not fully wired yet |

### Implementation Status Breakdown

**Implemented & Wired (Production Ready):**
SELECT, INSERT, UPDATE, DELETE, transactions, rollback, PRIMARY KEY, UNIQUE, secondary indexes, range scans, ORDER BY, LIMIT, aggregates, GROUP BY, JOIN, JSON, JSONB, nested JSON paths, arrays, NULL semantics, timestamp, timestamptz, time predicates, time aggregation, VACUUM, prepared statements, PGWire, WAL, checkpoint, recovery, restart, observability, security/network boundary.

**Implemented But Not Wired:**
- None identified in core execution path.

**Partially Implemented:**
- **TLS Security**: The server has configuration flags for TLS, but rejects connections if TLS is requested (`tls_mode_not_yet_implemented`).

**Unsupported (Out of Scope):**
- Vector search
- Graph capabilities
- Blob/object storage
- Distributed SQL
- Sharding
- Multi-region replication
- Consensus functionality

**Not Tested / Not Benchmarked:**
- **ANALYZE**: Parsed but executes as a no-op returning `ANALYZE`. Requires actual statistics collection logic.
- **REINDEX / CLUSTER / LOCK**: Parsed but act as dummy operations.
