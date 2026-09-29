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
//! The single authoritative PostgreSQL type registry for PLOMID.
//!
//! Every subsystem (parser, binder, executor, storage encoder, wire protocol,
//! catalog) must resolve type metadata through this registry instead of
//! duplicating type knowledge. `pg_type` projections are generated from the
//! same definitions in [`crate::pg_catalog`].
//!
//! The registry models the PostgreSQL 17 built-in type set: base types with
//! canonical names, aliases and OIDs; typmod semantics; range/multirange
//! element relationships; serial shorthand; pseudo-type restrictions; and
//! user-defined enums, domains, composites and their array types.

use crate::oid::TypeOid;
use plomid_core::{ErrorKind, PlomidError};
use std::collections::HashMap;
use std::sync::Arc;

pub type TResult<T> = std::result::Result<T, PlomidError>;

fn type_error<T>(message: impl Into<String>) -> TResult<T> {
    Err(PlomidError::new(ErrorKind::InvalidArgument, message.into()))
}

/// Type categories matching `pg_type.typcategory`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypeCategory {
    /// Array types (`A`).
    Array,
    /// Boolean types (`B`).
    Boolean,
    /// Composite types (`C`).
    Composite,
    /// Date/time types (`D`).
    DateTime,
    /// Enum types (`E`).
    Enum,
    /// Geometric types (`G`).
    Geometric,
    /// Network address types (`I`).
    NetworkAddress,
    /// Numeric types (`N`).
    Numeric,
    /// Pseudo-types (`P`).
    Pseudo,
    /// Range types (`R`).
    Range,
    /// String types (`S`).
    String,
    /// Timespan types (`T`).
    Timespan,
    /// User-defined types (`U`).
    User,
    /// Bit-string types (`V`).
    BitString,
    /// Unknown type (`X`).
    Unknown,
}

impl TypeCategory {
    /// Returns the single-letter `pg_type.typcategory` code.
    #[must_use]
    pub const fn code(self) -> char {
        match self {
            Self::Array => 'A',
            Self::Boolean => 'B',
            Self::Composite => 'C',
            Self::DateTime => 'D',
            Self::Enum => 'E',
            Self::Geometric => 'G',
            Self::NetworkAddress => 'I',
            Self::Numeric => 'N',
            Self::Pseudo => 'P',
            Self::Range => 'R',
            Self::String => 'S',
            Self::Timespan => 'T',
            Self::User => 'U',
            Self::BitString => 'V',
            Self::Unknown => 'X',
        }
    }
}

/// Static builtin type discriminants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[allow(clippy::enum_variant_names)]
pub enum PgType {
    Bool,
    Int2,
    Int4,
    Int8,
    Numeric,
    Float4,
    Float8,
    Money,
    Char,
    BpChar,
    VarChar,
    Text,
    Name,
    Cstring,
    Unknown,
    Bytea,
    Date,
    Time,
    TimeTz,
    Timestamp,
    Timestamptz,
    Interval,
    Point,
    Line,
    Lseg,
    Box,
    Path,
    Polygon,
    Circle,
    Inet,
    Cidr,
    Macaddr,
    Macaddr8,
    Bit,
    VarBit,
    TsVector,
    TsQuery,
    GtsVector,
    Uuid,
    Json,
    Jsonb,
    Xml,
    Oid,
    RegProc,
    RegProcedure,
    RegOper,
    RegOperator,
    RegClass,
    RegType,
    RegConfig,
    RegDictionary,
    RegRole,
    RegNamespace,
    RegCollation,
    Tid,
    Xid,
    Cid,
    Int2Vector,
    OidVector,
    PgLsn,
    PgSnapshot,
    AclItem,
    Int4Range,
    Int8Range,
    NumRange,
    TsRange,
    TstzRange,
    DateRange,
    Int4MultiRange,
    Int8MultiRange,
    NumMultiRange,
    TsMultiRange,
    TstzMultiRange,
    DateMultiRange,
    Any,
    AnyArray,
    AnyElement,
    AnyNonArray,
    AnyEnum,
    AnyRange,
    AnyMultiRange,
    AnyCompatible,
    AnyCompatibleArray,
    AnyCompatibleNonArray,
    AnyCompatibleRange,
    AnyCompatibleMultiRange,
    Void,
    Trigger,
    LanguageHandler,
    Internal,
    Opaque,
    Record,
    EventTrigger,
    FdwHandler,
    TableAmHandler,
    IndexAmHandler,
    TsmHandler,
}

