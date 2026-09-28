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
//! PostgreSQL frontend/backend wire protocol message definitions for PLOMID.
//!
//! This module implements the minimum subset of the PostgreSQL protocol
//! required for standard clients (such as DBeaver and psql) to connect,
//! authenticate, and execute SQL statements.
//!
//! Supported message flow:
//! - Startup (client -> server)
//! - SSL request (client -> server, denied)
//! - Authentication (server -> client)
//! - Password (client -> server)
//! - ParameterStatus (server -> client)
//! - BackendKeyData (server -> client)
//! - Query (client -> server)
//! - RowDescription (server -> client)
//! - DataRow (server -> client)
//! - CommandComplete (server -> client)
//! - ReadyForQuery (server -> client)
//! - ErrorResponse (server -> client)
//! - NoticeResponse (server -> client)
//! - Terminate (client -> server)
//!
//! The extended query protocol supports the basic Parse/Bind/Describe/Execute/
//! Sync lifecycle for clients that use prepared statements.

use std::fmt;

/// Message type tag used on the wire.
///
/// Each backend message on the PostgreSQL wire begins with a single ASCII
/// byte identifying the message type, followed by a 32-bit length, followed
/// by the payload. The length includes the 4-byte length field itself but
/// excludes the 1-byte type tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageTag {
    Authentication = b'R',
    BackendKeyData = b'K',
    BindComplete = b'2',
    CloseComplete = b'3',
    CommandComplete = b'C',
    CopyBothResponse = b'W',
    CopyData = b'd',
    CopyDone = b'c',
    CopyOutResponse = b'H',
    DataRow = b'D',
    EmptyQueryResponse = b'I',
    ErrorResponse = b'E',
    FunctionCallResponse = b'V',
    NoticeResponse = b'N',
    NoData = b'n',
    NotificationResponse = b'A',
    ParameterDescription = b't',
    ParameterStatus = b'S',
    ParseComplete = b'1',
    PortalSuspended = b's',
    ReadyForQuery = b'Z',
    RowDescription = b'T',
    Terminate = b'X',
    Query = b'Q',
    Password = b'p',
    CopyInResponse = b'G',
}

/// Frontend-only message tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontendTag {
    Query,
    Password,
    Terminate,
    Parse,
    Bind,
    Describe,
    Execute,
    Sync,
    Flush,
    Close,
    FunctionCall,
    CopyData,
    CopyDone,
    CopyFail,
}

impl FrontendTag {
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'Q' => Some(Self::Query),
            b'p' => Some(Self::Password),
            b'X' => Some(Self::Terminate),
            b'P' => Some(Self::Parse),
            b'B' => Some(Self::Bind),
            b'D' => Some(Self::Describe),
            b'E' => Some(Self::Execute),
            b'S' => Some(Self::Sync),
            b'H' => Some(Self::Flush),
            b'C' => Some(Self::Close),
            b'F' => Some(Self::FunctionCall),
            b'd' => Some(Self::CopyData),
            b'c' => Some(Self::CopyDone),
            b'f' => Some(Self::CopyFail),
            _ => None,
        }
    }
}

impl MessageTag {
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'R' => Some(Self::Authentication),
            b'K' => Some(Self::BackendKeyData),
            b'1' => Some(Self::ParseComplete),
            b'2' => Some(Self::BindComplete),
            b'3' => Some(Self::CloseComplete),
            b'C' => Some(Self::CommandComplete),
            b'H' => Some(Self::CopyOutResponse),
            b'I' => Some(Self::EmptyQueryResponse),
            b'D' => Some(Self::DataRow),
            b'E' => Some(Self::ErrorResponse),
            b'V' => Some(Self::FunctionCallResponse),
            b'N' => Some(Self::NoticeResponse),
            b'A' => Some(Self::NotificationResponse),
            b'n' => Some(Self::NoData),
            b't' => Some(Self::ParameterDescription),
            b's' => Some(Self::PortalSuspended),
            b'S' => Some(Self::ParameterStatus),
            b'Z' => Some(Self::ReadyForQuery),
            b'T' => Some(Self::RowDescription),
            b'X' => Some(Self::Terminate),
            b'Q' => Some(Self::Query),
            b'p' => Some(Self::Password),
            _ => None,
        }
    }
}

