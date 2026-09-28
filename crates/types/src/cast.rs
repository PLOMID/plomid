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
//! PostgreSQL-compatible cast registry and runtime cast application.

use crate::oid::TypeOid;
use crate::registry::PgType;
use crate::text;
use crate::typmod::{apply_typmod, encode_typmod, TypmodSpec, NO_TYPEMOD};
use crate::value::PgValue;
use plomid_core::{ErrorKind, PlomidError};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Cast context determines which casts are allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CastContext {
    /// Explicit cast (`CAST(x AS T)` or `x::T`).
    Explicit,
    /// Assignment cast (target column typing during INSERT/UPDATE).
    Assignment,
    /// Implicit cast (operator/function argument promotion).
    Implicit,
}

/// Kind of a registered cast.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CastKind {
    /// Performed via the text I/O functions or native coercion.
    InputOutput,
}

/// One registered cast from a source type to a destination type.
#[derive(Clone, Debug)]
pub struct CastEntry {
    /// Source type OID.
    pub src_oid: TypeOid,
    /// Destination type OID.
    pub dst_oid: TypeOid,
    /// Contexts in which the cast is permitted.
    pub contexts: &'static [CastContext],
    /// Mechanism used.
    pub kind: CastKind,
}

/// Registry of known type casts.
#[derive(Debug, Default)]
pub struct CastRegistry {
    entries: HashMap<(TypeOid, TypeOid), CastEntry>,
}

type TResult<T> = std::result::Result<T, PlomidError>;

fn cast_error(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::InvalidArgument, message.into())
}

fn text_cast(src: &PgValue, dst: PgType, dst_typmod: i32) -> TResult<PgValue> {
    let txt = src.to_sql_text();
    let parsed = text::parse_value(&txt, dst).map_err(|e| {
        // Avoid double-wrapping: if the inner error already starts with the
        // "invalid input syntax for type <name>" prefix, reuse it verbatim.
        let prefix = format!("invalid input syntax for type {}", dst.name());
        if e.starts_with(&prefix) {
            cast_error(e)
        } else {
            cast_error(format!(
                "invalid input syntax for type {}: {}",
                dst.name(),
                e
            ))
        }
    })?;
    if dst_typmod != NO_TYPEMOD {
        apply_typmod(parsed, dst_typmod, dst).map_err(cast_error)
    } else {
        Ok(parsed)
    }
}

fn identity_cast(src: PgValue, dst: PgType, dst_typmod: i32) -> TResult<PgValue> {
    let normalized = match (&src, dst) {
        (PgValue::Text(s), PgType::VarChar) => PgValue::VarChar(s.clone()),
        (PgValue::Text(s), PgType::BpChar) => PgValue::BpChar(s.clone()),
        (PgValue::VarChar(s), PgType::Text) => PgValue::Text(s.clone()),
        (PgValue::VarChar(s), PgType::BpChar) => PgValue::BpChar(s.clone()),
        (PgValue::BpChar(s), PgType::Text) => PgValue::Text(s.clone()),
        (PgValue::BpChar(s), PgType::VarChar) => PgValue::VarChar(s.trim_end().to_string()),
        _ => src,
    };
    if dst_typmod != NO_TYPEMOD {
        apply_typmod(normalized, dst_typmod, dst).map_err(cast_error)
    } else {
        Ok(normalized)
    }
}

