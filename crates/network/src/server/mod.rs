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
//! TCP connection handling and server lifecycle for PLOMID.
//!
//! [`PlomidServer`] owns the TCP listener and dispatches each accepted
//! connection onto a dedicated async task. The per-connection PostgreSQL
//! frontend/backend message flow lives in [`handler::ConnectionHandler`],
//! while multi-database routing state is encapsulated by
//! [`registry::DatabaseRegistry`].

mod handler;
mod registry;

use crate::session::ServerAuthConfig;
use plomid_txn::PlomidStorageEngine;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tracing::{info, info_span, warn, Instrument};

pub use handler::ConnectionHandler;

/// A running PLOMID server instance.
pub struct PlomidServer {
    listener: tokio::net::TcpListener,
    auth_config: ServerAuthConfig,
    process_id: u32,
    secret_key: u32,
    connection_counter: Arc<AtomicU64>,
    query_counter: Arc<AtomicU64>,
    connection_permits: Arc<Semaphore>,
    /// Connections currently being served. A graceful shutdown waits for this to
    /// reach zero (bounded) so in-flight statements finish before the final
    /// checkpoint is taken, instead of being cut off mid-commit.
    live_connections: Arc<AtomicU64>,
    /// Background maintenance endpoint handed to every session. When absent
    /// (tests, embedded use), sessions run maintenance passes inline.
    maintenance_link: Option<crate::WorkerLink>,
}

/// Admission limits for client work. Idle connections consume only a Tokio
/// task, protocol buffers, and this permit; database execution state is
/// created after authentication and is not held by the listener.
#[derive(Clone, Copy, Debug)]
pub struct ConnectionLimits {
    pub max_connections: usize,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        // A bounded, deployable default. Operators can choose a lower value
        // through `with_connection_limit` rather than risking unbounded task
        // and socket memory growth.
        Self {
            max_connections: 100_000,
        }
    }
}

impl PlomidServer {
    pub async fn bind(addr: SocketAddr, auth_config: ServerAuthConfig) -> std::io::Result<Self> {
        Self::bind_with_limits(addr, auth_config, ConnectionLimits::default()).await
    }

    pub async fn bind_with_limits(
        addr: SocketAddr,
        auth_config: ServerAuthConfig,
        limits: ConnectionLimits,
    ) -> std::io::Result<Self> {
        if limits.max_connections == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "max_connections must be greater than zero",
            ));
        }
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        info!(target: "server", "listening on {}", local_addr);
        let process_id = 0x454E50u32;
        let secret_key = {
            use std::time::{SystemTime, UNIX_EPOCH};
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time is before UNIX_EPOCH");
            (now.as_secs() ^ (now.subsec_nanos() as u64)) as u32
        };
        Ok(Self {
            listener,
            auth_config,
            process_id,
            secret_key,
            connection_counter: Arc::new(AtomicU64::new(0)),
            query_counter: Arc::new(AtomicU64::new(0)),
            connection_permits: Arc::new(Semaphore::new(limits.max_connections)),
            live_connections: Arc::new(AtomicU64::new(0)),
            maintenance_link: None,
        })
    }

    /// Installs the background maintenance endpoint for all sessions.
    /// Connections created afterwards submit due tables to the worker instead
    /// of running passes on the committing connection.
    pub fn set_maintenance_link(&mut self, link: crate::WorkerLink) {
        self.maintenance_link = Some(link);
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .expect("listener must be bound before local_addr is called")
    }

    /// Handle to the live-connection gauge, usable after [`Self::run`] has
    /// consumed the server.
    #[must_use]
    pub fn live_connections(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.live_connections)
    }

    /// Waits (bounded) for in-flight connections to finish.
    ///
    /// Returns the number of connections still being served when `timeout`
    /// elapses, so a shutdown can record whether it drained cleanly.
    pub async fn drain(live: &Arc<AtomicU64>, timeout: std::time::Duration) -> u64 {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let current = live.load(Ordering::Relaxed);
            if current == 0 {
                return 0;
            }
            if tokio::time::Instant::now() >= deadline {
                return current;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    pub async fn run(self, engine: Arc<Mutex<PlomidStorageEngine>>) {
        self.run_inner(engine, None).await;
    }

    /// Runs with per-connection database routing rooted at `data_dir`.
    /// The legacy `run` entry point remains shared-engine mode for protocol
    /// tests and callers that intentionally operate one database.
    pub async fn run_with_database_root(
        self,
        engine: Arc<Mutex<PlomidStorageEngine>>,
        data_dir: PathBuf,
    ) {
        let loaded_registry = registry::DatabaseRegistry::load(&data_dir);
        // The executor owns the live `pg_database` projection. Populate it
        // from the durable registry before accepting connections; otherwise a
        // server restart exposes only the default database until the next
        // CREATE/DROP DATABASE statement mutates the in-memory projection.
        let registry = Arc::new(tokio::sync::Mutex::new(loaded_registry));
        self.run_inner(engine, Some((data_dir, registry))).await;
    }

    async fn run_inner(
        self,
        engine: Arc<Mutex<PlomidStorageEngine>>,
        database_context: Option<(PathBuf, Arc<tokio::sync::Mutex<registry::DatabaseRegistry>>)>,
    ) {
        let connection_counter = self.connection_counter.clone();
        let query_counter = self.query_counter.clone();
        let connection_permits = self.connection_permits.clone();
        loop {
            let (socket, peer_addr) = match self.listener.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    warn!(target: "server", "accept_failed error={}", e);
                    continue;
                }
            };
            let permit = match connection_permits.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    warn!(target: "server", event = "connection_rejected", reason = "connection_limit", peer = %peer_addr);
                    let mut socket = socket;
                    let _ = socket.shutdown().await;
                    continue;
                }
            };
            let id = connection_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            info!(target: "server", "connection_open connection_id={} peer={}", id, peer_addr);

            let engine = engine.clone();
            let maintenance_link = self.maintenance_link.clone();
            let auth_config = self.auth_config.clone();
            let process_id = self.process_id;
            let secret_key = self.secret_key;
            let connection_query_counter = query_counter.clone();
            let connection_database_context = database_context.clone();
            let connection_span = info_span!(
                target: "server",
                "connection",
                connection_id = id,
                peer = %peer_addr,
            );

            let live = Arc::clone(&self.live_connections);
            live.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                let _permit = permit;
                let mut handler = ConnectionHandler::new(
                    id,
                    socket,
                    engine,
                    auth_config,
                    process_id,
                    secret_key,
                    connection_query_counter,
                    connection_database_context,
                    maintenance_link.clone(),
                );
                let result = handler.run().instrument(connection_span).await;
                match result {
                    Ok(()) => {
                        info!(target: "server", "connection_close connection_id={} reason=normal", id);
                    }
                    Err(e) => {
                        warn!(target: "server", "connection_close connection_id={} error={}", id, e);
                    }
                }
                live.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }
}

