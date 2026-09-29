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
//! Error types shared by PLOMID kernel crates.
//!
//! [`ErrorKind`] values and their codes are part of the stable error contract:
//! `PL-IO`, `PL-CORRUPTION`, `PL-INVALID-ARGUMENT`, `PL-NOT-FOUND`,
//! `PL-ALREADY-EXISTS`, `PL-CONFLICT`, `PL-ABORTED`, `PL-INTERNAL`,
//! `PL-UNSUPPORTED`, `PL-SYNTAX`, `PL-CATALOG`, `PL-TRANSACTION`,
//! `PL-WAL`, and `PL-ROW-ENCODING`. New kinds may be added without making
//! matching existing kinds non-exhaustive-safe.

use std::{error::Error, fmt, io};

/// Stable categories for errors returned by PLOMID APIs.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ErrorKind {
    /// An operating-system or filesystem operation failed.
    Io,
    /// Persisted data failed an integrity or format check.
    Corruption,
    /// An API argument is invalid.
    InvalidArgument,
    /// The requested object does not exist.
    NotFound,
    /// The requested object already exists.
    AlreadyExists,
    /// The operation conflicts with another operation or state.
    Conflict,
    /// The operation was aborted before completion.
    Aborted,
    /// An unexpected internal failure occurred.
    Internal,
    /// The requested operation is not supported.
    Unsupported,
    /// A SQL syntax or lexer error occurred.
    Syntax,
    /// A catalog metadata error occurred.
    Catalog,
    /// A transaction state or lifecycle error occurred.
    Transaction,
    /// A write-ahead log error occurred.
    Wal,
    /// A row encoding or decoding error occurred.
    RowEncoding,
}

impl ErrorKind {
    /// Returns the stable machine-readable code for this error category.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Io => "PL-IO",
            Self::Corruption => "PL-CORRUPTION",
            Self::InvalidArgument => "PL-INVALID-ARGUMENT",
            Self::NotFound => "PL-NOT-FOUND",
            Self::AlreadyExists => "PL-ALREADY-EXISTS",
            Self::Conflict => "PL-CONFLICT",
            Self::Aborted => "PL-ABORTED",
            Self::Internal => "PL-INTERNAL",
            Self::Unsupported => "PL-UNSUPPORTED",
            Self::Syntax => "PL-SYNTAX",
            Self::Catalog => "PL-CATALOG",
            Self::Transaction => "PL-TRANSACTION",
            Self::Wal => "PL-WAL",
            Self::RowEncoding => "PL-ROW-ENCODING",
        }
    }
}

/// The unified error returned by PLOMID library APIs.
//
// The optional source preserves lower-level failure details without exposing
// them in the default, path-safe display message. The optional detail carries
// internal diagnostic context separated from the user-facing message.
#[derive(Debug)]
pub struct PlomidError {
    kind: ErrorKind,
    message: String,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
    detail: Option<String>,
}

impl PlomidError {
    /// Creates an error with a stable kind and caller-provided safe message.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
            detail: None,
        }
    }

    /// Creates an error while preserving a lower-level source error.
    #[must_use]
    pub fn with_source<E>(kind: ErrorKind, message: impl Into<String>, source: E) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self {
            kind,
            message: message.into(),
            source: Some(Box::new(source)),
            detail: None,
        }
    }

    /// Creates an error with additional internal diagnostic context.
    #[must_use]
    pub fn with_detail(
        kind: ErrorKind,
        message: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
            detail: Some(detail.into()),
        }
    }

    /// Creates an error with a lower-level source and internal diagnostic context.
    #[must_use]
    pub fn with_source_and_detail<E>(
        kind: ErrorKind,
        message: impl Into<String>,
        source: E,
        detail: impl Into<String>,
    ) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self {
            kind,
            message: message.into(),
            source: Some(Box::new(source)),
            detail: Some(detail.into()),
        }
    }

    /// Returns the matchable category of this error.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Returns the stable machine-readable code of this error.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// Returns the safe, human-readable detail message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns optional internal diagnostic context.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl fmt::Display for PlomidError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message)
    }
}