fn numeric_promotion(src: &PgValue, dst: PgType, dst_typmod: i32) -> TResult<PgValue> {
    use PgValue::*;
    let result = match (src, dst) {
        (Int2(v), PgType::Int4) => Int4(i32::from(*v)),
        (Int2(v), PgType::Int8) => Int8(i64::from(*v)),
        (Int2(v), PgType::Float4) => Float4(f32::from(*v)),
        (Int2(v), PgType::Float8) => Float8(f64::from(*v)),
        (Int2(v), PgType::Numeric) => Numeric(crate::Numeric::from_i64(i64::from(*v))),
        (Int4(v), PgType::Int2) => Int2(
            (*v).try_into()
                .map_err(|_| cast_error(format!("smallint out of range: {v}")))?,
        ),
        (Int4(v), PgType::Int8) => Int8(i64::from(*v)),
        (Int4(v), PgType::Float4) => Float4(*v as f32),
        (Int4(v), PgType::Float8) => Float8(*v as f64),
        (Int4(v), PgType::Numeric) => Numeric(crate::Numeric::from_i64(i64::from(*v))),
        (Int8(v), PgType::Int2) => Int2(
            (*v).try_into()
                .map_err(|_| cast_error(format!("smallint out of range: {v}")))?,
        ),
        (Int8(v), PgType::Int4) => Int4(
            (*v).try_into()
                .map_err(|_| cast_error(format!("integer out of range: {v}")))?,
        ),
        (Int8(v), PgType::Float4) => Float4(*v as f32),
        (Int8(v), PgType::Float8) => Float8(*v as f64),
        (Int8(v), PgType::Numeric) => Numeric(crate::Numeric::from_i64(*v)),
        (Float4(v), PgType::Int2) => Int2(
            ((*v) as i64)
                .try_into()
                .map_err(|_| cast_error(format!("smallint out of range: {v}")))?,
        ),
        (Float4(v), PgType::Int4) => Int4(*v as i32),
        (Float4(v), PgType::Int8) => Int8(*v as i64),
        (Float4(v), PgType::Float8) => Float8(f64::from(*v)),
        (Float4(v), PgType::Numeric) => {
            Numeric(crate::Numeric::parse(&v.to_string()).map_err(cast_error)?)
        }
        (Float8(v), PgType::Int2) => Int2(
            ((*v) as i64)
                .try_into()
                .map_err(|_| cast_error(format!("smallint out of range: {v}")))?,
        ),
        (Float8(v), PgType::Int4) => Int4(*v as i32),
        (Float8(v), PgType::Int8) => Int8(*v as i64),
        (Float8(v), PgType::Float4) => Float4(*v as f32),
        (Float8(v), PgType::Numeric) => {
            Numeric(crate::Numeric::parse(&v.to_string()).map_err(cast_error)?)
        }
        (Numeric(n), PgType::Int2) => {
            // Round to integer first (PostgreSQL semantics)
            let rounded = n.clone().round_to_scale(0);
            let i = rounded
                .to_i64()
                .ok_or_else(|| cast_error("smallint out of range"))?;
            let v = i
                .try_into()
                .map_err(|_| cast_error("smallint out of range"))?;
            Int2(v)
        }
        (Numeric(n), PgType::Int4) => {
            // Round to integer first (PostgreSQL semantics)
            let rounded = n.clone().round_to_scale(0);
            let i = rounded
                .to_i64()
                .ok_or_else(|| cast_error("integer out of range"))?;
            let v = i
                .try_into()
                .map_err(|_| cast_error("integer out of range"))?;
            Int4(v)
        }
        (Numeric(n), PgType::Int8) => {
            // Round to integer first (PostgreSQL semantics)
            let rounded = n.clone().round_to_scale(0);
            let i = rounded
                .to_i64()
                .ok_or_else(|| cast_error("bigint out of range"))?;
            Int8(i)
        }
        (Numeric(n), PgType::Float4) => Float4(n.clone().to_f64() as f32),
        (Numeric(n), PgType::Float8) => Float8(n.clone().to_f64()),
        _ => return text_cast(src, dst, dst_typmod),
    };
    if dst_typmod != NO_TYPEMOD {
        apply_typmod(result, dst_typmod, dst).map_err(cast_error)
    } else {
        Ok(result)
    }
}

