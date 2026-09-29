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
//! PostgreSQL-compatible function registry and evaluation hooks.

use crate::value::PgValue;
use plomid_core::{ErrorKind, PlomidError};
use std::collections::HashMap;
use std::sync::OnceLock;

type TResult<T> = std::result::Result<T, PlomidError>;

fn func_error(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::InvalidArgument, message.into())
}

fn invalid_arg(function: &str, detail: &str) -> PlomidError {
    func_error(format!(
        "invalid argument for function {function}: {detail}"
    ))
}

fn require_args(name: &str, args: &[PgValue], min: usize, max: usize) -> TResult<()> {
    if args.len() < min || args.len() > max {
        return Err(func_error(format!(
            "function {name} expects {} argument(s), got {}",
            if min == max {
                min.to_string()
            } else {
                format!("{min}-{max}")
            },
            args.len()
        )));
    }
    Ok(())
}

fn scalar_text(value: &PgValue) -> String {
    match value {
        PgValue::Text(s) | PgValue::VarChar(s) | PgValue::BpChar(s) | PgValue::Name(s) => s.clone(),
        PgValue::Unknown(s) | PgValue::Cstring(s) => s.clone(),
        PgValue::Null => String::new(),
        other => other.to_sql_text(),
    }
}

fn scalar_f64(value: &PgValue) -> Option<f64> {
    match value {
        PgValue::Int2(v) => Some(f64::from(*v)),
        PgValue::Int4(v) => Some(f64::from(*v)),
        PgValue::Int8(v) => Some(*v as f64),
        PgValue::Float4(v) => Some(f64::from(*v)),
        PgValue::Float8(v) => Some(*v),
        PgValue::Numeric(n) => Some(n.clone().to_f64()),
        _ => None,
    }
}

fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            other => vec![other],
        })
        .collect()
}

fn append_json_value(output: &mut String, value: &PgValue) {
    match value {
        PgValue::Null => output.push_str("null"),
        PgValue::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        PgValue::Int2(value) => output.push_str(&value.to_string()),
        PgValue::Int4(value) => output.push_str(&value.to_string()),
        PgValue::Int8(value) => output.push_str(&value.to_string()),
        PgValue::Float4(value) => output.push_str(&value.to_string()),
        PgValue::Float8(value) => output.push_str(&value.to_string()),
        PgValue::Numeric(value) => output.push_str(&value.to_string()),
        PgValue::Json(value) => output.push_str(value),
        PgValue::Jsonb(value) => output.push_str(&String::from_utf8_lossy(value)),
        other => {
            output.push('"');
            output.push_str(&json_escape(&other.to_sql_text()));
            output.push('"');
        }
    }
}

fn func_row_to_json(args: &[PgValue]) -> TResult<PgValue> {
    require_args("row_to_json", args, 1, 2)?;
    if args[0].is_null() {
        return Ok(PgValue::Null);
    }
    let PgValue::Composite { fields, .. } = &args[0] else {
        return Err(invalid_arg(
            "row_to_json",
            "argument must be a composite value",
        ));
    };
    let mut json = String::from("{");
    for (index, (field, value)) in fields.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push('"');
        json.push_str(&json_escape(field));
        json.push_str("\":");
        append_json_value(&mut json, value);
    }
    json.push('}');
    Ok(PgValue::Json(json))
}

fn func_format(args: &[PgValue]) -> TResult<PgValue> {
    if args.is_empty() {
        return Err(invalid_arg("format", "requires a format string"));
    }
    if args[0].is_null() {
        return Ok(PgValue::Null);
    }
    let format_string = scalar_text(&args[0]);
    let mut output = String::new();
    let mut values = args.iter().skip(1);
    let mut chars = format_string.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            output.push(ch);
            continue;
        }
        let spec = chars
            .next()
            .ok_or_else(|| invalid_arg("format", "incomplete format specifier"))?;
        if spec == '%' {
            output.push('%');
            continue;
        }
        let value = values
            .next()
            .ok_or_else(|| invalid_arg("format", "too few arguments for format"))?;
        if value.is_null() {
            output.push_str("<NULL>");
        } else {
            let text = scalar_text(value);
            match spec {
                's' => output.push_str(&text),
                'I' => {
                    output.push('"');
                    output.push_str(&text.replace('"', "\"\""));
                    output.push('"');
                }
                'L' => {
                    output.push('\'');
                    output.push_str(&text.replace('\'', "''"));
                    output.push('\'');
                }
                _ => return Err(invalid_arg("format", "unsupported format specifier")),
            }
        }
    }
    Ok(PgValue::Text(output))
}

