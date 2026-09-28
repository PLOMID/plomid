#![forbid(unsafe_code)]
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
//! Query execution foundations for PLOMID.
//!
//! Module ownership, so a change can be placed without searching:
//!
//! * `executor` — the `Executor` entry point and session/statement dispatch.
//! * `query` — single-table scan, filtering, grouping and projection.
//! * `join` — multi-relation execution, join planning and candidate discovery.
//! * `window` — `OVER` clauses: partitions, frames, ranking and window aggregates.
//! * `projection` — which stored columns a statement is allowed to read.
//! * `distinct` — `SELECT DISTINCT` deduplication.
//! * `dml` / `ddl` / `update_from` / `transaction` — statements that mutate data,
//!   schema, or transaction state.
//! * `encoding` / `row` — physical row framing versus logical row semantics.
//! * `json` / `jsonpath` — JSON and JSONB document handling, provided by
//!   `plomid-json` and re-exported here under their historic paths. `JSON_TABLE`
//!   row production lives there too (`plomid_json::table`), reached through the
//!   `SqlExpressionEval` adapter in `join`.
//! * `scalar` — the scalar function registry.
//! * `coerce` — type resolution, comparison coercion and casting, provided
//!   by `plomid-sql`.
//! * `catalog_fn` / `system_catalog` — `pg_catalog`-style introspection.
//! * `index` / `statistics` / `maintenance` / `context` — supporting services.

mod catalog_fn;
// Type resolution / coercion lives in the type registry so every modality shares it.
pub(crate) use plomid_types::coerce;
pub(crate) mod columnar_freshness;
pub(crate) mod context;
mod ddl;
mod distinct;
mod dml;
mod encoding;
mod error;
mod executor;
pub(crate) mod index;
mod join;
// The JSON document modality lives in its own crate; these aliases keep the
// executor's internal `crate::json::…` / `crate::jsonpath::…` paths resolving.
pub(crate) use plomid_json::ops as json;
pub(crate) use plomid_json::path as jsonpath;
pub(crate) mod maintenance;
pub(crate) mod maintenance_worker;
mod projection;
mod query;
mod row;
mod scalar;
pub(crate) mod statistics;
mod system_catalog;
mod transaction;
mod update_from;
mod util;
mod window;

pub use encoding::{decode_row, encode_row};
pub use error::{SqlError, SqlResult};
pub use executor::Executor;
pub use maintenance::{MaintenanceCoordinator, MaintenancePolicy, TableMaintenanceState};
pub use maintenance_worker::{MaintenanceWorker, WorkerLink};