fn oid_casts(src: &PgValue, dst: PgType, dst_typmod: i32) -> TResult<PgValue> {
    use PgValue::*;
    match (src, dst) {
        (Oid(v), PgType::Int4) => Ok(Int4(*v as i32)),
        (Int4(v), PgType::Oid) => Ok(Oid(*v as u32)),
        (Reg { oid, .. }, PgType::Oid) => Ok(Oid(*oid)),
        (Reg { oid, .. }, PgType::Int4) => Ok(Int4(*oid as i32)),
        (Oid(v), PgType::RegClass) => Ok(Reg {
            oid: *v,
            name: None,
        }),
        (Oid(v), PgType::RegType) => Ok(Reg {
            oid: *v,
            name: None,
        }),
        (Int4(v), PgType::RegClass) => Ok(Reg {
            oid: *v as u32,
            name: None,
        }),
        (Int4(v), PgType::RegType) => Ok(Reg {
            oid: *v as u32,
            name: None,
        }),
        (Text(s), PgType::RegClass)
        | (VarChar(s), PgType::RegClass)
        | (BpChar(s), PgType::RegClass) => {
            let oid = s.parse::<u32>().unwrap_or_default();
            Ok(Reg {
                oid,
                name: Some(s.clone()),
            })
        }
        (Text(s), PgType::RegType)
        | (VarChar(s), PgType::RegType)
        | (BpChar(s), PgType::RegType) => {
            let oid = if let Ok(n) = s.parse::<u32>() {
                n
            } else {
                PgType::by_name(s).map(|t| t.oid().raw()).unwrap_or(0)
            };
            Ok(Reg {
                oid,
                name: Some(s.clone()),
            })
        }
        (Reg { oid, name }, PgType::Text) => {
            Ok(Text(name.clone().unwrap_or_else(|| oid.to_string())))
        }
        (Reg { oid, name }, PgType::VarChar) => {
            Ok(VarChar(name.clone().unwrap_or_else(|| oid.to_string())))
        }
        _ => text_cast(src, dst, dst_typmod),
    }
}

fn to_string_cast(src: &PgValue, dst: PgType, dst_typmod: i32) -> TResult<PgValue> {
    use PgValue::*;
    let text = match src {
        // PostgreSQL outputs boolean as 'true'/'false' when cast to text, not 't'/'f'
        Bool(b) => if *b { "true" } else { "false" }.to_string(),
        _ => src.to_sql_text(),
    };
    let result = match dst {
        PgType::Text => Text(text),
        PgType::VarChar => VarChar(text),
        PgType::BpChar => BpChar(text),
        PgType::Name => Name(text),
        PgType::Cstring => Cstring(text),
        PgType::Unknown => Unknown(text),
        PgType::Xml => Xml(text),
        _ => return text_cast(src, dst, dst_typmod),
    };
    if dst_typmod != NO_TYPEMOD {
        apply_typmod(result, dst_typmod, dst).map_err(cast_error)
    } else {
        Ok(result)
    }
}

impl CastRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a cast entry.
    pub fn register(&mut self, entry: CastEntry) {
        self.entries.insert((entry.src_oid, entry.dst_oid), entry);
    }

    /// Looks up a cast between two OIDs.
    #[must_use]
    pub fn lookup(&self, src: TypeOid, dst: TypeOid) -> Option<&CastEntry> {
        self.entries.get(&(src, dst))
    }

    /// True when the cast is permitted in `context`.
    #[must_use]
    pub fn allows(&self, src: TypeOid, dst: TypeOid, context: CastContext) -> bool {
        if src == dst {
            return true;
        }
        self.lookup(src, dst)
            .is_some_and(|e| e.contexts.contains(&context))
    }
}

/// Resolves a type name (with optional typmod suffix like `varchar(10)`) into a `(PgType, i32)`.
pub fn resolve_type_name(type_name: &str) -> TResult<(PgType, i32)> {
    let mut name = type_name.trim();
    let typmod_str = name.find('(').map(|start| {
        let end = name.rfind(')').unwrap_or(name.len());
        let modifier = &name[start..=end.min(name.len().saturating_sub(1))];
        name = name[..start].trim();
        modifier.to_string()
    });
    let is_array = name.ends_with("[]");
    if is_array {
        name = name[..name.len() - 2].trim();
    }
    let ty = PgType::by_name(name)
        .ok_or_else(|| cast_error(format!("type \"{name}\" does not exist")))?;
    let mut typmod = NO_TYPEMOD;
    if let Some(modifier) = typmod_str {
        let spec = TypmodSpec::parse(&modifier).map_err(cast_error)?;
        typmod = encode_typmod(ty, spec).map_err(cast_error)?;
    }
    let _ = is_array;
    Ok((ty, typmod))
}

