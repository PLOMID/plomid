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
//! Session state management for a single PLOMID client connection.
//!
//! A session tracks the authenticated identity, transaction state, and
//! execution context for one client connection. The server creates one
//! session per accepted TCP connection.
//!
//! Sessions are not shared between connections. All state in this module
//! is local to a single connection and its associated SQL execution.

use crate::protocol::TransactionStatus;
use std::collections::HashMap;
use std::sync::Arc;

/// Session identity and state for a single client connection.
pub struct Session {
    pub id: u64,
    pub user: String,
    pub database: String,
    pub transaction_status: TransactionStatus,
    pub parameters: HashMap<String, String>,
    /// Statements staged by an explicit session transaction. They are sent
    /// to the existing executor as one transaction at COMMIT, so no
    /// uncommitted page changes reach storage in V1.
    pub pending_statements: Vec<String>,
    pub savepoints: Vec<(String, usize)>,
}

impl Session {
    pub fn new(id: u64, user: impl Into<String>, database: impl Into<String>) -> Self {
        let mut parameters = HashMap::new();
        parameters.insert("server_version".to_string(), "17.0".to_string());
        parameters.insert("server_version_num".to_string(), "170000".to_string());
        parameters.insert("server_encoding".to_string(), "UTF8".to_string());
        parameters.insert("client_encoding".to_string(), "UTF8".to_string());
        parameters.insert("application_name".to_string(), "plomid".to_string());
        parameters.insert("DateStyle".to_string(), "ISO, MDY".to_string());
        parameters.insert("IntervalStyle".to_string(), "postgres".to_string());
        parameters.insert("TimeZone".to_string(), "UTC".to_string());
        parameters.insert("integer_datetimes".to_string(), "on".to_string());
        parameters.insert("standard_conforming_strings".to_string(), "on".to_string());
        parameters.insert("search_path".to_string(), "\"$user\", public".to_string());
        parameters.insert("datestyle".to_string(), "ISO".to_string());
        parameters.insert("isolation_level".to_string(), "read committed".to_string());
        let user = user.into();
        let database = database.into();
        parameters.insert("user".to_string(), user.clone());
        parameters.insert("database".to_string(), database.clone());
        Self {
            id,
            user,
            database,
            transaction_status: TransactionStatus::Idle,
            parameters,
            pending_statements: Vec::new(),
            savepoints: Vec::new(),
        }
    }

    pub fn set_parameter(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.parameters.insert(name.into(), value.into());
    }

    pub fn get_parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .get(name)
            .or_else(|| {
                self.parameters
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value)
            })
            .map(|s| s.as_str())
    }

    pub fn mark_in_transaction(&mut self) {
        self.transaction_status = TransactionStatus::InTransaction;
        self.pending_statements.clear();
        self.savepoints.clear();
    }

    pub fn mark_idle(&mut self) {
        self.transaction_status = TransactionStatus::Idle;
        self.pending_statements.clear();
        self.savepoints.clear();
    }

    pub fn mark_failed_transaction(&mut self) {
        self.transaction_status = TransactionStatus::InFailedTransaction;
    }

    pub fn is_in_transaction(&self) -> bool {
        self.transaction_status == TransactionStatus::InTransaction
    }

    pub fn stage_statement(&mut self, sql: impl Into<String>) {
        self.pending_statements.push(sql.into());
    }

    pub fn savepoint(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.savepoints
            .retain(|(saved, _)| !saved.eq_ignore_ascii_case(&name));
        self.savepoints.push((name, self.pending_statements.len()));
    }

    pub fn rollback_to_savepoint(&mut self, name: &str) -> bool {
        let Some((_, marker)) = self
            .savepoints
            .iter()
            .rev()
            .find(|(saved, _)| saved.eq_ignore_ascii_case(name))
            .cloned()
        else {
            return false;
        };
        self.pending_statements.truncate(marker);
        self.savepoints
            .retain(|(_, saved_marker)| *saved_marker <= marker);
        true
    }

    pub fn release_savepoint(&mut self, name: &str) -> bool {
        if let Some(index) = self
            .savepoints
            .iter()
            .rposition(|(saved, _)| saved.eq_ignore_ascii_case(name))
        {
            self.savepoints.remove(index);
            true
        } else {
            false
        }
    }

    pub fn take_staged_statements(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_statements)
    }

    pub fn restore_staged_statements(&mut self, statements: Vec<String>) {
        self.pending_statements = statements;
    }
}