fn func_obj_description(args: &[PgValue]) -> TResult<PgValue> {
    require_args("obj_description", args, 1, 2)?;
    Ok(PgValue::Null)
}

fn func_col_description(args: &[PgValue]) -> TResult<PgValue> {
    require_args("col_description", args, 2, 2)?;
    Ok(PgValue::Null)
}

/// `shobj_description(oid, catalog_name) -> text` shared-object descriptions; always NULL
/// here since PLOMID does not yet track shared-object comments.
fn func_shobj_description(args: &[PgValue]) -> TResult<PgValue> {
    require_args("shobj_description", args, 2, 2)?;
    Ok(PgValue::Null)
}

/// `pg_total_relation_size(regclass) -> bigint`: returns 0 (placeholder). The argument is
/// accepted but not used; PLOMID does not yet track real relation byte sizes.
fn func_pg_total_relation_size(args: &[PgValue]) -> TResult<PgValue> {
    require_args("pg_total_relation_size", args, 1, 1)?;
    Ok(PgValue::Int8(0))
}

/// `pg_table_size` / `pg_indexes_size` / `pg_relation_size`: byte-size stubs returning 0.
fn func_relation_size(args: &[PgValue]) -> TResult<PgValue> {
    require_args("pg_relation_size", args, 1, 1)?;
    Ok(PgValue::Int8(0))
}

/// `pg_get_partkeydef(index_oid) -> text`; NULL, since PLOMID has no range-partition
/// support yet.
fn func_pg_get_partkeydef(args: &[PgValue]) -> TResult<PgValue> {
    require_args("pg_get_partkeydef", args, 1, 1)?;
    Ok(PgValue::Null)
}

/// `pg_tablespace_location(oid) -> text`: returns the absolute on-disk path of the
/// tablespace. PLOMID's only real tablespace is the default (`1663`); other OIDs map to
/// a synthetic path. The value is used only for display by clients.
fn func_pg_tablespace_location(args: &[PgValue]) -> TResult<PgValue> {
    require_args("pg_tablespace_location", args, 1, 1)?;
    let oid_val = scalar_text(&args[0]);
    let path = match oid_val.as_str() {
        "1663" => "/plomid",
        "1664" => "/plomid/global",
        other => return Ok(PgValue::Text(format!("/plomid/pg_tblspc/{}", other))),
    };
    Ok(PgValue::Text(path.into()))
}

/// `has_database_privilege(...) -> bool`; stubbed to `true` so DBeaver catalog browsing
/// does not abort on privilege introspection.
fn func_has_privilege(args: &[PgValue]) -> TResult<PgValue> {
    // Accept any arity PostgreSQL exposes for has_*_privilege.
    require_args("has_database_privilege", args, 1, usize::MAX)?;
    Ok(PgValue::Bool(true))
}

