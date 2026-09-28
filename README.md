<div align="center">

<img src="assets/readme/banner.svg" alt="PLOMID — Platform for Modern Intelligence and Data" width="100%">

[![CI](https://github.com/plomid/plomid/actions/workflows/ci.yml/badge.svg)](https://github.com/plomid/plomid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/badge/workspace-19%20crates-14171b?labelColor=08090b&color=e2683c)](Cargo.toml)
[![License](https://img.shields.io/badge/license-Apache--2.0-14171b?labelColor=08090b&color=e2683c)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable%20%7C%202021%20edition-14171b?labelColor=08090b&color=e2683c)](rust-toolchain.toml)
[![Release](https://img.shields.io/badge/release-v0.1.0--beta.1-14171b?labelColor=08090b&color=e2683c)](https://github.com/plomid/plomid/releases)

**One data layer for rows, documents and time-ordered events — with one planning and execution path across all of them.**

</div>

---

PLOMID is an **enterprise universal database platform**. It is a complete database
engine written in Rust: an owned kernel that speaks the PostgreSQL Frontend/Backend Protocol v3
natively, so any standard PostgreSQL client connects without plugins or patches.

One logical database holds structured rows, JSON documents and time-ordered events, and every
statement — from any client — reaches the same parser, the same planner, the same executor and the
same storage core.

- **Own the engine.** PLOMID does not wrap PostgreSQL, RocksDB, ClickHouse, DuckDB or any other
  external database engine as its foundation.
- **SQL first.** The primary interface is SQL, over a wire protocol the industry already speaks.
- **One path.** No per-modality sidecars: documents and events go through the same engine as rows.
- **Honest status.** Solid means shipped today; dashed means roadmap. Nothing pretends otherwise.

**Contents** — [System map](#system-map) · [Pipeline](#the-pipeline) · [Capability status](#capability-status) · [PostgreSQL endpoint](#postgresql-compatible-endpoint) · [Foundation gates](#foundation-gates) · [Run](#run-the-server) · [Install](#install-a-release) · [CLI](#native-management-cli) · [Docs](#documentation) · [Contributing](#contributing) · [License](#copyright-and-license)

<br>

<img src="assets/readme/architecture.svg" alt="PLOMID system map: clients, wire protocol, SQL engine, storage core, base layers" width="100%">

## System Map

PLOMID is a Rust Cargo workspace using the `crates/*` pattern — 19 crates, one kernel:

| Layer | Crates | Role |
| --- | --- | --- |
| **Interface** | `server` · `network` · `cli` | PostgreSQL wire v3 endpoint, session transport, management CLI |
| **SQL engine** | `sql` · `optimizer` · `executor` · `types` | Parser and binder, logical→physical planning, operator execution, PG-compatible type system |
| **Consistency** | `mvcc` · `txn` | Snapshots and version chains, transaction lifecycle and locking |
| **Storage core** | `wal` · `storage` · `columnar` · `index` | Segmented write-ahead log and recovery, pages and blocks, immutable columnar segments, ART + B+Tree indexes |
| **Modalities** | `json` · `filters` | SQL/JSON documents, roaring bitmaps and XOR filters for predicate pruning |
| **Foundation** | `core` · `security` · `lifecycle` · `crc32c` | Kernel primitives, auth/authz/audit, mount/demount coordination, hardware-accelerated integrity |

### The pipeline

Every statement takes the same path, whatever shape of data it touches:

<img src="assets/readme/pipeline.svg" alt="SQL statement flowing through network, parser, binder, planner, executor, MVCC into WAL, storage and indexes" width="100%">

The planner currently ships the V1 SQL subset implemented by PLOMID's own parser and executor;
vector, graph and distribution are roadmap items (dashed in the diagrams above).

<br>

<img src="assets/readme/status.svg" alt="Capability status strip: data models, SQL surface, storage and durability, operations" width="100%">

## Capability Status

The design language of this project is: **solid copper = available today, dashed = roadmap.** The strip above reads the same way. The full implementation-derived record: https://www.plomid.in/docs/

| | Available today | Roadmap |
| --- | --- | --- |
| **Data models** | SQL tables, JSON documents, time series | Vector, graph, blobs, key-value |
| **SQL surface** | V1 SQL subset, joins, CTEs, window functions, prepared statements | Vector search, distribution |
| **Durability** | WAL, checkpoint recovery, indexes, CRC32C | Replication, backups |
| **Operations** | CLI diagnostics, structured logs, Docker, packages | TLS, multi-node |

## PostgreSQL-compatible endpoint

PLOMID speaks the PostgreSQL Frontend/Backend Protocol v3 natively, so any standard PostgreSQL
client connects without plugins or patches:

```bash
cargo run --bin plomid-server -- --port 5432 --username plomid --password secret
psql "postgresql://plomid:secret@localhost:5432/plomid"
```

See https://www.plomid.in/docs/ for the full
compatibility matrix (authentication, simple/extended query, prepared statements, `pg_catalog`,
`information_schema`, SQLSTATE mapping), configuration flags, client examples, and known
limitations.

## Foundation Gates

Every change must keep these commands green:

```sh
cargo build --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The CI pipeline also runs `cargo deny check` to enforce license, advisory, source, and external
database dependency policy — the same policy that keeps the engine owned end to end.

## Run the Server

Start PLOMID with:

```sh
./start.sh
```

Defaults: host `127.0.0.1` (loopback needs no opt-in), port `5432`, data directory `./data`, user
`plomid`, password `secret`. Override any of them with `PLOMID_DATA_DIR`, `PLOMID_HOST`,
`PLOMID_PORT`, `PLOMID_USER`, `PLOMID_PASSWORD`. Binding a non-loopback address requires the
explicit `--allow-remote-plaintext` opt-in because clients send credentials unencrypted until TLS
lands.

For DBeaver, create a PostgreSQL connection with:

| | | | |
| :--- | :--- | :--- | :--- |
| Host `127.0.0.1` | Port `5432` | Database `plomid` | User `plomid` · Password `secret` |

## Install a release

Every tagged release ships all platforms together from
[GitHub Releases](https://github.com/plomid/plomid/releases) (each with `SHA256SUMS.txt`):

- **macOS** (Apple silicon + Intel): `.dmg` — open it, drag to Applications
- **Windows**: installer `.exe`, or portable `.zip` (unpack, run `plomid.exe`)
- **Linux**: `.deb` (`sudo apt install ./plomid_*.deb`, ships a systemd unit), `.rpm`, or static
  `.tar.gz` for any distro
- **Docker** (any host): `docker run -d -p 5432:5432 plomid/plomid:latest` (pin a version with
  `plomid/plomid:<VERSION>`, e.g. `v0.1.0-beta.1`)
- **Source**: `git clone https://github.com/plomid/plomid.git` and `cargo build --release -p plomid-server`

Unsigned beta packages show the standard unknown-publisher prompt on first launch
(right-click > Open on macOS); Windows SmartScreen notes the same.

### Build & package from source

| Target | Command |
| --- | --- |
| Server binary (this host) | `make release` |
| Linux cross-build (amd64 / arm64) | `make linux-binary` / `make linux-binary-all` |
| Runtime Docker image | `make docker` |
| Hardened local container | `make docker-run PLOMID_PASSWORD=<secret>` |
| Gated beta tag → full release | `make release-beta VERSION=<x.y.z>` |

`make release-beta` runs every gate in order — fmt, check, clippy, deny, tests, a local release
build — and only then tags; CI takes over from there (matrix builds, checksums, GitHub Release,
multi-arch Docker).

### Startup and diagnostics

The server prints a terminal-aware startup banner and emits structured tracing logs. The default
level is `info`; override it with `PLOMID_LOG_LEVEL` or `RUST_LOG`:

```sh
PLOMID_LOG_LEVEL=debug ./start.sh
RUST_LOG=server=debug,server::protocol=trace ./start.sh
```

Connection and query logs include stable process-local `connection_id` and `query_id` fields,
statement type, duration, and error context. SQL values and passwords are intentionally excluded
from normal logs. Use `info` for normal operations, `debug` for lifecycle diagnostics, and `trace`
for protocol/parser investigation.

## Native Management CLI

Build the native CLI with `cargo build -p plomid-cli`; the executable is `target/debug/plomid`.
It probes the real PLOMID server over its wire protocol:

```sh
./target/debug/plomid db readiness --host 127.0.0.1 --port 55431 \
  --user plomid --password secret --database plomid --data-dir ./data
./target/debug/plomid db readiness --json --port 55431
./target/debug/plomid db health --port 55431
./target/debug/plomid db info --data-dir ./data
./target/debug/plomid db diagnostics --port 55431
./target/debug/plomid benchmark run --iterations 100 --port 55431
./target/debug/plomid test list
```

`db readiness` is non-destructive: write, transaction, and persistence probes are reported as
unavailable or warning until an isolated test-database mode is added. Benchmark results are written
to `DATA_DIR/benchmarks/<run-id>.json`. The current benchmark operation is `SELECT 1`; unsupported
concurrency or operations fail clearly instead of being simulated. The CLI's
`server start`/`server stop` commands use a pid file and are intended for local Unix development.

## Documentation

https://www.plomid.in/docs/

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. Security findings go to
[SECURITY.md](SECURITY.md); releases follow [RELEASE_BASELINE.md](RELEASE_BASELINE.md).

## Reusable Agent Checklist

Use this context before starting a coding task so agents do not repeatedly rediscover baseline
repository policy:

- Current milestone: M0 Engineering Foundation.
- Kernel language: Rust.
- Architecture rule: own the engine; do not introduce foundational dependencies on PostgreSQL,
  RocksDB, ClickHouse, DuckDB, SQLite, MySQL, or similar database engines.
- Monorepo rule: add Rust crates under `crates/*` and inherit workspace package metadata.
- Quality gates: build, fmt, clippy with `-D warnings`, tests, and dependency deny checks.
- Scope rule: do not add application logic to placeholder crates.
- Safety rule: no secrets, generated credentials, or machine-local paths in committed files.
- Review rule: inspect existing implementation before changing code and avoid unrelated rewrites.

## Copyright and License

```
// © 2026 PLOMID Technology Solutions
//
// PLOMID
// Platform for Modern Intelligence and Data
//
// Author: Sainath Sapa
// GitHub: https://github.com/sainathsapa
//
// Licensed under the Apache License, Version 2.0; see LICENSE for the full text.
```

- **Copyright:** © 2026 PLOMID Technology Solutions
- **License:** [Apache-2.0](LICENSE)
- **Author:** Sainath Sapa
- **GitHub:** https://github.com/sainathsapa