/// Applies a cast from `src` value of type `src_oid` to `dst_oid`/`dst_typmod` value.
pub fn apply_cast(
    src: PgValue,
    src_oid: TypeOid,
    dst_oid: TypeOid,
    dst_typmod: i32,
) -> TResult<PgValue> {
    if src.is_null() {
        return Ok(PgValue::Null);
    }
    if src_oid == dst_oid {
        let dst_ty = PgType::by_oid(dst_oid)
            .ok_or_else(|| cast_error(format!("unknown destination type oid {dst_oid}")))?;
        return identity_cast(src, dst_ty, dst_typmod);
    }
    let src_ty = PgType::by_oid(src_oid)
        .ok_or_else(|| cast_error(format!("unknown source type oid {src_oid}")))?;
    let dst_ty = PgType::by_oid(dst_oid)
        .ok_or_else(|| cast_error(format!("unknown destination type oid {dst_oid}")))?;
    use PgType::*;
    match dst_ty {
        Text | VarChar | BpChar | Name | Cstring | Unknown | Xml => {
            return to_string_cast(&src, dst_ty, dst_typmod);
        }
        Bool => {
            return match src_ty {
                Bool => identity_cast(src, dst_ty, dst_typmod),
                Text | VarChar | BpChar | Name | Unknown => text_cast(&src, dst_ty, dst_typmod),
                _ => text_cast(&src, dst_ty, dst_typmod),
            };
        }
        Int2 | Int4 | Int8 | Float4 | Float8 | Numeric => {
            return match src_ty {
                Int2 | Int4 | Int8 | Float4 | Float8 | Numeric => {
                    numeric_promotion(&src, dst_ty, dst_typmod)
                }
                Text | VarChar | BpChar | Name | Unknown => text_cast(&src, dst_ty, dst_typmod),
                _ => text_cast(&src, dst_ty, dst_typmod),
            };
        }
        Date | Time | TimeTz | Timestamp | Timestamptz | Interval => {
            return match src_ty {
                Text | VarChar | BpChar | Name | Unknown => text_cast(&src, dst_ty, dst_typmod),
                _ => text_cast(&src, dst_ty, dst_typmod),
            };
        }
        Uuid => {
            return match src_ty {
                Text | VarChar | BpChar | Name | Unknown => text_cast(&src, dst_ty, dst_typmod),
                _ => text_cast(&src, dst_ty, dst_typmod),
            };
        }
        Oid => {
            return match src_ty {
                Oid | Int4 | RegClass | RegType | RegProc | RegProcedure | RegOper
                | RegOperator | RegConfig | RegDictionary | RegRole | RegNamespace
                | RegCollation => oid_casts(&src, dst_ty, dst_typmod),
                Text | VarChar | BpChar => text_cast(&src, dst_ty, dst_typmod),
                _ => text_cast(&src, dst_ty, dst_typmod),
            };
        }
        RegClass | RegType => {
            return match src_ty {
                Text | VarChar | BpChar | Name | Unknown | Oid | Int4 => {
                    oid_casts(&src, dst_ty, dst_typmod)
                }
                _ => text_cast(&src, dst_ty, dst_typmod),
            };
        }
        _ => {}
    }
    text_cast(&src, dst_ty, dst_typmod)
}