/// `quote_ident(text) -> text`: wraps a possibly-qualified identifier in double quotes and
/// escapes inner double quotes by doubling them, matching PostgreSQL's `quote_ident`.
fn func_quote_ident(args: &[PgValue]) -> TResult<PgValue> {
    require_args("quote_ident", args, 1, 1)?;
    let value = &args[0];
    if value.is_null() {
        return Ok(PgValue::Null);
    }
    let text = scalar_text(value);
    let mut quoted = String::with_capacity(text.len() + text.matches('"').count() + 2);
    quoted.push('"');
    for ch in text.chars() {
        if ch == '"' {
            quoted.push_str("\"\"");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('"');
    Ok(PgValue::Text(quoted))
}

fn func_pg_show_all_settings(_args: &[PgValue]) -> TResult<PgValue> {
    Ok(PgValue::Null)
}

fn func_coalesce(args: &[PgValue]) -> TResult<PgValue> {
    for arg in args {
        if !arg.is_null() {
            return Ok(arg.clone());
        }
    }
    Ok(PgValue::Null)
}

fn func_nullif(args: &[PgValue]) -> TResult<PgValue> {
    require_args("nullif", args, 2, 2)?;
    let av = &args[0];
    let bv = &args[1];
    if av.is_null() || bv.is_null() {
        return Ok(av.clone());
    }
    if av.compare(bv) == std::cmp::Ordering::Equal {
        Ok(PgValue::Null)
    } else {
        Ok(av.clone())
    }
}

fn func_concat(args: &[PgValue]) -> TResult<PgValue> {
    let mut out = String::new();
    for arg in args {
        if !arg.is_null() {
            out.push_str(&scalar_text(arg));
        }
    }
    Ok(PgValue::Text(out))
}

fn func_concat_ws(args: &[PgValue]) -> TResult<PgValue> {
    require_args("concat_ws", args, 2, usize::MAX)?;
    if args[0].is_null() {
        return Ok(PgValue::Null);
    }
    let separator = scalar_text(&args[0]);
    let joined = args[1..]
        .iter()
        .filter(|arg| !arg.is_null())
        .map(scalar_text)
        .collect::<Vec<_>>()
        .join(&separator);
    Ok(PgValue::Text(joined))
}

fn func_lower(args: &[PgValue]) -> TResult<PgValue> {
    require_args("lower", args, 1, 1)?;
    if args[0].is_null() {
        return Ok(PgValue::Null);
    }
    Ok(PgValue::Text(scalar_text(&args[0]).to_lowercase()))
}

fn func_upper(args: &[PgValue]) -> TResult<PgValue> {
    require_args("upper", args, 1, 1)?;
    if args[0].is_null() {
        return Ok(PgValue::Null);
    }
    Ok(PgValue::Text(scalar_text(&args[0]).to_uppercase()))
}

fn func_length(args: &[PgValue]) -> TResult<PgValue> {
    require_args("length", args, 1, 1)?;
    if args[0].is_null() {
        return Ok(PgValue::Null);
    }
    Ok(PgValue::Int4(scalar_text(&args[0]).chars().count() as i32))
}

fn func_trim(lname: &str, args: &[PgValue]) -> TResult<PgValue> {
    require_args(lname, args, 1, 2)?;
    if args.iter().any(PgValue::is_null) {
        return Ok(PgValue::Null);
    }
    let mut text = scalar_text(&args[0]);
    let cutset: Vec<char> = if args.len() == 2 {
        scalar_text(&args[1]).chars().collect()
    } else {
        vec![' ']
    };
    match lname {
        "ltrim" => text = text.trim_start_matches(|c| cutset.contains(&c)).to_string(),
        "rtrim" => text = text.trim_end_matches(|c| cutset.contains(&c)).to_string(),
        _ => text = text.trim_matches(|c| cutset.contains(&c)).to_string(),
    }
    Ok(PgValue::Text(text))
}

fn func_substring(args: &[PgValue]) -> TResult<PgValue> {
    require_args("substring", args, 2, 3)?;
    if args.iter().any(PgValue::is_null) {
        return Ok(PgValue::Null);
    }
    let text: Vec<char> = scalar_text(&args[0]).chars().collect();
    let start =
        scalar_f64(&args[1]).ok_or_else(|| invalid_arg("substring", "start must be numeric"))?;
    let start = start.trunc().max(1.0) as usize;
    let len: Option<usize> = if args.len() == 3 {
        Some(
            scalar_f64(&args[2])
                .ok_or_else(|| invalid_arg("substring", "length must be numeric"))?
                .trunc()
                .max(0.0) as usize,
        )
    } else {
        None
    };
    let begin = (start - 1).min(text.len());
    let end = match len {
        Some(l) => begin + l.min(text.len() - begin),
        None => text.len(),
    };
    Ok(PgValue::Text(text[begin..end].iter().collect()))
}

/// `position(substring, string) -> int4`: returns the 1-based character
/// position of `substring` in `string`; 0 when not found; 1 when `substring`
/// is empty (matching PostgreSQL `strpos` / `position` semantics). NULL
/// propagation is handled by the registry's `strict` flag, so this body only
/// runs when both arguments are non-NULL. Positioning is character-based (not
/// byte-based) to match PostgreSQL's multi-byte text semantics and the
/// engine's own `length` implementation.
fn func_position(args: &[PgValue]) -> TResult<PgValue> {
    require_args("position", args, 2, 2)?;
    let needle = scalar_text(&args[0]);
    let haystack = scalar_text(&args[1]);
    let position = match haystack.find(needle.as_str()) {
        Some(byte_index) => haystack[..byte_index].chars().count() as i32 + 1,
        None => 0,
    };
    Ok(PgValue::Int4(position))
}

fn func_power(args: &[PgValue]) -> TResult<PgValue> {
    require_args("power", args, 2, 2)?;
    if args.iter().any(PgValue::is_null) {
        return Ok(PgValue::Null);
    }
    let a = scalar_f64(&args[0]).ok_or_else(|| invalid_arg("power", "base must be numeric"))?;
    let b = scalar_f64(&args[1]).ok_or_else(|| invalid_arg("power", "exponent must be numeric"))?;
    Ok(PgValue::Float8(a.powf(b)))
}

/// Function implementation type.
pub type FunctionImpl = fn(&[PgValue]) -> TResult<PgValue>;

/// Registered function metadata.
#[derive(Clone, Debug)]
pub struct FunctionEntry {
    /// Canonical (lowercase) function name.
    pub name: &'static str,
    /// Minimum argument count.
    pub min_args: usize,
    /// Maximum argument count (inclusive).
    pub max_args: usize,
    /// True when a single NULL argument forces a NULL result (strict).
    /// Non-strict functions handle NULL arguments themselves.
    pub strict: bool,
    /// True when the function returns a set of rows rather than one value.
    pub returns_set: bool,
    /// Implementation, or None when the function requires external context
    /// (session info, storage engine access) and is handled by the executor.
    pub implementation: Option<FunctionImpl>,
}

/// Lookup table for built-in functions.
#[derive(Debug, Default)]
pub struct FunctionRegistry {
    entries: HashMap<String, FunctionEntry>,
}

impl FunctionRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a function entry.
    pub fn register(&mut self, entry: FunctionEntry) {
        self.entries.insert(entry.name.to_string(), entry);
    }

    /// Looks up a function by (case-insensitive) name.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&FunctionEntry> {
        self.entries.get(&name.to_ascii_lowercase())
    }

    /// Evaluates a pure scalar function through the registry.
    ///
    /// Returns `Ok(None)` when the function is not registered or requires
    /// external context (caller should fall back to another path).
    pub fn call(&self, name: &str, args: Vec<PgValue>) -> TResult<Option<PgValue>> {
        let Some(entry) = self.lookup(name) else {
            return Ok(None);
        };
        if entry.returns_set {
            return Ok(None);
        }
        if entry.strict && args.iter().any(PgValue::is_null) {
            return Ok(Some(PgValue::Null));
        }
        let Some(imp) = entry.implementation else {
            return Ok(None);
        };
        Ok(Some(imp(&args)?))
    }
}