impl std::fmt::Display for PgType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Static definition of one builtin type, mirroring a `pg_type.dat` row.
#[derive(Debug)]
pub struct BuiltinDef {
    /// The type discriminant.
    pub ty: PgType,
    /// Canonical type name (`typname`).
    pub name: &'static str,
    /// Accepted SQL aliases (including the canonical name).
    pub aliases: &'static [&'static str],
    /// The type OID.
    pub oid: u32,
    /// Implicit array type OID (`typarray`), when applicable.
    pub array_oid: Option<u32>,
    /// `typcategory` code.
    pub category: TypeCategory,
    /// For range/multirange types: the element type OID.
    pub range_element: Option<u32>,
    /// True when the type cannot be stored in a table column.
    pub pseudo: bool,
}

macro_rules! def {
    ($ty:ident, $name:literal, [$($alias:literal),*], $oid:literal, $arr:expr, $cat:ident) => {
        BuiltinDef { ty: PgType::$ty, name: $name, aliases: &[$($alias),*], oid: $oid,
            array_oid: $arr, category: TypeCategory::$cat, range_element: None, pseudo: false }
    };
    ($ty:ident, $name:literal, [$($alias:literal),*], $oid:literal, $arr:expr, $cat:ident; element $el:literal) => {
        BuiltinDef { ty: PgType::$ty, name: $name, aliases: &[$($alias),*], oid: $oid,
            array_oid: $arr, category: TypeCategory::$cat, range_element: Some($el), pseudo: false }
    };
    ($ty:ident, $name:literal, $oid:literal, Pseudo) => {
        BuiltinDef { ty: PgType::$ty, name: $name, aliases: &[$name], oid: $oid,
            array_oid: None, category: TypeCategory::Pseudo, range_element: None, pseudo: true }
    };
}

