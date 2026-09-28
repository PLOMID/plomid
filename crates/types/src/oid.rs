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
//! PostgreSQL type OIDs.
//!
//! Values match `src/include/catalog/pg_type.h` and `pg_type.dat` of the
//! PostgreSQL 17 compatibility target. Every builtin type known to PLOMID
//! has a constant here; the type registry is built from these.

/// A PostgreSQL type identifier (`pg_type.oid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeOid(pub u32);

impl TypeOid {
    /// Invalid / unknown OID.
    pub const INVALID: TypeOid = TypeOid(0);

    /// Returns the raw numeric OID.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for TypeOid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Macro to declare OID constants compactly.
macro_rules! oids {
    ($($name:ident = $value:expr),* $(,)?) => {
        impl TypeOid {
            $(pub const $name: TypeOid = TypeOid($value);)*
        }
    };
}

oids! {
    // -- base types ------------------------------------------------------
    BOOL = 16,
    BYTEA = 17,
    CHAR = 18,             // "char" (single internal byte)
    NAME = 19,
    INT8 = 20,
    INT2 = 21,
    INT2VECTOR = 22,
    INT4 = 23,
    REGPROC = 24,
    TEXT = 25,
    OID = 26,
    TID = 27,
    XID = 28,
    CID = 29,
    OIDVECTOR = 30,
    JSON = 114,
    JSONB = 3802,
    XML = 142,
    POINT = 600,
    LSEG = 601,
    PATH = 602,
    BOX = 603,
    POLYGON = 604,
    LINE = 628,
    CIDR = 650,
    FLOAT4 = 700,
    FLOAT8 = 701,
    UNKNOWN = 705,
    CIRCLE = 718,
    MONEY = 790,
    MACADDR = 829,
    MACADDR8 = 774,
    INET = 869,
    BPCHAR = 1042,
    VARCHAR = 1043,
    DATE = 1082,
    TIME = 1083,
    TIMESTAMP = 1114,
    TIMESTAMPTZ = 1184,
    INTERVAL = 1186,
    TIMETZ = 1266,
    BIT = 1560,
    VARBIT = 1562,
    NUMERIC = 1700,
    UUID = 2950,
    TSVECTOR = 3614,
    TSQUERY = 3615,
    GTSVECTOR = 3642,
    PG_LSN = 3220,
    PG_SNAPSHOT = 5010,
    ACLITEM = 1033,
    // -- object identifier (reg*) types ----------------------------------
    REGPROCEDURE = 2202,
    REGOPER = 2203,
    REGOPERATOR = 2204,
    REGCLASS = 2205,
    REGTYPE = 2206,
    REGCONFIG = 3734,
    REGDICTIONARY = 3769,
    REGROLE = 4096,
    REGNAMESPACE = 4089,
    REGCOLLATION = 4191,
    // -- range types -----------------------------------------------------
    INT4RANGE = 3904,
    NUMRANGE = 3906,
    TSRANGE = 3908,
    TSTZRANGE = 3910,
    DATERANGE = 3912,
    INT8RANGE = 3926,
    // -- multirange types -------------------------------------------------
    INT4MULTIRANGE = 4451,
    INT8MULTIRANGE = 4536,
    NUMMULTIRANGE = 4537,
    TSMULTIRANGE = 4538,
    TSTZMULTIRANGE = 4539,
    DATEMULTIRANGE = 4540,
    // -- pseudo-types and polymorphic types --------------------------------
    ANY = 2276,
    ANYARRAY = 2277,
    VOID = 2278,
    TRIGGER = 2279,
    LANGUAGE_HANDLER = 2280,
    INTERNAL = 2281,
    OPAQUE = 2282,
    ANYELEMENT = 2283,
    ANYNONARRAY = 2776,
    ANYENUM = 3500,
    ANYRANGE = 3831,
    RECORD = 2249,
    EVENT_TRIGGER = 3838,
    FDW_HANDLER = 3115,
    TABLE_AM_HANDLER = 325,
    INDEX_AM_HANDLER = 326,
    TSM_HANDLER = 3830,
    ANYCOMPATIBLE = 5077,
    ANYCOMPATIBLEARRAY = 5078,
    ANYCOMPATIBLENONARRAY = 5079,
    ANYCOMPATIBLERANGE = 5080,
    ANYCOMPATIBLEMULTIRANGE = 4570,
    CSTRING = 22,
}

// -- array type OIDs (typarray of the corresponding base type) -----------
oids! {
    BOOL_ARRAY = 1000,
    BYTEA_ARRAY = 1001,
    CHAR_ARRAY = 1002,
    NAME_ARRAY = 1003,
    INT8_ARRAY = 1016,
    INT2_ARRAY = 1005,
    INT2VECTOR_ARRAY = 1006,
    INT4_ARRAY = 1007,
    REGPROC_ARRAY = 1008,
    TEXT_ARRAY = 1009,
    OID_ARRAY = 1028,
    TID_ARRAY = 1010,
    XID_ARRAY = 1011,
    CID_ARRAY = 1012,
    OIDVECTOR_ARRAY = 1013,
    JSON_ARRAY = 199,
    XML_ARRAY = 143,
    POINT_ARRAY = 1017,
    LSEG_ARRAY = 1018,
    PATH_ARRAY = 1019,
    BOX_ARRAY = 1020,
    POLYGON_ARRAY = 1027,
    LINE_ARRAY = 629,
    CIDR_ARRAY = 651,
    FLOAT4_ARRAY = 1021,
    FLOAT8_ARRAY = 1022,
    CIRCLE_ARRAY = 719,
    MONEY_ARRAY = 791,
    MACADDR_ARRAY = 1040,
    INET_ARRAY = 1041,
    MACADDR8_ARRAY = 775,
    BPCHAR_ARRAY = 1014,
    VARCHAR_ARRAY = 1015,
    DATE_ARRAY = 1182,
    TIME_ARRAY = 1183,
    TIMESTAMP_ARRAY = 1115,
    TIMESTAMPTZ_ARRAY = 1185,
    INTERVAL_ARRAY = 1187,
    TIMETZ_ARRAY = 1270,
    BIT_ARRAY = 1561,
    VARBIT_ARRAY = 1563,
    NUMERIC_ARRAY = 1231,
    UUID_ARRAY = 2951,
    TSVECTOR_ARRAY = 3643,
    TSQUERY_ARRAY = 3645,
    GTSVECTOR_ARRAY = 3644,
    REGPROCEDURE_ARRAY = 2207,
    REGOPER_ARRAY = 2208,
    REGOPERATOR_ARRAY = 2209,
    REGCLASS_ARRAY = 2210,
    REGTYPE_ARRAY = 2211,
    REGCONFIG_ARRAY = 3735,
    REGDICTIONARY_ARRAY = 3770,
    REGROLE_ARRAY = 4097,
    REGNAMESPACE_ARRAY = 4090,
    REGCOLLATION_ARRAY = 4192,
    INT4RANGE_ARRAY = 3905,
    NUMRANGE_ARRAY = 3907,
    TSRANGE_ARRAY = 3909,
    TSTZRANGE_ARRAY = 3911,
    DATERANGE_ARRAY = 3913,
    INT8RANGE_ARRAY = 3927,
    INT4MULTIRANGE_ARRAY = 615,
    INT8MULTIRANGE_ARRAY = 617,
    NUMMULTIRANGE_ARRAY = 1232,
    TSMULTIRANGE_ARRAY = 621,
    TSTZMULTIRANGE_ARRAY = 622,
    DATEMULTIRANGE_ARRAY = 623,
    PG_LSN_ARRAY = 3221,
    PG_SNAPSHOT_ARRAY = 4066,
    ACLITEM_ARRAY = 1034,
}

#[cfg(test)]
mod tests {
    use super::TypeOid;

    #[test]
    fn known_oids() {
        assert_eq!(TypeOid::INT4.raw(), 23);
        assert_eq!(TypeOid::TEXT.raw(), 25);
        assert_eq!(TypeOid::NUMERIC.raw(), 1700);
        assert_eq!(TypeOid::JSONB.raw(), 3802);
        assert_eq!(TypeOid::INT4RANGE.raw(), 3904);
        assert_eq!(TypeOid::INT4MULTIRANGE.raw(), 4451);
    }
}
