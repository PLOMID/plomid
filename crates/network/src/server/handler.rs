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
//! Per-connection PostgreSQL frontend/backend message handling.

use super::registry::{self, database_path, safe_database_name};
use super::statement_type;
use crate::protocol::{
    AuthMechanism, ErrorResponse, FrontendTag, MessageDecoder, MessageEncoder, TransactionStatus,
    MAX_FRONTEND_MESSAGE_SIZE,
};
use crate::session::{AuthMethod, ServerAuthConfig, Session};
use plomid_core::{ErrorKind, PlomidError};
use plomid_executor::Executor;
use plomid_executor::SqlError;
use plomid_txn::{ConcurrentPlomidStorageEngine, PlomidStorageEngine};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter, ReadHalf, WriteHalf};
use tracing::warn;

/// Runs a synchronous storage-engine call without starving the async runtime.
///
/// Engine work is blocking: a commit fsyncs the WAL inline, and a committer
/// that arrives while another flush is in flight parks on a `std::sync::Condvar`.
/// Polling that straight from a connection task pins the tokio worker for the
/// whole fsync. Two measured consequences: an unrelated connection scheduled on
/// the same worker cannot reach its own commit until the fsync returns, and —
/// because it therefore never arrives while a flush is in flight — the
/// group-commit coordinator sees a single committer per round (`group=1`, one
/// fsync per commit) instead of sharing one fsync across the commits that are
/// ready at the same time. `block_in_place` tells the multi-thread runtime this
/// worker is about to block, so its other tasks run on sibling threads.
fn blocking<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        // A current-thread runtime (the `#[tokio::test]` default) has no sibling
        // worker to hand tasks to, and calling `block_in_place` there panics.
        _ => f(),
    }
}

pub struct ConnectionHandler {
    connection_id: u64,
    reader: BufReader<ReadHalf<tokio::net::TcpStream>>,
    writer: BufWriter<WriteHalf<tokio::net::TcpStream>>,
    engine: Arc<Mutex<PlomidStorageEngine>>,
    executor: Option<Executor<ConcurrentPlomidStorageEngine>>,
    auth_config: ServerAuthConfig,
    process_id: u32,
    secret_key: u32,
    prepared_statements: HashMap<String, PreparedStatement>,
    portals: HashMap<String, Portal>,
    active_result_formats: Vec<u16>,
    active_result_type_oids: Vec<u32>,
    extended_error: bool,
    extended_description_sent: bool,
    query_counter: Arc<std::sync::atomic::AtomicU64>,
    database_context: Option<(PathBuf, Arc<tokio::sync::Mutex<registry::DatabaseRegistry>>)>,
    copy_state: Option<CopyState>,
    /// Background maintenance endpoint for this connection's sessions.
    /// Installed from the server; sessions submit due tables instead of
    /// running passes on committing connections.
    maintenance_link: Option<crate::WorkerLink>,
    /// INSERT statements received through one extended-protocol pipeline.
    /// psycopg executemany sends Parse once, then many Bind/Execute messages,
    /// followed by one Sync.  Keep those compatible writes together so the
    /// executor can coalesce them into one transaction and one durability
    /// boundary.
    pending_extended_inserts: Vec<String>,
    last_staged_insert_generation: Option<u64>,
    next_prepared_generation: u64,
}