/// The builtin type definition table — the single source of truth.
pub static BUILTINS: &[BuiltinDef] = &[
    def!(Bool, "bool", ["bool", "boolean"], 16, Some(1000), Boolean),
    def!(Int2, "int2", ["int2", "smallint"], 21, Some(1005), Numeric),
    def!(
        Int4,
        "int4",
        ["int4", "int", "integer"],
        23,
        Some(1007),
        Numeric
    ),
    def!(Int8, "int8", ["int8", "bigint"], 20, Some(1016), Numeric),
    def!(
        Numeric,
        "numeric",
        ["numeric", "decimal", "dec"],
        1700,
        Some(1231),
        Numeric
    ),
    def!(
        Float4,
        "float4",
        ["float4", "real"],
        700,
        Some(1021),
        Numeric
    ),
    def!(
        Float8,
        "float8",
        ["float8", "double precision", "double"],
        701,
        Some(1022),
        Numeric
    ),
    def!(Money, "money", ["money"], 790, Some(791), Numeric),
    def!(Char, "char", ["char"], 18, Some(1002), String),
    def!(
        BpChar,
        "bpchar",
        ["bpchar", "char", "character"],
        1042,
        Some(1014),
        String
    ),
    def!(
        VarChar,
        "varchar",
        ["varchar", "character varying"],
        1043,
        Some(1015),
        String
    ),
    def!(Text, "text", ["text"], 25, Some(1009), String),
    def!(Name, "name", ["name"], 19, Some(1003), String),
    def!(Cstring, "cstring", ["cstring"], 2275, None, Pseudo),
    def!(Unknown, "unknown", ["unknown"], 705, None, Pseudo),
    def!(Bytea, "bytea", ["bytea"], 17, Some(1001), User),
    def!(Date, "date", ["date"], 1082, Some(1182), DateTime),
    def!(
        Time,
        "time",
        ["time", "time without time zone"],
        1083,
        Some(1183),
        DateTime
    ),
    def!(
        TimeTz,
        "timetz",
        ["timetz", "time with time zone"],
        1266,
        Some(1270),
        DateTime
    ),
    def!(
        Timestamp,
        "timestamp",
        ["timestamp", "timestamp without time zone"],
        1114,
        Some(1115),
        DateTime
    ),
    def!(
        Timestamptz,
        "timestamptz",
        ["timestamptz", "timestamp with time zone"],
        1184,
        Some(1185),
        DateTime
    ),
    def!(
        Interval,
        "interval",
        ["interval"],
        1186,
        Some(1187),
        Timespan
    ),
    def!(Point, "point", ["point"], 600, Some(1017), Geometric),
    def!(Line, "line", ["line"], 628, Some(629), Geometric),
    def!(Lseg, "lseg", ["lseg"], 601, Some(1018), Geometric),
    def!(Box, "box", ["box"], 603, Some(1020), Geometric),
    def!(Path, "path", ["path"], 602, Some(1019), Geometric),
    def!(Polygon, "polygon", ["polygon"], 604, Some(1027), Geometric),
    def!(Circle, "circle", ["circle"], 718, Some(719), Geometric),
    def!(Inet, "inet", ["inet"], 869, Some(1041), NetworkAddress),
    def!(Cidr, "cidr", ["cidr"], 650, Some(651), NetworkAddress),
    def!(
        Macaddr,
        "macaddr",
        ["macaddr"],
        829,
        Some(1040),
        NetworkAddress
    ),
    def!(
        Macaddr8,
        "macaddr8",
        ["macaddr8"],
        774,
        Some(775),
        NetworkAddress
    ),
    def!(Bit, "bit", ["bit"], 1560, Some(1561), BitString),
    def!(
        VarBit,
        "varbit",
        ["varbit", "bit varying"],
        1562,
        Some(1563),
        BitString
    ),
    def!(TsVector, "tsvector", ["tsvector"], 3614, Some(3643), User),
    def!(TsQuery, "tsquery", ["tsquery"], 3615, Some(3645), User),
    def!(
        GtsVector,
        "gtsvector",
        ["gtsvector"],
        3642,
        Some(3644),
        User
    ),
    def!(Uuid, "uuid", ["uuid"], 2950, Some(2951), User),
    def!(Json, "json", ["json"], 114, Some(199), User),
    def!(Jsonb, "jsonb", ["jsonb"], 3802, Some(3807), User),
    def!(Xml, "xml", ["xml"], 142, Some(143), User),
    def!(Oid, "oid", ["oid"], 26, Some(1028), Numeric),
    def!(RegProc, "regproc", ["regproc"], 24, Some(1008), Numeric),
    def!(
        RegProcedure,
        "regprocedure",
        ["regprocedure"],
        2202,
        Some(2207),
        Numeric
    ),
    def!(RegOper, "regoper", ["regoper"], 2203, Some(2208), Numeric),
    def!(
        RegOperator,
        "regoperator",
        ["regoperator"],
        2204,
        Some(2209),
        Numeric
    ),
    def!(
        RegClass,
        "regclass",
        ["regclass"],
        2205,
        Some(2210),
        Numeric
    ),
    def!(RegType, "regtype", ["regtype"], 2206, Some(2211), Numeric),
    def!(
        RegConfig,
        "regconfig",
        ["regconfig"],
        3734,
        Some(3735),
        Numeric
    ),
    def!(
        RegDictionary,
        "regdictionary",
        ["regdictionary"],
        3769,
        Some(3770),
        Numeric
    ),
    def!(RegRole, "regrole", ["regrole"], 4096, Some(4097), Numeric),
    def!(
        RegNamespace,
        "regnamespace",
        ["regnamespace"],
        4089,
        Some(4090),
        Numeric
    ),
    def!(
        RegCollation,
        "regcollation",
        ["regcollation"],
        4191,
        Some(4192),
        Numeric
    ),
    def!(Tid, "tid", ["tid"], 27, Some(1010), User),
    def!(Xid, "xid", ["xid"], 28, Some(1011), Numeric),
    def!(Cid, "cid", ["cid"], 29, Some(1012), Numeric),
    def!(
        Int2Vector,
        "int2vector",
        ["int2vector"],
        22,
        Some(1006),
        User
    ),
    def!(OidVector, "oidvector", ["oidvector"], 30, Some(1013), User),
    def!(PgLsn, "pg_lsn", ["pg_lsn"], 3220, Some(3221), User),
    def!(
        PgSnapshot,
        "pg_snapshot",
        ["pg_snapshot"],
        5010,
        Some(4066),
        User
    ),
    def!(AclItem, "aclitem", ["aclitem"], 1033, Some(1034), User),
    def!(Int4Range, "int4range", ["int4range"], 3904, Some(3905), Range; element 23),
    def!(Int8Range, "int8range", ["int8range"], 3926, Some(3927), Range; element 20),
    def!(NumRange, "numrange", ["numrange"], 3906, Some(3907), Range; element 1700),
    def!(TsRange, "tsrange", ["tsrange"], 3908, Some(3909), Range; element 1114),
    def!(TstzRange, "tstzrange", ["tstzrange"], 3910, Some(3911), Range; element 1184),
    def!(DateRange, "daterange", ["daterange"], 3912, Some(3913), Range; element 1082),
    def!(Int4MultiRange, "int4multirange", ["int4multirange"], 4451, Some(615), User; element 23),
    def!(Int8MultiRange, "int8multirange", ["int8multirange"], 4536, Some(617), User; element 20),
    def!(NumMultiRange, "nummultirange", ["nummultirange"], 4537, Some(1232), User; element 1700),
    def!(TsMultiRange, "tsmultirange", ["tsmultirange"], 4538, Some(621), User; element 1114),
    def!(TstzMultiRange, "tstzmultirange", ["tstzmultirange"], 4539, Some(622), User; element 1184),
    def!(DateMultiRange, "datemultirange", ["datemultirange"], 4540, Some(623), User; element 1082),
    def!(Any, "any", 2276, Pseudo),
    def!(AnyArray, "anyarray", 2277, Pseudo),
    def!(AnyElement, "anyelement", 2283, Pseudo),
    def!(AnyNonArray, "anynonarray", 2776, Pseudo),
    def!(AnyEnum, "anyenum", 3500, Pseudo),
    def!(AnyRange, "anyrange", 3831, Pseudo),
    def!(AnyMultiRange, "anymultirange", 4557, Pseudo),
    def!(AnyCompatible, "anycompatible", 5077, Pseudo),
    def!(AnyCompatibleArray, "anycompatiblearray", 5078, Pseudo),
    def!(AnyCompatibleNonArray, "anycompatiblenonarray", 5079, Pseudo),
    def!(AnyCompatibleRange, "anycompatiblerange", 5080, Pseudo),
    def!(
        AnyCompatibleMultiRange,
        "anycompatiblemultirange",
        4570,
        Pseudo
    ),
    def!(Void, "void", 2278, Pseudo),
    def!(Trigger, "trigger", 2279, Pseudo),
    def!(LanguageHandler, "language_handler", 2280, Pseudo),
    def!(Internal, "internal", 2281, Pseudo),
    def!(Opaque, "opaque", 2282, Pseudo),
    def!(Record, "record", 2249, Pseudo),
    def!(EventTrigger, "event_trigger", 3838, Pseudo),
    def!(FdwHandler, "fdw_handler", 3115, Pseudo),
    def!(TableAmHandler, "table_am_handler", 325, Pseudo),
    def!(IndexAmHandler, "index_am_handler", 326, Pseudo),
    def!(TsmHandler, "tsm_handler", 3830, Pseudo),
];
impl PgType {
    /// Returns the static definition for this type.
    #[must_use]
    pub const fn def(self) -> &'static BuiltinDef {
        // BUILTINS is ordered identically to this enum.
        &BUILTINS[self as usize]
    }

    /// Canonical PostgreSQL type name (`pg_type.typname`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.def().name
    }

    /// The PostgreSQL type OID.
    #[must_use]
    pub const fn oid(self) -> TypeOid {
        TypeOid(self.def().oid)
    }

    /// All accepted SQL aliases (including the canonical name).
    #[must_use]
    pub const fn aliases(self) -> &'static [&'static str] {
        self.def().aliases
    }

    /// `pg_type.typcategory` category.
    #[must_use]
    pub const fn category(self) -> TypeCategory {
        self.def().category
    }

    /// OID of the implicit array type (`pg_type.typarray`), if any.
    #[must_use]
    pub const fn array_oid(self) -> Option<TypeOid> {
        match self.def().array_oid {
            Some(oid) => Some(TypeOid(oid)),
            None => None,
        }
    }

    /// For range/multirange types, the element type OID they are built over.
    #[must_use]
    pub const fn range_element(self) -> Option<TypeOid> {
        match self.def().range_element {
            Some(oid) => Some(TypeOid(oid)),
            None => None,
        }
    }

    /// True for pseudo-types (not storable in table columns).
    #[must_use]
    pub const fn is_pseudo(self) -> bool {
        matches!(self.def().category, TypeCategory::Pseudo) || self.def().pseudo
    }

    /// True if the type may be used as a table column type.
    #[must_use]
    pub const fn can_be_column_type(self) -> bool {
        !self.is_pseudo()
    }

    /// Looks up a builtin type by canonical name or alias (case-insensitive).
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        let lower = name.trim().to_ascii_lowercase();
        BUILTINS
            .iter()
            .find(|d| d.aliases.iter().any(|a| *a == lower))
            .map(|d| d.ty)
    }

    /// Looks up a builtin type by OID.
    #[must_use]
    pub fn by_oid(oid: TypeOid) -> Option<Self> {
        BUILTINS.iter().find(|d| d.oid == oid.raw()).map(|d| d.ty)
    }

    /// All builtin type discriminants.
    pub fn all() -> impl Iterator<Item = PgType> {
        BUILTINS.iter().map(|d| d.ty)
    }
}
/// Serial column kinds. These are column shorthand, not real `pg_type`
/// entries: `smallserial`/`serial`/`bigserial` are int2/int4/int8 columns
/// with a sequence default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SerialKind {
    /// `smallserial` / `serial2`.
    Small,
    /// `serial` / `serial4`.
    Regular,
    /// `bigserial` / `serial8`.
    Big,
}

