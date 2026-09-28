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
//! The builtin type of a runtime value.
//!
//! "What type is this value?" is a property of the type system, not of the
//! parser or the executor, so it is answered here — next to `PgValue` and
//! `PgType`. Returns `None` for SQL NULL and for values whose type is only
//! known from column metadata (arrays, enums, composites, `reg*` types).

use crate::{PgType, PgValue};

/// Maps a runtime value to its authoritative PostgreSQL builtin type..
/// Column types come from `plomid-types`, never from a parallel PLOMID type set..
/// Returns `None` for SQL NULL and for values whose type is only known from
/// column metadata (arrays, enums, composites, reg* types)..
pub fn value_pg_type(value: &PgValue) -> Option<PgType> {
    match value {
        PgValue::Null => None,
        PgValue::Bool(_) => Some(PgType::Bool),
        PgValue::Int2(_) => Some(PgType::Int2),
        PgValue::Int4(_) => Some(PgType::Int4),
        PgValue::Int8(_) => Some(PgType::Int8),
        PgValue::Numeric(_) => Some(PgType::Numeric),
        PgValue::Float4(_) => Some(PgType::Float4),
        PgValue::Float8(_) => Some(PgType::Float8),
        PgValue::Money(_) => Some(PgType::Money),
        PgValue::BpChar(_) | PgValue::VarChar(_) => Some(PgType::VarChar),
        PgValue::Text(_) => Some(PgType::Text),
        PgValue::Name(_) => Some(PgType::Name),
        PgValue::Xml(_) => Some(PgType::Xml),
        PgValue::Cstring(_) | PgValue::Unknown(_) => Some(PgType::Unknown),
        PgValue::Bytea(_) => Some(PgType::Bytea),
        PgValue::Date(_) => Some(PgType::Date),
        PgValue::Time(_) => Some(PgType::Time),
        PgValue::TimeTz { .. } => Some(PgType::TimeTz),
        PgValue::Timestamp(_) => Some(PgType::Timestamp),
        PgValue::Timestamptz(_) => Some(PgType::Timestamptz),
        PgValue::Interval(_) => Some(PgType::Interval),
        PgValue::Uuid(_) => Some(PgType::Uuid),
        PgValue::Json(_) => Some(PgType::Json),
        PgValue::Jsonb(_) => Some(PgType::Jsonb),
        PgValue::Bit { .. } => Some(PgType::Bit),
        PgValue::Point { .. } => Some(PgType::Point),
        PgValue::Line { .. } => Some(PgType::Line),
        PgValue::Lseg { .. } => Some(PgType::Lseg),
        PgValue::Box { .. } => Some(PgType::Box),
        PgValue::Path { .. } => Some(PgType::Path),
        PgValue::Polygon(_) => Some(PgType::Polygon),
        PgValue::Circle { .. } => Some(PgType::Circle),
        PgValue::Inet { .. } => Some(PgType::Inet),
        PgValue::Macaddr(_) => Some(PgType::Macaddr),
        PgValue::Macaddr8(_) => Some(PgType::Macaddr8),
        PgValue::TsVector(_) => Some(PgType::TsVector),
        PgValue::TsQuery(_) => Some(PgType::TsQuery),
        PgValue::Range { type_oid, .. } => PgType::by_oid(*type_oid),
        PgValue::MultiRange { type_oid, .. } => PgType::by_oid(*type_oid),
        PgValue::Array { .. } => None,
        PgValue::Enum { type_oid, .. } => PgType::by_oid(*type_oid),
        PgValue::Composite { type_oid, .. } => PgType::by_oid(*type_oid),
        PgValue::Oid(_) => Some(PgType::Oid),
        PgValue::Reg { .. } => None,
        PgValue::Tid { .. } => Some(PgType::Tid),
        PgValue::Xid(_) => Some(PgType::Xid),
        PgValue::Cid(_) => Some(PgType::Cid),
        PgValue::PgLsn(_) => Some(PgType::PgLsn),
        PgValue::PgSnapshot(_) => Some(PgType::PgSnapshot),
        PgValue::AclItem(_) => Some(PgType::AclItem),
        PgValue::Int2Vector(_) => Some(PgType::Int2Vector),
        PgValue::OidVector(_) => Some(PgType::OidVector),
    }
}