struct CopyState {
    table: String,
    columns: Option<String>,
    data: Vec<u8>,
    format: CopyFileFormat,
    delimiter: u8,
    header: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyFileDirection {
    From,
    To,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyFileFormat {
    Text,
    Csv,
}

#[derive(Debug, Clone)]
struct CopyFileSpec {
    table: String,
    columns: Option<String>,
    path: PathBuf,
    direction: CopyFileDirection,
    format: CopyFileFormat,
    delimiter: u8,
    header: bool,
}

#[derive(Debug, Clone)]
struct PreparedStatement {
    query: String,
    parameter_types: Vec<u32>,
    generation: u64,
}

#[derive(Debug, Clone)]
struct Portal {
    statement: String,
    parameters: Vec<Option<String>>,
    result_formats: Vec<u16>,
}

impl ConnectionHandler {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        connection_id: u64,
        socket: tokio::net::TcpStream,
        engine: Arc<Mutex<PlomidStorageEngine>>,
        auth_config: ServerAuthConfig,
        process_id: u32,
        secret_key: u32,
        query_counter: Arc<std::sync::atomic::AtomicU64>,
        database_context: Option<(PathBuf, Arc<tokio::sync::Mutex<registry::DatabaseRegistry>>)>,
        maintenance_link: Option<crate::WorkerLink>,
    ) -> Self {
        // PostgreSQL clients issue many small request/response exchanges.
        // Disable Nagle buffering so a response is not delayed behind a
        // subsequent packet on loopback or low-latency networks.
        let _ = socket.set_nodelay(true);
        let (reader, writer) = tokio::io::split(socket);
        Self {
            connection_id,
            reader: BufReader::new(reader),
            writer: BufWriter::new(writer),
            engine,
            executor: None,
            auth_config,
            process_id,
            secret_key,
            prepared_statements: HashMap::new(),
            portals: HashMap::new(),
            active_result_formats: Vec::new(),
            active_result_type_oids: Vec::new(),
            extended_error: false,
            extended_description_sent: false,
            query_counter,
            database_context,
            copy_state: None,
            maintenance_link,
            pending_extended_inserts: Vec::new(),
            last_staged_insert_generation: None,
            next_prepared_generation: 1,
        }
    }

    pub(crate) async fn run(&mut self) -> std::io::Result<()> {
        let startup = loop {
            let mut len_buf = [0u8; 4];
            match self.reader.read_exact(&mut len_buf).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            if len < 4 || len - 4 > MAX_FRONTEND_MESSAGE_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid or oversized startup length: {len}"),
                ));
            }
            if len == 8 {
                let mut code_buf = [0u8; 4];
                self.reader.read_exact(&mut code_buf).await?;
                let code = u32::from_be_bytes(code_buf);
                if code == 80877103 || code == 80877104 {
                    self.writer.write_all(b"N").await?;
                    self.writer.flush().await?;
                    continue;
                }
                // CancelRequest is a complete, untagged frontend message.
                // There is no response; close this connection after consuming it.
                if code == 80877102 {
                    return Ok(());
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unknown SSL request code",
                ));
            }
            let payload_len = len - 4;
            let mut payload = vec![0u8; payload_len];
            self.reader.read_exact(&mut payload).await?;
            break payload;
        };

        let params = MessageDecoder::parse_startup(&startup)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let user = params
            .get("user")
            .cloned()
            .unwrap_or_else(|| "plomid".to_string());
        let database = params
            .get("database")
            .cloned()
            .unwrap_or_else(|| "plomid".to_string());

        if self.auth_config.enabled {
            match self.auth_config.method {
                AuthMethod::Cleartext => self.authenticate_cleartext(&user).await?,
                AuthMethod::Md5 => self.authenticate_md5(&user).await?,
                AuthMethod::Scram => self.authenticate_scram(&user).await?,
            }
        } else {
            self.write_message(MessageEncoder::encode_authentication(AuthMechanism::Ok))
                .await?;
        }

        // The server's default database already owns the shared executor
        // passed by `plomid-server`. Opening another storage engine per
        // connection races the WAL/segment files and is the source of the
        // connection timeouts seen under concurrent clients. Keep the shared
        // engine for that database; isolated engines remain available for
        // explicitly routed secondary databases.
        if !database.eq_ignore_ascii_case("plomid") {
            if let Some((data_dir, registry)) = self.database_context.clone() {
                let mut registry = registry.lock().await;
                let database_dir = match registry.open_path(&data_dir, &database) {
                    Ok(path) => path,
                    Err(message) => {
                        drop(registry);
                        self.send_error_response_fatal("3D000", message.to_string())
                            .await?;
                        return Ok(());
                    }
                };
                let database_names = registry.names.iter().cloned().collect::<Vec<_>>();
                drop(registry);
                let engine_path = database_dir.clone();
                let wal_path = database_dir.join("wal");
                let opened = PlomidStorageEngine::open(&engine_path, &wal_path, 1024)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                self.engine = Arc::new(Mutex::new(opened));
                // Session/catalog state remains local to this connection;
                // only the authoritative storage engine is shared.
                let _ = database_names;
            }
        }

        let mut executor = Executor::new_shared(self.engine.clone())
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        executor.set_session_context(&database, &user);
        // Production automatic-maintenance policy: burst-gated so a write
        // burst cannot trigger a full materialization per 64 commits on an
        // unlucky OLTP write's commit path. Explicit VACUUM stays ungated.
        executor.set_maintenance_policy(plomid_executor::MaintenancePolicy::production());
        // Background maintenance when the server installed it: due tables go
        // to the worker instead of the committing connection.
        if let Some(link) = self.maintenance_link.clone() {
            executor.set_background_maintenance(link);
        }
        if let Some((_, registry)) = self.database_context.as_ref() {
            let names = registry.lock().await.names.iter().cloned().collect();
            executor.set_database_names(names);
        }
        self.executor = Some(executor);

        let mut session = Session::new(self.connection_id, user, database);
        for name in [
            "application_name",
            "client_encoding",
            "DateStyle",
            "TimeZone",
            "search_path",
        ] {
            if let Some(value) = params.get(name) {
                session.set_parameter(name, value);
            }
        }

        for (name, value) in &session.parameters {
            self.write_message_buffered(MessageEncoder::encode_parameter_status(name, value))
                .await?;
        }

        self.write_message_buffered(MessageEncoder::encode_backend_key_data(
            self.process_id,
            self.secret_key,
        ))
        .await?;
        self.write_message_buffered(MessageEncoder::encode_ready_for_query(
            TransactionStatus::Idle,
        ))
        .await?;
        self.writer.flush().await?;

        loop {
            let (tag, payload) = match self.read_message().await? {
                Some(pair) => pair,
                None => break,
            };
            if self.extended_error && !matches!(tag, FrontendTag::Sync | FrontendTag::Terminate) {
                continue;
            }
            match tag {
                FrontendTag::Query => {
                    let query_id = self.next_query_id();
                    let sql = String::from_utf8(payload)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
                        .trim_end_matches('\0')
                        .to_owned();
                    if sql.trim().is_empty() {
                        self.write_message(MessageEncoder::encode_empty_query())
                            .await?;
                        self.write_message(MessageEncoder::encode_ready_for_query(
                            session.transaction_status,
                        ))
                        .await?;
                        continue;
                    }
                    tracing::debug!(target: "server::protocol", event = "query_received", query_id, connection_id = self.connection_id, statement_type = statement_type(&sql), sql_len = sql.len());
                    if let Some(copy) = parse_copy_from_stdin(&sql) {
                        self.copy_state = Some(copy);
                        self.write_message(MessageEncoder::encode_copy_in_response())
                            .await?;
                    } else if let Some(table) = parse_copy_to_stdout(&sql) {
                        self.finish_copy_to(&mut session, &table).await?;
                    } else if let Some(spec) = parse_copy_file(&sql) {
                        self.finish_copy_file(&mut session, spec).await?;
                    } else {
                        self.handle_query(&mut session, &sql, true, query_id)
                            .await?;
                    }
                }
                FrontendTag::Parse => {
                    if let Err(error) = self.handle_parse(&payload).await {
                        self.extended_error = true;
                        self.send_protocol_error(error.to_string()).await?;
                    }
                }
                FrontendTag::Bind => {
                    if let Err(error) = self.handle_bind(&payload).await {
                        self.extended_error = true;
                        self.send_protocol_error(error.to_string()).await?;
                    }
                }
                FrontendTag::Describe => {
                    if let Err(error) = self.handle_describe(&payload, &mut session).await {
                        self.extended_error = true;
                        self.send_protocol_error(error.to_string()).await?;
                    }
                }
                FrontendTag::Execute => {
                    let query_id = self.next_query_id();
                    tracing::debug!(target: "server::protocol", event = "ext_execute", query_id, txn_status = %session.transaction_status);
                    let (portal_name, max_rows) = parse_execute(&payload)?;
                    let portal = self
                        .portals
                        .get(&portal_name)
                        .cloned()
                        .ok_or_else(|| protocol_error("portal does not exist"))?;
                    let statement = self
                        .prepared_statements
                        .get(&portal.statement)
                        .cloned()
                        .ok_or_else(|| protocol_error("prepared statement does not exist"))?;
                    let statement_generation = statement.generation;
                    let mut sql = bind_query(&statement.query, &portal.parameters);
                    if max_rows > 0 && should_apply_result_limit(&sql) {
                        sql.push_str(&format!(" LIMIT {max_rows}"));
                    }
                    if self.is_batchable_extended_insert(&sql) {
                        // Extended-protocol clients such as psycopg may send
                        // Sync after every Execute while keeping one implicit
                        // transaction open.  Start that transaction on the
                        // first row and retain staged rows across Sync; the
                        // explicit COMMIT then replays the whole batch.
                        if !session.is_in_transaction() {
                            session.mark_in_transaction();
                        }
                        if self.last_staged_insert_generation != Some(statement_generation) {
                            self.last_staged_insert_generation = Some(statement_generation);
                            self.handle_query(&mut session, &sql, false, query_id)
                                .await?;
                        } else {
                            session.stage_statement(sql);
                            self.write_message_buffered(MessageEncoder::encode_command_complete(
                                "INSERT 0 1",
                            ))
                            .await?;
                        }
                        continue;
                    }
                    // A failed batch must be reported to the client as an
                    // ErrorResponse, exactly like the Sync path does. Letting
                    // the `?` escape `run()` instead tears the whole TCP
                    // connection down for a single statement error, which the
                    // client sees as "server closed the connection
                    // unexpectedly" and cannot recover from.
                    if let Err(error) = self.flush_extended_inserts(&mut session).await {
                        self.extended_error = false;
                        self.send_error_response(
                            SqlError::Storage(PlomidError::from(error)),
                            session.transaction_status,
                        )
                        .await?;
                        continue;
                    }
                    self.active_result_formats = portal.result_formats.clone();
                    // Result-column OIDs come from the executor's result
                    // metadata.  Bind parameter OIDs describe inputs only;
                    // reusing them here corrupts RowDescription and binary
                    // output for expressions such as `$1::text`.
                    self.active_result_type_oids.clear();
                    self.handle_query(&mut session, &sql, false, query_id)
                        .await?;
                }
                FrontendTag::Sync => {
                    if !self.pending_extended_inserts.is_empty() {
                        if let Err(error) = self.flush_extended_inserts(&mut session).await {
                            self.extended_error = false;
                            self.send_error_response(
                                SqlError::Storage(PlomidError::from(error)),
                                session.transaction_status,
                            )
                            .await?;
                            continue;
                        }
                    }
                    self.extended_error = false;
                    tracing::debug!(target: "server::protocol", event = "ext_sync", txn_status = %session.transaction_status);
                    self.write_message(MessageEncoder::encode_ready_for_query(
                        session.transaction_status,
                    ))
                    .await?;
                }
                FrontendTag::Flush => {}
                FrontendTag::CopyDone => {
                    if self.copy_state.is_some() {
                        self.finish_copy(&mut session).await?;
                    } else {
                        self.send_protocol_error("COPY done received outside COPY FROM STDIN")
                            .await?;
                    }
                }
                FrontendTag::Close => {
                    if let Some((kind, name)) = parse_close(&payload)? {
                        if kind == b'S' {
                            self.prepared_statements.remove(&name);
                            self.portals.retain(|_, portal| portal.statement != name);
                        } else {
                            self.portals.remove(&name);
                        }
                    }
                    self.write_message(MessageEncoder::encode_close_complete())
                        .await?;
                }
                FrontendTag::CopyData => {
                    if let Some(copy) = self.copy_state.as_mut() {
                        copy.data.extend_from_slice(&payload);
                    } else {
                        self.send_protocol_error("COPY data received outside COPY FROM STDIN")
                            .await?;
                    }
                }
                FrontendTag::CopyFail => {
                    self.copy_state = None;
                    self.write_message(MessageEncoder::encode_ready_for_query(
                        session.transaction_status,
                    ))
                    .await?;
                }
                FrontendTag::Terminate => {
                    tracing::info!(target: "server::protocol", "connection_terminate connection_id={}", self.connection_id);
                    break;
                }
                _ => {
                    warn!(target: "server::protocol", "unexpected_message connection_id={} tag={:?}", self.connection_id, tag);
                }
            }
        }
        Ok(())
    }

    fn is_batchable_extended_insert(&self, sql: &str) -> bool {
        let upper = sql.trim().to_ascii_uppercase();
        upper.starts_with("INSERT ")
            && upper.contains(" VALUES ")
            && !upper.contains("RETURNING ")
            && !upper.contains("ON CONFLICT ")
            && !upper.contains(" INSERT ")
    }

    async fn flush_extended_inserts(&mut self, session: &mut Session) -> std::io::Result<()> {
        if self.pending_extended_inserts.is_empty() {
            return Ok(());
        }
        let statements = std::mem::take(&mut self.pending_extended_inserts);
        let batch = format!("BEGIN; {}; COMMIT;", statements.join("; "));
        let search_path = parse_search_path(session.get_parameter("search_path"));
        let engine = self.executor.as_mut().expect("executor initialized");
        let result = blocking(|| engine.execute_all_with_search_path(&batch, &search_path))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let inserted = result.0.iter().find_map(|result| match result {
            plomid_sql::QueryResult::Inserted(count) => Some(*count),
            _ => None,
        });
        if inserted.is_none() {
            return Err(std::io::Error::other(
                "batched INSERT produced no insert result",
            ));
        }
        // Each portal represents one VALUES row in psycopg executemany.  Emit
        // one command completion per Execute, preserving the extended wire
        // sequence while the actual storage work was committed once.
        for _ in 0..statements.len() {
            self.write_message_buffered(MessageEncoder::encode_command_complete("INSERT 0 1"))
                .await?;
        }
        Ok(())
    }
    /// Reads a PasswordMessage from the client and returns the trimmed
    /// password string. Returns `None` if the connection is closed.
    async fn read_password_message(&mut self) -> std::io::Result<Option<String>> {
        let password_bytes = match self.read_message().await? {
            Some((FrontendTag::Password, payload)) => payload,
            Some(_) => {
                self.send_error_response_fatal("08P01", "expected PasswordMessage")
                    .await?;
                return Ok(None);
            }
            None => return Ok(None),
        };
        let password = String::from_utf8(password_bytes)
            .map(|password| password.trim_end_matches('\0').to_owned())
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid password encoding")
            })?;
        Ok(Some(password))
    }

    async fn finish_copy(&mut self, session: &mut Session) -> std::io::Result<()> {
        let Some(copy) = self.copy_state.take() else {
            return Ok(());
        };
        let copy_spec = CopyFileSpec {
            table: copy.table.clone(),
            columns: copy.columns.clone(),
            path: PathBuf::new(),
            direction: CopyFileDirection::From,
            format: copy.format,
            delimiter: copy.delimiter,
            header: copy.header,
        };
        let rows = match parse_copy_file_rows(&copy.data, &copy_spec) {
            Ok(rows) => rows
                .into_iter()
                .filter(|row| !row.is_empty())
                .map(|row| row.join(", "))
                .collect::<Vec<_>>(),
            Err(message) => {
                self.send_error_response(
                    sql_error_from_io(format!("invalid COPY input: {message}")),
                    session.transaction_status,
                )
                .await?;
                return Ok(());
            }
        };
        if rows.is_empty() {
            self.write_message(MessageEncoder::encode_command_complete("COPY 0"))
                .await?;
            self.write_message(MessageEncoder::encode_ready_for_query(
                session.transaction_status,
            ))
            .await?;
            return Ok(());
        }
        let columns = copy.columns.unwrap_or_default();
        let sql = format!(
            "INSERT INTO {}{} VALUES {}",
            copy.table,
            columns,
            rows.into_iter()
                .map(|row| format!("({row})"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let result = {
            let engine = self.executor.as_mut().expect("executor initialized");
            engine.execute(&sql)
        };
        match result {
            Ok(plomid_sql::QueryResult::Inserted(count)) => {
                self.write_message(MessageEncoder::encode_command_complete(&format!(
                    "COPY {count}"
                )))
                .await?;
            }
            Ok(_) => {
                self.write_message(MessageEncoder::encode_command_complete("COPY 0"))
                    .await?;
            }
            Err(error) => {
                // COPY is a frontend protocol exchange. Even when the
                // generated INSERT fails, the client must receive both the
                // SQL error and ReadyForQuery; otherwise psycopg remains
                // blocked waiting for the command to finish.
                self.send_error_response(error, session.transaction_status)
                    .await?;
                return Ok(());
            }
        }
        self.write_message(MessageEncoder::encode_ready_for_query(
            session.transaction_status,
        ))
        .await
    }

    async fn finish_copy_to(&mut self, session: &mut Session, table: &str) -> std::io::Result<()> {
        let sql = format!("SELECT * FROM {table}");
        let result = {
            let engine = self.executor.as_mut().expect("executor initialized");
            engine.execute(&sql)
        };
        let rows = match result {
            Ok(plomid_sql::QueryResult::Rows { rows, .. }) => rows,
            Ok(_) => Vec::new(),
            Err(error) => {
                self.send_error_response(error, session.transaction_status)
                    .await?;
                return Ok(());
            }
        };
        self.write_message(MessageEncoder::encode_copy_out_response())
            .await?;
        for row in &rows {
            let mut line = Vec::new();
            for (index, value) in row.iter().enumerate() {
                if index != 0 {
                    line.push(b'\t');
                }
                copy_escape_value(value, &mut line);
            }
            line.push(b'\n');
            self.write_message_buffered(MessageEncoder::encode_copy_data(&line))
                .await?;
        }
        self.write_message_buffered(MessageEncoder::encode_copy_done())
            .await?;
        self.write_message_buffered(MessageEncoder::encode_command_complete(&format!(
            "COPY {}",
            rows.len()
        )))
        .await?;
        self.write_message_buffered(MessageEncoder::encode_ready_for_query(
            session.transaction_status,
        ))
        .await?;
        self.writer.flush().await
    }

    async fn finish_copy_file(
        &mut self,
        session: &mut Session,
        spec: CopyFileSpec,
    ) -> std::io::Result<()> {
        let result = match spec.direction {
            CopyFileDirection::From => {
                let data = match std::fs::read(&spec.path) {
                    Ok(data) => data,
                    Err(error) => {
                        self.send_error_response(
                            sql_error_from_io(format!(
                                "could not read COPY source file {}: {error}",
                                spec.path.display()
                            )),
                            session.transaction_status,
                        )
                        .await?;
                        return Ok(());
                    }
                };
                let rows = parse_copy_file_rows(&data, &spec)
                    .map_err(|message| sql_error_from_io(format!("invalid COPY input: {message}")));
                match rows {
                    Ok(rows) => {
                        let count = rows.len();
                        if count == 0 {
                            Ok(count)
                        } else {
                            let values = rows
                                .into_iter()
                                .map(|row| format!("({})", row.join(", ")))
                                .collect::<Vec<_>>()
                                .join(", ");
                            let sql = format!(
                                "INSERT INTO {}{} VALUES {values}",
                                spec.table,
                                spec.columns.clone().unwrap_or_default()
                            );
                            let engine = self.executor.as_mut().expect("executor initialized");
                            match engine.execute(&sql) {
                                Ok(plomid_sql::QueryResult::Inserted(_)) => Ok(count),
                                Ok(_) => Ok(0),
                                Err(error) => Err(error),
                            }
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            CopyFileDirection::To => {
                let sql = format!("SELECT * FROM {}", spec.table);
                let result = {
                    let engine = self.executor.as_mut().expect("executor initialized");
                    engine.execute(&sql)
                };
                let rows = match result {
                    Ok(plomid_sql::QueryResult::Rows { rows, .. }) => rows,
                    Ok(_) => Vec::new(),
                    Err(error) => {
                        self.send_error_response(error, session.transaction_status)
                            .await?;
                        return Ok(());
                    }
                };
                let mut output = Vec::new();
                for row in &rows {
                    copy_encode_file_row(row, &mut output, spec.format, spec.delimiter);
                }
                if let Err(error) = std::fs::write(&spec.path, output) {
                    self.send_error_response(
                        sql_error_from_io(format!(
                            "could not write COPY destination file {}: {error}",
                            spec.path.display()
                        )),
                        session.transaction_status,
                    )
                    .await?;
                    return Ok(());
                }
                Ok(rows.len())
            }
        };
        match result {
            Ok(count) => {
                self.write_message(MessageEncoder::encode_command_complete(&format!(
                    "COPY {count}"
                )))
                .await?;
                self.write_message(MessageEncoder::encode_ready_for_query(
                    session.transaction_status,
                ))
                .await?;
            }
            Err(error) => {
                self.send_error_response(error, session.transaction_status)
                    .await?;
            }
        }
        Ok(())
    }

    /// Runs the cleartext-password authentication exchange.
    async fn authenticate_cleartext(&mut self, user: &str) -> std::io::Result<()> {
        self.write_message(MessageEncoder::encode_authentication(
            AuthMechanism::CleartextPassword,
        ))
        .await?;
        let password = match self.read_password_message().await? {
            Some(password) => password,
            None => return Ok(()),
        };
        if !self.auth_config.authenticate(user, &password) {
            self.send_error_response_fatal(
                "28P01",
                format!("password authentication failed for user \"{user}\""),
            )
            .await?;
            return Ok(());
        }
        self.write_message(MessageEncoder::encode_authentication(AuthMechanism::Ok))
            .await?;
        Ok(())
    }

    /// Runs the MD5-password authentication exchange.
    async fn authenticate_md5(&mut self, user: &str) -> std::io::Result<()> {
        let salt = self.process_id.to_be_bytes();
        self.write_message(MessageEncoder::encode_authentication_md5(salt))
            .await?;
        let password = match self.read_password_message().await? {
            Some(password) => password,
            None => return Ok(()),
        };
        if !self.auth_config.authenticate_md5(user, &password, salt) {
            self.send_error_response_fatal(
                "28P01",
                format!("password authentication failed for user \"{user}\""),
            )
            .await?;
            return Ok(());
        }
        self.write_message(MessageEncoder::encode_authentication(AuthMechanism::Ok))
            .await?;
        Ok(())
    }
    /// Runs a SCRAM-SHA-256 authentication exchange.
    ///
    /// Flow: advertise SCRAM-SHA-256, receive the client-first message, send
    /// the server-first message, receive the client-final message, verify the
    /// proof, and send the server-final verifier plus AuthenticationOk.
    async fn authenticate_scram(&mut self, user: &str) -> std::io::Result<()> {
        let mut scram = crate::scram::ScramSession::with_verifier(
            &self.auth_config.password,
            self.auth_config.scram_salt,
            &self.auth_config.scram_salted_password,
        );

        self.write_message(MessageEncoder::encode_authentication_sasl(&[
            "SCRAM-SHA-256",
        ]))
        .await?;

        // SASLInitialResponse arrives on the wire as a PasswordMessage whose
        // payload is mechanism + \0 + client-first.
        let initial_raw = match self.read_message().await? {
            Some((FrontendTag::Password, payload)) => payload,
            Some(_) => {
                self.send_error_response_fatal("08P01", "expected SASL initial response")
                    .await?;
                return Ok(());
            }
            None => return Ok(()),
        };
        let client_first = parse_sasl_initial(&initial_raw);
        if client_first.is_empty() {
            self.send_error_response_fatal("08P01", "malformed SASL initial response")
                .await?;
            return Ok(());
        }

        let server_first = match scram.handle_initial(&client_first) {
            Ok(server_first) => server_first,
            Err(_) => {
                self.send_error_response_fatal("08P01", "malformed SCRAM client-first message")
                    .await?;
                return Ok(());
            }
        };
        self.write_message(MessageEncoder::encode_authentication_sasl_continue(
            &server_first,
        ))
        .await?;

        // SASLResponse arrives as a PasswordMessage carrying only the
        // client-final message text.
        let final_raw = match self.read_message().await? {
            Some((FrontendTag::Password, payload)) => payload,
            Some(_) => {
                self.send_error_response_fatal("08P01", "expected SASL response")
                    .await?;
                return Ok(());
            }
            None => return Ok(()),
        };
        let client_final = match String::from_utf8(final_raw) {
            Ok(client_final) => client_final.trim_end_matches('\0').to_owned(),
            Err(_) => {
                self.send_error_response_fatal("08P01", "invalid SASL response encoding")
                    .await?;
                return Ok(());
            }
        };

        let server_final = match scram.handle_final(&client_final) {
            Ok(server_final) => server_final,
            Err(_) => {
                self.send_error_response_fatal(
                    "28P01",
                    format!("password authentication failed for user \"{user}\""),
                )
                .await?;
                return Ok(());
            }
        };
        self.write_message(MessageEncoder::encode_authentication_sasl_final(
            &server_final,
        ))
        .await?;
        self.write_message(MessageEncoder::encode_authentication(AuthMechanism::Ok))
            .await?;
        Ok(())
    }

    async fn handle_parse(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let (name, remainder) = read_cstring(payload, "statement name")?;
        let (query, remainder) = read_cstring(remainder, "query")?;
        if remainder.len() < 2 {
            return Err(protocol_error("truncated Parse parameter types"));
        }
        let count = u16::from_be_bytes([remainder[0], remainder[1]]) as usize;
        let bytes = remainder
            .get(2..)
            .ok_or_else(|| protocol_error("truncated Parse parameter types"))?;
        if bytes.len() < count * 4 {
            return Err(protocol_error("truncated Parse parameter types"));
        }
        let parameter_types = (0..count)
            .map(|index| {
                u32::from_be_bytes([
                    bytes[index * 4],
                    bytes[index * 4 + 1],
                    bytes[index * 4 + 2],
                    bytes[index * 4 + 3],
                ])
            })
            .collect();
        self.prepared_statements.insert(
            name,
            PreparedStatement {
                query,
                parameter_types,
                generation: self.next_prepared_generation,
            },
        );
        self.next_prepared_generation = self.next_prepared_generation.wrapping_add(1);
        self.extended_description_sent = false;
        self.write_message(MessageEncoder::encode_parse_complete())
            .await
    }

    async fn handle_bind(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let (portal_name, rest) = read_cstring(payload, "portal name")?;
        let (statement_name, mut rest) = read_cstring(rest, "statement name")?;
        let statement = self
            .prepared_statements
            .get(&statement_name)
            .ok_or_else(|| protocol_error("prepared statement does not exist"))?
            .clone();
        if rest.len() < 2 {
            return Err(protocol_error("truncated Bind message"));
        }
        let format_count = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        let parameter_formats = rest[2..]
            .get(..format_count * 2)
            .ok_or_else(|| protocol_error("truncated Bind formats"))?
            .chunks_exact(2)
            .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        rest = rest
            .get(2 + format_count * 2..)
            .ok_or_else(|| protocol_error("truncated Bind formats"))?;
        if rest.len() < 2 {
            return Err(protocol_error("truncated Bind parameters"));
        }
        let count = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        rest = &rest[2..];
        let mut parameters = Vec::new();
        for _ in 0..count {
            if rest.len() < 4 {
                return Err(protocol_error("truncated Bind value"));
            }
            let length = i32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
            rest = &rest[4..];
            if length < 0 {
                parameters.push(None);
            } else {
                let length = length as usize;
                if rest.len() < length {
                    return Err(protocol_error("truncated Bind value"));
                }
                let format = parameter_formats
                    .get(if parameter_formats.len() == 1 {
                        0
                    } else {
                        parameters.len()
                    })
                    .copied()
                    .unwrap_or(0);
                let oid = statement
                    .parameter_types
                    .get(parameters.len())
                    .copied()
                    .unwrap_or(0);
                parameters.push(Some(decode_parameter(&rest[..length], format, oid)?));
                rest = &rest[length..];
            }
        }
        if rest.len() < 2 {
            return Err(protocol_error("truncated Bind result formats"));
        }
        let result_count = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        rest = &rest[2..];
        if rest.len() < result_count * 2 {
            return Err(protocol_error("truncated Bind result formats"));
        }
        let result_formats = (0..result_count)
            .map(|index| u16::from_be_bytes([rest[index * 2], rest[index * 2 + 1]]))
            .collect();
        self.portals.insert(
            portal_name,
            Portal {
                statement: statement_name,
                parameters,
                result_formats,
            },
        );
        self.write_message(MessageEncoder::encode_bind_complete())
            .await
    }

    async fn handle_describe(
        &mut self,
        payload: &[u8],
        session: &mut Session,
    ) -> std::io::Result<()> {
        let (kind, remainder) = payload
            .split_first()
            .ok_or_else(|| protocol_error("truncated Describe message"))?;
        let (name, _) = read_cstring(remainder, "Describe name")?;
        let (statement, described_sql, result_formats) = if *kind == b'P' {
            let portal = self
                .portals
                .get(&name)
                .ok_or_else(|| protocol_error("portal does not exist"))?;
            let statement = self
                .prepared_statements
                .get(&portal.statement)
                .ok_or_else(|| protocol_error("prepared statement does not exist"))?
                .clone();
            // Describe a portal with NULL placeholders, never the real bound
            // parameter values: executing the bound statement here would be a
            // side-effecting double-execute for DML and could raise a spurious
            // error (e.g. an invalid array cast) that PostgreSQL would not
            // surface until portal execution. NULL keeps Describe read-only.
            // Sized by the query's actual `$N` references, not by the declared
            // parameter types: clients routinely declare zero types and supply
            // values only at Bind, and sizing by declarations left raw `$1`
            // tokens in the described SQL.
            let placeholders =
                vec![None; max_placeholder(&statement.query).max(statement.parameter_types.len())];
            (
                statement.clone(),
                bind_query(&statement.query, &placeholders),
                portal.result_formats.clone(),
            )
        } else {
            let statement = self
                .prepared_statements
                .get(&name)
                .ok_or_else(|| protocol_error("prepared statement does not exist"))?
                .clone();
            let placeholders =
                vec![None; max_placeholder(&statement.query).max(statement.parameter_types.len())];
            (
                statement.clone(),
                bind_query(&statement.query, &placeholders),
                Vec::new(),
            )
        };
        if statement_type(&described_sql) == "SET" {
            if *kind == b'S' {
                self.write_message(MessageEncoder::encode_parameter_description(
                    &statement.parameter_types,
                ))
                .await?;
            }
            return self.write_message(MessageEncoder::encode_no_data()).await;
        }
        if *kind == b'S' {
            self.write_message(MessageEncoder::encode_parameter_description(
                &statement.parameter_types,
            ))
            .await?;
        }
        // Describe must NEVER side-effect. Only read-only result-producing
        // statements are executed (with NULL placeholders) so the client can
        // learn the result-column shape; every other statement type is
        // described with an empty result set.
        if !is_describe_query(statement_type(&described_sql)) {
            return self.write_message(MessageEncoder::encode_no_data()).await;
        }
        let engine = self.executor.as_mut().expect("executor initialized");
        let result = engine.execute(&described_sql);
        match result {
            Ok(plomid_sql::QueryResult::Rows {
                columns,
                column_types,
                rows,
            }) => {
                let mut column_types = infer_missing_result_types(column_types, &rows);
                // A Parse parameter's declared type is input metadata, not
                // result metadata.  For a typed parameter expression, infer
                // the result from the expression's explicit PostgreSQL cast
                // while describing the statement with NULL placeholders.
                for (index, column_type) in column_types.iter_mut().enumerate() {
                    if column_type.is_none() || column_type.is_some_and(|ty| ty.type_oid.0 == 25) {
                        if let Some(oid) = explicit_parameter_cast(&statement.query, index + 1) {
                            *column_type = Some(plomid_sql::ColumnType::new(
                                plomid_types::TypeOid(oid),
                                plomid_types::NO_TYPEMOD,
                            ));
                        }
                    }
                }
                self.write_message(MessageEncoder::encode_row_description(&row_descriptions(
                    &columns,
                    &column_types,
                    &result_formats,
                )))
                .await?;
                self.extended_description_sent = true;
                Ok(())
            }
            Ok(_) => self.write_message(MessageEncoder::encode_no_data()).await,
            Err(err) => {
                // A failing SELECT still aborts the surrounding transaction,
                // and the client must see the true state (InFailed, not Idle)
                // or it will skip recovery and leave the session stuck.
                if session.is_in_transaction() {
                    session.mark_failed_transaction();
                }
                // Extended-protocol errors must NOT carry ReadyForQuery: the
                // client recovers at the next Sync, which sends Ready itself.
                // Emitting Ready here desynchronizes Parse/Bind/Describe/
                // Execute pipelines (the client reads Ready as the end of the
                // whole sequence while its Sync is still outstanding).
                let response = executor_error_response(err);
                self.extended_description_sent = false;
                self.write_message(MessageEncoder::encode_error_response(&response))
                    .await
            }
        }
    }

    async fn handle_query(
        &mut self,
        session: &mut Session,
        sql: &str,
        ready: bool,
        query_id: u64,
    ) -> std::io::Result<()> {
        let started = std::time::Instant::now();
        let statement = statement_type(sql);
        tracing::debug!(target: "server::protocol", event = "query_started", query_id, connection_id = session.id, statement_type = statement, sql_len = sql.len());
        if self.database_context.is_some() {
            if let Some((create, name)) = parse_database_ddl(sql) {
                return self
                    .handle_database_ddl(create, &name, session, ready, query_id, started)
                    .await;
            }
            if statement == "USE" {
                if let Some(name) = parse_use_database(sql) {
                    return self
                        .handle_database_switch(&name, session, ready, query_id, started)
                        .await;
                }
            }
            if statement == "SHOW" && is_show_databases(sql) {
                return self
                    .handle_show_databases(session, ready, query_id, started)
                    .await;
            }
        }
        if statement == "DEALLOCATE" || statement == "DISCARD" || statement == "UNLISTEN" {
            // Housekeeping statements: acknowledged without engine round-trip
            // so client-side cleanup never aborts a transaction.
            let tag = match statement {
                "DISCARD" => "DISCARD ALL",
                other => other,
            };
            return self
                .send_query_result(
                    session,
                    plomid_sql::QueryResult::Created(tag.to_string()),
                    ready,
                )
                .await;
        }
        if statement == "LISTEN" || statement == "NOTIFY" {
            return self
                .send_query_result(
                    session,
                    plomid_sql::QueryResult::Created(statement.to_string()),
                    ready,
                )
                .await;
        }
        if statement == "COMMENT"
            || statement == "ANALYZE"
            || statement == "REINDEX"
            || statement == "LOCK"
            || statement == "CLUSTER"
            || statement == "REFRESH"
        {
            let tag = match statement {
                "ANALYZE" => "ANALYZE",
                "REINDEX" => "REINDEX",
                "LOCK" => "LOCK TABLE",
                "CLUSTER" => "CLUSTER",
                "REFRESH" => "REFRESH MATERIALIZED VIEW",
                "COMMENT" => "COMMENT",
                other => other,
            };
            return self
                .send_query_result(
                    session,
                    plomid_sql::QueryResult::Created(tag.to_string()),
                    ready,
                )
                .await;
        }
        // SAVEPOINT operations are represented by markers in the staged DML
        // list. The actual storage transaction is still opened at COMMIT, but
        // rollback-to-savepoint correctly discards later staged statements.
        if statement == "SAVEPOINT"
            || statement == "RELEASE"
            || statement == "ROLLBACK_TO_SAVEPOINT"
        {
            let name = sql
                .split_whitespace()
                .nth(match statement {
                    "ROLLBACK_TO_SAVEPOINT" => 3,
                    "RELEASE" => 2,
                    _ => 1,
                })
                .unwrap_or("")
                .trim_end_matches(';');
            match statement {
                "SAVEPOINT" => session.savepoint(name),
                "RELEASE" => {
                    session.release_savepoint(name);
                }
                "ROLLBACK_TO_SAVEPOINT" => {
                    session.rollback_to_savepoint(name);
                }
                _ => {}
            }
            let tag = match statement {
                "SAVEPOINT" => "SAVEPOINT",
                "RELEASE" => "RELEASE",
                "ROLLBACK_TO_SAVEPOINT" => "ROLLBACK TO SAVEPOINT",
                other => other,
            };
            return self
                .send_query_result(
                    session,
                    plomid_sql::QueryResult::Created(tag.to_string()),
                    ready,
                )
                .await;
        }
        if statement == "BEGIN" || statement == "COMMIT" || statement == "ROLLBACK" {
            return self
                .handle_session_transaction(session, sql, ready, query_id, started)
                .await;
        }
        if statement == "SET" || statement == "SHOW" {
            // A single simple-protocol Query string may batch several
            // statements (`SET ...; SELECT ...`). Split it at top-level
            // semicolons (ignoring those inside quotes/parens) so each
            // statement is classified, executed, and answered separately.
            // Otherwise SET would swallow the rest of the batch and SHOW
            // would return the SET text as its value.
            let parts = split_simple_protocol_statements(sql);
            if parts.len() > 1 {
                for part in parts {
                    Box::pin(self.handle_query(session, &part, false, query_id)).await?;
                }
                self.write_message(MessageEncoder::encode_ready_for_query(
                    session.transaction_status,
                ))
                .await?;
                if ready {
                    self.writer.flush().await?;
                }
                return Ok(());
            }
        }
        if statement == "SHOW" {
            if let Some(name) = show_parameter_name(sql) {
                let value = if name.eq_ignore_ascii_case("search_path") {
                    let engine = self.executor.as_mut().expect("executor initialized");
                    let search_path = parse_search_path(session.get_parameter("search_path"));
                    match blocking(|| {
                        engine.execute_all_with_search_path("SHOW search_path;", &search_path)
                    }) {
                        Ok((results, _)) => {
                            let value = results
                                .into_iter()
                                .find_map(|result| match result {
                                    plomid_sql::QueryResult::Rows { rows, .. } => rows
                                        .into_iter()
                                        .next()
                                        .and_then(|row| row.into_iter().next())
                                        .map(|value| match value {
                                            plomid_sql::Value::Text(text) => text,
                                            other => format!("{other:?}"),
                                        }),
                                    _ => None,
                                })
                                .unwrap_or_default();
                            return self
                                .send_query_result(
                                    session,
                                    plomid_sql::QueryResult::Rows {
                                        columns: vec![name],
                                        column_types: vec![Some(plomid_sql::ColumnType::text())],
                                        rows: vec![vec![plomid_sql::Value::Text(value)]],
                                    },
                                    ready,
                                )
                                .await;
                        }
                        Err(err) => {
                            self.log_query_failure(query_id, session.id, sql, &err, started);
                            self.send_query_error(err, session.transaction_status, ready)
                                .await?;
                            return Ok(());
                        }
                    }
                } else {
                    session
                        .get_parameter(&name)
                        .or_else(|| session.get_parameter(&name.to_ascii_lowercase()))
                        .unwrap_or("")
                        .to_string()
                };
                return self
                    .send_query_result(
                        session,
                        plomid_sql::QueryResult::Rows {
                            columns: vec![name],
                            column_types: vec![Some(plomid_sql::ColumnType::text())],
                            rows: vec![vec![plomid_sql::Value::Text(value)]],
                        },
                        ready,
                    )
                    .await;
            }
        }
        if statement == "SET" {
            if let Some((name, value)) = set_parameter(sql) {
                session.set_parameter(name, value);
            }
            // PostgreSQL clients commonly send session-level SET commands
            // that PLOMID does not need to act on (for example SET ROLE,
            // SET SESSION AUTHORIZATION, or a client-specific timeout).
            // They must not fall through to SQL execution, where an
            // identifier value can be mistaken for a table name.
            return self
                .send_query_result(session, plomid_sql::QueryResult::Set, ready)
                .await;
        }
        if session.transaction_status == TransactionStatus::InFailedTransaction {
            let err = SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                "current transaction is aborted, commands ignored until ROLLBACK",
            ));
            self.log_query_failure(query_id, session.id, sql, &err, started);
            self.send_query_error(err, session.transaction_status, ready)
                .await?;
            return Ok(());
        }
        if session.is_in_transaction() && matches!(statement, "INSERT" | "UPDATE" | "DELETE") {
            let engine = self.executor.as_mut().expect("executor initialized");
            // Parse/bind the statement now (so resolution errors surface at
            // statement time) and count the rows it would affect, without
            // executing it. The single real mutation happens at COMMIT, when
            // the staged batch is replayed — a throwaway trial execution here
            // would run every explicit-transaction DML twice.
            let search_path = parse_search_path(session.get_parameter("search_path"));
            let count =
                match blocking(|| engine.validate_dml_strict_with_search_path(sql, &search_path)) {
                    Ok(count) => count,
                    Err(err) => {
                        session.mark_failed_transaction();
                        self.log_query_failure(query_id, session.id, sql, &err, started);
                        self.send_query_error(err, session.transaction_status, ready)
                            .await?;
                        return Ok(());
                    }
                };
            session.stage_statement(sql.trim().trim_end_matches(';').trim().to_string());
            self.send_staged_command(statement, count, session, ready)
                .await?;
            return Ok(());
        }
        // A simple-protocol Query string may carry several independent
        // top-level statements. Running them through `execute_all` collects
        // every statement's result first and only then writes any of them, so
        // the client sees nothing until the whole batch has finished (for a
        // 20-statement batch of single-row INSERTs that is one WAL fsync per
        // statement before the first CommandComplete). PostgreSQL emits
        // CommandComplete as each statement completes; do the same here.
        //
        // Splitting is applied only when it is provably equivalent to running
        // the batch as one call: the session must be idle (so every statement
        // is its own autocommit transaction in both paths) and the batch must
        // contain no transaction control (an explicit `BEGIN ... COMMIT` must
        // stay a single transaction with a single commit, so it keeps the
        // buffered path below).
        if session.transaction_status == TransactionStatus::Idle && sql.contains(';') {
            let parts = split_simple_protocol_statements(sql);
            if parts.len() > 1 && !parts.iter().any(|part| is_transaction_control(part)) {
                for part in parts {
                    Box::pin(self.handle_query(session, &part, false, query_id)).await?;
                    // The response path buffers; flush so this statement's
                    // result is actually on the wire before the next one runs.
                    self.writer.flush().await?;
                }
                self.write_message(MessageEncoder::encode_ready_for_query(
                    session.transaction_status,
                ))
                .await?;
                if ready {
                    self.writer.flush().await?;
                }
                return Ok(());
            }
        }
        let engine = self.executor.as_mut().expect("executor initialized");
        let search_path = parse_search_path(session.get_parameter("search_path"));
        let (results, changed) =
            match blocking(|| engine.execute_all_with_search_path(sql, &search_path)) {
                Ok(result) => result,
                Err(err) => {
                    if session.is_in_transaction() {
                        session.mark_failed_transaction();
                    }
                    self.log_query_failure(query_id, session.id, sql, &err, started);
                    self.send_query_error(err, session.transaction_status, ready)
                        .await?;
                    return Ok(());
                }
            };
        let row_count = results
            .iter()
            .map(|result| match result {
                plomid_sql::QueryResult::Rows { rows, .. } => rows.len(),
                _ => 0,
            })
            .sum::<usize>();
        let result_set = results
            .iter()
            .any(|result| matches!(result, plomid_sql::QueryResult::Rows { .. }));
        tracing::debug!(
            target: "server::protocol",
            event = "query_completed",
            query_id,
            elapsed_ms = started.elapsed().as_millis() as u64,
            result_count = results.len(),
            row_count,
            row_description_sent = result_set,
            data_rows_sent = result_set && row_count > 0,
        );
        // Persist an in-band `SET search_path` into the wire session so later
        // queries on this connection keep resolving against the updated path.
        // `$user` is re-quoted to match PostgreSQL's `SHOW search_path` format.
        if let Some(next) = changed {
            let display = next
                .iter()
                .map(|schema| {
                    if schema == "$user" {
                        "\"$user\"".to_string()
                    } else {
                        schema.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            session.set_parameter("search_path", display);
        }
        let result_count = results.len();
        for (index, result) in results.into_iter().enumerate() {
            self.send_query_result(session, result, ready && index + 1 == result_count)
                .await?;
        }
        Ok(())
    }

    async fn handle_database_ddl(
        &mut self,
        create: bool,
        name: &str,
        session: &mut Session,
        ready: bool,
        query_id: u64,
        started: std::time::Instant,
    ) -> std::io::Result<()> {
        let Some((data_dir, registry)) = self.database_context.clone() else {
            return Ok(());
        };
        let mut registry = registry.lock().await;
        let result = if create {
            if registry
                .names
                .iter()
                .any(|registered| registered.eq_ignore_ascii_case(name))
            {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("database \"{name}\" already exists"),
                ))
            } else {
                let path = database_path(&data_dir, name);
                std::fs::create_dir_all(&path)?;
                if let Err(error) = PlomidStorageEngine::create(&path, &path.join("wal"), 64) {
                    let _ = std::fs::remove_dir_all(&path);
                    return self
                        .send_database_ddl_error(
                            session,
                            ready,
                            query_id,
                            sql_error_from_io(error.to_string()),
                            started,
                        )
                        .await;
                }
                registry.names.insert(name.to_string());
                if let Err(error) = registry.persist(&data_dir) {
                    registry.names.remove(name);
                    let _ = std::fs::remove_dir_all(&path);
                    Err(error)
                } else {
                    Ok(())
                }
            }
        } else if name.eq_ignore_ascii_case("plomid") {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "cannot drop the default database",
            ))
        } else if let Some(actual_name) = registry
            .names
            .iter()
            .find(|registered| registered.eq_ignore_ascii_case(name))
            .cloned()
        {
            registry.names.remove(&actual_name);
            std::fs::remove_dir_all(database_path(&data_dir, &actual_name))?;
            registry.persist(&data_dir)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("database \"{name}\" does not exist"),
            ))
        };
        let database_names = registry.names.iter().cloned().collect::<Vec<_>>();
        drop(registry);
        match result {
            Ok(()) => {
                self.executor
                    .as_mut()
                    .expect("executor initialized")
                    .set_database_names(database_names);
                let tag = if create {
                    "CREATE DATABASE"
                } else {
                    "DROP DATABASE"
                };
                tracing::info!(target: "server", event = "database_ddl", query_id, database = name, operation = tag,
                    elapsed_ms = started.elapsed().as_millis() as u64);
                self.send_staged_command(tag, 0, session, ready).await
            }
            Err(error) => {
                let err =
                    SqlError::Storage(PlomidError::new(ErrorKind::Conflict, error.to_string()));
                self.log_query_failure(query_id, session.id, "", &err, started);
                self.send_error_response(err, session.transaction_status)
                    .await
            }
        }
    }

    async fn handle_database_switch(
        &mut self,
        name: &str,
        session: &mut Session,
        ready: bool,
        query_id: u64,
        started: std::time::Instant,
    ) -> std::io::Result<()> {
        if session.transaction_status != TransactionStatus::Idle {
            let error = SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                "cannot change database during a transaction",
            ));
            self.log_query_failure(query_id, session.id, "USE", &error, started);
            return self
                .send_error_response(error, session.transaction_status)
                .await;
        }
        // USE of the already-selected database is a no-op in PostgreSQL
        // client workflows. Reopening the segment/WAL files here makes GUI
        // discovery appear hung and can contend with concurrent metadata
        // queries.
        if session.database.eq_ignore_ascii_case(name) {
            return self
                .send_query_result(session, plomid_sql::QueryResult::Set, ready)
                .await;
        }
        let Some((data_dir, registry)) = self.database_context.clone() else {
            let error = SqlError::Storage(PlomidError::new(
                ErrorKind::Unsupported,
                "database switching is unavailable",
            ));
            return self
                .send_error_response(error, TransactionStatus::Idle)
                .await;
        };
        let database_dir = {
            let mut registry = registry.lock().await;
            match registry.open_path(&data_dir, name) {
                Ok(path) => path,
                Err(error) => {
                    let sql_error = sql_error_from_io(error.to_string());
                    self.log_query_failure(query_id, session.id, "USE", &sql_error, started);
                    return self
                        .send_error_response(sql_error, TransactionStatus::Idle)
                        .await;
                }
            }
        };
        let opened = match PlomidStorageEngine::open(&database_dir, &database_dir.join("wal"), 64) {
            Ok(engine) => engine,
            Err(error) => {
                let sql_error = SqlError::Storage(error);
                self.log_query_failure(query_id, session.id, "USE", &sql_error, started);
                return self
                    .send_error_response(sql_error, TransactionStatus::Idle)
                    .await;
            }
        };
        let database_names = {
            let registry = registry.lock().await;
            registry.names.iter().cloned().collect::<Vec<_>>()
        };
        self.engine = Arc::new(Mutex::new(opened));
        self.executor = Some(
            Executor::new_shared(self.engine.clone())
                .map_err(|error| std::io::Error::other(error.to_string()))?,
        );
        self.executor
            .as_mut()
            .expect("executor initialized")
            .set_session_context(name, &session.user);
        self.executor
            .as_mut()
            .expect("executor initialized")
            .set_maintenance_policy(plomid_executor::MaintenancePolicy::production());
        if let Some(link) = self.maintenance_link.clone() {
            self.executor
                .as_mut()
                .expect("executor initialized")
                .set_background_maintenance(link);
        }
        self.executor
            .as_mut()
            .expect("executor initialized")
            .set_database_names(database_names);
        session.database = name.to_string();
        session.set_parameter("database", name);
        tracing::info!(target: "server", event = "database_switch", connection_id = session.id, database = name,
            elapsed_ms = started.elapsed().as_millis() as u64);
        self.send_query_result(session, plomid_sql::QueryResult::Set, ready)
            .await
    }

    async fn handle_show_databases(
        &mut self,
        session: &mut Session,
        ready: bool,
        query_id: u64,
        started: std::time::Instant,
    ) -> std::io::Result<()> {
        let Some((_data_dir, registry)) = &self.database_context else {
            return self
                .send_query_result(
                    session,
                    plomid_sql::QueryResult::Rows {
                        columns: vec!["database".to_string()],
                        column_types: vec![Some(plomid_sql::ColumnType::text())],
                        rows: vec![vec![plomid_sql::Value::Text("plomid".to_string())]],
                    },
                    ready,
                )
                .await;
        };
        let names = {
            let registry = registry.lock().await;
            registry.names.iter().cloned().collect::<Vec<_>>()
        };
        let rows: Vec<Vec<plomid_sql::Value>> = names
            .into_iter()
            .map(|name| vec![plomid_sql::Value::Text(name)])
            .collect();
        tracing::debug!(target: "server::protocol", event = "query_completed", query_id,
            elapsed_ms = started.elapsed().as_millis() as u64, result = "rows", row_count = rows.len(),
            row_description_sent = true, data_rows_sent = !rows.is_empty());
        self.send_query_result(
            session,
            plomid_sql::QueryResult::Rows {
                columns: vec!["database".to_string()],
                column_types: vec![Some(plomid_sql::ColumnType::text())],
                rows,
            },
            ready,
        )
        .await
    }

    async fn send_database_ddl_error(
        &mut self,
        session: &mut Session,
        _ready: bool,
        query_id: u64,
        error: SqlError,
        started: std::time::Instant,
    ) -> std::io::Result<()> {
        self.log_query_failure(query_id, session.id, "CREATE DATABASE", &error, started);
        self.send_error_response(error, session.transaction_status)
            .await
    }

    async fn handle_session_transaction(
        &mut self,
        session: &mut Session,
        sql: &str,
        ready: bool,
        query_id: u64,
        started: std::time::Instant,
    ) -> std::io::Result<()> {
        match statement_type(sql) {
            "BEGIN" => {
                if session.transaction_status == TransactionStatus::InTransaction {
                    self.send_staged_command("BEGIN", 0, session, ready).await
                } else if session.transaction_status == TransactionStatus::InFailedTransaction {
                    let err = SqlError::Storage(PlomidError::new(
                        ErrorKind::Conflict,
                        "current transaction is aborted, commands ignored until ROLLBACK",
                    ));
                    self.log_query_failure(query_id, session.id, sql, &err, started);
                    self.send_error_response(err, session.transaction_status)
                        .await
                } else {
                    session.mark_in_transaction();
                    self.send_staged_command("BEGIN", 0, session, ready).await
                }
            }
            "ROLLBACK" => {
                if matches!(session.transaction_status, TransactionStatus::Idle) {
                    let err = SqlError::Storage(PlomidError::new(
                        ErrorKind::Conflict,
                        "no active transaction",
                    ));
                    self.log_query_failure(query_id, session.id, sql, &err, started);
                    self.send_error_response(err, session.transaction_status)
                        .await
                } else {
                    self.last_staged_insert_generation = None;
                    session.mark_idle();
                    self.send_staged_command("ROLLBACK", 0, session, ready)
                        .await
                }
            }
            "COMMIT" => {
                if !session.is_in_transaction() {
                    let err = SqlError::Storage(PlomidError::new(
                        ErrorKind::Conflict,
                        "no active transaction",
                    ));
                    self.log_query_failure(query_id, session.id, sql, &err, started);
                    self.send_error_response(err, session.transaction_status)
                        .await
                } else {
                    let staged = session.take_staged_statements();
                    let replay_staged = self.coalesce_staged_inserts(&staged);
                    let batch = if replay_staged.is_empty() {
                        "BEGIN; COMMIT;".to_string()
                    } else {
                        format!("BEGIN; {}; COMMIT;", replay_staged.join("; "))
                    };
                    let engine = self.executor.as_mut().expect("executor initialized");
                    // Replay with the session search_path installed so
                    // unqualified table names resolve the same way they did at
                    // statement time (matching the autocommit path).
                    let search_path = parse_search_path(session.get_parameter("search_path"));
                    match blocking(|| engine.execute_all_with_search_path(&batch, &search_path)) {
                        Ok(_) => {
                            self.last_staged_insert_generation = None;
                            session.mark_idle();
                            self.send_staged_command("COMMIT", 0, session, ready).await
                        }
                        Err(err) => {
                            session.restore_staged_statements(staged);
                            session.mark_failed_transaction();
                            self.log_query_failure(query_id, session.id, sql, &err, started);
                            self.send_error_response(err, session.transaction_status)
                                .await
                        }
                    }
                }
            }
            _ => Ok(()),
        }
    }

    /// Collapse adjacent INSERT ... VALUES statements produced by the
    /// extended-protocol executemany path.  The wire protocol may deliver
    /// each row as its own Execute, but all of those statements are already
    /// held in the session transaction.  Replaying them as one multi-row
    /// statement lets the executor allocate transaction state, validate the
    /// schema, encode rows, and apply storage/index mutations once per batch.
    ///
    /// Only statements with an identical INSERT prefix are combined.  This
    /// keeps ordering, statement semantics, and rollback behavior unchanged
    /// for all other SQL forms.
    fn coalesce_staged_inserts(&self, staged: &[String]) -> Vec<String> {
        let mut result = Vec::with_capacity(staged.len());
        let mut current: Option<(String, String)> = None;

        for sql in staged {
            let Some((prefix, values)) = self.split_batchable_insert(sql) else {
                if let Some((prefix, values)) = current.take() {
                    result.push(format!("{prefix} VALUES {values}"));
                }
                result.push(sql.clone());
                continue;
            };

            match current.as_mut() {
                Some((current_prefix, current_values)) if *current_prefix == prefix => {
                    current_values.push_str(", ");
                    current_values.push_str(&values);
                }
                Some(_) => {
                    let (previous_prefix, previous_values) =
                        current.take().expect("current insert");
                    result.push(format!("{previous_prefix} VALUES {previous_values}"));
                    current = Some((prefix, values));
                }
                None => current = Some((prefix, values)),
            }
        }

        if let Some((prefix, values)) = current {
            result.push(format!("{prefix} VALUES {values}"));
        }
        result
    }

    fn split_batchable_insert(&self, sql: &str) -> Option<(String, String)> {
        if !self.is_batchable_extended_insert(sql) {
            return None;
        }
        let upper = sql.to_ascii_uppercase();
        let values_marker = " VALUES ";
        let values_start = upper.find(values_marker)?;
        let prefix = sql[..values_start].to_string();
        let values = sql[values_start + values_marker.len()..]
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string();
        if values.is_empty() {
            None
        } else {
            Some((prefix, values))
        }
    }

    async fn send_staged_command(
        &mut self,
        statement: &str,
        count: u64,
        session: &mut Session,
        ready: bool,
    ) -> std::io::Result<()> {
        let tag = match statement {
            "INSERT" => format!("INSERT 0 {count}"),
            "UPDATE" => format!("UPDATE {count}"),
            "DELETE" => format!("DELETE {count}"),
            _ => statement.to_string(),
        };
        self.write_message_buffered(MessageEncoder::encode_command_complete(&tag))
            .await?;
        if ready {
            self.write_message_buffered(MessageEncoder::encode_ready_for_query(
                session.transaction_status,
            ))
            .await?;
        }
        self.writer.flush().await?;
        self.active_result_formats.clear();
        self.active_result_type_oids.clear();
        Ok(())
    }

    fn log_query_failure(
        &self,
        query_id: u64,
        connection_id: u64,
        sql: &str,
        err: &SqlError,
        started: std::time::Instant,
    ) {
        let (code, message) = match err {
            SqlError::Syntax(e) | SqlError::Storage(e) => (e.code(), e.message()),
        };
        tracing::error!(target: "server::protocol", event = "query_failed", connection_id, query_id,
            error_code = code, error_message = message, elapsed_ms = started.elapsed().as_millis() as u64,
            statement_type = statement_type(sql), sql_len = sql.len());
    }

    async fn send_query_result(
        &mut self,
        session: &mut Session,
        result: plomid_sql::QueryResult,
        ready: bool,
    ) -> std::io::Result<()> {
        match result {
            plomid_sql::QueryResult::Rows {
                columns,
                column_types,
                rows,
            } => {
                let column_types = infer_missing_result_types(column_types, &rows);
                if columns.len() != column_types.len()
                    || rows.iter().any(|row| row.len() != columns.len())
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "result column metadata does not match row width",
                    ));
                }
                if !self.extended_description_sent {
                    self.write_message_buffered(MessageEncoder::encode_row_description(
                        &row_descriptions(&columns, &column_types, &self.active_result_formats),
                    ))
                    .await?;
                }
                for row in &rows {
                    let values: Vec<Option<Vec<u8>>> = row
                        .iter()
                        .enumerate()
                        .map(|(index, v)| match v {
                            plomid_sql::Value::Null => None,
                            other => {
                                let result_oid = self
                                    .active_result_type_oids
                                    .get(index)
                                    .copied()
                                    .or_else(|| {
                                        column_types
                                            .get(index)
                                            .and_then(|column| column.as_ref())
                                            .map(|column| column.type_oid.0)
                                    })
                                    .or_else(|| {
                                        plomid_sql::value_pg_type(other).map(|ty| ty.oid().0)
                                    });
                                Some(encode_result_value(
                                    other,
                                    wire_format(
                                        self.active_result_formats
                                            .get(index)
                                            .copied()
                                            .or_else(|| self.active_result_formats.first().copied())
                                            .unwrap_or(0),
                                        result_oid,
                                    ),
                                    result_oid,
                                ))
                            }
                        })
                        .collect();
                    self.write_message_buffered(MessageEncoder::encode_data_row(&values))
                        .await?;
                }
                let tag = format!("SELECT {}", rows.len());
                self.write_message_buffered(MessageEncoder::encode_command_complete(&tag))
                    .await?;
                self.extended_description_sent = false;
            }
            plomid_sql::QueryResult::Inserted(count) => {
                let tag = format!("INSERT 0 {}", count);
                self.write_message_buffered(MessageEncoder::encode_command_complete(&tag))
                    .await?;
            }
            plomid_sql::QueryResult::Updated(count) => {
                let tag = format!("UPDATE {}", count);
                self.write_message_buffered(MessageEncoder::encode_command_complete(&tag))
                    .await?;
            }
            plomid_sql::QueryResult::Deleted(count) => {
                let tag = format!("DELETE {}", count);
                self.write_message_buffered(MessageEncoder::encode_command_complete(&tag))
                    .await?;
            }
            plomid_sql::QueryResult::Set => {
                self.write_message_buffered(MessageEncoder::encode_command_complete("SET"))
                    .await?;
            }
            plomid_sql::QueryResult::Created(tag) => {
                self.write_message_buffered(MessageEncoder::encode_command_complete(&tag))
                    .await?;
            }
            plomid_sql::QueryResult::Committed => {
                self.write_message_buffered(MessageEncoder::encode_command_complete("COMMIT"))
                    .await?;
            }
            plomid_sql::QueryResult::RolledBack => {
                self.write_message_buffered(MessageEncoder::encode_command_complete("ROLLBACK"))
                    .await?;
                session.mark_idle();
            }
        }
        if ready {
            self.write_message_buffered(MessageEncoder::encode_ready_for_query(
                session.transaction_status,
            ))
            .await?;
        }
        self.writer.flush().await?;
        Ok(())
    }

    async fn send_error_response_fatal(
        &mut self,
        code: &str,
        message: impl Into<String>,
    ) -> std::io::Result<()> {
        let response = ErrorResponse::new("FATAL", code, message);
        self.send_error(response).await
    }

    async fn send_protocol_error(&mut self, message: impl Into<String>) -> std::io::Result<()> {
        self.send_error(ErrorResponse::new("ERROR", "08P01", message))
            .await
    }

    async fn send_error_response(
        &mut self,
        err: SqlError,
        status: TransactionStatus,
    ) -> std::io::Result<()> {
        self.extended_description_sent = false;
        let response = match err {
            SqlError::Syntax(_) | SqlError::Storage(_) => executor_error_response(err),
        };
        self.write_message_buffered(MessageEncoder::encode_error_response(&response))
            .await?;
        self.write_message_buffered(MessageEncoder::encode_ready_for_query(status))
            .await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn send_query_error(
        &mut self,
        err: SqlError,
        status: TransactionStatus,
        ready: bool,
    ) -> std::io::Result<()> {
        // A failed extended-protocol Execute must not leak the RowDescription
        // flag that Describe set; otherwise the next statement would skip its
        // own RowDescription and send DataRow without a prior T message.
        self.extended_description_sent = false;
        let response = executor_error_response(err);
        self.write_message_buffered(MessageEncoder::encode_error_response(&response))
            .await?;
        if ready {
            self.write_message_buffered(MessageEncoder::encode_ready_for_query(status))
                .await?;
        }
        self.writer.flush().await?;
        Ok(())
    }

    async fn send_error(&mut self, error: ErrorResponse) -> std::io::Result<()> {
        self.write_message(MessageEncoder::encode_error_response(&error))
            .await
    }

    async fn write_message(&mut self, data: Vec<u8>) -> std::io::Result<()> {
        tracing::debug!(target: "server::protocol", event = "wire_out", tag = *data.first().unwrap_or(&0u8));
        self.writer.write_all(&data).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn write_message_buffered(&mut self, data: Vec<u8>) -> std::io::Result<()> {
        tracing::debug!(target: "server::protocol", event = "wire_out", tag = *data.first().unwrap_or(&0u8));
        self.writer.write_all(&data).await
    }

    async fn read_message(&mut self) -> std::io::Result<Option<(FrontendTag, Vec<u8>)>> {
        MessageDecoder::read_frontend(&mut self.reader).await
    }

    fn next_query_id(&self) -> u64 {
        self.query_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }
}

fn infer_missing_result_types(
    mut column_types: Vec<Option<plomid_sql::ColumnType>>,
    rows: &[Vec<plomid_sql::Value>],
) -> Vec<Option<plomid_sql::ColumnType>> {
    for (index, column_type) in column_types.iter_mut().enumerate() {
        if column_type.is_some_and(|ty| ty.type_oid.0 != 25) {
            continue;
        }
        if let Some(value) = rows
            .iter()
            .filter_map(|row| row.get(index))
            .find(|value| !matches!(value, plomid_sql::Value::Null | plomid_sql::Value::Text(_)))
        {
            if let plomid_sql::Value::Array { element_oid, .. } = value {
                if let Some(element) = plomid_types::PgType::by_oid(*element_oid) {
                    if let Some(array_oid) = element.array_oid() {
                        *column_type = Some(plomid_sql::ColumnType::new(
                            array_oid,
                            plomid_types::NO_TYPEMOD,
                        ));
                    }
                }
            } else if let Some(pg_type) = plomid_sql::value_pg_type(value) {
                *column_type = Some(plomid_sql::ColumnType::new(
                    pg_type.oid(),
                    plomid_types::NO_TYPEMOD,
                ));
            }
        }
    }
    column_types
}

fn parse_database_ddl(sql: &str) -> Option<(bool, String)> {
    let words: Vec<&str> = sql
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect();
    if words.len() < 3
        || !words[0].eq_ignore_ascii_case("CREATE") && !words[0].eq_ignore_ascii_case("DROP")
    {
        return None;
    }
    if !words[1].eq_ignore_ascii_case("DATABASE") {
        return None;
    }
    let name = words[2].trim_matches('"');
    let is_create = words[0].eq_ignore_ascii_case("CREATE");
    if !is_create && words.len() != 3 {
        return None;
    }
    if is_create {
        let mut index = 3;
        while index < words.len() {
            let option = words[index].to_ascii_lowercase();
            let consumed = match option.as_str() {
                "encoding" | "template" | "owner" | "tablespace" | "strategy" | "lc_collate"
                | "lc_ctype" | "is_template" | "allow_connections" => 2,
                "connection"
                    if words
                        .get(index + 1)
                        .is_some_and(|word| word.eq_ignore_ascii_case("limit")) =>
                {
                    3
                }
                _ => return None,
            };
            if index + consumed > words.len() {
                return None;
            }
            index += consumed;
        }
    }
    safe_database_name(name).map(|name| (is_create, name.to_string()))
}

fn parse_use_database(sql: &str) -> Option<String> {
    let words: Vec<&str> = sql
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect();
    if words.len() == 2 && words[0].eq_ignore_ascii_case("USE") {
        return safe_database_name(words[1].trim_matches('"')).map(str::to_owned);
    }
    None
}

fn is_show_databases(sql: &str) -> bool {
    let words: Vec<&str> = sql
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect();
    words.len() == 2
        && words[0].eq_ignore_ascii_case("SHOW")
        && (words[1].eq_ignore_ascii_case("DATABASE") || words[1].eq_ignore_ascii_case("DATABASES"))
}

fn sql_error_from_io(message: String) -> SqlError {
    SqlError::Storage(PlomidError::new(ErrorKind::Io, message))
}

fn protocol_error(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

/// Parses a SASLInitialResponse PasswordMessage payload, returning the
/// client-first SCRAM message. The authentication identity comes from the
/// startup message (the `user` parameter), exactly as PostgreSQL does, so
/// the username carried inside the SCRAM exchange is not re-validated here.
fn parse_sasl_initial(payload: &[u8]) -> String {
    if let Some(index) = payload.iter().position(|byte| *byte == 0) {
        let mechanism = String::from_utf8(payload[..index].to_vec()).unwrap_or_default();
        let rest = &payload[index + 1..];
        let client_first = String::from_utf8(rest.to_vec()).unwrap_or_default();
        if mechanism != "SCRAM-SHA-256" {
            return String::new();
        }
        client_first
    } else {
        String::new()
    }
}

fn quote_bound_value(value: &str) -> String {
    // A text parameter whose content happens to spell "null" must stay a quoted
    // string: genuine SQL NULL arrives as `None` and is rendered by the caller.
    // (Previously this mapped the *text* "null" to NULL, corrupting data.)
    if value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok() {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "''"))
    }
}

