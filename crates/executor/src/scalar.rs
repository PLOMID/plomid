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
//! Scalar function registry.
//!
//! [`is_scalar_function`] is the membership test used by the planner, and
//! [`scalar_function_value`] / [`scalar_function_value_extended`] are the single
//! dispatch point for every non-aggregate builtin. Implementations that are
//! large enough to have their own representation live beside it here; JSON
//! document work is delegated to [`crate::json`].

use crate::coerce::{cast_value, format_type_display_name, format_type_typmod_suffix};
use crate::error::SqlError;
use crate::error::SqlResult;
use crate::json::{
    append_json_value, array_cardinality, array_dimension_length, array_or_single_strings,
    json_array_extended, json_build_value, json_delete_path, json_escape, json_lookup,
    json_object_extended, json_operand, json_path_function, json_populate_record_function,
    json_populate_recordset_function, json_pretty, json_query_function, json_scalar_value,
    json_serialize_extended, json_set_path, json_tree, json_type_name, json_value_function,
    jsonb_concat, jsonb_insert_path, jsonb_set_lax_function, parse_jsonb_arg, parse_jsonb_vars,
    strip_json_nulls,
};
use crate::jsonpath::jsonb_to_bool;
use crate::util::unqualify;
use plomid_core::ErrorKind;
use plomid_core::PlomidError;
use plomid_sql::value_pg_type;
use plomid_sql::Expression;
use plomid_sql::Value;
use plomid_types::function::{invalid_arg, require_args, scalar_text};
use plomid_types::JsonbValue;
use plomid_types::PgType;
use plomid_types::TypeOid;
use regex::RegexBuilder;

/// True when `name` is a pure scalar function evaluatable row-by-row without
/// engine or session context.
pub(crate) fn is_scalar_function(name: &str) -> bool {
    matches!(
        unqualify(name).to_ascii_lowercase().as_str(),
        "lower"
            | "upper"
            | "length"
            | "char_length"
            | "character_length"
            | "trim"
            | "btrim"
            | "ltrim"
            | "rtrim"
            | "substring"
            | "substr"
            | "concat"
            | "concat_ws"
            | "regexp_replace"
            | "regexp_match"
            | "regexp_matches"
            | "regexp_substr"
            | "regex_match"
            | "regex_match_i"
            | "regex_not_match"
            | "regex_not_match_i"
            | "json_path"
            | "json_path_text"
            | "json_contains"
            | "json_contained_by"
            | "json_exists"
            | "json_exists_any"
            | "json_exists_all"
            | "jsonb_build_object"
            | "json_build_object"
            | "jsonb_set"
            | "json_strip_nulls"
            | "jsonb_strip_nulls"
            | "json_typeof"
            | "jsonb_typeof"
            | "json_extract_path"
            | "json_extract_path_text"
            | "jsonb_extract_path"
            | "jsonb_extract_path_text"
            | "jsonb_array_elements"
            | "json_array_elements"
            | "jsonb_object_keys"
            | "json_object_keys"
            | "json_build_array"
            | "jsonb_build_array"
            | "json_array_length"
            | "jsonb_array_length"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
            | "jsonb_insert"
            | "jsonb_set_lax"
            | "jsonb_pretty"
            | "json_pretty"
            | "to_json"
            | "to_jsonb"
            | "jsonb_delete_path"
            | "row_to_json"
            | "row"
            | "format"
            | "obj_description"
            | "col_description"
            | "pg_total_relation_size"
            | "quote_ident"
            | "shobj_description"
            | "pg_get_partkeydef"
            | "pg_tablespace_location"
            | "pg_relation_size"
            | "pg_stat_get_numscans"
            | "pg_table_size"
            | "pg_indexes_size"
            | "has_database_privilege"
            | "has_table_privilege"
            | "has_schema_privilege"
            | "pg_table_is_visible"
            | "pg_get_statisticsobjdef_columns"
            | "pg_get_userbyid"
            | "array"
            | "abs"
            | "round"
            | "floor"
            | "ceil"
            | "ceiling"
            | "replace"
            | "left"
            | "right"
            | "reverse"
            | "position"
            | "power"
            | "pow"
            | "sqrt"
            | "mod"
            | "array_length"
            | "array_upper"
            | "cardinality"
            | "array_append"
            | "array_prepend"
            | "array_cat"
            | "array_position"
            | "array_to_string"
            | "string_to_array"
            | "unnest"
            | "pi"
            | "random"
            | "sin"
            | "cos"
            | "tan"
            | "ln"
            | "log"
            | "exp"
            | "pg_typeof"
            | "format_type"
            | "pg_get_expr"
            | "_pg_expandarray"
            | "date_part"
            | "date_trunc"
            | "age"
            | "make_date"
            | "make_timestamp"
            | "greatest"
            | "least"
            | "any_equal"
            | "all_equal"
            | "any_not_equal"
            | "all_not_equal"
            | "similar_to"
            | "similar_not"
            | "jsonb_path_query"
            | "jsonb_path_query_tz"
            | "jsonb_path_query_array"
            | "jsonb_path_query_array_tz"
            | "jsonb_path_query_first"
            | "jsonb_path_query_first_tz"
            | "jsonb_path_exists"
            | "jsonb_path_exists_tz"
            | "jsonb_path_match"
            | "jsonb_path_match_tz"
            | "jsonb_path_exists_op"
            | "jsonb_path_match_op"
            | "json_object"
            | "jsonb_object"
            | "json_array"
            | "jsonb_array"
            | "json_value"
            | "json_query"
            | "json_populate_record"
            | "json_populate_recordset"
            | "int4range"
            | "int4multirange"
            | "inet"
            | "json_scalar"
            | "json"
            | "json_serialize"
    )
}

fn scalar_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int2(v) => Some(f64::from(*v)),
        Value::Int4(v) => Some(f64::from(*v)),
        Value::Int8(v) => Some(*v as f64),
        Value::Float4(v) => Some(f64::from(*v)),
        Value::Float8(v) => Some(*v),
        Value::Numeric(n) => Some(n.clone().to_f64()),
        _ => None,
    }
}

fn build_regex(pattern: &str, flags: &str) -> SqlResult<regex::Regex> {
    let mut builder = RegexBuilder::new(pattern);
    for flag in flags.chars() {
        match flag {
            'i' => {
                builder.case_insensitive(true);
            }
            'm' => {
                builder.multi_line(true);
            }
            's' => {
                builder.dot_matches_new_line(true);
            }
            'n' => {
                builder.swap_greed(true);
            }
            'g' => {}
            other => {
                return Err(invalid_arg(
                    "regular expression",
                    &format!("unknown flag {other}"),
                ))
            }
        }
    }
    builder
        .build()
        .map_err(|error| invalid_arg("regular expression", &error.to_string()))
}

/// Evaluates a pure scalar SQL function over already-evaluated arguments.
///
/// NULL propagation follows PostgreSQL: every scalar function except
/// `concat` returns NULL when any argument is NULL.
pub(crate) fn scalar_function_value(name: &str, args: &[Value]) -> SqlResult<Value> {
    scalar_function_value_extended(name, args, None, None, None)
}

