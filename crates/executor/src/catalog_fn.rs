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
//! Catalog, session and introspection functions.
//!
//! Backs `pg_catalog`-style helpers (`pg_get_indexdef`, `regclass` lookup,
//! `current_database`, `version`) and sequence access. These read the live
//! catalog, so unknown OIDs must yield NULL rather than an error — GUI catalog
//! browsers probe them concurrently with DDL.

use crate::error::{SqlError, SqlResult};
use crate::util::unqualify;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{Catalog, ColumnType, InMemoryCatalog, Value};
use plomid_txn::{StorageEngine, StorageEngineTransaction};
use plomid_types::function::scalar_text;
use plomid_types::TypeOid;

/// Resolves `pg_get_constraintdef(oid)` / `pg_get_indexdef(oid)` against the
/// live catalog. Unknown OIDs yield NULL rather than an error: GUI catalog
/// browsers probe these concurrently with DDL and must not surface hard
/// failures for a definition that just disappeared.
#[must_use]
pub(crate) fn catalog_object_definition(
    catalog: &InMemoryCatalog,
    function_name: &str,
    arg: Option<&Value>,
) -> Value {
    let oid = match arg {
        Some(Value::Oid(o)) => Some(*o),
        Some(Value::Int2(v)) => Some(i32::from(*v) as u32),
        Some(Value::Int4(v)) => Some(*v as u32),
        Some(Value::Int8(v)) => Some(*v as u32),
        _ => None,
    };
    let Some(oid) = oid else {
        return Value::Null;
    };
    let definition = if function_name == "pg_get_constraintdef" {
        crate::system_catalog::constraint_definition(catalog, oid)
    } else {
        crate::system_catalog::index_definition(catalog, oid)
    };
    definition.map_or(Value::Null, Value::Text)
}

/// Resolves a name to its OID for the `to_regclass` and `to_regnamespace`
/// functions.  These are used by JDBC metadata queries to map relation and
/// namespace names to their catalog OIDs.  Unknown names yield NULL.
pub(crate) fn reg_lookup(
    catalog: &InMemoryCatalog,
    function_name: &str,
    arg: Option<&Value>,
) -> SqlResult<Value> {
    let name = match arg {
        Some(Value::Text(text)) => text.clone(),
        Some(other) => scalar_text(other),
        None => return Ok(Value::Null),
    };
    if name.is_empty() {
        return Ok(Value::Null);
    }
    if function_name == "to_regclass" {
        // Accept a bare name or a schema-qualified name.
        let lookup = if name.contains('.') {
            name.clone()
        } else {
            format!("public.{name}")
        };
        if let Ok(schema) = catalog.get_table(&lookup) {
            return Ok(Value::Oid(schema.table_id.get() as u32));
        }
        // The catalog may store tables under their bare name.
        if let Ok(schema) = catalog.get_table(&name) {
            return Ok(Value::Oid(schema.table_id.get() as u32));
        }
        // Fall back to a case-insensitive scan across all tables and views.
        let lower = lookup.to_ascii_lowercase();
        if let Some(table) = catalog
            .tables()
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(&lower))
        {
            return Ok(Value::Oid(table.table_id.get() as u32));
        }
        Ok(Value::Null)
    } else {
        // to_regnamespace
        match namespace_oid_value(catalog, &name) {
            Some(oid) => Ok(Value::Oid(oid)),
            None => Ok(Value::Null),
        }
    }
}

/// Mirrors the fixed OID assignments in `SystemCatalog::namespace_oid`.
fn namespace_oid_value(catalog: &InMemoryCatalog, name: &str) -> Option<u32> {
    match name {
        "pg_catalog" => Some(11),
        "information_schema" => Some(13207),
        "public" => Some(2200),
        other => catalog.schema_id(other).map(|id| id.get() as u32),
    }
}

pub(crate) fn sequence_key(name: &str) -> Vec<u8> {
    format!("__plomid_sequence:{name}").into_bytes()
}