impl SerialKind {
    /// Base integer type for this serial kind.
    #[must_use]
    pub const fn base(self) -> PgType {
        match self {
            Self::Small => PgType::Int2,
            Self::Regular => PgType::Int4,
            Self::Big => PgType::Int8,
        }
    }

    /// Parses a serial type name (case-insensitive).
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "smallserial" | "serial2" => Some(Self::Small),
            "serial" | "serial4" => Some(Self::Regular),
            "bigserial" | "serial8" => Some(Self::Big),
            _ => None,
        }
    }
}

/// A user-defined type (enum, domain or composite) known to the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserTypeDef {
    /// PostgreSQL enum type with ordered labels.
    Enum { labels: Vec<String> },
    /// Domain over a base type with validation constraints.
    Domain {
        base: TypeOid,
        typmod: i32,
        not_null: bool,
    },
    /// Composite type with named attributes.
    Composite { attributes: Vec<(String, TypeOid)> },
}

/// The kind a registered type entry can take.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeDef {
    /// Static builtin type.
    Builtin(PgType),
    /// Serial shorthand (base int type with sequence default).
    Serial(SerialKind),
    /// User-defined type.
    User(Arc<UserTypeDef>),
}

/// A resolved type reference: definition plus type modifier.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedType {
    /// Resolved type definition.
    pub def: TypeDef,
    /// Type modifier (`-1` for none).
    pub typmod: i32,
}