/// Server-side authentication configuration.
///
/// V1 supports a single username and password pair for all connections.
/// Authentication can be disabled entirely for local development. Future
/// versions will support per-user credentials, roles, and external
/// identity providers.
#[derive(Debug, Clone)]
pub struct ServerAuthConfig {
    pub username: String,
    pub password: String,
    pub enabled: bool,
    pub method: AuthMethod,
    pub(crate) scram_salt: [u8; 16],
    pub(crate) scram_salted_password: Arc<[u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Cleartext,
    Md5,
    Scram,
}

impl ServerAuthConfig {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        let password = password.into();
        Self {
            username: username.into(),
            scram_salt: scram_salt(&password),
            scram_salted_password: Arc::from(crate::scram::pbkdf2_sha256(
                password.as_bytes(),
                &scram_salt(&password),
                4096,
                32,
            )),
            password,
            enabled: true,
            method: AuthMethod::Cleartext,
        }
    }

    pub fn md5(username: impl Into<String>, password: impl Into<String>) -> Self {
        let password = password.into();
        Self {
            username: username.into(),
            scram_salt: scram_salt(&password),
            scram_salted_password: Arc::from(crate::scram::pbkdf2_sha256(
                password.as_bytes(),
                &scram_salt(&password),
                4096,
                32,
            )),
            password,
            enabled: true,
            method: AuthMethod::Md5,
        }
    }

    pub fn scram(username: impl Into<String>, password: impl Into<String>) -> Self {
        let password = password.into();
        let salt = scram_salt(&password);
        Self {
            username: username.into(),
            scram_salt: salt,
            scram_salted_password: Arc::from(crate::scram::pbkdf2_sha256(
                password.as_bytes(),
                &salt,
                4096,
                32,
            )),
            password,
            enabled: true,
            method: AuthMethod::Scram,
        }
    }

    pub fn disabled() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            enabled: false,
            method: AuthMethod::Cleartext,
            scram_salt: [0; 16],
            scram_salted_password: Arc::from(Vec::<u8>::new()),
        }
    }

    pub fn authenticate(&self, user: &str, password: &str) -> bool {
        if !self.enabled {
            return true;
        }
        self.username == user && self.password == password
    }

    pub fn authenticate_md5(&self, user: &str, response: &str, salt: [u8; 4]) -> bool {
        if !self.enabled {
            return true;
        }
        let inner = md5_hex(format!("{}{}", self.password, user).as_bytes());
        let mut payload = inner.into_bytes();
        payload.extend_from_slice(&salt);
        response.eq_ignore_ascii_case(&format!("md5{}", md5_hex(&payload))) && user == self.username
    }
}

fn scram_salt(password: &str) -> [u8; 16] {
    let digest = crate::scram::sha256(password.as_bytes());
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&digest[..16]);
    salt
}

fn md5_hex(bytes: &[u8]) -> String {
    let digest = md5(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn md5(input: &[u8]) -> [u8; 16] {
    let mut message = input.to_vec();
    let bit_len = (message.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());
    let mut state = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    let shifts = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let constants: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    for chunk in message.chunks_exact(64) {
        let mut words = [0u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            *word = u32::from_le_bytes(chunk[index * 4..index * 4 + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (state[0], state[1], state[2], state[3]);
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | (!b & d), i)
            } else if i < 32 {
                ((d & b) | (!d & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | !d), (7 * i) % 16)
            };
            let temp = a
                .wrapping_add(f)
                .wrapping_add(constants[i])
                .wrapping_add(words[g])
                .rotate_left(shifts[(i / 16) * 4 + i % 4]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(temp);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }
    let mut output = [0u8; 16];
    for (index, word) in state.iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    output
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn md5_authentication_matches_postgres_response_shape() {
        let config = ServerAuthConfig::md5("alice", "secret");
        let salt = [1, 2, 3, 4];
        let inner = md5_hex(b"secretalice");
        let mut payload = inner.into_bytes();
        payload.extend_from_slice(&salt);
        let response = format!("md5{}", md5_hex(&payload));
        assert!(config.authenticate_md5("alice", &response, salt));
        assert!(!config.authenticate_md5("alice", "md5bad", salt));
    }
}
