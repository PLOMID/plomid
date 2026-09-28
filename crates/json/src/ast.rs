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
//! The document modality's AST vocabulary.
//!
//! These describe document *syntax* — the JSON kind an `IS JSON` predicate
//! checks for, the NULL-handling clause of the SQL/JSON constructors, and the
//! shape of a `JSON_TABLE(... COLUMNS (...))` declaration. They live beside the
//! document operators so that the parser consumes the document modality's
//! vocabulary instead of defining it: adding a modality (vector, graph, blob)
//! follows the same pattern, with the parser depending on that crate.
//!
//! The types here carry only names, paths and literals as text; resolving them
//! against the type registry is the executor's job.

/// NULL-handling clause for SQL/JSON constructors that accept
/// `NULL ON NULL` or `ABSENT ON NULL` (JSON_ARRAY, JSON_OBJECT,
/// JSON_ARRAYAGG, JSON_OBJECTAGG).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullHandling {
    /// `NULL ON NULL`: keep SQL NULLs as JSON `null`.
    NullOnNull,
    /// `ABSENT ON NULL`: omit SQL NULL entries from the result.
    AbsentOnNull,
}

/// JSON kind requested by `expr IS [NOT] JSON [VALUE|OBJECT|ARRAY|SCALAR]`.
/// `Any` corresponds to the bare `IS JSON` form (any valid JSON document).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonKind {
    /// Bare `IS JSON`: any JSON document (object, array, scalar).
    Any,
    /// `IS JSON VALUE`: any JSON scalar or document (non-null JSON).
    Value,
    /// `IS JSON OBJECT`: a JSON object.
    Object,
    /// `IS JSON ARRAY`: a JSON array.
    Array,
    /// `IS JSON SCALAR`: a JSON scalar (number, string, boolean, null).
    Scalar,
}

/// A single column declaration in a `JSON_TABLE(... COLUMNS (...))` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonTableColumn {
    /// Column name as written (e.g. `id`, `name`). `None` only for a bare
    /// `NESTED` declaration (which introduces sub-columns, not a column).
    pub name: Option<String>,
    /// SQL type name (e.g. `INTEGER`, `TEXT`, `BOOLEAN`, `NUMERIC`, `JSON`).
    /// `None` for `FOR ORDINALITY` and `NESTED` declarations.
    pub type_name: Option<String>,
    pub kind: JsonTableColumnKind,
    /// JSON path applied to a row element (e.g. `'$.id'`).
    pub path: Option<String>,
    /// `DEFAULT '<json>' ON EMPTY` / `ON ERROR` / `ON EMPTY OR ERROR`.
    pub default_mode: JsonTableColumnDefault,
    pub default_value: Option<String>,
    /// A second `DEFAULT '<json>' ON ERROR` clause, distinguishing conversion
    /// errors from the empty/missing-path target of `default_mode`
    /// (e.g. `DEFAULT '999' ON EMPTY DEFAULT '888' ON ERROR`).
    pub error_default_mode: JsonTableColumnDefault,
    pub error_default_value: Option<String>,
}

/// The variant-specific part of a JSON_TABLE column declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonTableColumnKind {
    /// `name TYPE PATH '$.field'` (or `name TYPE FORMAT JSON PATH '$.field'`).
    Regular,
    /// `name FOR ORDINALITY` — the 1-based row ordinal.
    Ordinality,
    /// `name TYPE EXISTS PATH '$.field'` — true when the path exists.
    Exists,
    /// A nested expansion: `NESTED PATH '$.items[*]' COLUMNS ( ... )`.
    Nested { columns: Vec<JsonTableColumn> },
}

/// When a JSON_TABLE column's path misses (or errors), the declared default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonTableColumnDefault {
    None,
    Empty,
    Error,
    EmptyOrError,
}
