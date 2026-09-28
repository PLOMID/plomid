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
//! Statement execution context for PLOMID.
//!
//! PostgreSQL defines CURRENT_TIMESTAMP as the statement start time: every
//! reference within one statement sees the same logical timestamp. The parser
//! represents these keywords as zero-argument FunctionCalls, so evaluators
//! need a statement-scoped clock that outlives any single row evaluation.

use std::cell::{OnceCell, RefCell};

use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::Value;

use crate::error::{SqlError, SqlResult};

thread_local! {
    static TLS: RefCell<Option<std::rc::Rc<StatementContext>>> = const { RefCell::new(None) };
}

/// Statement-scoped execution context.
pub struct StatementContext {
    database: String,
    user: String,
    timestamp: OnceCell<Value>,
    /// Session `search_path` captured when the statement began. `current_schema()`
    /// and `current_schemas()` resolve against this rather than hard-coding
    /// `public` so an explicit `SET search_path` is honored for the statement.
    search_path: Vec<String>,
    /// First schema in the (existing) search path, per PostgreSQL's
    /// `current_schema()` semantics, or `None` when no schema is selectable.
    current_schema: Option<String>,
}

impl StatementContext {
    fn new(database: &str, user: &str) -> Self {
        Self {
            database: database.to_string(),
            user: user.to_string(),
            timestamp: OnceCell::new(),
            search_path: vec!["public".to_string()],
            current_schema: Some("public".to_string()),
        }
    }

    /// Cached statement timestamp (TIMESTAMP micros since 2000-01-01).
    pub fn statement_timestamp(&self) -> SqlResult<Value> {
        if let Some(v) = self.timestamp.get() {
            return Ok(v.clone());
        }
        let v = system_timestamp().map_err(SqlError::Storage)?;
        let _ = self.timestamp.set(v.clone());
        Ok(v)
    }

    /// CURRENT_DATE derived from the same instant as the statement timestamp.
    pub fn statement_date(&self) -> SqlResult<Value> {
        let Value::Timestamp(ts) = self.statement_timestamp()? else {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Internal,
                "statement timestamp is not a timestamp",
            )));
        };
        let unix_secs = ts.div_euclid(1_000_000) + 946_684_800;
        let days =
            unix_secs.div_euclid(86_400) as i32 - plomid_types::datetime::POSTGRES_EPOCH_JDATE;
        Ok(Value::Date(days))
    }

    /// CURRENT_TIME derived from the same instant as the statement timestamp.
    pub fn statement_time(&self) -> SqlResult<Value> {
        let Value::Timestamp(ts) = self.statement_timestamp()? else {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Internal,
                "statement timestamp is not a timestamp",
            )));
        };
        let unix_secs = ts.div_euclid(1_000_000) + 946_684_800;
        let micros_rem = ts.rem_euclid(1_000_000);
        let micros = unix_secs.rem_euclid(86_400) * 1_000_000 + micros_rem;
        Ok(Value::Time(micros))
    }
}

/// RAII guard restoring the previous context on drop.
pub struct Guard {
    is_owner: bool,
    prev: Option<std::rc::Rc<StatementContext>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.is_owner {
            TLS.with(|tls| {
                *tls.borrow_mut() = self.prev.take();
            });
        }
    }
}

impl StatementContext {
    /// Enter a statement context. Nested statements reuse the outer timestamp.
    pub fn enter_with(database: &str, user: &str) -> Guard {
        Self::enter_with_search_path(
            database,
            user,
            vec!["public".to_string()],
            Some("public".to_string()),
        )
    }