/// Highest `$N` placeholder index referenced *outside* string literals,
/// quoted identifiers, dollar-quoted bodies, and line comments.
///
/// The extended protocol lets clients declare zero parameter types and supply
/// values only at Bind, so the query text itself is the authority for how many
/// placeholders exist. Sizing substitution by the declared-type count instead
/// left raw `$1` tokens in the executed SQL whenever a client declared no
/// types (the common JDBC/pgx/psycopg3 shape), failing even `SELECT $1`.
fn max_placeholder(query: &str) -> usize {
    let bytes = query.as_bytes();
    let mut max = 0usize;
    let mut index = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_line_comment = false;
    let mut in_dollar = false;
    while index < bytes.len() {
        let ch = bytes[index];
        if in_line_comment {
            if ch == b'\n' {
                in_line_comment = false;
            }
            index += 1;
            continue;
        }
        if in_dollar {
            if ch == b'$' && index + 1 < bytes.len() && bytes[index + 1] == b'$' {
                in_dollar = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if in_single {
            if ch == b'\'' {
                if index + 1 < bytes.len() && bytes[index + 1] == b'\'' {
                    index += 2;
                } else {
                    in_single = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }
        if in_double {
            if ch == b'"' {
                if index + 1 < bytes.len() && bytes[index + 1] == b'"' {
                    index += 2;
                } else {
                    in_double = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }
        match ch {
            b'-' if index + 1 < bytes.len() && bytes[index + 1] == b'-' => {
                in_line_comment = true;
                index += 2;
            }
            b'\'' => {
                in_single = true;
                index += 1;
            }
            b'"' => {
                in_double = true;
                index += 1;
            }
            b'$' if index + 1 < bytes.len() && bytes[index + 1] == b'$' => {
                in_dollar = true;
                index += 2;
            }
            b'$' => {
                let mut end = index + 1;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > index + 1 {
                    if let Ok(number) = query[index + 1..end].parse::<usize>() {
                        max = max.max(number);
                    }
                    index = end;
                } else {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    max
}

fn copy_sql_value(value: &str) -> String {
    if value == r#"\N"# {
        return "NULL".into();
    }
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
        return value.to_ascii_uppercase();
    }
    if value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok() {
        return value.into();
    }
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}

fn copy_escape_value(value: &plomid_sql::Value, output: &mut Vec<u8>) {
    if matches!(value, plomid_sql::Value::Null) {
        output.extend_from_slice(br"\N");
        return;
    }
    for byte in value.to_sql_text().bytes() {
        match byte {
            b'\\' => output.extend_from_slice(br"\\"),
            b'\t' => output.extend_from_slice(br"\t"),
            b'\n' => output.extend_from_slice(br"\n"),
            b'\r' => output.extend_from_slice(br"\r"),
            other => output.push(other),
        }
    }
}

fn parse_copy_from_stdin(sql: &str) -> Option<CopyState> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    let suffix = lower.rfind(" from stdin")?;
    if !lower.starts_with("copy ") {
        return None;
    }
    let source = trimmed[5..suffix].trim();
    let options = trimmed[suffix + " from stdin".len()..].trim();
    let open = source.find('(');
    let (table, columns) = if let Some(open) = open {
        let close = source.rfind(')')?;
        let table = source[..open].trim().to_string();
        let columns = format!("({})", source[open + 1..close].trim());
        (table, Some(columns))
    } else {
        (source.trim().to_string(), None)
    };
    if table.is_empty() {
        None
    } else {
        let (format, delimiter, header) = parse_copy_options(options, CopyFileFormat::Text)?;
        Some(CopyState {
            table,
            columns,
            data: Vec::new(),
            format,
            delimiter,
            header,
        })
    }
}

fn parse_copy_to_stdout(sql: &str) -> Option<String> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() == 4
        && words[0].eq_ignore_ascii_case("copy")
        && words[2].eq_ignore_ascii_case("to")
        && words[3].eq_ignore_ascii_case("stdout")
    {
        let table = words[1];
        if !table.contains('(') && !table.contains(')') {
            return Some(table.to_string());
        }
    }
    None
}

fn parse_copy_file(sql: &str) -> Option<CopyFileSpec> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("copy ") {
        return None;
    }
    let (direction, marker) = if let Some(index) = lower.find(" from ") {
        (CopyFileDirection::From, index)
    } else if let Some(index) = lower.find(" to ") {
        (CopyFileDirection::To, index)
    } else {
        return None;
    };
    let source = trimmed[5..marker].trim();
    let rest = trimmed[marker + 4..].trim();
    let (path, options) = parse_copy_path_and_options(rest)?;
    let (table, columns) = if let Some(open) = source.find('(') {
        let close = source.rfind(')')?;
        if close <= open || !source[close + 1..].trim().is_empty() {
            return None;
        }
        (
            source[..open].trim().to_string(),
            Some(format!("({})", source[open + 1..close].trim())),
        )
    } else {
        (source.to_string(), None)
    };
    if table.is_empty() {
        return None;
    }
    let (format, delimiter, header) = parse_copy_options(&options, CopyFileFormat::Text)?;
    Some(CopyFileSpec {
        table,
        columns,
        path: PathBuf::from(path),
        direction,
        format,
        delimiter,
        header,
    })
}

fn parse_copy_options(
    options: &str,
    default_format: CopyFileFormat,
) -> Option<(CopyFileFormat, u8, bool)> {
    let options_lower = options.to_ascii_lowercase();
    let format = if options_lower.contains("format csv") {
        CopyFileFormat::Csv
    } else if options_lower.contains("format text") || options.is_empty() {
        default_format
    } else {
        return None;
    };
    let delimiter = parse_copy_delimiter(options, format)?;
    let header = options_lower.contains("header")
        && (!options_lower.contains("header false") && !options_lower.contains("header = false"));
    Some((format, delimiter, header))
}

fn parse_copy_path_and_options(input: &str) -> Option<(String, String)> {
    let bytes = input.as_bytes();
    if bytes.first().copied()? != b'\'' {
        return None;
    }
    let mut end = 1;
    while end < bytes.len() {
        if bytes[end] == b'\'' {
            if bytes.get(end + 1) == Some(&b'\'') {
                end += 2;
                continue;
            }
            let path = input[1..end].replace("''", "'");
            let options = input[end + 1..].trim();
            return Some((path, options.to_string()));
        }
        end += 1;
    }
    None
}

fn parse_copy_delimiter(options: &str, format: CopyFileFormat) -> Option<u8> {
    let lower = options.to_ascii_lowercase();
    let Some(index) = lower.find("delimiter") else {
        return Some(match format {
            CopyFileFormat::Text => b'\t',
            CopyFileFormat::Csv => b',',
        });
    };
    let quoted = options[index..].split('\'').nth(1)?;
    let mut chars = quoted.chars();
    let value = match chars.next()? {
        '\\' => match chars.next()? {
            't' => b'\t',
            'n' => b'\n',
            other => other as u8,
        },
        ch => ch as u8,
    };
    Some(value)
}

fn parse_copy_file_rows(data: &[u8], spec: &CopyFileSpec) -> Result<Vec<Vec<String>>, String> {
    let text = std::str::from_utf8(data).map_err(|_| "input is not UTF-8")?;
    let mut rows = match spec.format {
        CopyFileFormat::Text => text
            .lines()
            .map(|line| {
                line.split(spec.delimiter as char)
                    .map(|field| {
                        if field == r"\N" {
                            "NULL".to_string()
                        } else {
                            copy_sql_value(&field.replace(r"\t", "\t").replace(r"\n", "\n"))
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        CopyFileFormat::Csv => parse_csv_rows(text, spec.delimiter)?,
    };
    if spec.header && !rows.is_empty() {
        rows.remove(0);
    }
    Ok(rows)
}

fn parse_csv_rows(input: &str, delimiter: u8) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            match byte {
                b'"' if bytes.get(index + 1) == Some(&b'"') => {
                    field.push('"');
                    index += 1;
                }
                b'"' => quoted = false,
                other => field.push(other as char),
            }
        } else {
            match byte {
                b'"' if field.is_empty() => quoted = true,
                b if b == delimiter => {
                    row.push(copy_sql_value(&field));
                    field.clear();
                }
                b'\n' => {
                    row.push(copy_sql_value(&field));
                    field.clear();
                    rows.push(std::mem::take(&mut row));
                }
                b'\r' => {}
                other => field.push(other as char),
            }
        }
        index += 1;
    }
    if quoted {
        return Err("unterminated CSV quoted field".to_string());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(copy_sql_value(&field));
        rows.push(row);
    }
    Ok(rows)
}

fn copy_encode_file_row(
    row: &[plomid_sql::Value],
    output: &mut Vec<u8>,
    format: CopyFileFormat,
    delimiter: u8,
) {
    for (index, value) in row.iter().enumerate() {
        if index != 0 {
            output.push(delimiter);
        }
        let text = if matches!(value, plomid_sql::Value::Null) {
            String::new()
        } else {
            value.to_sql_text()
        };
        if format == CopyFileFormat::Csv
            && (text.contains(',') || text.contains('"') || text.contains('\n'))
        {
            output.push(b'"');
            output.extend_from_slice(text.replace('"', "\"\"").as_bytes());
            output.push(b'"');
        } else {
            output.extend_from_slice(text.as_bytes());
        }
    }
    output.push(b'\n');
}

fn encode_result_value(
    value: &plomid_sql::Value,
    format: u16,
    expected_oid: Option<u32>,
) -> Vec<u8> {
    if format == 0 {
        let text = match value {
            // Numeric is retained internally with scale for arithmetic and
            // executor compatibility. PostgreSQL's text output omits an
            // unnecessary fractional zero suffix for integral EXTRACT values.
            plomid_sql::Value::Numeric(_) => canonical_numeric_text(&value.to_sql_text()),
            _ => value.to_sql_text(),
        };
        return text.into_bytes();
    }
    match value {
        plomid_sql::Value::Text(text) if expected_oid == Some(23) => text
            .parse::<i32>()
            .unwrap_or_default()
            .to_be_bytes()
            .to_vec(),
        plomid_sql::Value::Bool(value) => vec![u8::from(*value)],
        plomid_sql::Value::Int2(value) => value.to_be_bytes().to_vec(),
        plomid_sql::Value::Int4(value) => value.to_be_bytes().to_vec(),
        plomid_sql::Value::Int8(value) => value.to_be_bytes().to_vec(),
        plomid_sql::Value::Float4(value) => value.to_bits().to_be_bytes().to_vec(),
        plomid_sql::Value::Float8(value) => value.to_bits().to_be_bytes().to_vec(),
        plomid_sql::Value::Oid(value) => value.to_be_bytes().to_vec(),
        plomid_sql::Value::Date(value) => value.to_be_bytes().to_vec(),
        plomid_sql::Value::Time(value)
        | plomid_sql::Value::Timestamp(value)
        | plomid_sql::Value::Timestamptz(value) => value.to_be_bytes().to_vec(),
        plomid_sql::Value::Bytea(value) => value.clone(),
        // PostgreSQL's binary representation for these types is not exposed
        // by the current type facade; use their canonical text payload.
        other => other.to_sql_text().into_bytes(),
    }
}

fn canonical_numeric_text(text: &str) -> String {
    let Some(dot) = text.find('.') else {
        return text.to_string();
    };
    let mut result = text[..dot + 1].to_string();
    result.push_str(text[dot + 1..].trim_end_matches('0'));
    if result.ends_with('.') {
        result.pop();
    }
    if result == "-0" {
        "0".to_string()
    } else {
        result
    }
}

fn wire_format(requested: u16, oid: Option<u32>) -> u16 {
    if requested == 1 && !oid.is_some_and(supports_binary_oid) {
        0
    } else {
        requested
    }
}

fn supports_binary_oid(oid: u32) -> bool {
    matches!(
        oid,
        16 | 17
            | 20
            | 21
            | 23
            | 25
            | 700
            | 701
            | 1042
            | 1043
            | 1082
            | 1083
            | 1114
            | 1184
            | 26
            | 1700
            | 2950
    )
}

fn decode_parameter(bytes: &[u8], format: u16, oid: u32) -> std::io::Result<String> {
    if format == 0 {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }
    let invalid = || protocol_error("unsupported or malformed binary parameter");
    match oid {
        16 if bytes.len() == 1 => Ok(if bytes[0] == 0 { "false" } else { "true" }.into()),
        21 if bytes.len() == 2 => Ok(i16::from_be_bytes([bytes[0], bytes[1]]).to_string()),
        23 if bytes.len() == 4 => {
            Ok(i32::from_be_bytes(bytes.try_into().map_err(|_| invalid())?).to_string())
        }
        26 if bytes.len() == 4 => {
            Ok(u32::from_be_bytes(bytes.try_into().map_err(|_| invalid())?).to_string())
        }
        20 if bytes.len() == 8 => {
            Ok(i64::from_be_bytes(bytes.try_into().map_err(|_| invalid())?).to_string())
        }
        700 if bytes.len() == 4 => {
            let bits = u32::from_be_bytes(bytes.try_into().map_err(|_| invalid())?);
            Ok(f32::from_bits(bits).to_string())
        }
        701 if bytes.len() == 8 => {
            let bits = u64::from_be_bytes(bytes.try_into().map_err(|_| invalid())?);
            Ok(f64::from_bits(bits).to_string())
        }
        1082 if bytes.len() == 4 => {
            let days = i32::from_be_bytes(bytes.try_into().map_err(|_| invalid())?);
            Ok(plomid_types::datetime::format_date(days))
        }
        1083 if bytes.len() == 8 => {
            let micros = i64::from_be_bytes(bytes.try_into().map_err(|_| invalid())?);
            Ok(plomid_types::datetime::format_time(micros, None))
        }
        1114 if bytes.len() == 8 => {
            let micros = i64::from_be_bytes(bytes.try_into().map_err(|_| invalid())?);
            Ok(plomid_types::datetime::format_timestamp(micros, None))
        }
        1184 if bytes.len() == 8 => {
            let micros = i64::from_be_bytes(bytes.try_into().map_err(|_| invalid())?);
            Ok(plomid_types::datetime::format_timestamp(micros, None))
        }
        17 => {
            let mut hex = String::with_capacity(2 + bytes.len() * 2);
            hex.push_str("\\x");
            for byte in bytes {
                use std::fmt::Write;
                let _ = write!(hex, "{byte:02x}");
            }
            Ok(hex)
        }
        2950 if bytes.len() == 16 => {
            let b = bytes;
            use std::fmt::Write;
            let mut s = String::with_capacity(36);
            let _ = write!(
                s,
                "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                b[0], b[1], b[2], b[3],
                b[4], b[5],
                b[6], b[7],
                b[8], b[9],
                b[10], b[11], b[12], b[13], b[14], b[15],
            );
            Ok(s)
        }
        1700 => decode_binary_numeric(bytes).ok_or_else(invalid),
        25 | 1043 | 1042 | 114 | 3802 => Ok(String::from_utf8_lossy(bytes).into_owned()),
        _ => Err(invalid()),
    }
}

fn decode_binary_numeric(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 8 {
        return None;
    }
    let read_i16 = |offset: usize| -> Option<i16> {
        let end = offset.checked_add(2)?;
        Some(i16::from_be_bytes(bytes.get(offset..end)?.try_into().ok()?))
    };
    let ndigits = usize::try_from(read_i16(0)?).ok()?;
    let weight = i32::from(read_i16(2)?);
    let sign = u16::from_be_bytes(bytes.get(4..6)?.try_into().ok()?);
    let dscale = usize::try_from(read_i16(6)?).ok()?;
    if sign == 0xC000 {
        return Some("NaN".to_string());
    }
    if sign != 0 && sign != 0x4000 || bytes.len() != 8 + ndigits * 2 {
        return None;
    }
    let mut groups = Vec::with_capacity(ndigits);
    for index in 0..ndigits {
        let digit = u16::from_be_bytes(bytes[8 + index * 2..10 + index * 2].try_into().ok()?);
        if digit >= 10_000 {
            return None;
        }
        groups.push(digit);
    }
    if groups.iter().all(|digit| *digit == 0) {
        return Some(if dscale == 0 {
            "0".to_string()
        } else {
            format!("0.{}", "0".repeat(dscale))
        });
    }
    let first_len = groups.first().map_or(1, |digit| digit.to_string().len());
    let mut digits = groups
        .iter()
        .enumerate()
        .map(|(index, digit)| {
            if index == 0 {
                digit.to_string()
            } else {
                format!("{digit:04}")
            }
        })
        .collect::<String>();
    let decimal_pos = first_len as i32 + weight * 4;
    if decimal_pos <= 0 {
        digits = format!("0.{}{}", "0".repeat((-decimal_pos) as usize), digits);
    } else if decimal_pos as usize >= digits.len() {
        digits.push_str(&"0".repeat(decimal_pos as usize - digits.len()));
    } else {
        digits.insert(decimal_pos as usize, '.');
    }
    if let Some(dot) = digits.find('.') {
        let actual_scale = digits.len() - dot - 1;
        if actual_scale < dscale {
            digits.push_str(&"0".repeat(dscale - actual_scale));
        } else if actual_scale > dscale {
            digits.truncate(dot + 1 + dscale);
        }
    }
    if sign == 0x4000 {
        digits.insert(0, '-');
    }
    Some(digits)
}

/// Maps an internal PLOMID error to a PostgreSQL SQLSTATE.
///
/// The classification is derived from the internal error kind plus the error's
/// own class keywords (constraint, object type), mirroring how PostgreSQL
/// assigns errcodes from error classes. No client or query introspection is
/// involved.
fn sqlstate_for(kind: ErrorKind, message: &str) -> &'static str {
    let message = message.to_ascii_lowercase();
    match kind {
        ErrorKind::Syntax => "42601",
        ErrorKind::Catalog | ErrorKind::NotFound => {
            if message.contains("database") {
                "3D000"
            } else if message.contains("column") {
                "42703"
            } else if message.contains("function") || message.contains("procedure") {
                "42883"
            } else {
                "42P01"
            }
        }
        ErrorKind::AlreadyExists => "42710",
        ErrorKind::Conflict => {
            if message.contains("null value") || message.contains("not-null") {
                "23502"
            } else if message.contains("unique") || message.contains("duplicate") {
                "23505"
            } else if message.contains("check") {
                "23514"
            } else if message.contains("foreign") {
                "23503"
            } else if message.contains("aborted") || message.contains("commands ignored") {
                "25P02"
            } else {
                "40001"
            }
        }
        ErrorKind::Transaction | ErrorKind::Aborted => "25P02",
        ErrorKind::InvalidArgument => {
            if message.contains("division by zero") {
                "22012"
            } else if message.contains("out of range") {
                "22003"
            } else {
                "22023"
            }
        }
        ErrorKind::RowEncoding => "22000",
        ErrorKind::Unsupported => "0A000",
        ErrorKind::Io => {
            if message.contains("no such file") || message.contains("not found") {
                "58P01"
            } else {
                "58000"
            }
        }
        ErrorKind::Wal => "58000",
        ErrorKind::Corruption => "XX001",
        ErrorKind::Internal => "XX000",
        _ => "XX000",
    }
}

/// Builds the wire error for an executor failure, translating the internal
/// error code into a PostgreSQL SQLSTATE and carrying the internal diagnostic
/// context as the `DETAIL` field.
fn executor_error_response(err: SqlError) -> ErrorResponse {
    let (kind, message, detail) = match err {
        SqlError::Syntax(plomid) => (
            plomid.kind(),
            plomid.message().to_owned(),
            plomid.detail().map(str::to_owned),
        ),
        SqlError::Storage(plomid) => (
            plomid.kind(),
            plomid.message().to_owned(),
            plomid.detail().map(str::to_owned),
        ),
    };
    let mut response = ErrorResponse::new("ERROR", sqlstate_for(kind, &message), message);
    if let Some(detail) = detail {
        response = response.with_detail(detail);
    }
    response
}

/// Returns true when a query returns a result set whose rows may be capped by
/// the extended-protocol Execute `max_rows`.
///
/// PostgreSQL's `max_rows` only caps the number of rows a portal *returns*; it
/// is never spliced into the SQL text and is ignored for statements that do not
/// produce a result set. Splicing ` LIMIT n` onto DDL/DML (e.g.
/// `CREATE SCHEMA x` -> `CREATE SCHEMA x LIMIT n`) is a syntax error, so the
/// projector must only be applied to genuinely row-returning queries.
fn should_apply_result_limit(sql: &str) -> bool {
    matches!(
        statement_type(sql),
        "SELECT" | "VALUES" | "SHOW" | "EXPLAIN" | "DESCRIBE"
    ) || sql
        .split_whitespace()
        .next()
        .map(|word| word.trim_end_matches(';').eq_ignore_ascii_case("WITH"))
        .unwrap_or(false)
}

fn bind_query(query: &str, parameters: &[Option<String>]) -> String {
    // Single-pass substitution. The previous implementation called
    // `str::replace` once per parameter, which corrupted multi-digit
    // placeholders (`$10` became `<value-of-$1>0`) and rewrote `$N` tokens
    // inside string literals. Placeholders inside literals, quoted
    // identifiers, dollar-quoted bodies, and comments are left intact; a
    // missing value renders as NULL, matching the old behavior for Bind NULLs.
    // The output is assembled as bytes so non-ASCII query text passes through
    // untouched (placeholder syntax is pure ASCII, so every slice boundary is
    // a char boundary).
    let bytes = query.as_bytes();
    let mut bound: Vec<u8> = Vec::with_capacity(query.len() + parameters.len() * 4);
    let mut index = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_line_comment = false;
    let mut in_dollar = false;
    while index < bytes.len() {
        let ch = bytes[index];
        if in_line_comment {
            bound.push(ch);
            if ch == b'\n' {
                in_line_comment = false;
            }
            index += 1;
            continue;
        }
        if in_dollar {
            if ch == b'$' && index + 1 < bytes.len() && bytes[index + 1] == b'$' {
                bound.extend_from_slice(b"$$");
                in_dollar = false;
                index += 2;
            } else {
                bound.push(ch);
                index += 1;
            }
            continue;
        }
        if in_single {
            bound.push(ch);
            if ch == b'\'' {
                if index + 1 < bytes.len() && bytes[index + 1] == b'\'' {
                    bound.push(b'\'');
                    index += 2;
                } else {
                    in_single = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }
        if in_double {
            bound.push(ch);
            if ch == b'"' {
                if index + 1 < bytes.len() && bytes[index + 1] == b'"' {
                    bound.push(b'"');
                    index += 2;
                } else {
                    in_double = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }
        match ch {
            b'-' if index + 1 < bytes.len() && bytes[index + 1] == b'-' => {
                bound.extend_from_slice(b"--");
                in_line_comment = true;
                index += 2;
            }
            b'\'' => {
                bound.push(b'\'');
                in_single = true;
                index += 1;
            }
            b'"' => {
                bound.push(b'"');
                in_double = true;
                index += 1;
            }
            b'$' if index + 1 < bytes.len() && bytes[index + 1] == b'$' => {
                bound.extend_from_slice(b"$$");
                in_dollar = true;
                index += 2;
            }
            b'$' => {
                let mut end = index + 1;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > index + 1 {
                    if let Ok(number) = query[index + 1..end].parse::<usize>() {
                        if number >= 1 {
                            if let Some(value) = parameters.get(number - 1) {
                                bound.extend_from_slice(
                                    value
                                        .as_deref()
                                        .map(quote_bound_value)
                                        .unwrap_or_else(|| "NULL".to_string())
                                        .as_bytes(),
                                );
                            } else {
                                bound.extend_from_slice(&bytes[index..end]);
                            }
                            index = end;
                            continue;
                        }
                    }
                    bound.extend_from_slice(&bytes[index..end]);
                    index = end;
                } else {
                    bound.push(ch);
                    index += 1;
                }
            }
            _ => {
                bound.push(ch);
                index += 1;
            }
        }
    }
    String::from_utf8(bound).unwrap_or_else(|_| query.to_string())
}

fn explicit_parameter_cast(query: &str, parameter: usize) -> Option<u32> {
    let marker = format!("${parameter}");
    let lower = query.to_ascii_lowercase();
    let start = lower.find(&marker)? + marker.len();
    let suffix = lower[start..].trim_start();
    let suffix = suffix.strip_prefix("::")?.trim_start();
    let type_name = suffix
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .next()
        .filter(|name| !name.is_empty())?;
    let is_array = suffix[type_name.len()..].starts_with("[]");
    let ty = plomid_types::PgType::by_name(type_name)?;
    if is_array {
        ty.array_oid().map(|oid| oid.0)
    } else {
        Some(ty.oid().0)
    }
}

fn parse_execute(payload: &[u8]) -> std::io::Result<(String, u32)> {
    let (portal, rest) = read_cstring(payload, "portal name")?;
    if rest.len() != 4 {
        return Err(protocol_error("invalid Execute message"));
    }
    Ok((
        portal,
        u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]),
    ))
}

fn parse_close(payload: &[u8]) -> std::io::Result<Option<(u8, String)>> {
    let (kind, rest) = payload
        .split_first()
        .ok_or_else(|| protocol_error("truncated Close message"))?;
    let (name, _) = read_cstring(rest, "Close name")?;
    Ok(Some((*kind, name)))
}

fn show_parameter_name(sql: &str) -> Option<String> {
    let words = sql
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>();
    if words.len() == 2 && words[0].eq_ignore_ascii_case("show") {
        if matches!(
            words[1].to_ascii_lowercase().as_str(),
            "tables" | "table" | "database" | "databases" | "schema" | "schemas" | "schema_name"
        ) {
            None
        } else {
            Some(words[1].to_string())
        }
    } else {
        None
    }
}

/// Returns true when a Describe can safely execute the statement to infer
/// result metadata. Only read-only result-producing queries (SELECT, set
/// operations, EXPLAIN, VALUES) qualify. Transaction-control statements
/// (BEGIN/COMMIT/ROLLBACK/SAVEPOINT/RELEASE) and DML (INSERT/UPDATE/DELETE)
/// must never be run during Describe: transaction statements have no result
/// set, and executing DML would either double-apply writes or, for BEGIN,
/// raise "BEGIN without COMMIT or ROLLBACK" from the executor.
fn is_describe_query(stmt: &str) -> bool {
    // SHOW is read-only and returns a result set (e.g. "search_path"), so it
    // is safe to describe by executing.
    matches!(stmt, "SELECT" | "EXPLAIN" | "VALUES" | "SHOW")
}

fn set_parameter(sql: &str) -> Option<(String, String)> {
    let body = sql.trim().trim_end_matches(';');
    let mut words = body.split_whitespace().peekable();
    if !words.next()?.eq_ignore_ascii_case("set") {
        return None;
    }
    if words.peek().is_some_and(|word| {
        word.eq_ignore_ascii_case("session") || word.eq_ignore_ascii_case("local")
    }) {
        words.next();
    }
    let first = words.next()?.to_string();
    if first.eq_ignore_ascii_case("transaction") {
        return parse_set_transaction_options(words);
    }
    let mut name = first;
    if name.eq_ignore_ascii_case("characteristics")
        && words
            .peek()
            .is_some_and(|word| word.eq_ignore_ascii_case("as"))
    {
        words.next();
        if words
            .peek()
            .is_some_and(|word| word.eq_ignore_ascii_case("transaction"))
        {
            words.next();
        }
        return parse_set_transaction_options(words);
    }
    if name.eq_ignore_ascii_case("time")
        && words
            .peek()
            .is_some_and(|word| word.eq_ignore_ascii_case("zone"))
    {
        words.next();
        name = "TimeZone".to_string();
    } else if name.eq_ignore_ascii_case("names") {
        name = "client_encoding".to_string();
    }
    let operator = words.next()?;
    if operator != "=" && !operator.eq_ignore_ascii_case("to") {
        return None;
    }
    let value = words
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches('\'')
        .to_string();
    Some((name, value))
}

fn parse_set_transaction_options<'a, I>(mut words: I) -> Option<(String, String)>
where
    I: Iterator<Item = &'a str> + Clone,
{
    let mut result: Option<(String, String)> = None;
    while let Some(word) = words.next() {
        if word.eq_ignore_ascii_case("isolation") {
            if words
                .next()
                .is_some_and(|w| w.eq_ignore_ascii_case("level"))
            {
                let mut level_words: Vec<&str> = Vec::new();
                for w in words.by_ref() {
                    if w.eq_ignore_ascii_case("read")
                        || w.eq_ignore_ascii_case("write")
                        || w.eq_ignore_ascii_case("only")
                        || w.eq_ignore_ascii_case("deferrable")
                        || w.eq_ignore_ascii_case("not")
                    {
                        break;
                    }
                    level_words.push(w);
                }
                if !level_words.is_empty() {
                    result = Some((
                        "isolation_level".to_string(),
                        level_words.join(" ").to_ascii_lowercase(),
                    ));
                }
            }
        } else if word.eq_ignore_ascii_case("read") {
            if let Some(next) = words.next() {
                if next.eq_ignore_ascii_case("write") {
                    result = Some(("transaction_read_only".to_string(), "off".to_string()));
                } else if next.eq_ignore_ascii_case("only") {
                    result = Some(("transaction_read_only".to_string(), "on".to_string()));
                }
            }
        } else if word.eq_ignore_ascii_case("deferrable") {
            result = Some(("transaction_deferrable".to_string(), "on".to_string()));
        } else if word.eq_ignore_ascii_case("not") {
            if words
                .next()
                .is_some_and(|w| w.eq_ignore_ascii_case("deferrable"))
            {
                result = Some(("transaction_deferrable".to_string(), "off".to_string()));
            }
        }
    }
    result
}

fn parse_search_path(value: Option<&str>) -> Vec<String> {
    let Some(value) = value else {
        return vec!["public".to_string()];
    };
    let mut path = Vec::new();
    for item in value.split(',') {
        let schema = item.trim().trim_matches('"').trim_matches('\'');
        if !schema.is_empty() && !path.iter().any(|known| known == schema) {
            path.push(schema.to_string());
        }
    }
    if path.is_empty() {
        path.push("public".to_string());
    }
    path
}

/// Splits a simple-protocol Query string at top-level semicolons, ignoring
/// semicolons inside single/double-quoted strings, dollar-quoted bodies, line
/// comments, and parenthesized expressions. Used to keep multi-statement
/// batches containing SET/SHOW classified per statement.
/// True when `sql` is a transaction-control statement, which must never be
/// separated from the rest of its batch: it either opens a transaction the
/// following statements belong to, or ends one for all of them.
fn is_transaction_control(sql: &str) -> bool {
    let leading = sql
        .split_whitespace()
        .next()
        .map(|word| word.trim_end_matches(';'));
    matches!(
        leading,
        Some(word)
            if word.eq_ignore_ascii_case("BEGIN")
                || word.eq_ignore_ascii_case("COMMIT")
                || word.eq_ignore_ascii_case("ROLLBACK")
                || word.eq_ignore_ascii_case("SAVEPOINT")
                || word.eq_ignore_ascii_case("RELEASE")
                || word.eq_ignore_ascii_case("START")
                || word.eq_ignore_ascii_case("END")
    )
}

fn split_simple_protocol_statements(sql: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_line_comment = false;
    let mut in_dollar = false;
    let mut depth: usize = 0;
    while let Some(ch) = chars.next() {
        if in_line_comment {
            current.push(ch);
            if ch == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_dollar {
            current.push(ch);
            if ch == '$' && chars.peek() == Some(&'$') {
                current.push(chars.next().unwrap());
                in_dollar = false;
            }
            continue;
        }
        if in_single {
            current.push(ch);
            if ch == '\'' {
                if chars.peek() == Some(&'\'') {
                    current.push(chars.next().unwrap());
                } else {
                    in_single = false;
                }
            }
            continue;
        }
        if in_double {
            current.push(ch);
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    current.push(chars.next().unwrap());
                } else {
                    in_double = false;
                }
            }
            continue;
        }
        match ch {
            '-' if chars.peek() == Some(&'-') => {
                current.push(ch);
                current.push(chars.next().unwrap());
                in_line_comment = true;
            }
            '\'' => {
                in_single = true;
                current.push(ch);
            }
            '"' => {
                in_double = true;
                current.push(ch);
            }
            '$' if chars.peek() == Some(&'$') => {
                current.push(ch);
                current.push(chars.next().unwrap());
                in_dollar = true;
            }
            '(' => {
                depth = depth.saturating_add(1);
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ';' if depth == 0 => {
                current.push(ch);
                if !current.trim().is_empty() {
                    parts.push(current.trim().to_string());
                }
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

fn row_descriptions(
    columns: &[String],
    column_types: &[Option<plomid_sql::ColumnType>],
    result_formats: &[u16],
) -> Vec<crate::protocol::RowFieldDescription> {
    columns
        .iter()
        .zip(column_types.iter())
        .enumerate()
        .map(|(index, (name, column_type))| {
            let (type_oid, type_size) = match column_type {
                Some(ct) => {
                    let oid = ct.type_oid.0;
                    let size = match ct.pg_type() {
                        Some(plomid_types::PgType::Bool) => 1,
                        Some(plomid_types::PgType::Int2) => 2,
                        Some(plomid_types::PgType::Int4) => 4,
                        Some(plomid_types::PgType::Int8) => 8,
                        Some(plomid_types::PgType::Float4) => 4,
                        Some(plomid_types::PgType::Float8) => 8,
                        Some(plomid_types::PgType::Uuid) => 16,
                        _ => -1,
                    };
                    (oid, size)
                }
                None => (25, -1),
            };
            crate::protocol::RowFieldDescription {
                name: name.clone(),
                type_oid,
                type_size,
                type_modifier: column_type.as_ref().map_or(-1, |column| column.typmod),
                format: result_formats
                    .get(index)
                    .copied()
                    .or_else(|| result_formats.first().copied())
                    .map(|format| {
                        if format == 1 && !supports_binary_oid(type_oid) {
                            0
                        } else {
                            format
                        }
                    })
                    .unwrap_or(0),
            }
        })
        .collect()
}

fn read_cstring<'a>(bytes: &'a [u8], field: &str) -> std::io::Result<(String, &'a [u8])> {
    let end = bytes.iter().position(|byte| *byte == 0).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("truncated extended-query {field}"),
        )
    })?;
    let value = std::str::from_utf8(&bytes[..end])
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid UTF-8"))?
        .to_owned();
    Ok((value, &bytes[end + 1..]))
}

#[cfg(test)]
mod sqlstate_tests {
    use super::*;

    fn state(kind: ErrorKind, message: &str) -> &'static str {
        sqlstate_for(kind, message)
    }

    #[test]
    fn object_errors_map_to_postgres_class_42() {
        assert_eq!(state(ErrorKind::Syntax, "syntax error"), "42601");
        assert_eq!(
            state(ErrorKind::NotFound, "table \"missing\" does not exist"),
            "42P01"
        );
        assert_eq!(
            state(ErrorKind::Catalog, "column \"x\" does not exist"),
            "42703"
        );
        assert_eq!(
            state(ErrorKind::Catalog, "function f() does not exist"),
            "42883"
        );
        assert_eq!(
            state(ErrorKind::AlreadyExists, "relation \"t\" already exists"),
            "42710"
        );
    }

    #[test]
    fn constraint_errors_map_to_postgres_class_23() {
        assert_eq!(
            state(
                ErrorKind::Conflict,
                "null value in column \"name\" violates not-null constraint"
            ),
            "23502"
        );
        assert_eq!(
            state(
                ErrorKind::Conflict,
                "duplicate key value violates unique index \"users_pkey\""
            ),
            "23505"
        );
        assert_eq!(
            state(
                ErrorKind::Conflict,
                "value in column \"age\" violates check constraint"
            ),
            "23514"
        );
    }

    #[test]
    fn aborted_transaction_maps_to_25p02() {
        assert_eq!(
            state(
                ErrorKind::Conflict,
                "current transaction is aborted, commands ignored until ROLLBACK"
            ),
            "25P02"
        );
    }

    #[test]
    fn data_and_transaction_errors_map_to_their_classes() {
        assert_eq!(
            state(ErrorKind::InvalidArgument, "division by zero"),
            "22012"
        );
        assert_eq!(state(ErrorKind::Transaction, "txn"), "25P02");
        assert_eq!(state(ErrorKind::Unsupported, "feature"), "0A000");
        assert_eq!(state(ErrorKind::Internal, "boom"), "XX000");
    }
}

#[cfg(test)]
mod bind_tests {
    use super::*;

    fn some(values: &[&str]) -> Vec<Option<String>> {
        values.iter().map(|v| Some(v.to_string())).collect()
    }

    #[test]
    fn multi_digit_placeholders_bind_independently() {
        let bound = bind_query(
            "SELECT $1, $2, $10",
            &some(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]),
        );
        assert_eq!(bound, "SELECT 'a', 'b', 'j'");
    }

    #[test]
    fn placeholders_inside_literals_are_preserved() {
        let bound = bind_query("SELECT 'a$1b' AS t, $1 AS v", &some(&["X"]));
        assert_eq!(bound, "SELECT 'a$1b' AS t, 'X' AS v");
    }

    #[test]
    fn placeholders_inside_comments_and_dollar_bodies_are_preserved() {
        let bound = bind_query("SELECT $1 -- $2\n", &some(&["7", "8"]));
        assert_eq!(bound, "SELECT 7 -- $2\n");
        let bound = bind_query("SELECT $$ $1 $$, $1", &some(&["7"]));
        assert_eq!(bound, "SELECT $$ $1 $$, 7");
    }

    #[test]
    fn missing_values_stay_placeholders_or_null() {
        // Supplied None renders NULL; a $N beyond the supplied slice is left
        // for the executor to reject rather than silently corrupted.
        assert_eq!(bind_query("SELECT $1", &[None]), "SELECT NULL");
        assert_eq!(bind_query("SELECT $2", &some(&["7"])), "SELECT $2");
    }

    #[test]
    fn max_placeholder_counts_references_outside_literals() {
        assert_eq!(max_placeholder("SELECT $1, $10"), 10);
        assert_eq!(max_placeholder("SELECT 'a$99b', $2"), 2);
        assert_eq!(max_placeholder("SELECT 1"), 0);
        assert_eq!(max_placeholder("SELECT -- $5\n$3"), 3);
    }

    #[test]
    fn text_null_stays_a_string() {
        assert_eq!(quote_bound_value("null"), "'null'");
        assert_eq!(quote_bound_value("NULL"), "'NULL'");
        assert_eq!(quote_bound_value("42"), "42");
        assert_eq!(quote_bound_value("o'brien"), "'o''brien'");
    }

    #[test]
    fn utf8_query_text_round_trips() {
        let bound = bind_query("SELECT 'héllo $1 wörld', $1", &some(&["✓"]));
        assert_eq!(bound, "SELECT 'héllo $1 wörld', '✓'");
    }
}