fn register_builtins() -> CastRegistry {
    let mut r = CastRegistry::new();
    let all_text = &[
        CastContext::Explicit,
        CastContext::Assignment,
        CastContext::Implicit,
    ];
    let explicit_only = &[CastContext::Explicit, CastContext::Assignment];
    let implicit_num = &[
        CastContext::Explicit,
        CastContext::Assignment,
        CastContext::Implicit,
    ];
    use PgType::*;
    for src in PgType::all() {
        if src.is_pseudo() {
            continue;
        }
        for dst in [Text, VarChar, BpChar, Name, Xml] {
            r.register(CastEntry {
                src_oid: src.oid(),
                dst_oid: dst.oid(),
                contexts: all_text,
                kind: CastKind::InputOutput,
            });
        }
    }
    for dst in [
        Bool,
        Int2,
        Int4,
        Int8,
        Float4,
        Float8,
        Numeric,
        Date,
        Time,
        TimeTz,
        Timestamp,
        Timestamptz,
        Interval,
        Uuid,
        Oid,
        RegClass,
        RegType,
    ] {
        for src in [Text, VarChar, BpChar] {
            r.register(CastEntry {
                src_oid: src.oid(),
                dst_oid: dst.oid(),
                contexts: explicit_only,
                kind: CastKind::InputOutput,
            });
        }
    }
    let numeric_types = [Int2, Int4, Int8, Float4, Float8, Numeric];
    for (i, &src) in numeric_types.iter().enumerate() {
        for (j, &dst) in numeric_types.iter().enumerate() {
            if i != j {
                r.register(CastEntry {
                    src_oid: src.oid(),
                    dst_oid: dst.oid(),
                    contexts: if i < j { implicit_num } else { explicit_only },
                    kind: CastKind::InputOutput,
                });
            }
        }
    }
    for src in [Oid, Int4, Text, VarChar, BpChar] {
        for dst in [RegClass, RegType] {
            r.register(CastEntry {
                src_oid: src.oid(),
                dst_oid: dst.oid(),
                contexts: explicit_only,
                kind: CastKind::InputOutput,
            });
        }
    }
    for src in [RegClass, RegType] {
        for dst in [Int4, Text, VarChar, BpChar, Oid] {
            r.register(CastEntry {
                src_oid: src.oid(),
                dst_oid: dst.oid(),
                contexts: all_text,
                kind: CastKind::InputOutput,
            });
        }
    }
    r.register(CastEntry {
        src_oid: Int4.oid(),
        dst_oid: Oid.oid(),
        contexts: implicit_num,
        kind: CastKind::InputOutput,
    });
    r.register(CastEntry {
        src_oid: Oid.oid(),
        dst_oid: Int4.oid(),
        contexts: implicit_num,
        kind: CastKind::InputOutput,
    });
    r
}

/// Global built-in cast registry singleton.
#[must_use]
pub fn builtin_casts() -> &'static CastRegistry {
    static REG: OnceLock<CastRegistry> = OnceLock::new();
    REG.get_or_init(register_builtins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typmod::decode_typmod;
    use crate::value::PgValue::*;

    #[test]
    fn cast_int_to_text() {
        let v = apply_cast(Int4(42), PgType::Int4.oid(), PgType::Text.oid(), NO_TYPEMOD).unwrap();
        assert_eq!(v, Text("42".into()));
    }

    #[test]
    fn cast_text_to_int4() {
        let v = apply_cast(
            Text("123".into()),
            PgType::Text.oid(),
            PgType::Int4.oid(),
            NO_TYPEMOD,
        )
        .unwrap();
        assert_eq!(v, Int4(123));
    }

    #[test]
    fn cast_int4_numeric_promotion() {
        let v = apply_cast(
            Int4(10),
            PgType::Int4.oid(),
            PgType::Numeric.oid(),
            NO_TYPEMOD,
        )
        .unwrap();
        assert!(matches!(v, Numeric(_)));
    }

    #[test]
    fn cast_varchar_with_typmod_error() {
        let (_, tm) = resolve_type_name("varchar(5)").unwrap();
        let v = apply_cast(
            Text("hello world".into()),
            PgType::Text.oid(),
            PgType::VarChar.oid(),
            tm,
        );
        assert!(v.is_err());
    }

    #[test]
    fn cast_null_propagates() {
        let v = apply_cast(Null, PgType::Int4.oid(), PgType::Text.oid(), NO_TYPEMOD).unwrap();
        assert_eq!(v, Null);
    }

    #[test]
    fn resolve_type_name_with_typmod() {
        let (ty, tm) = resolve_type_name("numeric(10,2)").unwrap();
        assert_eq!(ty, PgType::Numeric);
        assert_eq!(decode_typmod(ty, tm), Some((10, 2)));
    }

    #[test]
    fn oid_to_int4_and_back() {
        let v = apply_cast(
            Oid(12345),
            PgType::Oid.oid(),
            PgType::Int4.oid(),
            NO_TYPEMOD,
        )
        .unwrap();
        assert_eq!(v, Int4(12345));
        let v = apply_cast(v, PgType::Int4.oid(), PgType::Oid.oid(), NO_TYPEMOD).unwrap();
        assert_eq!(v, Oid(12345));
    }
}