/// Extended scalar dispatch carrying SQL/JSON constructor clauses:
/// `returning` (RETURNING <type>), `null_handling`
/// (`NULL ON NULL` / `ABSENT ON NULL`), `unique_keys`
/// (`WITH UNIQUE KEYS` / `WITHOUT UNIQUE KEYS`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn scalar_function_value_extended(
    name: &str,
    args: &[Value],
    returning: Option<String>,
    null_handling: Option<plomid_sql::NullHandling>,
    unique_keys: Option<bool>,
) -> SqlResult<Value> {
    let lname = unqualify(name).to_ascii_lowercase();
    // Delegate to the authoritative FunctionRegistry singleton for all pure functions it
    // implements.  Functions that require external context (sequence/session state) or
    // the registry does not know are handled by the hardcoded arms below.
    if let Some(value) = plomid_types::builtin_functions()
        .call(&lname, args.to_vec())
        .map_err(SqlError::Storage)?
    {
        return Ok(value);
    }
    match lname.as_str() {
        // SQL/JSON constructor: JSON('{...}') validates and returns the JSON text.
        "json" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let text = scalar_text(&args[0]);
            plomid_types::JsonbValue::parse(&text).map_err(|e| {
                SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    format!("invalid input syntax for type json: {e}"),
                ))
            })?;
            let base = Value::Json(text);
            match returning {
                Some(ref ty) => cast_value(&base, ty),
                None => Ok(base),
            }
        }
        // JSON scalar constructor: JSON_SCALAR(x) -> JSON text of a scalar.
        "json_scalar" => {
            require_args(&lname, args, 1, 1)?;
            Ok(Value::Json(json_scalar_value(&args[0])))
        }
        // SQL/JSON constructor: JSON_OBJECT([KEY k VALUE v] [, ...])
        // Also supports the array form json_object(array[k1,v1,...]).
        "json_object" => json_object_extended(&lname, &args, returning, null_handling, unique_keys),
        "jsonb_object" => {
            json_object_extended(&lname, &args, returning, null_handling, unique_keys)
        }
        // SQL/JSON constructor: JSON_ARRAY(...)
        "json_array" => json_array_extended(&lname, &args, returning, null_handling),
        "jsonb_array" => json_array_extended(&lname, &args, returning, null_handling),
        // SQL/JSON: JSON_VALUE / JSON_QUERY
        "json_value" => json_value_function(&lname, &args),
        "json_query" => json_query_function(&lname, &args),
        // SQL/JSON: JSON_SERIALIZE(json [RETURNING type])
        "json_serialize" => json_serialize_extended(&lname, &args, returning),
        // json_object(array[...]) and jsonb_object(array[...]) are handled above.
        // jsonb_set_lax is a scalar JSON mutation function.
        "jsonb_set_lax" => jsonb_set_lax_function(&lname, &args),
        // int4range() constructor function.
        "int4range" => int4range_function(&lname, &args),
        // int4multirange() constructor function.
        "int4multirange" => int4multirange_function(&lname, &args),
        // inet/cidr input cast helper when called as a function form.
        "inet" => inet_function(&lname, &args),
        // json_populate_record / json_populate_recordset
        "json_populate_record" => json_populate_record_function(&lname, &args),
        "json_populate_recordset" => json_populate_recordset_function(&lname, &args),
        "row" => {
            // ROW(a, b, ...) constructs an anonymous composite/record value with
            // positional field names (f1, f2, ...), matching PostgreSQL's record
            // constructor semantics used by `row_to_json(ROW(...))`.
            let fields = args
                .iter()
                .enumerate()
                .map(|(i, value)| (format!("f{}", i + 1), value.clone()))
                .collect();
            Ok(Value::Composite {
                type_oid: plomid_types::TypeOid::RECORD,
                fields,
            })
        }
        "row_to_json" => {
            require_args(&lname, args, 1, 2)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let Value::Composite { fields, .. } = &args[0] else {
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
            Ok(Value::Json(json))
        }
        "format" => {
            if args.is_empty() {
                return Err(invalid_arg(&lname, "requires a format string"));
            }
            if args[0].is_null() {
                return Ok(Value::Null);
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
                    .ok_or_else(|| invalid_arg(&lname, "incomplete format specifier"))?;
                if spec == '%' {
                    output.push('%');
                    continue;
                }
                let value = values
                    .next()
                    .ok_or_else(|| invalid_arg(&lname, "too few arguments for format"))?;
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
                        _ => return Err(invalid_arg(&lname, "unsupported format specifier")),
                    }
                }
            }
            Ok(Value::Text(output))
        }
        // Object comments are catalog metadata.  The executor supplies the
        // catalog-backed relation when comments exist; NULL is the SQL result
        // for an object without a comment.
        "obj_description" => {
            require_args(&lname, args, 1, 2)?;
            Ok(Value::Null)
        }
        "col_description" => {
            require_args(&lname, args, 2, 2)?;
            Ok(Value::Null)
        }
        "pg_total_relation_size" => {
            require_args(&lname, args, 1, 1)?;
            Ok(Value::Int8(0))
        }
        // Index statistics are not persisted yet.  Returning zero is the
        // accurate value for a newly opened Plomid relation and, unlike an
        // unsupported-function error, allows PostgreSQL catalog clients to
        // inspect index metadata normally.
        "pg_stat_get_numscans" => {
            require_args(&lname, args, 1, 1)?;
            Ok(Value::Int8(0))
        }
        "lower" | "upper" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let text = scalar_text(&args[0]);
            Ok(Value::Text(if lname == "lower" {
                text.to_lowercase()
            } else {
                text.to_uppercase()
            }))
        }
        "length" | "char_length" | "character_length" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            Ok(Value::Int4(scalar_text(&args[0]).chars().count() as i32))
        }
        "trim" | "btrim" | "ltrim" | "rtrim" => {
            require_args(&lname, args, 1, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let mut text = scalar_text(&args[0]);
            let cutset: Vec<char> = if args.len() == 2 {
                scalar_text(&args[1]).chars().collect()
            } else {
                vec![' ']
            };
            match lname.as_str() {
                "ltrim" => text = text.trim_start_matches(|c| cutset.contains(&c)).to_string(),
                "rtrim" => text = text.trim_end_matches(|c| cutset.contains(&c)).to_string(),
                _ => text = text.trim_matches(|c| cutset.contains(&c)).to_string(),
            }
            Ok(Value::Text(text))
        }
        "substring" | "substr" => {
            require_args(&lname, args, 2, 3)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let text: Vec<char> = scalar_text(&args[0]).chars().collect();
            let start =
                scalar_f64(&args[1]).ok_or_else(|| invalid_arg(&lname, "start must be numeric"))?;
            let start = start.trunc().max(1.0) as usize; // SQL is 1-based
            let len: Option<usize> = if args.len() == 3 {
                Some(
                    scalar_f64(&args[2])
                        .ok_or_else(|| invalid_arg(&lname, "length must be numeric"))?
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
            Ok(Value::Text(text[begin..end].iter().collect()))
        }
        "concat" => {
            // PostgreSQL concat() skips NULL arguments entirely.
            let mut out = String::new();
            for arg in args {
                if !arg.is_null() {
                    out.push_str(&scalar_text(arg));
                }
            }
            Ok(Value::Text(out))
        }
        "concat_ws" => {
            require_args(&lname, args, 2, usize::MAX)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let separator = scalar_text(&args[0]);
            let joined = args[1..]
                .iter()
                .filter(|arg| !arg.is_null())
                .map(scalar_text)
                .collect::<Vec<_>>()
                .join(&separator);
            Ok(Value::Text(joined))
        }
        "pi" => {
            require_args(&lname, args, 0, 0)?;
            Ok(Value::Float8(std::f64::consts::PI))
        }
        "random" => {
            require_args(&lname, args, 0, 0)?;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| invalid_arg(&lname, "system clock before epoch"))?
                .subsec_nanos();
            Ok(Value::Float8(f64::from(nanos) / 1_000_000_000.0))
        }
        "sin" | "cos" | "tan" | "ln" | "exp" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let value = scalar_f64(&args[0])
                .ok_or_else(|| invalid_arg(&lname, "argument must be numeric"))?;
            let result = match lname.as_str() {
                "sin" => value.sin(),
                "cos" => value.cos(),
                "tan" => value.tan(),
                "ln" => value.ln(),
                _ => value.exp(),
            };
            Ok(Value::Float8(result))
        }
        "log" => {
            require_args(&lname, args, 1, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let value = scalar_f64(args.last().expect("log argument"))
                .ok_or_else(|| invalid_arg(&lname, "argument must be numeric"))?;
            let result = if args.len() == 2 {
                let base = scalar_f64(&args[0])
                    .ok_or_else(|| invalid_arg(&lname, "base must be numeric"))?;
                value.log(base)
            } else {
                value.log10()
            };
            Ok(Value::Float8(result))
        }
        "pg_typeof" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Text("unknown".into()));
            }
            if let Value::Array { element_oid, .. } = &args[0] {
                let element_name = plomid_types::PgType::by_oid(*element_oid)
                    .map(|ty| ty.name().to_string())
                    .unwrap_or_else(|| "unknown".into());
                return Ok(Value::Text(format!("{element_name}[]")));
            }
            Ok(Value::Text(
                value_pg_type(&args[0])
                    .map_or("unknown", |ty| ty.name())
                    .to_string(),
            ))
        }
        // PostgreSQL's catalog visibility predicate is used by psql's `\d`
        // queries.  OIDs returned by pg_class are already resolved through
        // the active catalog/search path before this scalar function is
        // evaluated; a NULL OID is the only non-visible case here.
        "pg_table_is_visible" => {
            require_args(&lname, args, 1, 1)?;
            Ok(Value::Bool(!args[0].is_null()))
        }
        "pg_get_statisticsobjdef_columns" | "pg_get_userbyid" => {
            require_args(&lname, args, 1, 1)?;
            Ok(Value::Null)
        }
        "format_type" => {
            require_args(&lname, args, 2, 2)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let oid = match args[0] {
                Value::Oid(oid) => oid,
                Value::Int4(oid) if oid >= 0 => oid as u32,
                Value::Int2(oid) if oid >= 0 => oid as u32,
                Value::Int8(oid) if oid >= 0 => oid as i32 as u32,
                _ => return Err(invalid_arg(&lname, "type OID must be an integer")),
            };
            let typmod = match &args[1] {
                Value::Null => plomid_types::typmod::NO_TYPEMOD,
                Value::Int2(v) => i32::from(*v),
                Value::Int4(v) => *v,
                Value::Int8(v) => *v as i32,
                Value::Oid(v) => *v as i32,
                other => scalar_text(other)
                    .parse::<i32>()
                    .map_err(|_| invalid_arg(&lname, "typmod must be an integer"))?,
            };
            // Resolve the element type first so implicit array OIDs (which are
            // not standalone registry entries) still produce `int4[]`-style
            // names; PostgreSQL's format_type never emits a bare array OID.
            let (base, is_array) = {
                let element = plomid_types::PgType::all()
                    .find(|ty| ty.array_oid() == Some(plomid_types::TypeOid(oid)));
                match element {
                    Some(ty) => (Some(ty), true),
                    None => (
                        plomid_types::PgType::by_oid(plomid_types::TypeOid(oid)),
                        false,
                    ),
                }
            };
            let Some(ty) = base else {
                return Ok(Value::Text(oid.to_string()));
            };
            let mut name = format_type_display_name(ty).to_string();
            if is_array {
                name.push_str("[]");
            }
            if let Some(suffix) = format_type_typmod_suffix(ty, typmod) {
                name.push_str(&suffix);
            }
            Ok(Value::Text(name))
        }
        "pg_get_expr" => {
            require_args(&lname, args, 2, 3)?;
            if args.first().is_none_or(Value::is_null) {
                return Ok(Value::Null);
            }
            Ok(Value::Text(args[0].to_sql_text()))
        }
        "date_part" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            // DATE_PART and EXTRACT share PostgreSQL's numeric result type.
            // Keep the scale in the executor; the wire layer applies the
            // canonical text representation expected by PostgreSQL clients.
            crate::query::extract_datetime_field(&args[1], &scalar_text(&args[0]))
        }
        "date_trunc" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let field = scalar_text(&args[0]).to_ascii_lowercase();
            let value = &args[1];
            let (date, time, is_timestamp) = match value {
                Value::Timestamp(micros) | Value::Timestamptz(micros) => {
                    let (date, time) = plomid_types::datetime::timestamp_to_parts(*micros);
                    (date, time, true)
                }
                Value::Date(days) => (
                    plomid_types::datetime::date_to_parts(*days),
                    plomid_types::datetime::TimeParts {
                        hour: 0,
                        minute: 0,
                        second: 0,
                        micros: 0,
                    },
                    false,
                ),
                _ => return Err(invalid_arg(&lname, "value must be date or timestamp")),
            };
            let time = match field.as_str() {
                "year" => plomid_types::datetime::TimeParts {
                    hour: 0,
                    minute: 0,
                    second: 0,
                    micros: 0,
                },
                "month" => plomid_types::datetime::TimeParts {
                    hour: 0,
                    minute: 0,
                    second: 0,
                    micros: 0,
                },
                "day" => plomid_types::datetime::TimeParts {
                    hour: 0,
                    minute: 0,
                    second: 0,
                    micros: 0,
                },
                "hour" => plomid_types::datetime::TimeParts {
                    hour: time.hour,
                    minute: 0,
                    second: 0,
                    micros: 0,
                },
                "minute" => plomid_types::datetime::TimeParts {
                    hour: time.hour,
                    minute: time.minute,
                    second: 0,
                    micros: 0,
                },
                "second" => plomid_types::datetime::TimeParts {
                    hour: time.hour,
                    minute: time.minute,
                    second: time.second,
                    micros: 0,
                },
                _ => return Err(invalid_arg(&lname, "unsupported date_trunc field")),
            };
            let days = plomid_types::datetime::parts_to_date(plomid_types::datetime::DateParts {
                year: date.year,
                month: if field == "year" { 1 } else { date.month },
                day: if matches!(field.as_str(), "year" | "month") {
                    1
                } else {
                    date.day
                },
            })
            .ok_or_else(|| invalid_arg(&lname, "invalid date"))?;
            if is_timestamp {
                let micros = plomid_types::datetime::parts_to_time(time)
                    .ok_or_else(|| invalid_arg(&lname, "invalid time"))?;
                Ok(if matches!(value, Value::Timestamptz(_)) {
                    Value::Timestamptz(plomid_types::datetime::parts_to_timestamp(days, micros))
                } else {
                    Value::Timestamp(plomid_types::datetime::parts_to_timestamp(days, micros))
                })
            } else {
                Ok(Value::Date(days))
            }
        }
        "make_date" => {
            require_args(&lname, args, 3, 3)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let y = scalar_f64(&args[0])
                .ok_or_else(|| invalid_arg(&lname, "year must be numeric"))?
                as i32;
            let m = scalar_f64(&args[1])
                .ok_or_else(|| invalid_arg(&lname, "month must be numeric"))?
                as i32;
            let d = scalar_f64(&args[2])
                .ok_or_else(|| invalid_arg(&lname, "day must be numeric"))?
                as i32;
            let text = format!("{y:04}-{m:02}-{d:02}");
            plomid_types::text::parse_value(&text, plomid_types::PgType::Date)
                .map_err(|error| invalid_arg(&lname, &error))
        }
        "make_timestamp" => {
            require_args(&lname, args, 6, 6)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let n = args
                .iter()
                .map(|v| {
                    scalar_f64(v).ok_or_else(|| invalid_arg(&lname, "arguments must be numeric"))
                })
                .collect::<SqlResult<Vec<_>>>()?;
            let text = format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:09.6}",
                n[0] as i32, n[1] as i32, n[2] as i32, n[3] as i32, n[4] as i32, n[5]
            );
            plomid_types::text::parse_value(&text, plomid_types::PgType::Timestamp)
                .map_err(|error| invalid_arg(&lname, &error))
        }
        "age" => {
            require_args(&lname, args, 1, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            // age(end) uses the statement clock; age(end, start) diffs two.
            let end_micros = match &args[0] {
                Value::Timestamp(m) | Value::Timestamptz(m) => *m,
                Value::Date(days) => i64::from(*days) * plomid_types::datetime::USECS_PER_DAY,
                other => {
                    return Err(invalid_arg(
                        &lname,
                        &format!("cannot compute age of {other:?}"),
                    ))
                }
            };
            let start_micros = if args.len() == 2 {
                match &args[1] {
                    Value::Timestamp(m) | Value::Timestamptz(m) => *m,
                    Value::Date(days) => i64::from(*days) * plomid_types::datetime::USECS_PER_DAY,
                    other => {
                        return Err(invalid_arg(
                            &lname,
                            &format!("cannot compute age against {other:?}"),
                        ))
                    }
                }
            } else {
                crate::context::statement_timestamp_value_micros()
            };
            Ok(Value::Interval(
                plomid_types::datetime::age_between_timestamps(end_micros, start_micros),
            ))
        }
        "replace" => {
            require_args(&lname, args, 3, 3)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            Ok(Value::Text(
                scalar_text(&args[0]).replace(&scalar_text(&args[1]), &scalar_text(&args[2])),
            ))
        }
        "regexp_replace" => {
            require_args(&lname, args, 3, 4)?;
            if args[..3].iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let flags = args
                .get(3)
                .filter(|v| !v.is_null())
                .map(scalar_text)
                .unwrap_or_default();
            let regex = build_regex(&scalar_text(&args[1]), &flags)?;
            let input = scalar_text(&args[0]);
            let replacement = scalar_text(&args[2]);
            let output = if flags.contains('g') {
                regex.replace_all(&input, replacement.as_str()).into_owned()
            } else {
                regex.replace(&input, replacement.as_str()).into_owned()
            };
            Ok(Value::Text(output))
        }
        "regexp_match" | "regexp_matches" => {
            require_args(&lname, args, 2, 3)?;
            if args[..2].iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let flags = args
                .get(2)
                .filter(|v| !v.is_null())
                .map(scalar_text)
                .unwrap_or_default();
            let regex = build_regex(&scalar_text(&args[1]), &flags)?;
            let input = scalar_text(&args[0]);
            let Some(captures) = regex.captures(&input) else {
                return Ok(Value::Null);
            };
            let elements = if captures.len() > 1 {
                captures
                    .iter()
                    .skip(1)
                    .map(|capture| {
                        capture.map_or(Value::Null, |m| Value::Text(m.as_str().to_string()))
                    })
                    .collect()
            } else {
                vec![Value::Text(
                    captures
                        .get(0)
                        .expect("full regex match")
                        .as_str()
                        .to_string(),
                )]
            };
            Ok(Value::Array {
                element_oid: TypeOid::TEXT,
                elements,
            })
        }
        "regex_match" | "regex_match_i" | "regex_not_match" | "regex_not_match_i" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let flags = if lname.ends_with("_i") { "i" } else { "" };
            let regex = build_regex(&scalar_text(&args[1]), flags)?;
            let matched = regex.is_match(&scalar_text(&args[0]));
            Ok(Value::Bool(if lname.starts_with("regex_not") {
                !matched
            } else {
                matched
            }))
        }
        "regexp_substr" => {
            require_args(&lname, args, 2, 5)?;
            if args[..2].iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let start = args.get(2).and_then(scalar_f64).unwrap_or(1.0).max(1.0) as usize;
            let occurrence = args.get(3).and_then(scalar_f64).unwrap_or(1.0).max(1.0) as usize;
            let flags = args
                .get(4)
                .filter(|v| !v.is_null())
                .map(scalar_text)
                .unwrap_or_default();
            let regex = build_regex(&scalar_text(&args[1]), &flags)?;
            let input = scalar_text(&args[0]);
            let slice = input.chars().skip(start - 1).collect::<String>();
            let result = regex
                .find_iter(&slice)
                .nth(occurrence - 1)
                .map(|m| Value::Text(m.as_str().to_string()))
                .unwrap_or(Value::Null);
            Ok(result)
        }
        "left" | "right" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let chars: Vec<char> = scalar_text(&args[0]).chars().collect();
            let n = scalar_f64(&args[1])
                .ok_or_else(|| invalid_arg(&lname, "count must be numeric"))?
                .trunc() as usize;
            let n = n.min(chars.len());
            let slice = if lname == "left" {
                &chars[..n]
            } else {
                &chars[chars.len() - n..]
            };
            Ok(Value::Text(slice.iter().collect()))
        }
        "reverse" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            Ok(Value::Text(scalar_text(&args[0]).chars().rev().collect()))
        }
        "array" => {
            // `ARRAY[...]` literal: build a one-dimensional typed array value.
            // The element type is inferred from the first non-NULL element.
            if let Some(Value::Array { element_oid, .. }) =
                args.iter().find(|v| matches!(v, Value::Array { .. }))
            {
                if args
                    .iter()
                    .any(|v| !matches!(v, Value::Array { .. } | Value::Null))
                {
                    return Err(invalid_arg(
                        &lname,
                        "multidimensional arrays require array elements",
                    ));
                }
                return Ok(Value::Array {
                    element_oid: *element_oid,
                    elements: args.to_vec(),
                });
            }
            if args.iter().all(Value::is_null) {
                let element_oid = TypeOid::TEXT;
                return Ok(Value::Array {
                    element_oid,
                    elements: vec![Value::Null; args.len()],
                });
            }
            let sample = args.iter().find(|v| !v.is_null()).unwrap();
            // Resolve the element type. For builtin types, `value_pg_type` gives
            // us the canonical `PgType`. For user-defined types (enums,
            // domains, composites) the OID is carried directly on the value and
            // is not in the builtin registry, so we extract it from the value
            // itself. This lets `ARRAY['pending'::status_enum, ...]` work.
            let (ty, element_oid) = match value_pg_type(sample) {
                Some(ty) => (Some(ty), ty.oid()),
                None => {
                    let oid = match sample {
                        Value::Composite { type_oid, .. } => *type_oid,
                        Value::Enum { type_oid, .. } => *type_oid,
                        _ => {
                            return Err(invalid_arg(&lname, "array elements must be typed values"));
                        }
                    };
                    (None, oid)
                }
            };
            let mut elements = args.to_vec();
            for element in &mut elements {
                if !element.is_null() {
                    // For builtin types, cast via the type name (the existing
                    // behaviour). For user-defined types, the values are already
                    // the right type — casting through `cast_value` with a name
                    // won't resolve, so we leave them as-is. They already carry
                    // the correct OID from the earlier type-cast in the parser.
                    if let Some(ref t) = ty {
                        *element = cast_value(element, t.name())?;
                    }
                }
            }
            Ok(Value::Array {
                element_oid,
                elements,
            })
        }
        "abs" => {
            require_args(&lname, args, 1, 1)?;
            match &args[0] {
                Value::Null => Ok(Value::Null),
                Value::Int2(v) => Ok(Value::Int2(v.abs())),
                Value::Int4(v) => Ok(Value::Int4(v.abs())),
                Value::Int8(v) => Ok(Value::Int8(v.abs())),
                Value::Float4(v) => Ok(Value::Float4(v.abs())),
                Value::Float8(v) => Ok(Value::Float8(v.abs())),
                Value::Numeric(n) => Ok(Value::Numeric(n.clone().abs())),
                other => Err(invalid_arg(&lname, &format!("unsupported type {other:?}"))),
            }
        }
        "round" => {
            require_args(&lname, args, 1, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let digits = if args.len() == 2 {
                scalar_f64(&args[1])
                    .ok_or_else(|| invalid_arg(&lname, "digits must be numeric"))?
                    .trunc()
            } else {
                0.0
            };
            let factor = 10f64.powi(digits as i32);
            let round_half_away = |f: f64| -> f64 {
                if f >= 0.0 {
                    (f * factor + 0.5).floor() / factor
                } else {
                    (f * factor - 0.5).ceil() / factor
                }
            };
            match &args[0] {
                Value::Int2(_) | Value::Int4(_) | Value::Int8(_) if args.len() == 1 => {
                    Ok(args[0].clone())
                }
                Value::Int2(v) => Ok(Value::Int2(round_half_away(f64::from(*v)) as i16)),
                Value::Int4(v) => Ok(Value::Int4(round_half_away(f64::from(*v)) as i32)),
                Value::Int8(v) => Ok(Value::Int8(round_half_away(*v as f64) as i64)),
                Value::Float4(v) => Ok(Value::Float4(round_half_away(f64::from(*v)) as f32)),
                Value::Float8(v) => Ok(Value::Float8(round_half_away(*v))),
                // Keep NUMERIC type for NUMERIC input
                Value::Numeric(n) => {
                    let scaled = n.clone().round_to_scale(digits as u16);
                    Ok(Value::Numeric(scaled))
                }
                other => Err(invalid_arg(&lname, &format!("unsupported type {other:?}"))),
            }
        }
        "floor" | "ceil" | "ceiling" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let Some(f) = scalar_f64(&args[0]) else {
                return Err(invalid_arg(&lname, "argument must be numeric"));
            };
            let applied = if lname == "floor" {
                f.floor()
            } else {
                f.ceil()
            };
            match &args[0] {
                Value::Int2(v) if applied == f64::from(*v) => Ok(Value::Int2(*v)),
                Value::Int4(v) if applied == f64::from(*v) => Ok(Value::Int4(*v)),
                Value::Int8(v) if applied == *v as f64 => Ok(Value::Int8(*v)),
                Value::Float4(_) => Ok(Value::Float4(applied as f32)),
                _ => Ok(Value::Float8(applied)),
            }
        }
        "power" | "pow" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let a =
                scalar_f64(&args[0]).ok_or_else(|| invalid_arg(&lname, "base must be numeric"))?;
            let b = scalar_f64(&args[1])
                .ok_or_else(|| invalid_arg(&lname, "exponent must be numeric"))?;
            Ok(Value::Float8(a.powf(b)))
        }
        "sqrt" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let a = scalar_f64(&args[0])
                .ok_or_else(|| invalid_arg(&lname, "argument must be numeric"))?;
            if a < 0.0 {
                return Err(invalid_arg(
                    &lname,
                    "cannot take square root of a negative number",
                ));
            }
            Ok(Value::Float8(a.sqrt()))
        }
        "mod" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let a = scalar_f64(&args[0])
                .ok_or_else(|| invalid_arg(&lname, "arguments must be numeric"))?;
            let b = scalar_f64(&args[1])
                .ok_or_else(|| invalid_arg(&lname, "arguments must be numeric"))?;
            if b == 0.0 {
                return Err(invalid_arg(&lname, "division by zero"));
            }
            Ok(Value::Float8(a % b))
        }
        "array_length" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let Value::Array { elements, .. } = &args[0] else {
                return Err(invalid_arg(&lname, "first argument must be an array"));
            };
            let dim = scalar_f64(&args[1])
                .ok_or_else(|| invalid_arg(&lname, "dimension must be numeric"))?;
            array_dimension_length(elements, dim as usize)
                .map(|length| Value::Int4(length as i32))
                .ok_or_else(|| invalid_arg(&lname, "array dimension must be positive"))
        }
        "array_upper" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let dim = scalar_f64(&args[1])
                .ok_or_else(|| invalid_arg(&lname, "dimension must be numeric"))?;
            if dim <= 0.0 || dim.fract() != 0.0 {
                return Err(invalid_arg(&lname, "array dimension must be positive"));
            }
            let dimension = dim as usize;
            let length = match &args[0] {
                Value::Array { elements, .. } => array_dimension_length(elements, dimension),
                // pg_index.indkey is PostgreSQL's int2vector.  Plomid's
                // single-column catalog projection may also be a scalar
                // int2, but both represent a one-dimensional vector.
                Value::Int2Vector(values) => (dimension == 1).then_some(values.len()),
                Value::OidVector(values) => (dimension == 1).then_some(values.len()),
                Value::Int2(_) | Value::Int4(_) | Value::Int8(_) => (dimension == 1).then_some(1),
                Value::Text(text) | Value::Name(text) if dimension == 1 => {
                    let trimmed = text.trim();
                    if trimmed.starts_with('{') && trimmed.ends_with('}') {
                        Some(if trimmed.len() <= 2 {
                            0
                        } else {
                            trimmed[1..trimmed.len() - 1].split(',').count()
                        })
                    } else {
                        return Err(invalid_arg(&lname, "first argument must be an array"));
                    }
                }
                _ => return Err(invalid_arg(&lname, "first argument must be an array")),
            };
            length
                .map(|length| Value::Int4(length as i32))
                .ok_or_else(|| invalid_arg(&lname, "array dimension does not exist"))
        }
        "cardinality" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let Value::Array { elements, .. } = &args[0] else {
                return Err(invalid_arg(&lname, "argument must be an array"));
            };
            Ok(Value::Int4(array_cardinality(elements) as i32))
        }
        "array_append" | "array_prepend" => {
            require_args(&lname, args, 2, 2)?;
            if lname == "array_append" {
                if args[0].is_null() {
                    return Ok(Value::Null);
                }
                let Value::Array {
                    element_oid,
                    elements,
                } = &args[0]
                else {
                    return Err(invalid_arg(&lname, "first argument must be an array"));
                };
                let mut output = elements.clone();
                output.push(args[1].clone());
                Ok(Value::Array {
                    element_oid: *element_oid,
                    elements: output,
                })
            } else {
                if args[1].is_null() {
                    return Ok(Value::Null);
                }
                let Value::Array {
                    element_oid,
                    elements,
                } = &args[1]
                else {
                    return Err(invalid_arg(&lname, "second argument must be an array"));
                };
                let mut output = elements.clone();
                output.insert(0, args[0].clone());
                Ok(Value::Array {
                    element_oid: *element_oid,
                    elements: output,
                })
            }
        }
        "array_cat" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let (
                Value::Array {
                    element_oid,
                    elements: left,
                },
                Value::Array {
                    elements: right, ..
                },
            ) = (&args[0], &args[1])
            else {
                return Err(invalid_arg(&lname, "arguments must be arrays"));
            };
            let mut elements = left.clone();
            elements.extend(right.clone());
            Ok(Value::Array {
                element_oid: *element_oid,
                elements,
            })
        }
        "array_position" => {
            require_args(&lname, args, 2, 2)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let Value::Array { elements, .. } = &args[0] else {
                return Err(invalid_arg(&lname, "first argument must be an array"));
            };
            Ok(elements
                .iter()
                .position(|value| crate::row::values_equal(value, &args[1]))
                .map_or(Value::Null, |index| Value::Int4(index as i32 + 1)))
        }
        "array_to_string" => {
            require_args(&lname, args, 2, 3)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let Value::Array { elements, .. } = &args[0] else {
                return Err(invalid_arg(&lname, "first argument must be an array"));
            };
            let delimiter = scalar_text(&args[1]);
            let null_string = args.get(2).filter(|v| !v.is_null()).map(scalar_text);
            let values = elements
                .iter()
                .filter_map(|value| {
                    if value.is_null() {
                        null_string.clone()
                    } else {
                        Some(value.to_sql_text())
                    }
                })
                .collect::<Vec<_>>();
            Ok(Value::Text(values.join(&delimiter)))
        }
        "string_to_array" => {
            require_args(&lname, args, 2, 3)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let input = scalar_text(&args[0]);
            let delimiter = scalar_text(&args[1]);
            let null_string = args.get(2).filter(|v| !v.is_null()).map(scalar_text);
            let elements = if delimiter.is_empty() {
                input.chars().map(|c| Value::Text(c.to_string())).collect()
            } else {
                input
                    .split(&delimiter)
                    .map(|part| {
                        if null_string.as_deref() == Some(part) {
                            Value::Null
                        } else {
                            Value::Text(part.to_string())
                        }
                    })
                    .collect()
            };
            Ok(Value::Array {
                element_oid: TypeOid::TEXT,
                elements,
            })
        }
        "unnest" => {
            require_args(&lname, args, 1, 1)?;
            Ok(args[0].clone())
        }
        // `_pg_expandarray(anyarray)` expands an index-key vector into a record
        // with `x` (the first element) and `n` (its 1-based position). PostgreSQL
        // returns a set-of-records; for the metadata queries DBeaver issues this
        // suffices for single-column indexes/keys, which is the common case.
        "_pg_expandarray" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let (element, count): (Value, usize) = match &args[0] {
                Value::Int2Vector(v) => {
                    (v.first().map_or(Value::Null, |e| Value::Int2(*e)), v.len())
                }
                Value::OidVector(v) => (v.first().map_or(Value::Null, |e| Value::Oid(*e)), v.len()),
                Value::Array { elements, .. } => (
                    elements.first().cloned().unwrap_or(Value::Null),
                    elements.len(),
                ),
                // Plomid models single-column index keys as a bare scalar
                // (pg_index.indkey is INT2), equivalent to a 1-element vector.
                Value::Int2(e) => (Value::Int2(*e), 1),
                Value::Int4(e) => (Value::Int4(*e), 1),
                Value::Int8(e) => (Value::Int8(*e), 1),
                Value::Oid(e) => (Value::Oid(*e), 1),
                other => {
                    return Err(invalid_arg(
                        "_pg_expandarray",
                        &format!("_pg_expandarray expects an array argument, got {other:?}"),
                    ))
                }
            };
            if count == 0 {
                return Ok(Value::Null);
            }
            Ok(Value::Composite {
                type_oid: TypeOid::RECORD,
                fields: vec![("x".into(), element), ("n".into(), Value::Int2(1))],
            })
        }
        "jsonb_array_elements" | "json_array_elements" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let value = json_tree(&args[0])?;
            let JsonbValue::Array(elements) = value else {
                return Err(invalid_arg(&lname, "argument must be a JSON array"));
            };
            let elements = elements
                .into_iter()
                .map(|element| {
                    if lname == "json_array_elements" {
                        Value::Json(element.to_text())
                    } else {
                        Value::Jsonb(element.encode())
                    }
                })
                .collect();
            Ok(Value::Array {
                element_oid: if lname == "json_array_elements" {
                    TypeOid::JSON
                } else {
                    TypeOid::JSONB
                },
                elements,
            })
        }
        "jsonb_object_keys" | "json_object_keys" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let value = json_tree(&args[0])?;
            let JsonbValue::Object(pairs) = value else {
                return Err(invalid_arg(&lname, "argument must be a JSON object"));
            };
            Ok(Value::Array {
                element_oid: TypeOid::TEXT,
                elements: pairs.into_iter().map(|(key, _)| Value::Text(key)).collect(),
            })
        }
        "json_path"
        | "json_path_text"
        | "json_extract_path"
        | "json_extract_path_text"
        | "jsonb_extract_path"
        | "jsonb_extract_path_text" => {
            let text = lname.ends_with("_text");
            let value = json_path_function(&args[0], &args[1..], text)?;
            Ok(value)
        }
        "json_contains" | "json_contained_by" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let left = json_operand(&args[0], "json_contains")?;
            let right = json_operand(&args[1], "json_contains")?;
            let contains = left.contains(&right);
            Ok(Value::Bool(if lname == "json_contains" {
                contains
            } else {
                right.contains(&left)
            }))
        }
        "json_exists" | "json_exists_any" | "json_exists_all" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let root = json_tree(&args[0])?;
            let keys = array_or_single_strings(&args[1])?;
            let exists = keys
                .iter()
                .map(|key| json_lookup(&root, key).is_some())
                .collect::<Vec<_>>();
            Ok(Value::Bool(match lname.as_str() {
                "json_exists" => exists[0],
                "json_exists_any" => exists.into_iter().any(|v| v),
                _ => exists.into_iter().all(|v| v),
            }))
        }
        "jsonb_build_object" | "json_build_object" => {
            if args.len() % 2 != 0 {
                return Err(invalid_arg(&lname, "requires an even number of arguments"));
            }
            let mut pairs = Vec::new();
            for chunk in args.chunks(2) {
                if chunk[0].is_null() {
                    return Err(invalid_arg(&lname, "object keys cannot be NULL"));
                }
                pairs.push((scalar_text(&chunk[0]), json_build_value(&chunk[1])?));
            }
            let value = JsonbValue::Object(pairs);
            Ok(if lname == "json_build_object" {
                Value::Json(value.to_text())
            } else {
                Value::Jsonb(value.encode())
            })
        }
        "jsonb_set" => {
            require_args(&lname, args, 3, 4)?;
            if args[..3].iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let mut root = json_tree(&args[0])?;
            let path = array_or_single_strings(&args[1])?;
            let create_missing = args
                .get(3)
                .map(|value| match value {
                    Value::Bool(flag) => Ok(*flag),
                    Value::Null => Ok(false),
                    _ => Err(invalid_arg(&lname, "create_missing must be boolean")),
                })
                .transpose()?
                .unwrap_or(true);
            if !json_set_path(&mut root, &path, json_tree(&args[2])?, create_missing) {
                return Ok(Value::Jsonb(root.encode()));
            }
            Ok(Value::Jsonb(root.encode()))
        }
        "json_strip_nulls" | "jsonb_strip_nulls" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let value = strip_json_nulls(json_tree(&args[0])?);
            Ok(if lname == "json_strip_nulls" {
                Value::Json(value.to_text())
            } else {
                Value::Jsonb(value.encode())
            })
        }
        "json_typeof" | "jsonb_typeof" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            Ok(Value::Text(
                json_type_name(&json_tree(&args[0])?).to_string(),
            ))
        }
        "to_json" | "to_jsonb" => {
            require_args(&lname, args, 1, 1)?;
            let value = json_build_value(&args[0])?;
            Ok(if lname == "to_json" {
                Value::Json(value.to_text())
            } else {
                Value::Jsonb(value.encode())
            })
        }
        "json_build_array" | "jsonb_build_array" => {
            let elements = args
                .iter()
                .map(json_build_value)
                .collect::<SqlResult<Vec<JsonbValue>>>()?;
            let value = JsonbValue::Array(elements);
            Ok(if lname == "json_build_array" {
                Value::Json(value.to_text())
            } else {
                Value::Jsonb(value.encode())
            })
        }
        "json_array_length" | "jsonb_array_length" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let value = json_tree(&args[0])?;
            let JsonbValue::Array(elements) = value else {
                return Err(invalid_arg(&lname, "argument must be a JSON array"));
            };
            Ok(Value::Int4(elements.len() as i32))
        }
        "json_array_elements_text" | "jsonb_array_elements_text" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let value = json_tree(&args[0])?;
            let JsonbValue::Array(elements) = value else {
                return Err(invalid_arg(&lname, "argument must be a JSON array"));
            };
            let elements = elements
                .into_iter()
                .map(|element| match element {
                    JsonbValue::Null => Value::Null,
                    JsonbValue::String(s) => Value::Text(s),
                    other => Value::Text(other.to_text()),
                })
                .collect();
            Ok(Value::Array {
                element_oid: TypeOid::TEXT,
                elements,
            })
        }
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let as_text = lname.ends_with("_text");
            let value = json_tree(&args[0])?;
            let JsonbValue::Object(pairs) = value else {
                return Err(invalid_arg(&lname, "argument must be a JSON object"));
            };
            let is_jsonb = lname == "jsonb_each";
            let elements = pairs
                .into_iter()
                .map(|(key, value)| {
                    let value_value = match value {
                        JsonbValue::Null if as_text => Value::Null,
                        JsonbValue::String(s) if as_text => Value::Text(s),
                        v if as_text => Value::Text(v.to_text()),
                        v if is_jsonb => Value::Jsonb(v.encode()),
                        v => Value::Json(v.to_text()),
                    };
                    Value::Composite {
                        type_oid: TypeOid::RECORD,
                        fields: vec![
                            ("key".into(), Value::Text(key)),
                            ("value".into(), value_value),
                        ],
                    }
                })
                .collect();
            Ok(Value::Array {
                element_oid: TypeOid::RECORD,
                elements,
            })
        }
        "jsonb_delete_path" => {
            require_args(&lname, args, 2, 2)?;
            if args[..2].iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let mut root = json_tree(&args[0])?;
            let path = array_or_single_strings(&args[1])?;
            json_delete_path(&mut root, &path, 0)?;
            Ok(Value::Jsonb(root.encode()))
        }
        "jsonb_insert" => {
            require_args(&lname, args, 3, 4)?;
            if args[..3].iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let mut root = json_tree(&args[0])?;
            let path = array_or_single_strings(&args[1])?;
            let new_value = json_tree(&args[2])?;
            let insert_after = args
                .get(3)
                .map(|value| match value {
                    Value::Bool(flag) => Ok(*flag),
                    Value::Null => Ok(false),
                    _ => Err(invalid_arg(&lname, "insert_after must be boolean")),
                })
                .transpose()?
                .unwrap_or(false);
            jsonb_insert_path(&mut root, &path, new_value, insert_after)?;
            Ok(Value::Jsonb(root.encode()))
        }
        "jsonb_pretty" | "json_pretty" => {
            require_args(&lname, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let value = json_tree(&args[0])?;
            Ok(Value::Text(json_pretty(&value, 0)))
        }
        "greatest" | "least" => {
            if args.is_empty() {
                return Err(invalid_arg(&lname, "requires at least one argument"));
            }
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let mut result = args[0].clone();
            for value in &args[1..] {
                let ord = result.compare(value);
                if (lname == "greatest" && ord == std::cmp::Ordering::Less)
                    || (lname == "least" && ord == std::cmp::Ordering::Greater)
                {
                    result = value.clone();
                }
            }
            Ok(result)
        }
        "any_equal" | "all_equal" | "any_not_equal" | "all_not_equal" => {
            require_args(&lname, args, 2, 2)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let scalar_elements: Vec<Value>;
            let elements: &[Value] = match &args[1] {
                Value::Array { elements, .. } => elements,
                // PostgreSQL catalog vectors such as pg_index.indkey are
                // array-like values.  Some Plomid catalog projections use a
                // scalar for a one-column vector, so normalize both forms.
                Value::Int2Vector(values) => {
                    scalar_elements = values.iter().copied().map(Value::Int2).collect();
                    &scalar_elements
                }
                Value::Int2(_) | Value::Int4(_) | Value::Int8(_) | Value::Oid(_) => {
                    scalar_elements = vec![args[1].clone()];
                    &scalar_elements
                }
                _ => return Err(invalid_arg(&lname, "right operand must be an array")),
            };
            let is_all = lname.starts_with("all_");
            let is_equal = matches!(lname.as_str(), "any_equal" | "all_equal");
            let mut saw_null = false;
            let mut matched = false;
            for element in elements {
                if element.is_null() {
                    saw_null = true;
                    continue;
                }
                let equal = crate::row::values_equal(&args[0], element);
                if (is_equal && equal) || (!is_equal && !equal) {
                    matched = true;
                    break;
                }
                if is_all && !equal && is_equal {
                    // Continue checking ALL equality; a single unequal value
                    // makes the predicate false.
                    continue;
                }
            }
            if is_all {
                if (is_equal
                    && elements
                        .iter()
                        .any(|v| !v.is_null() && !crate::row::values_equal(&args[0], v)))
                    || (!is_equal
                        && elements
                            .iter()
                            .any(|v| !v.is_null() && crate::row::values_equal(&args[0], v)))
                {
                    return Ok(Value::Bool(false));
                }
                if saw_null {
                    return Ok(Value::Null);
                }
                return Ok(Value::Bool(true));
            }
            if matched {
                Ok(Value::Bool(true))
            } else if saw_null {
                Ok(Value::Null)
            } else {
                Ok(Value::Bool(false))
            }
        }
        "similar_to" | "similar_not" => {
            require_args(&lname, args, 2, 2)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let mut pattern = String::new();
            for ch in scalar_text(&args[1]).chars() {
                match ch {
                    '%' => pattern.push_str(".*"),
                    '_' => pattern.push('.'),
                    other => pattern.push_str(&regex::escape(&other.to_string())),
                }
            }
            let regex = build_regex(&format!("^(?:{pattern})$"), "")?;
            let matched = regex.is_match(&scalar_text(&args[0]));
            Ok(Value::Bool(if lname == "similar_to" {
                matched
            } else {
                !matched
            }))
        }
        "jsonb_path_query" | "jsonb_path_query_tz" => {
            require_args(&lname, args, 2, 3)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let doc = parse_jsonb_arg(&args[0])?;
            let path_str = scalar_text(&args[1]);
            let vars = parse_jsonb_vars(&args.get(2))?;
            let path = crate::jsonpath::parse(&path_str).map_err(|e| invalid_arg(&lname, &e))?;
            let ctx = crate::jsonpath::JsonpathContext {
                root: &doc,
                outer: None,
                vars: &vars,
                mode: crate::jsonpath::JsonpathMode::Lax,
            };
            let results = crate::jsonpath::evaluate(&path, &doc, &ctx)
                .map_err(|e| invalid_arg(&lname, &e.to_string()))?;
            let elements: Vec<Value> = results.iter().map(|r| Value::Jsonb(r.encode())).collect();
            return Ok(Value::Array {
                element_oid: TypeOid::JSONB,
                elements,
            });
        }
        "jsonb_path_query_array" | "jsonb_path_query_array_tz" => {
            require_args(&lname, args, 2, 3)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let doc = parse_jsonb_arg(&args[0])?;
            let path_str = scalar_text(&args[1]);
            let vars = parse_jsonb_vars(&args.get(2))?;
            let path = crate::jsonpath::parse(&path_str).map_err(|e| invalid_arg(&lname, &e))?;
            let ctx = crate::jsonpath::JsonpathContext {
                root: &doc,
                outer: None,
                vars: &vars,
                mode: crate::jsonpath::JsonpathMode::Lax,
            };
            let results = crate::jsonpath::evaluate(&path, &doc, &ctx)
                .map_err(|e| invalid_arg(&lname, &e.to_string()))?;
            Ok(Value::Jsonb(JsonbValue::Array(results).encode()))
        }
        "jsonb_path_query_first" | "jsonb_path_query_first_tz" => {
            require_args(&lname, args, 2, 3)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let doc = parse_jsonb_arg(&args[0])?;
            let path_str = scalar_text(&args[1]);
            let vars = parse_jsonb_vars(&args.get(2))?;
            let path = crate::jsonpath::parse(&path_str).map_err(|e| invalid_arg(&lname, &e))?;
            let ctx = crate::jsonpath::JsonpathContext {
                root: &doc,
                outer: None,
                vars: &vars,
                mode: crate::jsonpath::JsonpathMode::Lax,
            };
            let results = crate::jsonpath::evaluate(&path, &doc, &ctx)
                .map_err(|e| invalid_arg(&lname, &e.to_string()))?;
            match results.into_iter().next() {
                Some(v) => Ok(Value::Jsonb(v.encode())),
                None => Ok(Value::Null),
            }
        }
        "jsonb_path_exists" | "jsonb_path_exists_tz" => {
            require_args(&lname, args, 2, 3)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let doc = parse_jsonb_arg(&args[0])?;
            let path_str = scalar_text(&args[1]);
            let vars = parse_jsonb_vars(&args.get(2))?;
            let path = crate::jsonpath::parse(&path_str).map_err(|e| invalid_arg(&lname, &e))?;
            let ctx = crate::jsonpath::JsonpathContext {
                root: &doc,
                outer: None,
                vars: &vars,
                mode: crate::jsonpath::JsonpathMode::Lax,
            };
            let results = crate::jsonpath::evaluate(&path, &doc, &ctx);
            Ok(Value::Bool(matches!(results, Ok(r) if !r.is_empty())))
        }
        "jsonb_path_match" | "jsonb_path_match_tz" => {
            require_args(&lname, args, 2, 3)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let doc = parse_jsonb_arg(&args[0])?;
            let path_str = scalar_text(&args[1]);
            let vars = parse_jsonb_vars(&args.get(2))?;
            let path = crate::jsonpath::parse(&path_str).map_err(|e| invalid_arg(&lname, &e))?;
            let ctx = crate::jsonpath::JsonpathContext {
                root: &doc,
                outer: None,
                vars: &vars,
                mode: crate::jsonpath::JsonpathMode::Lax,
            };
            let results = crate::jsonpath::evaluate(&path, &doc, &ctx)
                .map_err(|e| invalid_arg(&lname, &e.to_string()))?;
            Ok(Value::Bool(!results.is_empty()))
        }
        "jsonb_path_exists_op" => {
            require_args(&lname, args, 2, 2)?;
            let doc = parse_jsonb_arg(&args[0])?;
            let path_str = scalar_text(&args[1]);
            let vars = Vec::new();
            let path = crate::jsonpath::parse(&path_str).map_err(|e| invalid_arg(&lname, &e))?;
            let ctx = crate::jsonpath::JsonpathContext {
                root: &doc,
                outer: None,
                vars: &vars,
                mode: crate::jsonpath::JsonpathMode::Lax,
            };
            Ok(Value::Bool(
                matches!(crate::jsonpath::evaluate(&path, &doc, &ctx), Ok(r) if !r.is_empty()),
            ))
        }
        "jsonb_path_match_op" => {
            // PostgreSQL `@@` operator evaluates a JSONPath expression as a
            // predicate against a JSONB value. Unlike `@?` (which checks for
            // existence of results), `@@` evaluates the path expression and
            // returns its boolean truth value.
            //
            // For comparison expressions like `$.a == 1`, the JSONPath parser
            // produces a `Compare` expression that evaluates to a boolean. We
            // use `eval_single` to evaluate the expression and convert the
            // result to a boolean, which correctly handles predicates.
            require_args(&lname, args, 2, 2)?;
            let doc = parse_jsonb_arg(&args[0])?;
            let path_str = scalar_text(&args[1]);
            let vars = Vec::new();
            let path = crate::jsonpath::parse(&path_str).map_err(|e| invalid_arg(&lname, &e))?;
            let ctx = crate::jsonpath::JsonpathContext {
                root: &doc,
                outer: None,
                vars: &vars,
                mode: crate::jsonpath::JsonpathMode::Lax,
            };
            // Evaluate the path expression. For predicates (comparisons,
            // logical operators), this returns a boolean value. For path
            // expressions, this returns the matched values.
            let results = crate::jsonpath::evaluate(&path, &doc, &ctx)
                .map_err(|e| invalid_arg(&lname, &e.to_string()))?;
            // Convert results to boolean: if the first result is a boolean,
            // use it directly; otherwise, non-empty results mean true.
            let bool_result = results.first().map(jsonb_to_bool).unwrap_or(false);
            Ok(Value::Bool(bool_result))
        }
        _ => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            format!("function \"{name}\" is not supported"),
        ))),
    }
}