pub(crate) fn sequence_value<E: StorageEngine>(
    engine: &mut E,
    name: &str,
    argument: &str,
) -> SqlResult<Value> {
    if !matches!(name.to_ascii_lowercase().as_str(), "nextval" | "currval") {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            format!("function \"{name}\" is not supported"),
        )));
    }
    let key = sequence_key(argument);
    let bytes = engine.get(&key)?.ok_or_else(|| {
        PlomidError::new(
            ErrorKind::NotFound,
            format!("sequence \"{argument}\" does not exist"),
        )
    })?;
    if bytes.len() != 8 {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Corruption,
            "sequence state is corrupted",
        )));
    }
    let current = i64::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| PlomidError::new(ErrorKind::Corruption, "sequence state is corrupted"))?,
    );
    if name.eq_ignore_ascii_case("currval") {
        return Ok(Value::Int8(current));
    }
    let next = current
        .checked_add(1)
        .ok_or_else(|| PlomidError::new(ErrorKind::Conflict, "sequence value exhausted"))?;
    let mut txn = engine.begin()?;
    txn.put(&key, &next.to_le_bytes())?;
    txn.commit()?;
    Ok(Value::Int8(next))
}

pub(crate) fn function_value(
    name: &str,
    current_database: &str,
    current_user: &str,
) -> SqlResult<Value> {
    let value = match unqualify(name).to_ascii_lowercase().as_str() {
        "version" => Value::Text("PLOMID 0.1.0".to_string()),
        "current_database" => Value::Text(current_database.to_string()),
        "current_schema" => match crate::context::current_schema() {
            Some(schema) => Value::Text(schema),
            None => Value::Null,
        },
        "current_schemas" => {
            // PG catalog function: current_schemas(bool) returns name[].
            let schemas = crate::context::current_search_path()
                .into_iter()
                .filter(|schema| !schema.is_empty() && schema != "$user")
                .map(Value::Name)
                .collect::<Vec<_>>();
            Value::Array {
                element_oid: TypeOid::NAME,
                elements: schemas,
            }
        }
        "current_user" => Value::Text(current_user.to_string()),
        "session_user" => Value::Text(current_user.to_string()),
        // The wire protocol uses the same stable backend identifier in
        // BackendKeyData.  Exposing it here lets PostgreSQL clients that
        // probe pg_backend_pid() initialize normally.
        "pg_backend_pid" => return Ok(Value::Int4(0x454E50)),
        // Clock built-ins resolve against the statement execution context so
        // every reference within one statement observes the same logical
        // timestamp (PostgreSQL transaction-timestamp semantics scoped here
        // to the statement). `crate::context` caches the timestamp once per
        // statement; the bare `SystemTime::now()` path below is only a
        // fallback when no statement context is active.
        "current_date" => return crate::context::statement_date(),
        "current_time" => return crate::context::statement_time(),
        "current_timestamp" | "now" => return crate::context::statement_timestamp(),
        _ => {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Unsupported,
                format!("function \"{name}\" is not supported"),
            )))
        }
    };
    Ok(value)
}

pub(crate) fn session_function_type(name: &str) -> Option<ColumnType> {
    match unqualify(name).to_ascii_lowercase().as_str() {
        // PG catalog function: current_schemas(bool) returns name[].
        "current_schemas" => Some(ColumnType::new(
            TypeOid::NAME_ARRAY,
            plomid_types::NO_TYPEMOD,
        )),
        "version" | "current_database" | "current_schema" | "current_user" | "session_user"
        | "pg_backend_pid" | "current_date" | "current_time" | "current_timestamp" | "now" => {
            Some(ColumnType::text())
        }
        _ => None,
    }
}

pub(crate) fn is_session_function(name: &str) -> bool {
    matches!(
        unqualify(name).to_ascii_lowercase().as_str(),
        "version"
            | "current_database"
            | "current_schema"
            | "current_user"
            | "session_user"
            | "current_schemas"
            | "pg_backend_pid"
            | "current_date"
            | "current_time"
            | "current_timestamp"
            | "now"
    )
}

pub(crate) fn is_aggregate_function(name: &str) -> bool {
    matches!(
        name.to_uppercase().as_str(),
        "COUNT"
            | "SUM"
            | "AVG"
            | "MIN"
            | "MAX"
            | "STRING_AGG"
            | "ARRAY_AGG"
            | "JSON_AGG"
            | "JSONB_AGG"
            | "JSON_OBJECT_AGG"
            | "JSONB_OBJECT_AGG"
            | "JSON_ARRAYAGG"
            | "JSON_OBJECTAGG"
    )
}