impl ResolvedType {
    /// Base builtin type OID (unwraps serials and domains).
    #[must_use]
    pub fn base_oid(&self) -> Option<TypeOid> {
        match &self.def {
            TypeDef::Builtin(ty) => Some(ty.oid()),
            TypeDef::Serial(serial) => Some(serial.base().oid()),
            TypeDef::User(user) => match user.as_ref() {
                UserTypeDef::Domain { base, .. } => Some(*base),
                UserTypeDef::Enum { .. } | UserTypeDef::Composite { .. } => None,
            },
        }
    }
}

pub use plomid_core::{FIRST_USER_OID, NO_TYPEMOD};
/// The authoritative type registry.
///
/// Contains all PostgreSQL builtin types plus dynamically registered
/// user-defined types (enums, domains, composites). All type metadata used
/// by PLOMID subsystems flows through this registry.
#[derive(Clone, Debug)]
pub struct TypeRegistry {
    /// Next OID for user-defined types.
    next_oid: u32,
    /// All entries keyed by OID (builtins seeded in `new`).
    entries: HashMap<u32, TypeDef>,
    /// Builtin name/alias -> definition.
    builtin_names: HashMap<String, TypeDef>,
    /// User-defined name -> OID.
    user_name_oids: HashMap<String, u32>,
}