/// Applies `SELECT DISTINCT` / `SELECT DISTINCT ON` post-processing:
/// deduplicate rows (preserving order), then apply LIMIT/OFFSET (which
/// PostgreSQL defers until after the duplicate removal).
///
/// `DISTINCT ON` deduplication itself happens inside the SELECT engines
/// (which evaluate the ON expressions on pre-projection source rows after
/// ORDER BY sorting); by the time the result reaches this helper the rows
/// are already de-duplicated, so only LIMIT/OFFSET remain to be applied.
/// Evaluates PostgreSQL's bitwise integer operators (`&`, `|`, `#`, `<<`, `>>`).
/// Both operands must be integers; NULL operands are handled by the caller.
pub(crate) fn integer_bitwise(expr: &Expression, left: &Value, right: &Value) -> SqlResult<Value> {
    fn as_int(value: &Value) -> Option<i64> {
        match value {
            Value::Int2(v) => Some(i64::from(*v)),
            Value::Int4(v) => Some(i64::from(*v)),
            Value::Int8(v) => Some(*v),
            _ => None,
        }
    }
    fn result(value: i64, left: &Value, right: &Value) -> Value {
        match (left, right) {
            (Value::Int2(_), _) if value >= i16::MIN as i64 && value <= i16::MAX as i64 => {
                Value::Int2(value as i16)
            }
            (Value::Int4(_), _) if value >= i32::MIN as i64 && value <= i32::MAX as i64 => {
                Value::Int4(value as i32)
            }
            _ => Value::Int8(value),
        }
    }
    let (Some(l), Some(r)) = (as_int(left), as_int(right)) else {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("bitwise operators require integer operands, got {left:?} and {right:?}"),
        )));
    };
    let value = match expr {
        Expression::BitAnd(_, _) => l & r,
        Expression::BitOr(_, _) => l | r,
        Expression::BitXor(_, _) => l ^ r,
        Expression::ShiftLeft(_, _) => {
            if !(0..=63).contains(&r) {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "shift amount out of range",
                )));
            }
            l.wrapping_shl(r as u32)
        }
        Expression::ShiftRight(_, _) => {
            if !(0..=63).contains(&r) {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "shift amount out of range",
                )));
            }
            l.wrapping_shr(r as u32)
        }
        _ => unreachable!("integer_bitwise only handles bitwise expressions"),
    };
    Ok(result(value, left, right))
}

