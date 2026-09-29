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
//! Output schema of a SELECT: column names and column types.
//!
//! Names come from `AS` aliases, table columns or synthesized expressions;
//! types come from the target expression or, where that is undecidable, from
//! the function involved.

use crate::catalog_fn::function_value;
use crate::catalog_fn::is_session_function;
use crate::catalog_fn::sequence_value;
use crate::catalog_fn::session_function_type;
use crate::coerce::value_type;
use crate::error::{SqlError, SqlResult};
use crate::util::unqualify;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::ColumnType;
use plomid_sql::Expression;
use plomid_sql::InMemoryCatalog;
use plomid_sql::SelectTarget;
use plomid_sql::TableSchema;
use plomid_sql::Value;
use plomid_txn::StorageEngine;

pub(super) fn select_columns(
    schema: &TableSchema,
    targets: &[SelectTarget],
) -> SqlResult<Vec<String>> {
    if targets.len() == 1
        && matches!(
            targets[0],
            SelectTarget::All | SelectTarget::QualifiedStar { .. }
        )
    {
        return Ok(schema.columns.iter().map(|c| c.name.clone()).collect());
    }
    let mut columns = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            SelectTarget::All | SelectTarget::QualifiedStar { .. } => {
                columns.extend(schema.columns.iter().map(|c| c.name.clone()));
            }
            SelectTarget::Expr { expr, alias } => {
                columns.push(alias.clone().unwrap_or_else(|| match expr {
                    Expression::ColumnRef(name) => unqualify(name).to_string(),
                    _ => "?column?".to_string(),
                }))
            }
            SelectTarget::Function(name) => columns.push(name.clone()),
            SelectTarget::FunctionCall { name, .. } => columns.push(name.clone()),
            SelectTarget::WindowFunction { name, .. } => {
                columns.push(name.to_ascii_lowercase());
            }
            SelectTarget::Aliased { target, alias } => {
                let _ = select_columns(schema, std::slice::from_ref(target))?;
                columns.push(alias.clone());
            }
        }
    }
    Ok(columns)
}

pub(super) fn select_column_types(
    schema: &TableSchema,
    targets: &[SelectTarget],
) -> SqlResult<Vec<Option<ColumnType>>> {
    if targets.len() == 1
        && matches!(
            targets[0],
            SelectTarget::All | SelectTarget::QualifiedStar { .. }
        )
    {
        return Ok(schema
            .columns
            .iter()
            .map(|column| Some(column.col_type))
            .collect());
    }
    let mut types = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            SelectTarget::All | SelectTarget::QualifiedStar { .. } => {
                types.extend(schema.columns.iter().map(|column| Some(column.col_type)))
            }
            SelectTarget::Expr { expr, .. } => types.push(match expr {
                Expression::ColumnRef(name) => {
                    Some(schema.columns[schema.column_index(unqualify(name))?].col_type)
                }
                Expression::Literal(value) => value_type(value),
                Expression::FunctionCall { name, args, .. } => {
                    aggregate_function_type(name, args, schema).or_else(|| bare_function_type(name))
                }
                Expression::Cast { type_name, .. } | Expression::TypeCast { type_name, .. } => {
                    type_name_to_column_type(type_name)
                }
                Expression::Extract { .. } => Some(ColumnType::new(
                    plomid_types::TypeOid::FLOAT8,
                    plomid_types::NO_TYPEMOD,
                )),
                _ => None,
            }),
            SelectTarget::Function(name) => types.push(session_function_type(name)),
            SelectTarget::FunctionCall { name, .. } => {
                types.push(if let Some(ty) = session_function_type(name) {
                    Some(ty)
                } else if let SelectTarget::FunctionCall { args, .. } = target {
                    aggregate_function_type(name, args, schema)
                } else if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "format"
                        | "obj_description"
                        | "col_description"
                        | "format_type"
                        | "row_to_json"
                ) {
                    Some(ColumnType::text())
                } else if name.eq_ignore_ascii_case("pg_total_relation_size") {
                    Some(ColumnType::new(
                        plomid_types::TypeOid::INT8,
                        plomid_types::NO_TYPEMOD,
                    ))
                } else {
                    Some(ColumnType::int4())
                })
            }
            SelectTarget::Aliased { target, .. } => {
                types.extend(select_column_types(schema, std::slice::from_ref(target))?);
            }
            SelectTarget::WindowFunction { .. } => types.push(None),
        }
    }
    Ok(types)
}

