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
//! PostgreSQL-compatible type system for PLOMID.
//!
//! This crate is the single authoritative type registry for all PLOMID
//! subsystems: builtin types with correct OIDs/names/aliases, typmod
//! handling, runtime values, text and binary codecs, casts and operators.
//!
//! # The multi-modal extension point
//!
//! [`PgValue`] is the one row value every layer shares, and its payload
//! variants are how a modality appears in a row: [`PgValue::Json`] and
//! [`PgValue::Jsonb`] carry the document modality today, alongside the scalar,
//! temporal, array and composite forms. A hybrid table is a column list whose
//! [`ColumnType`]s resolve to different OIDs — there is no per-modality row or
//! table type.
//!
//! Adding a modality (vector, graph, timeseries, binary object) therefore means:
//!
//! 1. a payload variant on [`PgValue`] and an OID on [`PgType`] in the registry,
//! 2. text/binary codecs and casts next to the existing ones ([`coerce`], [`cast`]),
//! 3. a modality crate at the `plomid-json` layer owning its data model,
//!    operators and AST vocabulary.
//!
//! Coercion ([`coerce`]), name→OID resolution ([`custom_type_oid`]), "what type
//! is this value" ([`value_pg_type`]) and argument/value plumbing
//! ([`function`]) live here rather than in the parser or the executor so every
//! modality answers those questions identically.

#![forbid(unsafe_code)]

pub mod cast;
pub mod coerce;
pub mod column;
pub mod datetime;
pub mod func;
pub mod function;
pub mod jsonb;
pub mod name;
pub mod numeric;
pub mod oid;
pub mod ops;
pub mod registry;
pub mod text;
pub mod typmod;
pub mod value;
pub mod value_type;

pub use cast::{
    apply_cast, builtin_casts, resolve_type_name, CastContext, CastEntry, CastKind, CastRegistry,
};
pub use column::ColumnType;
pub use datetime::Interval;
pub use func::{builtin_functions, FunctionEntry, FunctionImpl, FunctionRegistry};
pub use jsonb::JsonbValue;
pub use name::{bare_type_name, custom_type_oid};
pub use numeric::Numeric;
pub use oid::TypeOid;
pub use ops::{
    builtin_operators, BinaryOp, BinaryOpEntry, OperatorRegistry, UnaryOp, UnaryOpEntry,
};
pub use registry::{PgType, SerialKind, TypeDef, TypeRegistry, UserTypeDef};
pub use typmod::{apply_typmod, decode_typmod, encode_typmod, TypmodSpec, NO_TYPEMOD};
pub use value::{PgValue, RangeData, SortKey, TsLexeme, TsQueryNode};
pub use value_type::value_pg_type;

/// Parses a SQL text literal into a typed value for the given builtin type.
pub fn parse_text_literal(input: &str, ty: PgType) -> Result<PgValue, String> {
    text::parse_value(input, ty)
}

/// Renders a value as SQL text output.
pub fn format_text(value: &PgValue) -> String {
    text::format_value_with_typmod(value, None)
}
