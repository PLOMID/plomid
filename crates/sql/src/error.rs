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
//! SQL-layer error plumbing.
//!
//! The [`SqlError`] / [`SqlResult`] definitions live in `plomid-core` so that
//! the parser, executor, modality crates and wire protocol all share one error
//! type. This module adds the parser-specific conversions (which can only live
//! where [`LexError`] / [`ParseError`] are defined) and keeps the historic
//! `plomid_sql::error::…` path resolving.

pub use plomid_core::{SqlError, SqlResult};

use plomid_core::{ErrorKind, PlomidError};

use crate::{LexError, ParseError};

impl From<LexError> for SqlError {
    fn from(err: LexError) -> Self {
        let plomid = PlomidError::new(
            ErrorKind::Syntax,
            format!("syntax error at line {}: {}", err.line, err.message),
        );
        Self::Syntax(plomid)
    }
}

impl From<ParseError> for SqlError {
    fn from(err: ParseError) -> Self {
        let plomid = match err {
            ParseError::UnexpectedToken {
                expected,
                found,
                line,
                column,
            } => PlomidError::new(
                ErrorKind::Syntax,
                format!("syntax error at line {line}, column {column}: expected {expected}, found {found}"),
            ),
            ParseError::LexError(err) => PlomidError::new(
                ErrorKind::Syntax,
                format!("syntax error at line {}: {}", err.line, err.message),
            ),
            ParseError::DuplicateTable(name) => {
                PlomidError::new(ErrorKind::AlreadyExists, format!("table \"{name}\" already exists"))
            }
            ParseError::DuplicateColumn(name) => {
                PlomidError::new(ErrorKind::InvalidArgument, format!("duplicate column: {name}"))
            }
            ParseError::UnknownTable(name) => {
                PlomidError::new(ErrorKind::NotFound, format!("table \"{name}\" does not exist"))
            }
            ParseError::Unsupported { message, detail } => {
                if let Some(detail) = detail {
                    PlomidError::with_detail(ErrorKind::Unsupported, message, detail)
                } else {
                    PlomidError::new(ErrorKind::Unsupported, message)
                }
            }
        };
        Self::Syntax(plomid)
    }
}