/// Extracts a named attribute from a composite/record value, implementing
/// PostgreSQL's `(record).field` access. Only `Value::Composite` supports
/// named attribute access.
pub(crate) fn row_field_value(value: &Value, field: &str) -> SqlResult<Value> {
    match value {
        Value::Composite { fields, .. } => {
            for (name, field_value) in fields {
                if name.eq_ignore_ascii_case(field) {
                    return Ok(field_value.clone());
                }
            }
            Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("record has no attribute \"{field}\""),
            )))
        }
        other => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("cannot access field \"{field}\" of non-record value {other:?}"),
        ))),
    }
}

/// Implements PostgreSQL's `||` operator for string and JSON/JSONB values.
pub(crate) fn concat_operator(left: &Value, right: &Value) -> SqlResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    // JSONB/JSON `||` operator: object merge or array concatenation.
    if matches!(left, Value::Json(_) | Value::Jsonb(_))
        && matches!(right, Value::Json(_) | Value::Jsonb(_))
    {
        return jsonb_concat(left, right);
    }
    fn string_value(value: &Value) -> Option<&str> {
        match value {
            Value::BpChar(s)
            | Value::VarChar(s)
            | Value::Text(s)
            | Value::Name(s)
            | Value::Xml(s)
            | Value::Cstring(s)
            | Value::Unknown(s) => Some(s),
            _ => None,
        }
    }
    // PostgreSQL's `||` for text (`textcat`) applies an implicit cast to text
    // for operands whose type permits one - e.g. `text || integer` or
    // `text || bigint`.  The fast-path above only covers textual types; here we
    // resolve non-textual operands through the same implicit-cast rules that
    // the rest of the engine already relies on (`coerce_to_common_type`,
    // `cast_value`), rather than unconditionally stringifying.
    fn resolve_text(value: &Value, side: &str) -> SqlResult<String> {
        if let Some(s) = string_value(value) {
            return Ok(s.to_string());
        }
        if let Some(src_ty) = value_pg_type(value) {
            let text_oid = PgType::Text.oid();
            if plomid_types::builtin_casts().allows(
                src_ty.oid(),
                text_oid,
                plomid_types::CastContext::Implicit,
            ) {
                let casted = plomid_types::apply_cast(
                    value.clone(),
                    src_ty.oid(),
                    text_oid,
                    plomid_types::NO_TYPEMOD,
                )
                .map_err(SqlError::Storage)?;
                return Ok(casted.to_sql_text());
            }
        }
        Err(invalid_arg(
            "||",
            &format!("operator does not exist for the {side} operand type"),
        ))
    }
    let left_text = resolve_text(left, "left")?;
    let right_text = resolve_text(right, "right")?;
    Ok(Value::Text(format!("{left_text}{right_text}")))
}