    /// Enter a statement context carrying the session `search_path` and the
    /// resolved `current_schema` computed from the catalog.
    pub fn enter_with_search_path(
        database: &str,
        user: &str,
        search_path: Vec<String>,
        current_schema: Option<String>,
    ) -> Guard {
        let already = TLS.with(|tls| tls.borrow().is_some());
        if already {
            return Guard {
                is_owner: false,
                prev: None,
            };
        }
        let mut ctx = StatementContext::new(database, user);
        ctx.search_path = search_path;
        ctx.current_schema = current_schema;
        let ctx = std::rc::Rc::new(ctx);
        TLS.with(|tls| {
            *tls.borrow_mut() = Some(ctx);
        });
        Guard {
            is_owner: true,
            prev: None,
        }
    }

    /// Enter with default session ids if none is active.
    pub fn enter_if_none() -> Guard {
        Self::enter_with("plomid", "plomid")
    }

    /// The effective session search path for this statement.
    pub fn statement_search_path(&self) -> &[String] {
        &self.search_path
    }

    /// The first existing schema in the search path (PostgreSQL
    /// `current_schema()` semantics), if any.
    pub fn statement_current_schema(&self) -> Option<&str> {
        self.current_schema.as_deref()
    }
}

fn system_timestamp() -> Result<Value, PlomidError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| PlomidError::new(ErrorKind::InvalidArgument, "system clock before epoch"))?;
    Ok(Value::Timestamp(
        (now.as_secs() as i64 - 946_684_800) * 1_000_000 + i64::from(now.subsec_micros()),
    ))
}

fn with_current<T>(f: impl FnOnce(&StatementContext) -> T, fallback: impl FnOnce() -> T) -> T {
    let rc = TLS.with(|tls| tls.borrow().clone());
    match rc {
        Some(ctx) => f(&ctx),
        None => fallback(),
    }
}

/// Current statement timestamp, or fresh system time outside a statement.
pub fn statement_timestamp() -> SqlResult<Value> {
    with_current(
        |ctx| ctx.statement_timestamp(),
        || system_timestamp().map_err(SqlError::Storage),
    )
}

/// Current statement date, or fresh system date outside a statement.
pub fn statement_date() -> SqlResult<Value> {
    with_current(
        |ctx| ctx.statement_date(),
        || {
            let days = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| {
                    SqlError::Storage(PlomidError::new(
                        ErrorKind::InvalidArgument,
                        "system clock before epoch",
                    ))
                })?
                .as_secs()
                / 86_400;
            Ok(Value::Date(
                days as i32 - plomid_types::datetime::POSTGRES_EPOCH_JDATE,
            ))
        },
    )
}

/// Current statement time, or fresh system time outside a statement.
pub fn statement_time() -> SqlResult<Value> {
    with_current(
        |ctx| ctx.statement_time(),
        || {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| {
                    SqlError::Storage(PlomidError::new(
                        ErrorKind::InvalidArgument,
                        "system clock before epoch",
                    ))
                })?;
            Ok(Value::Time(
                (now.as_secs() % 86_400) as i64 * 1_000_000 + i64::from(now.subsec_micros()),
            ))
        },
    )
}

/// Current statement timestamp as raw micros (for scalar functions like age()).
pub fn statement_timestamp_value_micros() -> i64 {
    match statement_timestamp() {
        Ok(Value::Timestamp(micros)) | Ok(Value::Timestamptz(micros)) => micros,
        _ => 0,
    }
}

/// Session ids visible to the current statement.
pub fn current_ids() -> (String, String) {
    with_current(
        |ctx| (ctx.database.clone(), ctx.user.clone()),
        || ("plomid".to_string(), "plomid".to_string()),
    )
}

/// Effective session search path visible to the current statement.
pub fn current_search_path() -> Vec<String> {
    with_current(
        |ctx| ctx.statement_search_path().to_vec(),
        || vec!["public".to_string()],
    )
}

/// Current schema (first existing schema in the search path), or `None` outside
/// a statement context / when no schema is selectable.
pub fn current_schema() -> Option<String> {
    with_current(
        |ctx| ctx.statement_current_schema().map(str::to_string),
        || Some("public".to_string()),
    )
}
