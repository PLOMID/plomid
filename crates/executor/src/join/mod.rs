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
//! Join-aware SELECT execution.
//!
//! This module executes `FROM` trees (single tables, subqueries, and JOIN
//! combinations) with alias handling, qualified column resolution, ambiguous
//! column detection, window functions, and scalar / `IN` / `EXISTS`
//! subqueries. The single-table fast path in `crate::query` remains
//! authoritative for simple scans; this module is the general engine those
//! queries fall back to when they need join, subquery, or window semantics.
//!
//! # Module map
//!
//! * [`scope`] — `JoinScope`/`OuterContext`: the relations a row is evaluated
//!   against, and column resolution across them.
//! * [`aggregate`] — the join engine's aggregate accumulator and slot
//!   collection.
//! * [`support`] — relation materialization (table scan, FROM-subquery,
//!   `JSON_TABLE`) and the join algorithms: nested-loop, hash, indexed and
//!   lateral.
//! * [`eval`] — `JoinEval`, the expression evaluator for joined rows,
//!   subqueries, window functions and aggregates.
//! * [`expr`] — scalar helpers shared by evaluation and planning (arithmetic,
//!   LIKE matching, parameter substitution).
//! * [`plan`] — target planning, type inference, grouping validation, and
//!   `WITH` / recursive CTE execution and rewriting.
//! * [`statement`] — statement-level dispatch (`execute_statement`,
//!   `execute_with`) and set operations.
//! * [`select`] — the join SELECT driver: `execute_join_select`, ordering,
//!   grouped execution and set-returning functions.
mod aggregate;
mod eval;
mod expr;
mod plan;
mod scope;
mod select;
mod statement;
mod support;

pub(crate) use eval::JoinEval;
pub(crate) use expr::as_f64;
pub(crate) use plan::execute_with;
pub(crate) use scope::{JoinScope, OuterContext};
pub(crate) use select::execute_join_select;
pub(crate) use statement::execute_statement;
pub(crate) use support::unsupported;