/// Converts a PostgreSQL ordinal date (days since 2000-01-01) into an ISO
/// `YYYY-MM-DD` text form. Values before the epoch yield a negative year sign.
/// Implements `int4range(lower, upper)` generating the `[lower, upper)` range.
#[must_use]
/// Either bound may be NULL, in which case that side is unbounded.
fn int4range_function(lname: &str, args: &[Value]) -> SqlResult<Value> {
    require_args(lname, args, 2, 2)?;
    let lower = match &args[0] {
        Value::Null => None,
        Value::Int2(n) => Some(Box::new(Value::Int4(i32::from(*n)))),
        Value::Int4(n) => Some(Box::new(Value::Int4(*n))),
        Value::Int8(n) => Some(Box::new(Value::Int4(*n as i32))),
        _ => return Err(invalid_arg(lname, "range lower bound must be an integer")),
    };
    let upper = match &args[1] {
        Value::Null => None,
        Value::Int2(n) => Some(Box::new(Value::Int4(i32::from(*n)))),
        Value::Int4(n) => Some(Box::new(Value::Int4(*n))),
        Value::Int8(n) => Some(Box::new(Value::Int4(*n as i32))),
        _ => return Err(invalid_arg(lname, "range upper bound must be an integer")),
    };
    Ok(Value::Range {
        type_oid: plomid_types::TypeOid::INT4RANGE,
        range: plomid_types::RangeData {
            empty: false,
            lower,
            upper,
            lower_inclusive: true,
            upper_inclusive: false,
        },
    })
}

