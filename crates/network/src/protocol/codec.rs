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
//! PostgreSQL wire protocol encoder and decoder.
//!
//! This module handles the byte-level encoding and decoding of protocol
//! messages. It reads from an async byte stream and produces typed message
//! structs, and vice versa.
//!
//! # Wire Format
//!
//! Every backend message has the form:
//!
//! ```text
//! byte1: message type tag
//! byte2..byte5: message length (Int32, including length field, excluding type tag)
//! byte6..: message payload
//! ```
//!
//! Frontend startup messages omit the type tag and begin directly with the
//! 4-byte length.
//!
//! # Text vs Binary
//!
//! V1 uses text-format values for all data fields. Binary column format is
//! not implemented.

use crate::protocol::{
    AuthMechanism, ErrorResponse, FrontendTag, MessageTag, RowFieldDescription, TransactionStatus,
};

use bytes::{BufMut, BytesMut};
use std::io;

/// Reads a single PostgreSQL wire message from a byte stream.
///
/// Returns the message type tag and the complete payload (including the
/// 4-byte length prefix for frontend messages). Returns `None` if the
/// stream is closed.
pub struct MessageDecoder;

/// Maximum frontend message payload accepted by the protocol reader.
///
/// PostgreSQL allows very large protocol messages, but allocating an
/// attacker-controlled vector before authentication would let a client
/// Defined once in `plomid_core::constants`.
pub use plomid_core::MAX_FRONTEND_MESSAGE_SIZE;

impl MessageDecoder {
    /// Reads the next frontend message from the reader.
    ///
    /// Frontend messages (except StartupMessage and SSLRequest) begin with
    /// a 1-byte tag followed by a 4-byte length. StartupMessage begins
    /// directly with the length.
    pub async fn read_frontend<R>(reader: &mut R) -> io::Result<Option<(FrontendTag, Vec<u8>)>>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut hdr = [0u8; 5];
        match tokio::io::AsyncReadExt::read_exact(reader, &mut hdr).await {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let tag = FrontendTag::from_byte(hdr[0]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown frontend message tag: 0x{:02X}", hdr[0]),
            )
        })?;

        let len = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        if len < 4 || len - 4 > MAX_FRONTEND_MESSAGE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid or oversized message length: {len}"),
            ));
        }
        let payload_len = len - 4;
        let mut payload = vec![0u8; payload_len];
        tokio::io::AsyncReadExt::read_exact(reader, &mut payload)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Some((tag, payload)))
    }

    /// Reads a frontend StartupMessage or SSLRequest.
    ///
    /// These messages do not have a type tag; they begin directly with the
    /// 4-byte length followed by the protocol version number or SSL
    /// request code.
    pub async fn read_startup<R>(reader: &mut R) -> io::Result<Option<Vec<u8>>>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut len_buf = [0u8; 4];
        match tokio::io::AsyncReadExt::read_exact(reader, &mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len < 4 || len - 4 > MAX_FRONTEND_MESSAGE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid or oversized startup length: {len}"),
            ));
        }
        let mut payload = vec![0u8; len - 4];
        tokio::io::AsyncReadExt::read_exact(reader, &mut payload)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Some(payload))
    }

    /// Parses a StartupMessage payload.
    ///
    /// Startup payloads are a sequence of null-terminated key-value pairs
    /// followed by a final null terminator. The first 4 bytes are the
    /// protocol version number.
    pub fn parse_startup(payload: &[u8]) -> io::Result<std::collections::HashMap<String, String>> {
        if payload.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "startup payload too short",
            ));
        }
        let mut params = std::collections::HashMap::new();
        let protocol_version = u32::from_be_bytes(payload[..4].try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid startup protocol version",
            )
        })?);
        if protocol_version != 196608 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported startup protocol version: {protocol_version}"),
            ));
        }
        let mut pos = 4;
        while pos < payload.len() {
            let mut end = pos;
            while end < payload.len() && payload[end] != 0 {
                end += 1;
            }
            if end >= payload.len() {
                break;
            }
            let key = std::str::from_utf8(&payload[pos..end])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            pos = end + 1;
            if pos >= payload.len() {
                break;
            }
            let mut end = pos;
            while end < payload.len() && payload[end] != 0 {
                end += 1;
            }
            let value = std::str::from_utf8(&payload[pos..end])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            pos = end + 1;
            if !key.is_empty() {
                params.insert(key.to_string(), value.to_string());
            }
            if key.is_empty() && value.is_empty() {
                break;
            }
        }
        Ok(params)
    }
}

