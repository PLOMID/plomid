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
//! A column's declared type.
//!
//! A column type is a resolved PostgreSQL type OID plus a type modifier. Kept
//! in the type registry rather than in the SQL AST because it is the *column*
//! contract every layer shares: the catalog stores it, the executor enforces it,
//! and the modality crates (document, and in time vector/graph/blob) cast into
//! and out of it.
//!
//! PLOMID's registry is the single type model — a hybrid table simply mixes
//! column types from several modalities in one column list; there is no
//! separate "document column type" or "vector column type" concept beyond the
//! OID the registry assigns.

/// A SQL data type.
///
/// Column types are resolved through the authoritative `plomid-types` registry
/// and stored as a resolved PostgreSQL type OID plus an optional type modifier
/// (`typmod`). All metadata, text input/output, wire encoding, constraint
/// enforcement, and casting delegate to `plomid-types`. There is no separate
/// PLOMID type model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColumnType {
    pub type_oid: crate::TypeOid,
    pub typmod: i32,
    /// When true, the column was declared SERIAL/BIGSERIAL/SMALLSERIAL and
    /// the catalog should provision a backing sequence automatically.
    pub serial: bool,
}

impl ColumnType {
    /// Creates a column type from a resolved PostgreSQL type OID and modifier.
    #[must_use]
    pub const fn new(type_oid: crate::TypeOid, typmod: i32) -> Self {
        Self {
            type_oid,
            typmod,
            serial: false,
        }
    }

    /// Creates a SERIAL-family column type (auto-provisions a sequence).
    #[must_use]
    pub const fn new_serial(type_oid: crate::TypeOid) -> Self {
        Self {
            type_oid,
            typmod: crate::NO_TYPEMOD,
            serial: true,
        }
    }

    /// The `integer` (`int4`) column type.
    #[must_use]
    pub const fn int4() -> Self {
        Self {
            type_oid: crate::TypeOid::INT4,
            typmod: crate::NO_TYPEMOD,
            serial: false,
        }
    }

    /// The `integer` (`int4`) column type.
    #[must_use]
    pub const fn integer() -> Self {
        Self {
            type_oid: crate::TypeOid::INT4,
            typmod: crate::NO_TYPEMOD,
            serial: false,
        }
    }

    /// The `bigint` (`int8`) column type.
    #[must_use]
    pub const fn bigint() -> Self {
        Self {
            type_oid: crate::TypeOid::INT8,
            typmod: crate::NO_TYPEMOD,
            serial: false,
        }
    }

    /// The `text` column type.
    #[must_use]
    pub const fn text() -> Self {
        Self {
            type_oid: crate::TypeOid::TEXT,
            typmod: crate::NO_TYPEMOD,
            serial: false,
        }
    }

    /// The `boolean` column type.
    #[must_use]
    pub const fn boolean() -> Self {
        Self {
            type_oid: crate::TypeOid::BOOL,
            typmod: crate::NO_TYPEMOD,
            serial: false,
        }
    }

    /// Creates a column type from a SQL type name (e.g. "INTEGER", "TEXT").
    /// Returns `None` for unrecognized types.
    #[must_use]
    pub fn from_type_name(type_name: &str) -> Option<Self> {
        let normalized = type_name.trim().to_ascii_lowercase();
        // Strip typmod suffix like varchar(10) -> varchar.
        let bare = normalized.split('(').next().unwrap_or("").trim();
        let oid = match bare {
            "smallint" | "int2" => crate::TypeOid::INT2,
            "integer" | "int" | "int4" => crate::TypeOid::INT4,
            "bigint" | "int8" => crate::TypeOid::INT8,
            "real" | "float4" => crate::TypeOid::FLOAT4,
            "double" | "double precision" | "float8" => crate::TypeOid::FLOAT8,
            "numeric" | "decimal" => crate::TypeOid::NUMERIC,
            "text" => crate::TypeOid::TEXT,
            "varchar" | "character varying" => crate::TypeOid::VARCHAR,
            "char" | "character" | "bpchar" => crate::TypeOid::BPCHAR,
            "boolean" | "bool" => crate::TypeOid::BOOL,
            "date" => crate::TypeOid::DATE,
            "time" | "time without time zone" => crate::TypeOid::TIME,
            "timetz" | "time with time zone" => crate::TypeOid::TIMETZ,
            "timestamp" | "timestamp without time zone" => crate::TypeOid::TIMESTAMP,
            "timestamptz" | "timestamp with time zone" => crate::TypeOid::TIMESTAMPTZ,
            "interval" => crate::TypeOid::INTERVAL,
            "uuid" => crate::TypeOid::UUID,
            "json" => crate::TypeOid::JSON,
            "jsonb" => crate::TypeOid::JSONB,
            "bytea" => crate::TypeOid::BYTEA,
            "oid" => crate::TypeOid::OID,
            "name" => crate::TypeOid::NAME,
            "inet" => crate::TypeOid::INET,
            "cidr" => crate::TypeOid::CIDR,
            "macaddr" => crate::TypeOid::MACADDR,
            "macaddr8" => crate::TypeOid::MACADDR8,
            _ => return None,
        };
        Some(Self {
            type_oid: oid,
            typmod: crate::NO_TYPEMOD,
            serial: false,
        })
    }

    /// Resolves the PostgreSQL builtin discriminant for this column type, if any.
    #[must_use]
    pub fn pg_type(&self) -> Option<crate::PgType> {
        crate::PgType::by_oid(self.type_oid)
    }

    /// Returns `true` if this type may be stored as a table column.
    #[must_use]
    pub fn can_be_column_type(&self) -> bool {
        self.pg_type().is_some_and(|ty| ty.can_be_column_type())
    }

    /// Stable serialization key: the raw PostgreSQL type OID.
    ///
    /// The catalog persists types as their authoritative OID rather than a
    /// PLOMID-proprietary tag, so persisted metadata is not a second type system.
    #[must_use]
    pub const fn type_tag(&self) -> u32 {
        self.type_oid.raw()
    }

    /// Rebuilds a column type from a persisted type OID (typmod recovered as
    /// none; it is re-derived at plan time).
    #[must_use]
    pub const fn from_type_oid(oid: crate::TypeOid) -> Self {
        Self {
            type_oid: oid,
            typmod: crate::NO_TYPEMOD,
            serial: false,
        }
    }
}
