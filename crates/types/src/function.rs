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
//! Argument validation and value rendering for function implementations.
//!
//! Shared by the executor's scalar-function registry and the modality crates'
//! operator libraries, so a function argument is validated and rendered exactly
//! one way no matter which crate implements it.

use plomid_core::{ErrorKind, PlomidError, SqlError, SqlResult};

use crate::PgValue;

/// Extracts displayable text from a runtime value for scalar string functions.
pub fn scalar_text(value: &PgValue) -> String {
    match value {
        PgValue::Text(s) | PgValue::VarChar(s) | PgValue::BpChar(s) | PgValue::Name(s) => s.clone(),
        PgValue::Unknown(s) | PgValue::Cstring(s) => s.clone(),
        PgValue::Null => String::new(),
        other => other.to_sql_text(),
    }
}

pub fn invalid_arg(function: &str, detail: &str) -> SqlError {
    SqlError::Storage(PlomidError::new(
        ErrorKind::InvalidArgument,
        format!("invalid argument for function {function}: {detail}"),
    ))
}

pub fn require_args(name: &str, args: &[PgValue], min: usize, max: usize) -> SqlResult<()> {
    if args.len() < min || args.len() > max {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!(
                "function {name} expects {} argument(s), got {}",
                if min == max {
                    min.to_string()
                } else {
                    format!("{min}-{max}")
                },
                args.len()
            ),
        )));
    }
    Ok(())
}

pub fn ordinal_to_ymd(days: i32) -> String {
    crate::datetime::format_date(days)
}
