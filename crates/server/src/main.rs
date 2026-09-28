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
//! PLOMID database server binary.
//!
//! Entry point for the `plomid-server` command. Parses command-line
//! arguments, initializes the storage engine, opens the executor, and
//! starts the TCP listener.

use plomid_network::server::PlomidServer;
use plomid_network::session::ServerAuthConfig;
use plomid_txn::{CheckpointTrigger, PlomidStorageEngine};
use std::sync::Arc;
use std::sync::Mutex;
use tracing::{error, info, info_span, warn};
use tracing_subscriber::{
    fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter,
};

mod config;

use config::ServerConfig;

#[tokio::main]
async fn main() {
    let config = match ServerConfig::parse() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("configuration error: {e}");
            std::process::exit(1);
        }
    };

    init_logging(&config);

    print_startup_banner(&config);
    let startup_span = info_span!(
        target: "server",
        "server",
        service = "plomid",
        host = %config.host,
        port = config.port,
        data_dir = %config.data_dir.display(),
    );
    startup_span.in_scope(|| {
        info!(target: "server", event = "server_start", auth = true);
    });

    if let Err(e) = std::fs::create_dir_all(&config.data_dir) {
        error!(target: "server", "data_dir_create_failed error={}", e);
        std::process::exit(1);
    }

    let storage_path = config.data_dir.clone();
    let wal_path = config.data_dir.join("wal");

    // Keep the hot working set resident during bulk inserts and scans. The
    // previous 64-page pool caused avoidable eviction/reload churn once a
    // table and its catalog/index pages exceeded the tiny default.
    let mut engine = match PlomidStorageEngine::open(&storage_path, &wal_path, 1024) {
        Ok(engine) => engine,
        Err(e) => {
            error!(target: "server", "storage_open_failed error={}", e);
            std::process::exit(1);
        }
    };
    // Automatic WAL checkpointing. Without it, a long-running server retains
    // WAL until an explicit checkpoint, which is the difference between a
    // bounded restart and one that replays every commit since startup.
    engine.set_checkpoint_policy(config.checkpoint_policy());
    info!(target: "server", event = "storage_ready", path = %storage_path.display());

    let engine = Arc::new(Mutex::new(engine));
    let auth_config = match config.auth_method.as_ref() {
        "none" | "disabled" => ServerAuthConfig::disabled(),
        "md5" => ServerAuthConfig::md5(config.username, config.password),
        "password" | "cleartext" => ServerAuthConfig::new(config.username, config.password),
        // Default and preferred: SCRAM-SHA-256.
        "scram" => ServerAuthConfig::scram(config.username, config.password),
        other => {
            error!(target: "server", "unknown_auth_method method={}", other);
            std::process::exit(1);
        }
    };

    if config.tls.enabled {
        let cert_empty = config.tls.cert.display().to_string().is_empty();
        let key_empty = config.tls.key.display().to_string().is_empty();
        if cert_empty || key_empty {
            error!(
                target: "server",
                "tls_requires_cert_and_key; pass --tls-cert and --tls-key"
            );
            std::process::exit(1);
        }
        error!(
            target: "server",
            "tls_mode_not_yet_implemented; SSL negotiation is declined and clients fall back to plaintext"
        );
        std::process::exit(1);
    }

    let addr = std::net::SocketAddr::new(config.host, config.port);
    if config.allow_remote_plaintext && !config.host.is_loopback() {
        // Repeated on every start on purpose: this is the one configuration in
        // which credentials cross the network unencrypted. Authentication is
        // still enforced, so the warning is about transport, not access.
        warn!(
            target: "server",
            event = "insecure_remote_bind",
            host = %config.host,
            "accepting remote clients without TLS; credentials are sent in the clear. \
             Restrict this listener to trusted networks"
        );
    }
    let mut server = match PlomidServer::bind(addr, auth_config).await {
        Ok(server) => server,
        Err(e) => {
            error!(target: "server", "bind_failed error={}", e);
            std::process::exit(1);
        }
    };
    // Background maintenance moves generation passes off committing
    // connections: sessions submit due tables instead of materializing them
    // inline. The worker derives its own engine facade, so passes never
    // borrow session state; a full queue (or a stopped worker) degrades to
    // inline passes, exactly the historical behavior.
    let maintenance_worker = plomid_network::MaintenanceWorker::spawn(Arc::clone(&engine));
    server.set_maintenance_link(maintenance_worker.link());
    info!(target: "server", event = "network_ready", listening_on = %server.local_addr());

    let actual_addr = server.local_addr();
    startup_span.in_scope(|| {
        info!(
            target: "server",
            event = "server_ready",
            listening_on = %actual_addr,
            storage = %storage_path.display(),
            wal = %wal_path.display(),
            "PLOMID is ready to accept connections"
        );
    });

    // Graceful shutdown. Without a signal handler the process dies wherever the
    // kernel's SIGTERM lands, leaving every commit since the last checkpoint for
    // the next start to replay -- seconds of WAL replay on a busy system, and a
    // startup that grows with the write history. On SIGINT/SIGTERM the listener
    // stops accepting, in-flight statements are given a bounded window to
    // finish, and one final checkpoint flushes data pages, publishes the
    // recovery boundary, and reclaims the WAL it made unreachable. Durability is
    // unchanged: the checkpoint is the same crash-safe path used at runtime, and
    // a kill between commits is still recovered from the WAL.
    let live = server.live_connections();
    let engine_for_shutdown = Arc::clone(&engine);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let mut drained_cleanly = true;
    let mut server_run = Box::pin(server.run_with_database_root(engine, config.data_dir.clone()));
    tokio::select! {
        _ = &mut server_run => {}
        _ = &mut shutdown => {
            info!(target: "server", event = "shutdown_requested");
            // Dropping the accept loop closes the listener; existing handlers
            // keep running until they finish or the grace period expires.
            drop(server_run);
            let outstanding = plomid_network::server::PlomidServer::drain(
                &live,
                std::time::Duration::from_secs(SHUTDOWN_GRACE_SECS),
            )
            .await;
            if outstanding > 0 {
                drained_cleanly = false;
                warn!(
                    target: "server",
                    event = "shutdown_drain_timeout",
                    connections = outstanding,
                    "grace period expired with connections still active"
                );
            }
        }
    }

    // Stop background maintenance before the final checkpoint: queued items
    // are dropped (their debt re-derives from session accounting), and the
    // join is bounded by one in-flight pass, which is crash-safe at every
    // boundary like a kill during VACUUM.
    let (submitted, completed, inline_fallbacks) = maintenance_worker.link().stats();
    maintenance_worker.shutdown();
    info!(
        target: "server",
        event = "maintenance_worker_shutdown",
        submitted,
        completed,
        inline_fallbacks,
        queue_depth = 0,
    );

    let checkpoint = engine_for_shutdown.lock().ok().and_then(|mut engine| {
        engine
            .checkpoint_with_report(CheckpointTrigger::Shutdown)
            .ok()
    });
    match checkpoint {
        Some(outcome) => info!(
            target: "server",
            event = "shutdown_checkpoint",
            lsn = outcome.lsn.get(),
            duration_ms = outcome.duration.as_millis() as u64,
            reclaimed_segments = outcome.reclaimed_segments,
            wal_bytes = outcome.wal_bytes,
        ),
        None => warn!(
            target: "server",
            event = "shutdown_checkpoint_failed",
            "final checkpoint did not complete; the next start will replay the WAL tail"
        ),
    }

    info!(
        target: "server",
        event = "server_shutdown",
        drained_cleanly,
    );
}

