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
//! Query execution for single-table statements.
//!
//! The fast paths here answer plain scans and simple projections straight
//! from key/value rows; anything that needs joins, CTEs, window functions or
//! subqueries is delegated to the general engine in `crate::join`.
//!
//! # Module map
//!
//! * [`scan`] — physical-row decoding, zone-map pruning and value comparison.
//! * [`aggregate`] — aggregate accumulators, slot collection and grouped
//!   expression evaluation.
//! * [`select`] — the single-table SELECT fast path and its planning
//!   predicates.
//! * [`group`] — `GROUP BY` / `ORDER BY` / `DISTINCT` execution and
//!   projection.
//! * [`project`] — output column names and types.
//! * [`expr`] — predicate and scalar expression evaluation.
//! * [`compare`] — arithmetic, comparisons, `ANY` / `ALL` and datetime
//!   extraction.
//! * [`explain`] — `EXPLAIN` output and table statistics.
//! * [`vector`] — vector columnar planning for supported aggregate shapes.

mod aggregate;
mod compare;
mod explain;
mod expr;
mod group;
mod project;
mod scan;
mod select;
mod vector;

pub(crate) use aggregate::AggregateAccumulator;
pub(crate) use compare::{compare_values_for_any_all, extract_datetime_field};
pub(crate) use explain::explain_statement;
pub(crate) use expr::{evaluate_expression, evaluate_predicate, json_kind_matches};
pub(crate) use scan::value_cmp;
pub use select::execute_select;
pub(crate) use select::index_bounds_for_predicate;