pub(super) fn project_bare_select<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    targets: &[SelectTarget],
    current_database: &str,
    current_user: &str,
) -> SqlResult<Vec<Value>> {
    let mut row = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            SelectTarget::Expr {
                expr: Expression::Literal(value),
                ..
            } => row.push(value.clone()),
            SelectTarget::Expr {
                expr:
                    Expression::TypeCast { expr, type_name } | Expression::Cast { expr, type_name },
                ..
            } => {
                let Expression::Literal(value) = expr.as_ref() else {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::Unsupported,
                        "bare casts require a literal expression",
                    )));
                };
                row.push(crate::coerce::cast_value(value, type_name)?);
            }
            SelectTarget::Expr {
                expr: Expression::ColumnRef(name),
                ..
            } => row.push(function_value(name, current_database, current_user)?),
            SelectTarget::Expr { .. } => {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Unsupported,
                    "expression requires a FROM clause",
                )))
            }
            SelectTarget::Function(name) => {
                row.push(function_value(name, current_database, current_user)?)
            }
            SelectTarget::FunctionCall { name, args } => {
                if is_session_function(name) {
                    row.push(function_value(name, current_database, current_user)?);
                } else if crate::scalar::is_scalar_function(name)
                    || matches!(
                        name.to_ascii_lowercase().as_str(),
                        "pg_get_constraintdef" | "pg_get_indexdef"
                    )
                {
                    let values = args
                        .iter()
                        .map(|arg| match arg {
                            Expression::Literal(value) => Ok(value.clone()),
                            Expression::TypeCast { expr, type_name }
                            | Expression::Cast { expr, type_name } => match expr.as_ref() {
                                Expression::Literal(value) => {
                                    crate::coerce::cast_value(value, type_name)
                                }
                                _ => Err(SqlError::Storage(PlomidError::new(
                                    ErrorKind::Unsupported,
                                    "bare function arguments must be scalar literals",
                                ))),
                            },
                            _ => Err(SqlError::Storage(PlomidError::new(
                                ErrorKind::Unsupported,
                                "bare function arguments must be scalar literals",
                            ))),
                        })
                        .collect::<SqlResult<Vec<_>>>()?;
                    if matches!(
                        name.to_ascii_lowercase().as_str(),
                        "pg_get_constraintdef" | "pg_get_indexdef"
                    ) {
                        row.push(crate::catalog_fn::catalog_object_definition(
                            catalog,
                            &name.to_ascii_lowercase(),
                            values.first(),
                        ));
                    } else {
                        row.push(crate::scalar::scalar_function_value(name, &values)?);
                    }
                } else {
                    let argument = args
                        .first()
                        .and_then(|e| match e {
                            Expression::Literal(Value::Text(v)) => Some(v.as_str()),
                            _ => None,
                        })
                        .unwrap_or("");
                    row.push(sequence_value(engine, name, argument)?);
                }
            }
            SelectTarget::WindowFunction { name, .. } => {
                let _ = name;
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Unsupported,
                    "window functions must be evaluated by the general query engine",
                )));
            }
            SelectTarget::Aliased { target, .. } => {
                row.extend(project_bare_select(
                    engine,
                    catalog,
                    std::slice::from_ref(target),
                    current_database,
                    current_user,
                )?);
            }
            SelectTarget::All | SelectTarget::QualifiedStar { .. } => {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "* is not allowed without a FROM clause",
                )));
            }
        }
    }
    Ok(row)
}

pub(super) fn select_bare_columns(targets: &[SelectTarget]) -> SqlResult<Vec<String>> {
    let mut columns = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            SelectTarget::All | SelectTarget::QualifiedStar { .. } => {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "* is not allowed without a FROM clause",
                )));
            }
            SelectTarget::Expr {
                expr: Expression::ColumnRef(name),
                ..
            } => {
                if is_session_function(name) {
                    columns.push(name.clone());
                } else {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::InvalidArgument,
                        format!("column \"{name}\" does not exist without a FROM clause"),
                    )));
                }
            }
            SelectTarget::Expr { alias, .. } => {
                columns.push(alias.clone().unwrap_or_else(|| "?column?".into()))
            }
            SelectTarget::Function(name) => columns.push(name.clone()),
            SelectTarget::FunctionCall { name, .. } => columns.push(name.clone()),
            SelectTarget::WindowFunction { name, .. } => {
                columns.push(name.to_ascii_lowercase());
            }
            SelectTarget::Aliased { alias, .. } => columns.push(alias.clone()),
        }
    }
    Ok(columns)
}