/// How long a shutdown waits for in-flight statements before checkpointing.
const SHUTDOWN_GRACE_SECS: u64 = 5;

/// Resolves when the process is asked to stop (Ctrl-C or SIGTERM).
///
/// `docker stop`, systemd, Kubernetes and a plain `kill` all send SIGTERM, so
/// handling only Ctrl-C would leave every deploy path with an unclean stop.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(e) => {
                error!(target: "server", "sigterm_handler_failed error={}", e);
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn init_logging(config: &ServerConfig) {
    // INFO stays the default so operators keep visibility, but every
    // per-operation event is emitted at DEBUG: the `plomid::perf` stage timers
    // (`mvcc_get`, `mvcc_begin`, `commit_prepare`, `commit_publish`,
    // `mvcc_install`, `storage_apply`, `group_flush`, `txn_commit`, the
    // update/delete stage timers), the `sql::txn` transaction lifecycle, the
    // `write_gate` events, and `wire_out`/query-lifecycle protocol messages.
    //
    // Several of those timers run once per point read / per commit *inside*
    // the shared engine mutex, so emitting them at INFO did not just add log
    // volume -- it serialized every connection behind stdout writes. INFO now
    // carries the low-volume operational record (startup, connections, DDL,
    // maintenance passes and failures); the detailed per-operation timings are
    // one flag away via `PLOMID_LOG_LEVEL=debug` or
    // `RUST_LOG=plomid::perf=debug`.
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(match config.log_level.as_deref() {
            Some("trace") => "trace",
            Some("debug") => "debug",
            Some("warn") => "warn",
            Some("error") => "error",
            _ => "info",
        })
    });

    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(ansi)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(false)
        .with_line_number(false)
        .with_span_events(FmtSpan::CLOSE);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .init();
}