impl Error for PlomidError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn Error + 'static))
    }
}

impl From<io::Error> for PlomidError {
    fn from(source: io::Error) -> Self {
        let message = format!("I/O operation failed ({:?})", source.kind());
        Self::with_source(ErrorKind::Io, message, source)
    }
}

/// The standard result type for PLOMID library APIs.
pub type Result<T> = std::result::Result<T, PlomidError>;

/// Faults raised while parsing, planning or executing a statement.
///
/// This type lives in the kernel, next to [`PlomidError`], because every layer
/// of PLOMID's front end speaks in it: the SQL parser, the executor, the
/// document/vector/graph modality crates that contribute operators, and the
/// wire protocol. The parser-specific `From<LexError>` / `From<ParseError>`
/// conversions are implemented in `plomid-sql`, where those error types live.
#[derive(Debug)]
pub enum SqlError {
    /// The statement could not be lexed/parsed/bound.
    Syntax(PlomidError),
    /// The statement was understood but failed while running.
    Storage(PlomidError),
}

impl From<PlomidError> for SqlError {
    fn from(err: PlomidError) -> Self {
        Self::Storage(err)
    }
}

impl std::fmt::Display for SqlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(err) => write!(formatter, "{err}"),
            Self::Storage(err) => write!(formatter, "{err}"),
        }
    }
}

impl std::error::Error for SqlError {}

/// Convenience alias for the SQL layer's fallible results.
pub type SqlResult<T> = std::result::Result<T, SqlError>;

#[cfg(test)]
mod tests {
    use super::{ErrorKind, PlomidError};
    use std::{error::Error, io};

    #[test]
    fn displays_stable_codes_for_major_kinds() {
        let cases = [
            (ErrorKind::Io, "PL-IO"),
            (ErrorKind::Corruption, "PL-CORRUPTION"),
            (ErrorKind::InvalidArgument, "PL-INVALID-ARGUMENT"),
            (ErrorKind::NotFound, "PL-NOT-FOUND"),
            (ErrorKind::AlreadyExists, "PL-ALREADY-EXISTS"),
            (ErrorKind::Conflict, "PL-CONFLICT"),
            (ErrorKind::Aborted, "PL-ABORTED"),
            (ErrorKind::Internal, "PL-INTERNAL"),
            (ErrorKind::Unsupported, "PL-UNSUPPORTED"),
            (ErrorKind::Syntax, "PL-SYNTAX"),
            (ErrorKind::Catalog, "PL-CATALOG"),
            (ErrorKind::Transaction, "PL-TRANSACTION"),
            (ErrorKind::Wal, "PL-WAL"),
            (ErrorKind::RowEncoding, "PL-ROW-ENCODING"),
        ];

        for (kind, code) in cases {
            let error = PlomidError::new(kind, "operation failed");
            assert_eq!(error.code(), code);
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), format!("{code}: operation failed"));
        }
    }

    #[test]
    fn converts_io_error_and_preserves_source_without_path_leakage() {
        let source = io::Error::new(io::ErrorKind::PermissionDenied, "/private/secret.key");
        let error = PlomidError::from(source);

        assert_eq!(error.kind(), ErrorKind::Io);
        assert!(error.source().is_some());
        assert!(error.to_string().contains("PermissionDenied"));
        assert!(!error.to_string().contains("/private/secret.key"));
    }

    #[test]
    fn carries_internal_detail() {
        let error = PlomidError::with_detail(
            ErrorKind::Corruption,
            "corrupt row data",
            "table=users key=users:1 version=1",
        );
        assert_eq!(error.kind(), ErrorKind::Corruption);
        assert_eq!(error.message(), "corrupt row data");
        assert_eq!(error.detail(), Some("table=users key=users:1 version=1"));
        assert!(error.to_string().contains("PL-CORRUPTION"));
        assert!(!error.to_string().contains("table=users"));
    }

    #[test]
    fn is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<PlomidError>();
    }
}