fn register_builtins() -> FunctionRegistry {
    let mut r = FunctionRegistry::new();
    let strict = true;
    let nonstrict = false;
    let no_set = false;
    r.register(FunctionEntry {
        name: "format",
        min_args: 1,
        max_args: usize::MAX,
        strict: nonstrict,
        returns_set: no_set,
        implementation: Some(func_format),
    });
    r.register(FunctionEntry {
        name: "obj_description",
        min_args: 1,
        max_args: 2,
        strict,
        returns_set: no_set,
        implementation: Some(func_obj_description),
    });
    r.register(FunctionEntry {
        name: "col_description",
        min_args: 2,
        max_args: 2,
        strict,
        returns_set: no_set,
        implementation: Some(func_col_description),
    });
    r.register(FunctionEntry {
        name: "pg_total_relation_size",
        min_args: 1,
        max_args: 1,
        strict,
        returns_set: no_set,
        implementation: Some(func_pg_total_relation_size),
    });
    r.register(FunctionEntry {
        name: "pg_show_all_settings",
        min_args: 0,
        max_args: 0,
        strict: nonstrict,
        returns_set: true,
        implementation: Some(func_pg_show_all_settings),
    });
    r.register(FunctionEntry {
        name: "shobj_description",
        min_args: 2,
        max_args: 2,
        strict,
        returns_set: no_set,
        implementation: Some(func_shobj_description),
    });
    r.register(FunctionEntry {
        name: "pg_get_partkeydef",
        min_args: 1,
        max_args: 1,
        strict,
        returns_set: no_set,
        implementation: Some(func_pg_get_partkeydef),
    });
    r.register(FunctionEntry {
        name: "quote_ident",
        min_args: 1,
        max_args: 1,
        strict,
        returns_set: no_set,
        implementation: Some(func_quote_ident),
    });
    for name in [
        "has_database_privilege",
        "has_table_privilege",
        "has_schema_privilege",
    ] {
        r.register(FunctionEntry {
            name,
            min_args: 1,
            max_args: usize::MAX,
            strict,
            returns_set: no_set,
            implementation: Some(func_has_privilege),
        });
    }
    r.register(FunctionEntry {
        name: "pg_tablespace_location",
        min_args: 1,
        max_args: 1,
        strict,
        returns_set: no_set,
        implementation: Some(func_pg_tablespace_location),
    });
    for name in ["pg_relation_size", "pg_table_size", "pg_indexes_size"] {
        r.register(FunctionEntry {
            name,
            min_args: 1,
            max_args: 1,
            strict,
            returns_set: no_set,
            implementation: Some(func_relation_size),
        });
    }
    r.register(FunctionEntry {
        name: "row_to_json",
        min_args: 1,
        max_args: 2,
        strict: nonstrict,
        returns_set: no_set,
        implementation: Some(func_row_to_json),
    });
    for name in ["nextval", "currval", "lastval"] {
        r.register(FunctionEntry {
            name,
            min_args: if name == "lastval" { 0 } else { 1 },
            max_args: if name == "lastval" { 0 } else { 1 },
            strict,
            returns_set: no_set,
            implementation: None,
        });
    }
    for name in [
        "current_database",
        "current_schema",
        "current_user",
        "session_user",
        "user",
        "version",
    ] {
        r.register(FunctionEntry {
            name,
            min_args: 0,
            max_args: 0,
            strict: nonstrict,
            returns_set: no_set,
            implementation: None,
        });
    }
    r.register(FunctionEntry {
        name: "coalesce",
        min_args: 1,
        max_args: usize::MAX,
        strict: nonstrict,
        returns_set: no_set,
        implementation: Some(func_coalesce),
    });
    r.register(FunctionEntry {
        name: "nullif",
        min_args: 2,
        max_args: 2,
        strict: nonstrict,
        returns_set: no_set,
        implementation: Some(func_nullif),
    });
    r.register(FunctionEntry {
        name: "concat",
        min_args: 0,
        max_args: usize::MAX,
        strict: nonstrict,
        returns_set: no_set,
        implementation: Some(func_concat),
    });
    r.register(FunctionEntry {
        name: "concat_ws",
        min_args: 2,
        max_args: usize::MAX,
        strict: nonstrict,
        returns_set: no_set,
        implementation: Some(func_concat_ws),
    });
    for name in ["lower", "upper", "length"] {
        r.register(FunctionEntry {
            name,
            min_args: 1,
            max_args: 1,
            strict,
            returns_set: no_set,
            implementation: Some(match name {
                "lower" => func_lower,
                "upper" => func_upper,
                _ => func_length,
            }),
        });
    }
    for name in ["trim", "btrim", "ltrim", "rtrim"] {
        r.register(FunctionEntry {
            name,
            min_args: 1,
            max_args: 2,
            strict,
            returns_set: no_set,
            implementation: Some(match name {
                "ltrim" => |a| func_trim("ltrim", a),
                "rtrim" => |a| func_trim("rtrim", a),
                _ => |a| func_trim("btrim", a),
            }),
        });
    }
    for name in ["substring", "substr"] {
        r.register(FunctionEntry {
            name,
            min_args: 2,
            max_args: 3,
            strict,
            returns_set: no_set,
            implementation: Some(func_substring),
        });
    }
    r.register(FunctionEntry {
        name: "position",
        min_args: 2,
        max_args: 2,
        strict,
        returns_set: no_set,
        implementation: Some(func_position),
    });
    for name in ["power", "pow"] {
        r.register(FunctionEntry {
            name,
            min_args: 2,
            max_args: 2,
            strict,
            returns_set: no_set,
            implementation: Some(func_power),
        });
    }
    r
}

