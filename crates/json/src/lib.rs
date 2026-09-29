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
//! PLOMID's document modality: JSON / JSONB.
//!
//! This crate owns everything about JSON as a *data model* rather than as a
//! set of functions:
//!
//! * [`ast`] — the document modality's AST vocabulary (JSON kinds, the
//!   SQL/JSON NULL-handling clause, and `JSON_TABLE` column declarations).
//! * [`path`] — the SQL/JSON path language (`JSONPath`) parser and evaluator.
//! * [`ops`] — the JSON operators and functions (`->`/`@>`, `jsonb_set`,
//!   `json_build_object`, the JSON aggregates, `jsonb_pretty`, ...), and
//!   [`ops::json_tree`], the document bridge used by `JSON_TABLE`.
//! * [`subscript`] — `jsonb['key']` / `jsonb[0]` / `array[i]` subscripting.
//! * [`table`] — `JSON_TABLE` / `json_to_record` row production, behind the
//!   [`table::ExpressionEval`] trait so the host supplies expression
//!   evaluation.
//!
//! It is deliberately a peer of the `sql` crate rather than part of the
//! executor, so that the other modalities PLOMID is growing — time series,
//! vector, graph, and binary objects — can each take the same shape (a
//! self-contained data-model crate with its own operators, storage format and
//! execution hooks) without re-entering the query executor.
//!
//! The dependency direction is one-way and this crate sits *below* the SQL
//! layer: it depends only on `plomid-types` — for [`plomid_types::PgValue`] (the
//! unified row value every modality shares, aliased `Value` throughout the SQL
//! layer), [`plomid_types::JsonbValue`] (the single decoded tree, never
//! `serde_json`) and [`plomid_types::ColumnType`] — plus `plomid-core` for
//! errors. It never depends on the parser or the executor; those depend on it.
//! The one thing it cannot own, expression evaluation, is inverted through
//! [`table::ExpressionEval`].

pub mod ast;
// Conversions between the decoded tree and SQL values are an implementation
// detail of `table`; nothing outside this crate needs them.
pub(crate) mod convert;
pub mod ops;
pub mod path;
pub mod subscript;
pub mod table;