fn print_startup_banner(config: &ServerConfig) {
    let color = std::io::IsTerminal::is_terminal(&std::io::stdout());
    // PLOMID brand: copper accent (#e2683c); status greens/yellows keep
    // their meaning. Ink (#08090b) / graphite (#14171b) / paper (#f3f1ec)
    // stay with the terminal: forcing grounds breaks light/dark themes, so
    // the banner only paints the accent and lets the background show through.
    let (accent, green, yellow, dim, bold, reset) = if color {
        (
            "\x1b[38;2;226;104;60m",
            "\x1b[32m",
            "\x1b[33m",
            "\x1b[2m",
            "\x1b[1m",
            "\x1b[0m",
        )
    } else {
        ("", "", "", "", "", "")
    };

    // Visible (non-ANSI) length, so box edges stay aligned with colors present.
    fn vis(s: &str) -> usize {
        let mut n = 0;
        let mut esc = false;
        for ch in s.chars() {
            if esc {
                if ch == 'm' {
                    esc = false;
                }
                continue;
            }
            if ch == '\x1b' {
                esc = true;
                continue;
            }
            n += 1;
        }
        n
    }
    // Inner width of every panel line.
    const W: usize = 76;
    let top = format!("  ┌{}┐", "─".repeat(W));
    let bottom = format!("  └{}┘", "─".repeat(W));
    let row = |content: &str| -> String {
        let need = W.saturating_sub(vis(content));
        let mut padded = String::from("  │ ");
        padded.push_str(content);
        padded.extend(std::iter::repeat_n(' ', need));
        padded.push('│');
        padded
    };

    // Aligned monospaced block-wordmark: P · L · O · M · I · D

    let logo = [

    "                                                              ÆÆ                                                              ",
    "                                                         ÆÆÆÆÆÆÆÆÆÆ                                                         ",
    "                                                      ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                                       ",
    "                                                  ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                                    ",
    "                                               ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                                ",
    "                                           ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                             ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ   ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ       ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ             ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆÆ                   ÆÆÆÆÆÆÆÆÆÆÆÆ                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ                     ÆÆÆÆÆÆÆÆÆÆÆ                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ                     ÆÆÆÆÆÆÆÆÆÆÆ                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ                   ÆÆÆÆÆÆÆÆÆÆÆÆÆ                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ                ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ            ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ          ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                           ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ          ÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                               ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ          ÆÆÆÆÆÆÆÆÆÆÆÆÆÆ                                                 ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ          ÆÆÆÆÆÆÆÆÆÆ                                                      ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ          ÆÆÆÆÆÆÆ                                                         ",
    "                                        ÆÆÆÆÆÆÆÆÆÆÆ          ÆÆÆ                                                             ",
    "                                         ÆÆÆÆÆÆÆÆÆÆ                                                                         ",
    "                                            ÆÆÆÆÆÆÆ                                                                         ",
    "                                                ÆÆÆ                                                                         ",
    "                                                                                                                            ",
    "                 ÆÆÆÆÆÆÆÆÆÆÆÆÆ    ÆÆ               ÆÆÆÆÆÆÆÆÆÆÆ      ÆÆÆ        ÆÆÆÆ    ÆÆ    ÆÆÆÆÆÆÆÆÆÆÆÆÆ                  ",
    "                            ÆÆ    ÆÆÆ            ÆÆÆ        ÆÆÆÆ    ÆÆÆÆÆ    ÆÆÆÆÆÆ    ÆÆ               ÆÆÆ                 ",
    "                           ÆÆÆ    ÆÆÆ            ÆÆ          ÆÆÆ    ÆÆ  ÆÆÆÆÆÆÆ ÆÆÆ    ÆÆ    ÆÆÆ        ÆÆÆ                 ",
    "                 ÆÆÆÆÆÆÆÆÆÆÆ      ÆÆÆ            ÆÆ          ÆÆÆ    ÆÆ    ÆÆ    ÆÆÆ    ÆÆ    ÆÆÆ        ÆÆÆ                 ",
    "                 ÆÆÆ              ÆÆÆ            ÆÆÆÆ       ÆÆÆ     ÆÆ          ÆÆÆ    ÆÆ    ÆÆÆ        ÆÆÆ                 ",
    "                 ÆÆÆ              ÆÆÆÆÆÆÆÆÆÆÆÆ     ÆÆÆÆÆÆÆÆÆÆ       ÆÆ          ÆÆÆ    ÆÆ    ÆÆÆÆÆÆÆÆÆÆÆÆ                   ",

    "                                                                                                                            ",

];

    let version = env!("CARGO_PKG_VERSION");
    let edition = "PostgreSQL wire protocol v3";

    println!();
    println!(
        "{accent}  ┌──────────────────────────────────────────────────────────────────────┐{reset}"
    );
    println!(
        "{accent}  │{reset}  {bold}{accent}PLOMID{reset}  {dim} - Platform for Modern Intelligence and Data SYSTEM{reset}  {dim}v{version}{reset} {accent}│{reset}"
    );
    println!(
        "{accent}  └──────────────────────────────────────────────────────────────────────┘{reset}"
    );
    println!();

    for line in &logo {
        println!("{bold}{accent}     {line}{reset}");
    }
    println!();
    println!("{dim}       autonomous storage · pg-wire compatible · {edition}{reset}");
    println!();

    // ── boot checklist (system status) ─────────────────────────────────
    println!("{dim}  boot ⋯ {reset}{accent}system status{reset}");
    println!("{top}");
    let boot_rows: [(&str, &str, &str); 4] = [
        ("storage engine", "initialising", yellow),
        ("write-ahead log", "mounting", yellow),
        ("query executor", "booting", yellow),
        ("network listener", "binding", yellow),
    ];
    for (n, st, c) in boot_rows {
        let line = format!("{c}◌{reset}  {bold}{}{reset}  {dim}{:>14}{reset}", n, st);
        println!("{}", row(&line));
    }
    println!(
        "{}",
        row(&format!(
            "{yellow}◌{reset}  {bold}PLOMID{reset}  {dim}starting storage and network services{reset}"
        ))
    );
    println!("{bottom}");
    println!();

    // ── connection / configuration panel ──────────────────────────────
    println!("{dim}  configuration{reset}");
    println!("{top}");
    let auth_desc = match config.auth_method.as_ref() {
        "none" | "disabled" => "disabled".to_string(),
        "md5" => "enabled (md5)".to_string(),
        "password" | "cleartext" => "enabled (password)".to_string(),
        _ => "enabled (scram-sha-256)".to_string(),
    };
    let tls_desc = if config.tls.enabled {
        format!(
            "tls {}",
            if config.tls.required {
                "required"
            } else {
                "preferred"
            }
        )
    } else {
        "tls off".to_string()
    };
    let cfg: [(&str, String); 6] = [
        ("listen", config.host.to_string()),
        ("port", config.port.to_string()),
        ("data dir", config.data_dir.display().to_string()),
        ("role", config.username.clone()),
        ("auth", auth_desc),
        ("tls", tls_desc),
    ];
    for (k, v) in cfg {
        let line = format!(
            "{dim}{key:>10}{reset}  {accent}│{reset}  {green}{value}{reset}",
            key = k,
            value = v,
        );
        println!("{}", row(&line));
    }
    println!("{bottom}");
    println!();

    println!(
        "{dim}  address  {accent}{bold}{}:{}{reset}   {dim}edition  {accent}{}{reset}   {dim}platform  {accent}{}/{}{reset}",
        config.host, config.port, edition, std::env::consts::OS, std::env::consts::ARCH,
    );
    println!("{dim}  ctrl-c to stop{reset}   {dim}debug  {yellow}PLOMID_LOG_LEVEL=debug{reset}");
    println!();
}