/// Global built-in function registry singleton.
#[must_use]
pub fn builtin_functions() -> &'static FunctionRegistry {
    static REG: OnceLock<FunctionRegistry> = OnceLock::new();
    REG.get_or_init(register_builtins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::PgValue::*;

    #[test]
    fn lookup_and_call_lower() {
        let reg = builtin_functions();
        let res = reg
            .call("LOWER", vec![Text("HELLO".into())])
            .unwrap()
            .unwrap();
        assert_eq!(res, Text("hello".into()));
    }

    #[test]
    fn strict_null_propagates() {
        let reg = builtin_functions();
        let res = reg.call("length", vec![Null]).unwrap().unwrap();
        assert_eq!(res, Null);
    }

    #[test]
    fn concat_skips_nulls() {
        let reg = builtin_functions();
        let res = reg
            .call("concat", vec![Text("a".into()), Null, Text("b".into())])
            .unwrap()
            .unwrap();
        assert_eq!(res, Text("ab".into()));
    }

    #[test]
    fn coalesce_returns_first_nonnull() {
        let reg = builtin_functions();
        let res = reg
            .call("coalesce", vec![Null, Int4(42), Int4(99)])
            .unwrap()
            .unwrap();
        assert_eq!(res, Int4(42));
    }

    #[test]
    fn power_works() {
        let reg = builtin_functions();
        let res = reg.call("pow", vec![Int4(2), Int4(10)]).unwrap().unwrap();
        match res {
            Float8(v) => assert!((v - 1024.0).abs() < 0.001),
            _ => panic!("expected float8"),
        }
    }

    #[test]
    fn trim_variants() {
        let reg = builtin_functions();
        let t = Text("  hi  ".into());
        let l = reg.call("ltrim", vec![t.clone()]).unwrap().unwrap();
        assert_eq!(l, Text("hi  ".into()));
        let r = reg.call("rtrim", vec![t.clone()]).unwrap().unwrap();
        assert_eq!(r, Text("  hi".into()));
        let b = reg.call("btrim", vec![t]).unwrap().unwrap();
        assert_eq!(b, Text("hi".into()));
    }

    #[test]
    fn substring_basic() {
        let reg = builtin_functions();
        let res = reg
            .call("substr", vec![Text("abcdef".into()), Int4(2), Int4(3)])
            .unwrap()
            .unwrap();
        assert_eq!(res, Text("bcd".into()));
    }

    #[test]
    fn position_basic_semantics() {
        let reg = builtin_functions();
        // 1-based position, 0 when absent, 1 for an empty substring.
        assert_eq!(
            reg.call("position", vec![Text("a".into()), Text("banana".into())])
                .unwrap()
                .unwrap(),
            Int4(2)
        );
        assert_eq!(
            reg.call("position", vec![Text("n".into()), Text("banana".into())])
                .unwrap()
                .unwrap(),
            Int4(3)
        );
        assert_eq!(
            reg.call("position", vec![Text("z".into()), Text("banana".into())])
                .unwrap()
                .unwrap(),
            Int4(0)
        );
        assert_eq!(
            reg.call("position", vec![Text("".into()), Text("banana".into())])
                .unwrap()
                .unwrap(),
            Int4(1)
        );
    }

    #[test]
    fn position_null_propagates() {
        let reg = builtin_functions();
        // strict: a NULL argument yields a NULL result (never panics).
        assert_eq!(
            reg.call("position", vec![Null, Text("banana".into())])
                .unwrap()
                .unwrap(),
            Null
        );
        assert_eq!(
            reg.call("position", vec![Text("a".into()), Null])
                .unwrap()
                .unwrap(),
            Null
        );
    }

    #[test]
    fn position_is_character_based_for_multibyte() {
        let reg = builtin_functions();
        // 'é' is two UTF-8 bytes. In 'élève' the 'l' is the 2nd character, so
        // position must be 2 (char-based); a byte-based calculation would give 3.
        let res = reg
            .call("position", vec![Text("l".into()), Text("élève".into())])
            .unwrap()
            .unwrap();
        assert_eq!(res, Int4(2));
    }
}