/// Writes PostgreSQL backend messages to a byte stream.
pub struct MessageEncoder;

impl MessageEncoder {
    /// Encodes a backend message with the given tag and payload.
    ///
    /// The encoded message has the form: tag (1 byte) + length (4 bytes,
    /// including length field) + payload.
    pub fn encode(tag: MessageTag, payload: &[u8]) -> Vec<u8> {
        let len = (payload.len() + 4) as u32;
        let mut buf = BytesMut::with_capacity(1 + 4 + payload.len());
        buf.put_u8(tag as u8);
        buf.put_u32(len);
        buf.put_slice(payload);
        buf.to_vec()
    }

    /// Encodes an Authentication request message.
    pub fn encode_authentication(mechanism: AuthMechanism) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u32(mechanism as u32);
        Self::encode(MessageTag::Authentication, &payload)
    }

    pub fn encode_authentication_md5(salt: [u8; 4]) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u32(AuthMechanism::Md5Password as u32);
        payload.put_slice(&salt);
        Self::encode(MessageTag::Authentication, &payload)
    }

    /// Encodes an AuthenticationSASL request advertising the given mechanism
    /// names (e.g. `SCRAM-SHA-256`).
    pub fn encode_authentication_sasl(mechanisms: &[&str]) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u32(AuthMechanism::Sasl as u32);
        for mechanism in mechanisms {
            payload.put_slice(mechanism.as_bytes());
            payload.put_u8(0);
        }
        payload.put_u8(0);
        Self::encode(MessageTag::Authentication, &payload)
    }

    /// Encodes an AuthenticationSASLContinue message carrying the server-first
    /// SCRAM data.
    pub fn encode_authentication_sasl_continue(data: &str) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u32(AuthMechanism::SaslContinue as u32);
        payload.put_slice(data.as_bytes());
        Self::encode(MessageTag::Authentication, &payload)
    }

    /// Encodes an AuthenticationSASLFinal message carrying the server-final
    /// SCRAM data (the `v=...` verifier).
    pub fn encode_authentication_sasl_final(data: &str) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u32(AuthMechanism::SaslFinal as u32);
        payload.put_slice(data.as_bytes());
        Self::encode(MessageTag::Authentication, &payload)
    }

    /// Encodes a ParameterStatus message.
    pub fn encode_parameter_status(name: &str, value: &str) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_slice(name.as_bytes());
        payload.put_u8(0);
        payload.put_slice(value.as_bytes());
        payload.put_u8(0);
        Self::encode(MessageTag::ParameterStatus, &payload)
    }

    /// Encodes a BackendKeyData message.
    pub fn encode_backend_key_data(process_id: u32, secret_key: u32) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u32(process_id);
        payload.put_u32(secret_key);
        Self::encode(MessageTag::BackendKeyData, &payload)
    }

    /// Encodes a ReadyForQuery message.
    pub fn encode_ready_for_query(status: TransactionStatus) -> Vec<u8> {
        let payload = [status.as_byte()];
        Self::encode(MessageTag::ReadyForQuery, &payload)
    }

    /// Starts a text-format `COPY FROM STDIN` stream. The zero column count
    /// asks the client to use the table's declared columns.
    pub fn encode_copy_in_response() -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u8(0);
        payload.put_u16(0);
        Self::encode(MessageTag::CopyInResponse, &payload)
    }

    /// Starts a text-format `COPY TO STDOUT` stream.
    pub fn encode_copy_out_response() -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u8(0);
        payload.put_u16(0);
        Self::encode(MessageTag::CopyOutResponse, &payload)
    }

    pub fn encode_copy_data(data: &[u8]) -> Vec<u8> {
        Self::encode(MessageTag::CopyData, data)
    }

    pub fn encode_copy_done() -> Vec<u8> {
        Self::encode(MessageTag::CopyDone, &[])
    }

    /// Encodes a RowDescription message.
    ///
    /// The field descriptions follow the PostgreSQL wire format: field name
    /// (null-terminated), table OID (4 bytes), column attribute number
    /// (2 bytes), type OID (4 bytes), type size (2 bytes), type modifier
    /// (4 bytes), format (2 bytes). In V1 the table and column identifiers
    /// are set to zero because the mapping from PLOMID table names to
    /// PostgreSQL OIDs is not yet implemented.
    pub fn encode_row_description(fields: &[RowFieldDescription]) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u16(fields.len() as u16);
        for field in fields {
            payload.put_slice(field.name.as_bytes());
            payload.put_u8(0);
            payload.put_u32(0);
            payload.put_u16(0);
            payload.put_u32(field.type_oid);
            payload.put_i16(field.type_size);
            payload.put_i32(field.type_modifier);
            payload.put_u16(field.format);
        }
        Self::encode(MessageTag::RowDescription, &payload)
    }

    /// Encodes a DataRow message.
    ///
    /// Each column value is encoded as a 4-byte length followed by the
    /// value bytes. NULL values are encoded as a length of -1 (0xFFFFFFFF).
    pub fn encode_data_row(values: &[Option<Vec<u8>>]) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u16(values.len() as u16);
        for value in values {
            match value {
                Some(bytes) => {
                    payload.put_i32(bytes.len() as i32);
                    payload.put_slice(bytes);
                }
                None => {
                    payload.put_i32(-1);
                }
            }
        }
        Self::encode(MessageTag::DataRow, &payload)
    }

    /// Encodes a CommandComplete message.
    pub fn encode_command_complete(tag: &str) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_slice(tag.as_bytes());
        payload.put_u8(0);
        Self::encode(MessageTag::CommandComplete, &payload)
    }

    /// Encodes an ErrorResponse message.
    ///
    /// Fields are encoded as a sequence of type-tagged, null-terminated
    /// strings. The message is terminated by a 0x00 byte. Fields that are
    /// not set are omitted.
    pub fn encode_error_response(error: &ErrorResponse) -> Vec<u8> {
        let mut payload = BytesMut::new();
        if !error.severity.is_empty() {
            payload.put_u8(b'S');
            payload.put_slice(error.severity.as_bytes());
            payload.put_u8(0);
        }
        if !error.code.is_empty() {
            payload.put_u8(b'C');
            payload.put_slice(error.code.as_bytes());
            payload.put_u8(0);
        }
        if !error.message.is_empty() {
            payload.put_u8(b'M');
            payload.put_slice(error.message.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref detail) = error.detail {
            payload.put_u8(b'D');
            payload.put_slice(detail.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref hint) = error.hint {
            payload.put_u8(b'H');
            payload.put_slice(hint.as_bytes());
            payload.put_u8(0);
        }
        if let Some(position) = error.position {
            payload.put_u8(b'P');
            let s = position.to_string();
            payload.put_slice(s.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref internal_query) = error.internal_query {
            payload.put_u8(b'q');
            payload.put_slice(internal_query.as_bytes());
            payload.put_u8(0);
        }
        if let Some(internal_position) = error.internal_position {
            payload.put_u8(b'p');
            let s = internal_position.to_string();
            payload.put_slice(s.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref context) = error.context {
            payload.put_u8(b'W');
            payload.put_slice(context.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref schema_name) = error.schema_name {
            payload.put_u8(b'n');
            payload.put_slice(schema_name.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref table_name) = error.table_name {
            payload.put_u8(b't');
            payload.put_slice(table_name.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref column_name) = error.column_name {
            payload.put_u8(b'c');
            payload.put_slice(column_name.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref data_type_name) = error.data_type_name {
            payload.put_u8(b'T');
            payload.put_slice(data_type_name.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref constraint_name) = error.constraint_name {
            payload.put_u8(b'd');
            payload.put_slice(constraint_name.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref source_file) = error.source_file {
            payload.put_u8(b'F');
            payload.put_slice(source_file.as_bytes());
            payload.put_u8(0);
        }
        if let Some(source_line) = error.source_line {
            payload.put_u8(b'L');
            let s = source_line.to_string();
            payload.put_slice(s.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref source_function) = error.source_function {
            payload.put_u8(b'R');
            payload.put_slice(source_function.as_bytes());
            payload.put_u8(0);
        }
        payload.put_u8(0);
        Self::encode(MessageTag::ErrorResponse, &payload)
    }

    /// Encodes a NoticeResponse message.
    ///
    /// NoticeResponse uses the same field encoding as ErrorResponse but
    /// conveys non-fatal warnings and informational messages.
    pub fn encode_notice_response(error: &ErrorResponse) -> Vec<u8> {
        let mut payload = BytesMut::new();
        if !error.severity.is_empty() {
            payload.put_u8(b'S');
            payload.put_slice(error.severity.as_bytes());
            payload.put_u8(0);
        }
        if !error.code.is_empty() {
            payload.put_u8(b'C');
            payload.put_slice(error.code.as_bytes());
            payload.put_u8(0);
        }
        if !error.message.is_empty() {
            payload.put_u8(b'M');
            payload.put_slice(error.message.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref detail) = error.detail {
            payload.put_u8(b'D');
            payload.put_slice(detail.as_bytes());
            payload.put_u8(0);
        }
        if let Some(ref hint) = error.hint {
            payload.put_u8(b'H');
            payload.put_slice(hint.as_bytes());
            payload.put_u8(0);
        }
        payload.put_u8(0);
        Self::encode(MessageTag::NoticeResponse, &payload)
    }

    /// Encodes a Terminate message.
    ///
    /// The Terminate message is sent by the client to close the connection.
    /// The server does not send this message; it simply closes the TCP
    /// connection after processing any pending work.
    pub fn encode_terminate() -> Vec<u8> {
        Self::encode(MessageTag::Terminate, &[])
    }

    /// Encodes a PasswordMessage from the client.
    ///
    /// Carries the password string as a null-terminated UTF-8 payload.
    pub fn encode_password_message(password: &str) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_slice(password.as_bytes());
        payload.put_u8(0);
        Self::encode(MessageTag::Password, &payload)
    }

    /// Encodes an extended-protocol completion message.
    pub fn encode_completion(tag: u8) -> Vec<u8> {
        Self::encode_raw(tag, &[])
    }

    pub fn encode_parse_complete() -> Vec<u8> {
        Self::encode(MessageTag::ParseComplete, &[])
    }

    pub fn encode_bind_complete() -> Vec<u8> {
        Self::encode(MessageTag::BindComplete, &[])
    }

    /// Encodes an extended-protocol NoData response.
    pub fn encode_no_data() -> Vec<u8> {
        Self::encode(MessageTag::NoData, &[])
    }

    pub fn encode_parameter_description(type_oids: &[u32]) -> Vec<u8> {
        let mut payload = BytesMut::new();
        payload.put_u16(type_oids.len() as u16);
        for oid in type_oids {
            payload.put_u32(*oid);
        }
        Self::encode(MessageTag::ParameterDescription, &payload)
    }

    pub fn encode_empty_query() -> Vec<u8> {
        Self::encode(MessageTag::EmptyQueryResponse, &[])
    }

    pub fn encode_close_complete() -> Vec<u8> {
        Self::encode(MessageTag::CloseComplete, &[])
    }

    fn encode_raw(tag: u8, payload: &[u8]) -> Vec<u8> {
        let len = (payload.len() + 4) as u32;
        let mut buf = BytesMut::with_capacity(1 + 4 + payload.len());
        buf.put_u8(tag);
        buf.put_u32(len);
        buf.put_slice(payload);
        buf.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_authentication_ok_has_correct_tag_and_length() {
        let data = MessageEncoder::encode_authentication(AuthMechanism::Ok);
        assert_eq!(data[0], b'R');
        let len = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        assert_eq!(len, 8);
        assert_eq!(data[5..9], [0, 0, 0, 0]);
    }

    #[test]
    fn encode_authentication_cleartext_has_correct_tag_and_length() {
        let data = MessageEncoder::encode_authentication(AuthMechanism::CleartextPassword);
        assert_eq!(data[0], b'R');
        let len = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        assert_eq!(len, 8);
        assert_eq!(data[5..9], [0, 0, 0, 3]);
    }

    #[test]
    fn encode_ready_for_query_idle() {
        let data = MessageEncoder::encode_ready_for_query(TransactionStatus::Idle);
        assert_eq!(data[0], b'Z');
        let len = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        assert_eq!(len, 5);
        assert_eq!(data[5], b'I');
    }

    #[test]
    fn encode_ready_for_query_in_transaction() {
        let data = MessageEncoder::encode_ready_for_query(TransactionStatus::InTransaction);
        assert_eq!(data[5], b'T');
    }

    #[test]
    fn encode_command_complete() {
        let data = MessageEncoder::encode_command_complete("CREATE TABLE");
        assert_eq!(data[0], b'C');
        let payload = &data[5..];
        assert!(payload.starts_with(b"CREATE TABLE\0"));
    }

    #[test]
    fn encode_error_response_has_required_fields() {
        let error = ErrorResponse::new("ERROR", "42P01", "relation \"missing\" does not exist");
        let data = MessageEncoder::encode_error_response(&error);
        assert_eq!(data[0], b'E');
        let payload_str = std::str::from_utf8(&data[5..]).unwrap();
        assert!(payload_str.contains("SERROR"));
        assert!(payload_str.contains("C42P01"));
        assert!(payload_str.contains("Mrelation \"missing\" does not exist"));
    }

    #[test]
    fn encode_error_response_with_detail_and_hint() {
        let error = ErrorResponse::new("ERROR", "42601", "syntax error")
            .with_detail("missing semicolon")
            .with_hint("add a semicolon at the end");
        let data = MessageEncoder::encode_error_response(&error);
        let payload_str = std::str::from_utf8(&data[5..]).unwrap();
        assert!(payload_str.contains("Dmissing semicolon"));
        assert!(payload_str.contains("Hadd a semicolon at the end"));
    }

    #[test]
    fn encode_row_description_maps_columns() {
        let fields = vec![
            RowFieldDescription {
                name: "id".to_string(),
                type_oid: 23,
                type_size: 4,
                type_modifier: -1,
                format: 0,
            },
            RowFieldDescription {
                name: "name".to_string(),
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format: 0,
            },
        ];
        let data = MessageEncoder::encode_row_description(&fields);
        assert_eq!(data[0], b'T');
        let field_count = u16::from_be_bytes([data[5], data[6]]);
        assert_eq!(field_count, 2);
    }

    #[test]
    fn encode_data_row_handles_null_and_values() {
        let values = vec![Some(b"1".to_vec()), None, Some(b"Alice".to_vec())];
        let data = MessageEncoder::encode_data_row(&values);
        assert_eq!(data[0], b'D');
        let col_count = u16::from_be_bytes([data[5], data[6]]);
        assert_eq!(col_count, 3);
    }

    #[test]
    fn encode_parameter_status() {
        let data = MessageEncoder::encode_parameter_status("server_version", "14.0");
        assert_eq!(data[0], b'S');
        let payload = std::str::from_utf8(&data[5..]).unwrap();
        assert!(payload.starts_with("server_version\0"));
        assert!(payload.ends_with("14.0\0"));
    }

    #[test]
    fn encode_backend_key_data() {
        let data = MessageEncoder::encode_backend_key_data(12345, 67890);
        assert_eq!(data[0], b'K');
        let pid = u32::from_be_bytes([data[5], data[6], data[7], data[8]]);
        assert_eq!(pid, 12345);
        let key = u32::from_be_bytes([data[9], data[10], data[11], data[12]]);
        assert_eq!(key, 67890);
    }

    #[test]
    fn parse_startup_extracts_parameters() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&196608u32.to_be_bytes());
        payload.extend_from_slice(b"user\0plomid\0database\0mydb\0\0");
        let params = MessageDecoder::parse_startup(&payload).unwrap();
        assert_eq!(params.get("user"), Some(&"plomid".to_string()));
        assert_eq!(params.get("database"), Some(&"mydb".to_string()));
    }

    #[test]
    fn parse_startup_handles_empty_payload() {
        let result = MessageDecoder::parse_startup(&[]);
        assert!(result.is_err(), "empty payload must be rejected");
    }

    #[test]
    fn parse_startup_returns_error_for_short_payload() {
        let result = MessageDecoder::parse_startup(&[1, 2, 3]);
        assert!(
            result.is_err(),
            "payload shorter than version must be rejected"
        );
    }
}