pub(crate) fn statement_type(sql: &str) -> &'static str {
    let word = sql
        .split_whitespace()
        .next()
        .map(|word| word.trim_end_matches(';'))
        .unwrap_or("");
    // Zero-allocation dispatch: case-insensitive compare without building an
    // uppercase String (the previous version allocated once per call, and the
    // request path calls this several times per statement).
    if word.eq_ignore_ascii_case("BEGIN") || word.eq_ignore_ascii_case("START") {
        return "BEGIN";
    }
    if word.eq_ignore_ascii_case("COMMIT") {
        return "COMMIT";
    }
    if word.eq_ignore_ascii_case("CREATE") {
        return "CREATE";
    }
    if word.eq_ignore_ascii_case("INSERT") {
        return "INSERT";
    }
    if word.eq_ignore_ascii_case("SELECT") {
        return "SELECT";
    }
    if word.eq_ignore_ascii_case("UPDATE") {
        return "UPDATE";
    }
    if word.eq_ignore_ascii_case("DELETE") {
        return "DELETE";
    }
    if word.eq_ignore_ascii_case("SET") {
        return "SET";
    }
    if word.eq_ignore_ascii_case("SHOW") {
        return "SHOW";
    }
    if word.eq_ignore_ascii_case("USE") {
        return "USE";
    }
    if word.eq_ignore_ascii_case("ALTER") {
        return "ALTER";
    }
    if word.eq_ignore_ascii_case("DROP") {
        return "DROP";
    }
    if word.eq_ignore_ascii_case("COMMENT") {
        return "COMMENT";
    }
    if word.eq_ignore_ascii_case("VACUUM") {
        return "VACUUM";
    }
    if word.eq_ignore_ascii_case("ANALYZE") {
        return "ANALYZE";
    }
    if word.eq_ignore_ascii_case("REINDEX") {
        return "REINDEX";
    }
    if word.eq_ignore_ascii_case("LOCK") {
        return "LOCK";
    }
    if word.eq_ignore_ascii_case("CLUSTER") {
        return "CLUSTER";
    }
    if word.eq_ignore_ascii_case("TRUNCATE") {
        return "TRUNCATE";
    }
    if word.eq_ignore_ascii_case("REFRESH") {
        return "REFRESH";
    }
    if word.eq_ignore_ascii_case("EXPLAIN") {
        return "EXPLAIN";
    }
    if word.eq_ignore_ascii_case("COPY") {
        return "COPY";
    }
    if word.eq_ignore_ascii_case("GRANT") {
        return "GRANT";
    }
    if word.eq_ignore_ascii_case("REVOKE") {
        return "REVOKE";
    }
    if word.eq_ignore_ascii_case("VALUES") {
        return "VALUES";
    }
    if word.eq_ignore_ascii_case("DESCRIBE") {
        return "DESCRIBE";
    }
    if word.eq_ignore_ascii_case("DEALLOCATE") {
        return "DEALLOCATE";
    }
    if word.eq_ignore_ascii_case("DISCARD") {
        return "DISCARD";
    }
    if word.eq_ignore_ascii_case("UNLISTEN") {
        return "UNLISTEN";
    }
    if word.eq_ignore_ascii_case("LISTEN") {
        return "LISTEN";
    }
    if word.eq_ignore_ascii_case("NOTIFY") {
        return "NOTIFY";
    }
    if word.eq_ignore_ascii_case("CLOSE") {
        return "CLOSE";
    }
    if word.eq_ignore_ascii_case("SAVEPOINT") {
        return "SAVEPOINT";
    }
    if word.eq_ignore_ascii_case("RELEASE") {
        return "RELEASE";
    }
    if word.eq_ignore_ascii_case("ROLLBACK") {
        let mut tokens = sql.split_whitespace();
        let second = tokens.nth(1).unwrap_or("").trim_end_matches(';');
        let third = tokens.next().unwrap_or("").trim_end_matches(';');
        let fourth = tokens.next().unwrap_or("").trim_end_matches(';');
        let _ = fourth;
        if second.eq_ignore_ascii_case("TO") && third.eq_ignore_ascii_case("SAVEPOINT") {
            // Distinguish `ROLLBACK TO SAVEPOINT x` (4+ tokens) from bare
            // `ROLLBACK`: the original check required tokens.len() >= 4 with
            // tokens[1]=="TO" and tokens[2]=="SAVEPOINT".
            let count = sql.split_whitespace().count();
            if count >= 4 {
                return "ROLLBACK_TO_SAVEPOINT";
            }
        }
        return "ROLLBACK";
    }
    "UNKNOWN"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ErrorResponse, MessageEncoder, TransactionStatus};
    use crate::session::{ServerAuthConfig, Session};

    #[test]
    fn session_tracks_transaction_status() {
        let mut session = Session::new(1, "alice", "mydb");
        assert_eq!(session.transaction_status, TransactionStatus::Idle);
        session.mark_in_transaction();
        assert_eq!(session.transaction_status, TransactionStatus::InTransaction);
        session.mark_idle();
        assert_eq!(session.transaction_status, TransactionStatus::Idle);
    }

    #[test]
    fn session_parameters_are_populated() {
        let session = Session::new(1, "alice", "mydb");
        assert_eq!(session.get_parameter("server_version"), Some("17.0"));
        assert_eq!(session.get_parameter("client_encoding"), Some("UTF8"));
    }

    #[test]
    fn auth_config_validates_credentials() {
        let config = ServerAuthConfig::new("alice", "secret");
        assert!(config.authenticate("alice", "secret"));
        assert!(!config.authenticate("alice", "wrong"));
        assert!(!config.authenticate("bob", "secret"));
    }

    #[test]
    fn auth_config_disabled_accepts_any() {
        let config = ServerAuthConfig::disabled();
        assert!(config.authenticate("anyone", "anything"));
    }

    #[test]
    fn protocol_encoder_tag_and_length_are_correct() {
        let data = MessageEncoder::encode_ready_for_query(TransactionStatus::Idle);
        assert_eq!(data[0], b'Z');
        let len = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        assert_eq!(len as usize, data.len() - 1);
    }

    #[test]
    fn error_response_serializes_severity_code_and_message() {
        let err = ErrorResponse::new("ERROR", "42P01", "relation \"x\" does not exist");
        let data = MessageEncoder::encode_error_response(&err);
        let payload = std::str::from_utf8(&data[5..]).unwrap();
        assert!(payload.contains("SERROR"));
        assert!(payload.contains("C42P01"));
        assert!(payload.contains("Mrelation \"x\" does not exist"));
    }

    #[tokio::test]
    async fn server_binds_to_configured_address() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let server = PlomidServer::bind(addr, ServerAuthConfig::disabled())
            .await
            .expect("server binds");
        assert_eq!(server.local_addr(), addr);
    }
}