impl Default for TypeRegistry {
    fn default() -> Self {
        let mut registry = Self {
            next_oid: FIRST_USER_OID,
            entries: HashMap::new(),
            builtin_names: HashMap::new(),
            user_name_oids: HashMap::new(),
        };
        for ty in PgType::all() {
            let def = TypeDef::Builtin(ty);
            registry.entries.insert(ty.oid().raw(), def.clone());
            for alias in ty.aliases() {
                registry
                    .builtin_names
                    .entry((*alias).to_string())
                    .or_insert(def.clone());
            }
            if let Some(array_oid) = ty.array_oid() {
                registry
                    .entries
                    .insert(array_oid.raw(), TypeDef::Builtin(ty));
            }
        }
        for name in [
            "smallserial",
            "serial2",
            "serial",
            "serial4",
            "bigserial",
            "serial8",
        ] {
            let kind = SerialKind::by_name(name).expect("valid serial name");
            registry
                .builtin_names
                .insert(name.to_string(), TypeDef::Serial(kind));
        }
        registry
    }
}
impl TypeRegistry {
    /// Creates a registry seeded with all builtin types.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolves a type name (canonical name, alias or serial name).
    ///
    /// # Errors
    /// Fails when the name is not a known type.
    pub fn resolve(&self, name: &str) -> TResult<ResolvedType> {
        let lower = name.trim().to_ascii_lowercase();
        if let Some(def) = self.builtin_names.get(&lower) {
            return Ok(ResolvedType {
                def: def.clone(),
                typmod: NO_TYPEMOD,
            });
        }
        if let Some(oid) = self.user_name_oids.get(&lower) {
            if let Some(def) = self.entries.get(oid) {
                return Ok(ResolvedType {
                    def: def.clone(),
                    typmod: NO_TYPEMOD,
                });
            }
        }
        type_error(format!("type \"{name}\" does not exist"))
    }

    /// Looks up a type entry by OID.
    #[must_use]
    pub fn by_oid(&self, oid: TypeOid) -> Option<&TypeDef> {
        self.entries.get(&oid.raw())
    }