impl fmt::Display for MessageTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", *self as u8 as char)
    }
}

/// A client-to-server query string message.
///
/// Carries a single SQL statement string encoded as UTF-8. This is the
/// primary input path for the simple query protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryMessage {
    pub sql: String,
}

/// Server response to a startup request.
///
/// Indicates whether the server requires authentication and, if so, which
/// mechanism to use. V1 supports only clear-text password authentication
/// for development use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationRequest {
    Ok,
    CleartextPassword,
}

/// Server-to-client session parameters.
///
/// These parameters are part of the standard PostgreSQL handshake and are
/// used by clients to configure session state. Only a minimal set is
/// provided in V1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterStatus {
    pub name: String,
    pub value: String,
}

/// Server-to-client connection identity.
///
/// Provides the backend process ID and secret key used to tag messages
/// in the extended protocol. In V1 these values are fixed placeholders
/// because the extended protocol is not implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendKeyData {
    pub process_id: u32,
    pub secret_key: u32,
}

/// Transaction status indicator sent in ReadyForQuery messages.
///
/// Tracks whether the session is currently in an idle state, inside a
/// transaction block, or inside a failed transaction block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    Idle,
    InTransaction,
    InFailedTransaction,
}

impl TransactionStatus {
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Idle => b'I',
            Self::InTransaction => b'T',
            Self::InFailedTransaction => b'E',
        }
    }
}

impl fmt::Display for TransactionStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.as_byte() as char)
    }
}

/// Metadata for a result set column.
///
/// Describes the fields required by PostgreSQL RowDescription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowFieldDescription {
    pub name: String,
    pub type_oid: u32,
    pub type_size: i16,
    pub type_modifier: i32,
    pub format: u16,
}

/// A single row of result data.
///
/// Values are encoded as byte strings following the PostgreSQL wire format
/// for text-format results. NULL values are represented by setting the
/// length field to -1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRowMessage {
    pub values: Vec<Option<Vec<u8>>>,
}

/// Command completion indicator.
///
/// Reports the tag of the completed command (e.g., "SELECT 1", "INSERT 0 1",
/// "CREATE TABLE", "BEGIN", "COMMIT", "ROLLBACK") so clients can display
/// row counts and command names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCompleteMessage {
    pub tag: String,
}

/// Server-side error or notice information.
///
/// Carries structured fields including the SQLSTATE error code, a
/// human-readable message, detail, hint, and position. Fields are encoded
/// as a sequence of key-value pairs on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorResponse {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<u32>,
    pub internal_query: Option<String>,
    pub internal_position: Option<u32>,
    pub context: Option<String>,
    pub schema_name: Option<String>,
    pub table_name: Option<String>,
    pub column_name: Option<String>,
    pub data_type_name: Option<String>,
    pub constraint_name: Option<String>,
    pub source_file: Option<String>,
    pub source_line: Option<u32>,
    pub source_function: Option<String>,
}

impl Default for ErrorResponse {
    fn default() -> Self {
        Self {
            severity: "ERROR".to_string(),
            code: String::new(),
            message: String::new(),
            detail: None,
            hint: None,
            position: None,
            internal_query: None,
            internal_position: None,
            context: None,
            schema_name: None,
            table_name: None,
            column_name: None,
            data_type_name: None,
            constraint_name: None,
            source_file: None,
            source_line: None,
            source_function: None,
        }
    }
}

impl ErrorResponse {
    pub fn new(
        severity: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: severity.into(),
            code: code.into(),
            message: message.into(),
            ..Self::default()
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_position(mut self, position: u32) -> Self {
        self.position = Some(position);
        self
    }
}

/// Authentication mechanism indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMechanism {
    Ok = 0,
    CleartextPassword = 3,
    Md5Password = 5,
    Sasl = 10,
    SaslContinue = 11,
    SaslFinal = 12,
}