/// Implements `inet(addr[, prefix])`: constructs an `inet` value from a scalar
/// IP-address text and optional prefix length. Used when the input cast syntax
/// `'1.2.3.4'::inet` is invoked in function-call form.
fn inet_function(lname: &str, args: &[Value]) -> SqlResult<Value> {
    require_args(lname, args, 1, 2)?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let text = scalar_text(&args[0]);
    // Parse "addr" or "addr/prefix" text.
    let (addr_text, explicit_prefix) = match text.split_once('/') {
        Some((a, p)) => (a, Some(p.trim().parse::<u8>().unwrap_or(0u8))),
        None => (text.as_str(), None),
    };
    let parse_result = addr_text.trim().parse::<std::net::IpAddr>();
    let Some(addr) = parse_result.ok() else {
        return Err(invalid_arg(lname, "invalid IP address"));
    };
    let max_prefix = match addr {
        std::net::IpAddr::V4(_) => 32u8,
        std::net::IpAddr::V6(_) => 128u8,
    };
    let prefix = explicit_prefix.unwrap_or(max_prefix);
    if prefix > max_prefix {
        return Err(invalid_arg(
            lname,
            "prefix length exceeds address family width",
        ));
    }
    Ok(Value::Inet {
        addr,
        prefix,
        cidr: false,
    })
}