    /// Registers a user-defined enum type and returns its new OID.
    ///
    /// # Errors
    /// Fails on duplicate names or empty label lists.
    pub fn register_enum(&mut self, name: &str, labels: Vec<String>) -> TResult<TypeOid> {
        let oid = self.allocate_user_type(name)?;
        if labels.is_empty() {
            return type_error("enum type must have at least one label");
        }
        self.entries.insert(
            oid.raw(),
            TypeDef::User(Arc::new(UserTypeDef::Enum { labels })),
        );
        Ok(oid)
    }

    /// Registers a domain over an existing type.
    ///
    /// # Errors
    /// Fails when the base type does not exist or the name is taken.
    pub fn register_domain(
        &mut self,
        name: &str,
        base: TypeOid,
        typmod: i32,
        not_null: bool,
    ) -> TResult<TypeOid> {
        if self.by_oid(base).is_none() {
            return type_error(format!("base type oid {base} does not exist"));
        }
        let oid = self.allocate_user_type(name)?;
        self.entries.insert(
            oid.raw(),
            TypeDef::User(Arc::new(UserTypeDef::Domain {
                base,
                typmod,
                not_null,
            })),
        );
        Ok(oid)
    }

    /// Registers a composite type from named attribute OIDs.
    ///
    /// # Errors
    /// Fails on duplicate names or unknown attribute types.
    pub fn register_composite(
        &mut self,
        name: &str,
        attributes: Vec<(String, TypeOid)>,
    ) -> TResult<TypeOid> {
        for (attr_name, attr_oid) in &attributes {
            if self.by_oid(*attr_oid).is_none() {
                return type_error(format!(
                    "attribute \"{attr_name}\" has unknown type oid {attr_oid}"
                ));
            }
        }
        let oid = self.allocate_user_type(name)?;
        self.entries.insert(
            oid.raw(),
            TypeDef::User(Arc::new(UserTypeDef::Composite { attributes })),
        );
        Ok(oid)
    }

