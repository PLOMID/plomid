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
//! SQL parser, binder, and catalog-facing SQL types for PLOMID.
//!
//! # Modalities
//!
//! PLOMID is multi-modal: a single table can mix relational columns with
//! document, and in time vector, graph, timeseries and binary-object columns,
//! and one statement can query across them. That is reflected in the layering,
//! not in a separate "document parser" or "vector executor":
//!
//! ```text
//! plomid-core      errors and identifiers
//!      │
//! plomid-types     the type and value registry: PgType / PgValue (one row
//!      │           value whose payloads cover every modality), JsonbValue,
//!      │           coercion and casting
//!      │
//! plomid-json      one crate per modality: data model, operators, path
//! plomid-vector?   language, AST vocabulary and conversions. A modality crate
//! plomid-graph?    depends on core + types only — never on the parser or the
//! plomid-blob?     executor.
//!      │
//! plomid-sql       the parser/binder consumes each modality's vocabulary and
//!      │           re-exports it here (`JsonKind`, `JsonTableColumn`, …), so
//!      │           callers never reach into a modality crate directly
//!      │
//! plomid-executor  executes statements by dispatching into the modality crates
//! ```
//!
//! A hybrid table is therefore just a column list whose members resolve to
//! different `TypeOid`s; nothing needs to branch on "what kind of table is
//! this". Adding a modality means adding a crate at the `plomid-json` layer (a
//! payload in `PgValue`, OIDs in the registry, an AST vocabulary re-exported
//! here, and its operators), not extending the executor.
//!
//! [`Value`] is [`plomid_types::PgValue`]; the name is kept because every SQL
//! layer reads it as the runtime value of a row.

mod ast;
mod catalog;
mod error;
mod lexer;
mod parser;
mod result;

pub use ast::{
    ColumnDef, ColumnType, ComparisonOperator, Constraint, ConstraintKind, CopyDirection,
    CreateStatement, Cte, DomainConstraint, Expression, ForeignKeyAction, ForeignKeyMatch,
    FrameBound, FrameBounds, FrameSpec, FromClause, FunctionArg, GroupByClause, InsertSource,
    InsertValue, IsBooleanKind, JoinKind, JsonKind, JsonTableColumn, JsonTableColumnDefault,
    JsonTableColumnKind, NullHandling, OnConflict, OnConflictTarget, OrderByItem, Quantifier,
    SelectTarget, SetOpKind, Statement, Value, WindowSpec,
};
pub use catalog::{
    bare_type_name, custom_type_oid, load_catalog, save_catalog, Catalog, ColumnRule,
    DomainConstraintDefinition, DomainDefinition, FunctionArgDefinition, FunctionDefinition,
    InMemoryCatalog, IndexDefinition, RoleDefinition, StoredDomain, StoredFunction, StoredType,
    StoredView, TableSchema, TypeDefinition,
};
pub use error::{SqlError, SqlResult};
pub use lexer::{Keyword, LexError, Lexer, Token, TokenKind};
pub use parser::{ParseError, Parser};
pub use result::{value_pg_type, QueryResult};