pub(super) fn select_bare_column_types(
    targets: &[SelectTarget],
) -> SqlResult<Vec<Option<ColumnType>>> {
    let mut types = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            SelectTarget::Expr {
                expr: Expression::Literal(value),
                ..
            } => types.push(value_type(value)),
            SelectTarget::Function(name) => types.push(session_function_type(name)),
            SelectTarget::FunctionCall { name, .. } => {
                types.push(session_function_type(name).or_else(|| Some(ColumnType::int4())))
            }
            SelectTarget::Aliased { target, .. } => {
                types.extend(select_bare_column_types(std::slice::from_ref(target))?);
            }
            SelectTarget::All | SelectTarget::QualifiedStar { .. } => {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "* is not allowed without a FROM clause",
                )));
            }
            SelectTarget::Expr {
                expr: Expression::ColumnRef(name),
                ..
            } => {
                if let Some(ty) = session_function_type(name) {
                    types.push(Some(ty));
                } else {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::InvalidArgument,
                        format!("column \"{name}\" does not exist without a FROM clause"),
                    )));
                }
            }
            SelectTarget::Expr { expr, .. } => types.push(match expr {
                Expression::FunctionCall { name, .. } => bare_function_type(name),
                Expression::Cast { type_name, .. } | Expression::TypeCast { type_name, .. } => {
                    type_name_to_column_type(type_name)
                }
                Expression::Extract { .. } => Some(ColumnType::new(
                    plomid_types::TypeOid::FLOAT8,
                    plomid_types::NO_TYPEMOD,
                )),
                _ => None,
            }),
            SelectTarget::WindowFunction { .. } => types.push(None),
        }
    }
    Ok(types)
}

fn bare_function_type(name: &str) -> Option<ColumnType> {
    match name.to_ascii_lowercase().as_str() {
        "length" | "char_length" | "character_length" => Some(ColumnType::int4()),
        "lower" | "upper" | "trim" | "btrim" | "ltrim" | "rtrim" | "substring" | "substr"
        | "replace" | "concat" | "concat_ws" => Some(ColumnType::text()),
        "extract" | "date_part" => Some(ColumnType::new(
            plomid_types::TypeOid::FLOAT8,
            plomid_types::NO_TYPEMOD,
        )),
        _ => None,
    }
}

/// Result metadata for aggregates handled by the single-table executor.
/// Keeping this beside the fast path prevents the wire layer from inferring
/// AVG/SUM from the first runtime value as an integer type.
fn aggregate_function_type(
    name: &str,
    args: &[Expression],
    schema: &TableSchema,
) -> Option<ColumnType> {
    let oid = match name.to_ascii_lowercase().as_str() {
        "count" => return Some(ColumnType::bigint()),
        "avg" => {
            return Some(ColumnType::new(
                plomid_types::TypeOid::NUMERIC,
                plomid_types::NO_TYPEMOD,
            ))
        }
        _ => {}
    };
    let Expression::ColumnRef(column) = args.first()? else {
        return None;
    };
    let input = schema
        .columns
        .get(schema.column_index(unqualify(column)).ok()?)?
        .col_type;
    match name.to_ascii_lowercase().as_str() {
        "min" | "max" => Some(input),
        "sum" => match input.type_oid {
            plomid_types::TypeOid::INT2 | plomid_types::TypeOid::INT4 => Some(ColumnType::bigint()),
            plomid_types::TypeOid::INT8 | plomid_types::TypeOid::NUMERIC => Some(ColumnType::new(
                plomid_types::TypeOid::NUMERIC,
                plomid_types::NO_TYPEMOD,
            )),
            plomid_types::TypeOid::FLOAT4 | plomid_types::TypeOid::FLOAT8 => Some(ColumnType::new(
                plomid_types::TypeOid::FLOAT8,
                plomid_types::NO_TYPEMOD,
            )),
            _ => None,
        },
        _ => {
            let _ = oid;
            None
        }
    }
}

fn type_name_to_column_type(type_name: &str) -> Option<ColumnType> {
    let normalized = type_name.to_ascii_lowercase();
    if let Some(element_name) = normalized.strip_suffix("[]") {
        let element_type = type_name_to_column_type(element_name.trim())?;
        let element = plomid_types::PgType::by_oid(element_type.type_oid)?;
        return Some(ColumnType::new(
            element.array_oid()?,
            plomid_types::NO_TYPEMOD,
        ));
    }
    let oid = match normalized.trim() {
        "smallint" | "int2" => plomid_types::TypeOid::INT2,
        "integer" | "int" | "int4" => plomid_types::TypeOid::INT4,
        "bigint" | "int8" => plomid_types::TypeOid::INT8,
        "real" | "float4" => plomid_types::TypeOid::FLOAT4,
        "double precision" | "float8" => plomid_types::TypeOid::FLOAT8,
        "numeric" | "decimal" => plomid_types::TypeOid::NUMERIC,
        "text" => plomid_types::TypeOid::TEXT,
        "varchar" | "character varying" => plomid_types::TypeOid::VARCHAR,
        "date" => plomid_types::TypeOid::DATE,
        "timestamp" => plomid_types::TypeOid::TIMESTAMP,
        "timestamptz" | "timestamp with time zone" => plomid_types::TypeOid::TIMESTAMPTZ,
        "boolean" | "bool" => plomid_types::TypeOid::BOOL,
        "json" => plomid_types::TypeOid::JSON,
        "jsonb" => plomid_types::TypeOid::JSONB,
        _ => return None,
    };
    Some(ColumnType::new(oid, plomid_types::NO_TYPEMOD))
}