    /// Resolves the base type of a domain OID.
    #[must_use]
    pub fn domain_base(&self, oid: TypeOid) -> Option<(TypeOid, i32, bool)> {
        match self.by_oid(oid) {
            Some(TypeDef::User(user)) => match user.as_ref() {
                UserTypeDef::Domain {
                    base,
                    typmod,
                    not_null,
                } => Some((*base, *typmod, *not_null)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Resolves enum labels for an enum type OID.
    #[must_use]
    pub fn enum_labels(&self, oid: TypeOid) -> Option<&[String]> {
        match self.by_oid(oid) {
            Some(TypeDef::User(user)) => match user.as_ref() {
                UserTypeDef::Enum { labels } => Some(labels),
                _ => None,
            },
            _ => None,
        }
    }

    fn allocate_user_type(&mut self, name: &str) -> TResult<TypeOid> {
        let lower = name.trim().to_ascii_lowercase();
        if self.builtin_names.contains_key(&lower) || self.user_name_oids.contains_key(&lower) {
            return type_error(format!("type \"{name}\" already exists"));
        }
        let valid = lower
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
            && lower
                .chars()
                .all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric());
        if !valid {
            return type_error(format!("\"{name}\" is not a valid type name"));
        }
        let oid = TypeOid(self.next_oid);
        self.next_oid += 1;
        self.user_name_oids.insert(lower, oid.raw());
        Ok(oid)
    }
}

#[cfg(test)]
mod tests {
    use super::{PgType, SerialKind, TypeDef, TypeRegistry, FIRST_USER_OID, NO_TYPEMOD};

    #[test]
    fn builtin_lookup_by_name_and_alias() {
        assert_eq!(PgType::by_name("integer"), Some(PgType::Int4));
        assert_eq!(PgType::by_name("INT8"), Some(PgType::Int8));
        assert_eq!(PgType::by_name("boolean"), Some(PgType::Bool));
        assert_eq!(PgType::by_name("double precision"), Some(PgType::Float8));
        assert_eq!(PgType::by_name("bit varying"), Some(PgType::VarBit));
        assert_eq!(PgType::by_name("timestamptz"), Some(PgType::Timestamptz));
        assert_eq!(PgType::by_name("nonexistent"), None);
    }

    #[test]
    fn all_builtin_oids_are_unique() {
        let mut oids: Vec<u32> = PgType::all().map(|t| t.oid().raw()).collect();
        oids.sort_unstable();
        let len = oids.len();
        oids.dedup();
        assert_eq!(oids.len(), len, "duplicate OIDs in builtin type list");
    }

    #[test]
    fn oids_match_postgresql() {
        assert_eq!(PgType::Int4.oid().raw(), 23);
        assert_eq!(PgType::Text.oid().raw(), 25);
        assert_eq!(PgType::Numeric.oid().raw(), 1700);
        assert_eq!(PgType::Jsonb.oid().raw(), 3802);
        assert_eq!(PgType::Int4Range.oid().raw(), 3904);
        assert_eq!(PgType::Int4MultiRange.oid().raw(), 4451);
        assert_eq!(PgType::Uuid.oid().raw(), 2950);
        assert_eq!(PgType::RegClass.oid().raw(), 2205);
        assert_eq!(PgType::Timestamptz.oid().raw(), 1184);
        assert_eq!(PgType::Macaddr8.oid().raw(), 774);
        assert_eq!(PgType::Int4.array_oid().unwrap().raw(), 1007);
    }

    #[test]
    fn pseudo_types_cannot_be_columns() {
        assert!(PgType::Any.is_pseudo());
        assert!(!PgType::Any.can_be_column_type());
        assert!(PgType::Trigger.is_pseudo());
        assert!(PgType::Int4.can_be_column_type());
        assert!(PgType::Cstring.is_pseudo());
    }

    #[test]
    fn range_elements() {
        assert_eq!(PgType::Int4Range.range_element(), Some(PgType::Int4.oid()));
        assert_eq!(
            PgType::NumMultiRange.range_element(),
            Some(PgType::Numeric.oid())
        );
        assert_eq!(PgType::Text.range_element(), None);
    }

    #[test]
    fn serial_kinds() {
        assert_eq!(SerialKind::by_name("serial"), Some(SerialKind::Regular));
        assert_eq!(SerialKind::by_name("serial2"), Some(SerialKind::Small));
        assert_eq!(SerialKind::by_name("bigserial"), Some(SerialKind::Big));
        assert_eq!(SerialKind::Regular.base(), PgType::Int4);
        let registry = TypeRegistry::new();
        assert!(matches!(
            registry.resolve("bigserial").unwrap().def,
            TypeDef::Serial(SerialKind::Big)
        ));
    }

    #[test]
    fn registry_resolve_and_register_user_types() {
        let mut registry = TypeRegistry::new();
        assert!(matches!(
            registry.resolve("text").unwrap().def,
            TypeDef::Builtin(PgType::Text)
        ));
        let enum_oid = registry
            .register_enum("mood", vec!["sad".into(), "happy".into()])
            .expect("register enum");
        assert!(enum_oid.raw() >= FIRST_USER_OID);
        assert!(registry.resolve("mood").is_ok());
        assert_eq!(registry.enum_labels(enum_oid).unwrap().len(), 2);
        let domain_oid = registry
            .register_domain("posint", PgType::Int4.oid(), NO_TYPEMOD, true)
            .expect("register domain");
        assert_eq!(
            registry.domain_base(domain_oid),
            Some((PgType::Int4.oid(), NO_TYPEMOD, true))
        );
        assert!(registry.resolve("nosuchtype").is_err());
        assert!(registry.register_enum("mood", vec!["x".into()]).is_err());
    }
}