/// Implements `int4multirange(range, ...)` constructing a multirange from one or
/// more `int4range` values. PostgreSQL's `int4multirange()` constructor accepts
/// a variable number of range arguments and returns the union as a multirange
/// value. The element type is `integer` (int4), matching the underlying
/// `int4range` element type.
fn int4multirange_function(lname: &str, args: &[Value]) -> SqlResult<Value> {
    require_args(lname, args, 1, 16)?;
    let mut ranges = Vec::with_capacity(args.len());
    for (i, arg) in args.iter().enumerate() {
        let (lower, upper, lower_inclusive, upper_inclusive) = match arg {
            Value::Null => {
                return Err(invalid_arg(lname, &format!("argument {i} is NULL")));
            }
            Value::Range { range, .. } => {
                // Reuse the existing range's bounds directly — they are already
                // `Option<Box<PgValue>>` with the correct integer element type.
                (
                    range.lower.clone(),
                    range.upper.clone(),
                    range.lower_inclusive,
                    range.upper_inclusive,
                )
            }
            _ => return Err(invalid_arg(lname, "argument must be an int4range")),
        };
        ranges.push(plomid_types::RangeData {
            empty: false,
            lower,
            upper,
            lower_inclusive,
            upper_inclusive,
        });
    }
    Ok(Value::MultiRange {
        type_oid: plomid_types::TypeOid::INT4MULTIRANGE,
        ranges,
    })
}
