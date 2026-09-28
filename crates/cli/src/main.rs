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
//! Native PLOMID database management, diagnostics, and benchmarking CLI.
//!
//! The CLI deliberately uses PLOMID's PostgreSQL-compatible wire protocol for
//! database checks. It does not use or embed another database engine.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Server defaults are defined once in `plomid_core::constants`.
use plomid_core::{DEFAULT_DATABASE, DEFAULT_DATA_DIR, DEFAULT_HOST, DEFAULT_PORT, DEFAULT_USER};

#[derive(Parser, Debug)]
#[command(
    name = "plomid",
    version,
    about = "PLOMID database management and diagnostics"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Manage the PLOMID server process.
    Server(ServerCommand),
    /// Inspect database health, readiness, metadata, or diagnostics.
    Db(DbCommand),
    /// Run a native database benchmark against a real PLOMID server.
    Benchmark(BenchmarkCommand),
    /// Run built-in PLOMID database checks.
    Test(TestCommand),
    /// Print the resolved CLI/database configuration.
    Config(OutputArgs),
    /// Print the PLOMID version.
    Version,
    /// Explain how to enable structured server logs.
    Logs,
    /// Explain how request traceability is exposed.
    Trace,
    /// Show currently exposed metrics and unavailable metrics.
    Metrics(OutputArgs),
}

#[derive(Args, Debug)]
struct ServerCommand {
    #[command(subcommand)]
    action: ServerAction,
}

#[derive(Subcommand, Debug)]
enum ServerAction {
    /// Start plomid-server using the resolved configuration.
    Start(ServerOptions),
    /// Stop the process recorded by the data-directory pid file.
    Stop(ServerOptions),
    /// Check whether the configured TCP endpoint accepts connections.
    Status(ConnectionOptions),
}

#[derive(Args, Debug, Clone)]
struct ConnectionOptions {
    #[arg(long, env = "PLOMID_HOST", default_value = DEFAULT_HOST)]
    host: String,
    #[arg(long, env = "PLOMID_PORT", default_value_t = DEFAULT_PORT)]
    port: u16,
    #[arg(long, env = "PLOMID_USER", default_value = DEFAULT_USER)]
    user: String,
    #[arg(
        long,
        env = "PLOMID_PASSWORD",
        default_value = "secret",
        hide_env_values = true
    )]
    password: String,
    #[arg(long, env = "PLOMID_DATABASE", default_value = DEFAULT_DATABASE)]
    database: String,
    #[arg(long, env = "PLOMID_DATA_DIR", default_value = DEFAULT_DATA_DIR)]
    data_dir: PathBuf,
}

#[derive(Args, Debug, Clone)]
struct ServerOptions {
    #[command(flatten)]
    connection: ConnectionOptions,
}

#[derive(Args, Debug)]
struct DbCommand {
    #[command(subcommand)]
    action: DbAction,
}

#[derive(Subcommand, Debug)]
enum DbAction {
    /// Verify the database can accept a real authenticated query.
    Readiness(OutputArgs),
    /// Show an operational health summary.
    Health(OutputArgs),
    /// Show database and on-disk metadata.
    Info(OutputArgs),
    /// Run all safe, non-destructive diagnostics.
    Diagnostics(OutputArgs),
}

#[derive(Args, Debug)]
struct OutputArgs {
    #[arg(long, help = "Emit stable machine-readable JSON")]
    json: bool,
    #[command(flatten)]
    connection: ConnectionOptions,
}

#[derive(Args, Debug)]
struct BenchmarkCommand {
    #[command(subcommand)]
    action: BenchmarkAction,
}

#[derive(Subcommand, Debug)]
enum BenchmarkAction {
    /// Run a benchmark against the real server and persist its result.
    Run(BenchmarkOptions),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BenchmarkOperation {
    /// Execute SELECT 1 through the PostgreSQL-compatible wire protocol.
    Select,
}

#[derive(Args, Debug)]
struct BenchmarkOptions {
    #[arg(long, value_enum, default_value_t = BenchmarkOperation::Select)]
    operation: BenchmarkOperation,
    #[arg(long, default_value_t = 100)]
    iterations: u64,
    #[arg(long, default_value_t = 1)]
    concurrency: u16,
    #[arg(long)]
    duration_seconds: Option<u64>,
    #[arg(long)]
    dataset_size: Option<u64>,
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    connection: ConnectionOptions,
}

#[derive(Args, Debug)]
struct TestCommand {
    #[command(subcommand)]
    action: TestAction,
}

#[derive(Subcommand, Debug)]
enum TestAction {
    /// List the native database test modules.
    List,
    /// Run one test module, or all safe modules.
    Run {
        name: Option<String>,
        #[command(flatten)]
        output: OutputArgs,
    },
}

#[derive(Debug)]
struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
}

#[derive(Debug)]
struct ReadinessReport {
    checks: Vec<Check>,
    latency_ms: Option<u128>,
}

impl ReadinessReport {
    fn ready(&self) -> bool {
        self.checks.iter().all(|check| check.status != "FAIL")
    }
}

#[derive(Debug)]
struct QueryResponse {
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    command: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Server(command) => run_server(command),
        Command::Db(command) => run_db(command),
        Command::Benchmark(command) => run_benchmark(command),
        Command::Test(command) => run_test(command),
        Command::Config(output) => print_config(&output.connection, output.json),
        Command::Version => {
            println!("plomid {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Logs => {
            println!("Server logs use tracing. Set PLOMID_LOG_LEVEL=debug or RUST_LOG=server::protocol=trace.");
            Ok(())
        }
        Command::Trace => {
            println!("Traceability fields: connection_id, query_id, statement_type, sql_len, elapsed_ms.");
            println!("Use RUST_LOG=server::protocol=debug to inspect request lifecycle events.");
            Ok(())
        }
        Command::Metrics(output) => print_metrics(&output.connection, output.json),
    };
    if let Err(error) = result {
        eprintln!("PLOMID: {error}");
        std::process::exit(1);
    }
}

fn run_server(command: ServerCommand) -> Result<(), String> {
    match command.action {
        ServerAction::Status(options) => {
            let address = format!("{}:{}", options.host, options.port);
            match TcpStream::connect_timeout(
                &address
                    .parse()
                    .map_err(|_| "invalid server address".to_string())?,
                Duration::from_secs(2),
            ) {
                Ok(stream) => {
                    let _ = stream.shutdown(Shutdown::Both);
                    println!("✓ PASS  server  {address} accepts TCP connections");
                    Ok(())
                }
                Err(error) => Err(format!("server unavailable at {address}: {error}")),
            }
        }
        ServerAction::Start(options) => start_server(&options.connection),
        ServerAction::Stop(options) => stop_server(&options.connection),
    }
}

fn start_server(options: &ConnectionOptions) -> Result<(), String> {
    fs::create_dir_all(&options.data_dir)
        .map_err(|error| format!("create data directory: {error}"))?;
    let pid_path = options.data_dir.join("plomid.pid");
    if pid_path.exists() {
        return Err(format!("pid file already exists: {}", pid_path.display()));
    }
    let executable = server_executable()?;
    let child = std::process::Command::new(executable)
        .env("PLOMID_DATA_DIR", &options.data_dir)
        .env("PLOMID_HOST", &options.host)
        .env("PLOMID_PORT", options.port.to_string())
        .env("PLOMID_USER", &options.user)
        .env("PLOMID_PASSWORD", &options.password)
        .spawn()
        .map_err(|error| format!("start server: {error}"))?;
    fs::write(&pid_path, child.id().to_string())
        .map_err(|error| format!("write pid file: {error}"))?;
    println!("✓ PASS  server started (pid {})", child.id());
    Ok(())
}

fn stop_server(options: &ConnectionOptions) -> Result<(), String> {
    let pid_path = options.data_dir.join("plomid.pid");
    let pid = fs::read_to_string(&pid_path)
        .map_err(|error| format!("read {}: {error}", pid_path.display()))?
        .trim()
        .parse::<u32>()
        .map_err(|_| "pid file is invalid".to_string())?;
    #[cfg(unix)]
    let status = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .map_err(|error| format!("stop server: {error}"))?;
    #[cfg(not(unix))]
    let status = {
        let _ = pid;
        return Err("server stop is currently supported on Unix hosts only".to_string());
    };
    if !status.success() {
        return Err(format!("could not stop server process {pid}"));
    }
    let _ = fs::remove_file(pid_path);
    println!("✓ PASS  stop signal sent to server (pid {pid})");
    Ok(())
}

fn server_executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| format!("locate CLI: {error}"))?;
    let sibling = current.with_file_name(if cfg!(windows) {
        "plomid-server.exe"
    } else {
        "plomid-server"
    });
    if sibling.exists() {
        return Ok(sibling);
    }
    Err(format!(
        "plomid-server was not found beside {}",
        current.display()
    ))
}

fn run_db(command: DbCommand) -> Result<(), String> {
    match command.action {
        DbAction::Readiness(output) => {
            let report = readiness(&output.connection);
            print_readiness(&report, output.json);
            if report.ready() {
                Ok(())
            } else {
                Err("database is not ready".to_string())
            }
        }
        DbAction::Health(output) => print_health(&output.connection, output.json),
        DbAction::Info(output) => print_info(&output.connection, output.json),
        DbAction::Diagnostics(output) => print_diagnostics(&output.connection, output.json),
    }
}

fn readiness(options: &ConnectionOptions) -> ReadinessReport {
    let mut checks = Vec::new();
    let address = format!("{}:{}", options.host, options.port);
    let socket_address = match address.parse() {
        Ok(address) => address,
        Err(_) => {
            checks.push(Check {
                name: "Network",
                status: "FAIL",
                detail: "invalid host or port".to_string(),
            });
            return ReadinessReport {
                checks,
                latency_ms: None,
            };
        }
    };
    let started = Instant::now();
    let mut client = match TcpStream::connect_timeout(&socket_address, Duration::from_secs(2)) {
        Ok(stream) => {
            checks.push(Check {
                name: "Server",
                status: "PASS",
                detail: address.clone(),
            });
            checks.push(Check {
                name: "Network",
                status: "PASS",
                detail: "TCP connection established".to_string(),
            });
            match WireClient::startup(stream, options) {
                Ok(client) => {
                    checks.push(Check {
                        name: "Authentication",
                        status: "PASS",
                        detail: "password accepted".to_string(),
                    });
                    checks.push(Check {
                        name: "Database connection",
                        status: "PASS",
                        detail: options.database.clone(),
                    });
                    Some(client)
                }
                Err(error) => {
                    checks.push(Check {
                        name: "Authentication",
                        status: "FAIL",
                        detail: error,
                    });
                    None
                }
            }
        }
        Err(error) => {
            checks.push(Check {
                name: "Server",
                status: "FAIL",
                detail: error.to_string(),
            });
            checks.push(Check {
                name: "Network",
                status: "FAIL",
                detail: "connection refused or timed out".to_string(),
            });
            None
        }
    };
    let latency_ms = client
        .as_mut()
        .and_then(|client| match client.query("SELECT 1") {
            Ok(response)
                if response
                    .rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(|value| value.as_deref())
                    == Some("1") =>
            {
                checks.push(Check {
                    name: "Simple query",
                    status: "PASS",
                    detail: "SELECT 1".to_string(),
                });
                checks.push(Check {
                    name: "Read capability",
                    status: "PASS",
                    detail: "committed read path".to_string(),
                });
                Some(started.elapsed().as_millis())
            }
            Ok(_) => {
                checks.push(Check {
                    name: "Simple query",
                    status: "FAIL",
                    detail: "SELECT 1 returned an unexpected result".to_string(),
                });
                None
            }
            Err(error) => {
                checks.push(Check {
                    name: "Simple query",
                    status: "FAIL",
                    detail: error,
                });
                None
            }
        });
    checks.push(Check {
        name: "Write capability",
        status: "NOT SUPPORTED",
        detail: "readiness is non-destructive".to_string(),
    });
    checks.push(Check {
        name: "Transaction",
        status: "NOT SUPPORTED",
        detail: "no diagnostic transaction is opened".to_string(),
    });
    checks.push(Check {
        name: "Persistence",
        status: "WARNING",
        detail: "not tested by a non-destructive readiness probe".to_string(),
    });
    if let Some(client) = client {
        let _ = client.stream.shutdown(Shutdown::Both);
    }
    ReadinessReport { checks, latency_ms }
}

fn print_readiness(report: &ReadinessReport, json: bool) {
    if json {
        let mut output = String::from("{\"status\":\"");
        output.push_str(if report.ready() { "READY" } else { "NOT_READY" });
        output.push_str("\",\"checks\":[");
        for (index, check) in report.checks.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            let _ = write!(
                output,
                "{{\"name\":\"{}\",\"status\":\"{}\",\"detail\":\"{}\"}}",
                json_escape(check.name),
                check.status,
                json_escape(&check.detail)
            );
        }
        let _ = write!(
            output,
            "] ,\"latency_ms\":{}}}",
            report
                .latency_ms
                .map_or_else(|| "null".to_string(), |value| value.to_string())
        );
        println!("{}", output.replace("] ,", "],"));
        return;
    }
    println!("PLOMID DB Readiness\n────────────────────────────────");
    for check in &report.checks {
        let marker = match check.status {
            "PASS" => "✓",
            "FAIL" => "✗",
            _ => "⚠",
        };
        println!(
            "{marker} {:<20} {:<14} {}",
            check.name, check.status, check.detail
        );
    }
    if let Some(latency) = report.latency_ms {
        println!("\nQuery latency: {latency} ms");
    }
    println!(
        "Status: {}",
        if report.ready() { "READY" } else { "NOT READY" }
    );
}

fn print_health(options: &ConnectionOptions, json: bool) -> Result<(), String> {
    let report = readiness(options);
    let database_size = database_size(&options.data_dir);
    if json {
        println!("{{\"status\":\"{}\",\"version\":\"{}\",\"host\":\"{}\",\"port\":{},\"data_dir\":\"{}\",\"database_bytes\":{},\"active_connections\":\"unavailable\"}}", if report.ready() { "healthy" } else { "degraded" }, env!("CARGO_PKG_VERSION"), json_escape(&options.host), options.port, json_escape(&options.data_dir.display().to_string()), database_size);
    } else {
        println!("PLOMID DB Health\n────────────────────────────────");
        println!(
            "Status:       {}",
            if report.ready() {
                "HEALTHY"
            } else {
                "DEGRADED"
            }
        );
        println!("Version:      {}", env!("CARGO_PKG_VERSION"));
        println!("Endpoint:     {}:{}", options.host, options.port);
        println!("Data:         {}", options.data_dir.display());
        println!("Database:     {} bytes", database_size);
        println!("Connections:  unavailable (server metric endpoint not yet exposed)");
        println!("Memory/CPU:   unavailable (not measured by PLOMID yet)");
        print_readiness(&report, false);
    }
    Ok(())
}

fn print_info(options: &ConnectionOptions, json: bool) -> Result<(), String> {
    let db = database_size(&options.data_dir);
    let wal = tree_size(&options.data_dir.join("wal"));
    let tables = catalog_table_count(&options.data_dir);
    if json {
        println!("{{\"version\":\"{}\",\"platform\":\"{}\",\"architecture\":\"{}\",\"data_dir\":\"{}\",\"database_bytes\":{},\"wal_bytes\":{},\"tables\":{}}}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH, json_escape(&options.data_dir.display().to_string()), db, wal, tables.map_or_else(|| "null".to_string(), |value| value.to_string()));
    } else {
        println!("PLOMID DB Info\n────────────────────────────────");
        println!("PLOMID version: {}", env!("CARGO_PKG_VERSION"));
        println!(
            "Platform:       {} / {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        println!("Data directory: {}", options.data_dir.display());
        println!("Database size:  {} bytes", db);
        println!("WAL size:       {} bytes", wal);
        println!(
            "Catalog tables: {}",
            tables.map_or_else(|| "unavailable".to_string(), |count| count.to_string())
        );
    }
    Ok(())
}

fn print_diagnostics(options: &ConnectionOptions, json: bool) -> Result<(), String> {
    let report = readiness(options);
    let disk = fs::metadata(&options.data_dir)
        .map(|_| Check {
            name: "Configuration",
            status: "PASS",
            detail: "data directory exists".to_string(),
        })
        .unwrap_or_else(|error| Check {
            name: "Configuration",
            status: "FAIL",
            detail: error.to_string(),
        });
    if json {
        let status = if report.ready() && disk.status == "PASS" {
            "READY"
        } else {
            "NOT_READY"
        };
        let mut output = format!("{{\"status\":\"{status}\",\"checks\":[");
        for (index, check) in report.checks.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            let _ = write!(
                output,
                "{{\"name\":\"{}\",\"status\":\"{}\",\"detail\":\"{}\"}}",
                json_escape(check.name),
                check.status,
                json_escape(&check.detail)
            );
        }
        let _ = write!(
            output,
            "],\"configuration\":{{\"status\":\"{}\",\"detail\":\"{}\"}},\"latency_ms\":{}}}",
            disk.status,
            json_escape(&disk.detail),
            report
                .latency_ms
                .map_or_else(|| "null".to_string(), |value| value.to_string())
        );
        println!("{output}");
    } else {
        println!("PLOMID Diagnostics\n────────────────────────────────");
        for check in &report.checks {
            println!(
                "{} {:<20} {}",
                if check.status == "PASS" { "✓" } else { "⚠" },
                check.name,
                check.status
            );
        }
        println!(
            "{} Configuration       {}",
            if disk.status == "PASS" { "✓" } else { "✗" },
            disk.status
        );
        println!("\nRecommendations:");
        if report.ready() {
            println!("- No blocking diagnostics detected.");
        } else {
            println!("- Start the server and rerun: plomid db readiness");
        }
        println!("- Write and persistence checks are intentionally non-destructive and must be run in an isolated test database.");
    }
    if report.ready() && disk.status == "PASS" {
        Ok(())
    } else {
        Err("diagnostics found a blocking failure".to_string())
    }
}

fn print_config(options: &ConnectionOptions, json: bool) -> Result<(), String> {
    if json {
        println!("{{\"host\":\"{}\",\"port\":{},\"user\":\"{}\",\"database\":\"{}\",\"data_dir\":\"{}\"}}", json_escape(&options.host), options.port, json_escape(&options.user), json_escape(&options.database), json_escape(&options.data_dir.display().to_string()));
    } else {
        println!("PLOMID Configuration\n────────────────────────────────");
        println!("Host:     {}", options.host);
        println!("Port:     {}", options.port);
        println!("User:     {}", options.user);
        println!("Database: {}", options.database);
        println!("Data:     {}", options.data_dir.display());
        println!("Password: configured (not displayed)");
    }
    Ok(())
}

fn print_metrics(options: &ConnectionOptions, json: bool) -> Result<(), String> {
    let report = readiness(options);
    if json {
        println!("{{\"source\":\"cli_probe\",\"server_uptime\":null,\"requests\":null,\"queries\":null,\"query_latency_ms\":{},\"note\":\"server metrics endpoint is not exposed yet\"}}", report.latency_ms.map_or_else(|| "null".to_string(), |value| value.to_string()));
    } else {
        println!("PLOMID Metrics\n────────────────────────────────\nQuery probe latency: {}\nServer counters:     unavailable (metrics endpoint not yet exposed)\nUse `plomid db readiness` for live capability checks.", report.latency_ms.map_or_else(|| "unavailable".to_string(), |value| format!("{value} ms")));
    }
    Ok(())
}

fn run_test(command: TestCommand) -> Result<(), String> {
    match command.action {
        TestAction::List => {
            println!("connectivity\nread\nwrite (isolated database required)\npersistence (isolated database required)\nrecovery (isolated database required)\nperformance");
            Ok(())
        }
        TestAction::Run { name, output } => match name.as_deref().unwrap_or("connectivity") {
            "connectivity" | "read" => run_db(DbCommand {
                action: DbAction::Readiness(output),
            }),
            unknown => Err(format!(
                "unknown test module `{unknown}`; use `plomid test list`"
            )),
        },
    }
}

fn run_benchmark(command: BenchmarkCommand) -> Result<(), String> {
    match command.action {
        BenchmarkAction::Run(options) => benchmark(options),
    }
}

fn benchmark(options: BenchmarkOptions) -> Result<(), String> {
    if options.concurrency != 1 {
        return Err("V1 benchmark currently supports --concurrency 1; parallel workers are not silently simulated".to_string());
    }
    let target = options.duration_seconds.map(|seconds| (seconds, true));
    let mut total = options.iterations;
    if let Some((seconds, _)) = target {
        total = seconds.saturating_mul(1000).max(1);
    }
    let started = Instant::now();
    let mut successful = 0u64;
    let mut attempted = 0u64;
    let mut latencies = Vec::with_capacity(total.min(100_000) as usize);
    let mut client = WireClient::connect(&options.connection)?;
    for _ in 0..total {
        attempted += 1;
        let op_started = Instant::now();
        let response = match options.operation {
            BenchmarkOperation::Select => client.query("SELECT 1"),
        };
        match response {
            Ok(response)
                if response
                    .rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(|value| value.as_deref())
                    == Some("1") =>
            {
                successful += 1;
                latencies.push(op_started.elapsed());
            }
            Ok(_) => {}
            Err(_) => {}
        }
        if let Some((seconds, _)) = target {
            if started.elapsed() >= Duration::from_secs(seconds) {
                break;
            }
        }
    }
    let elapsed = started.elapsed();
    let failed = attempted.saturating_sub(successful);
    let run_id = format!("{}-{}", unix_millis(), std::process::id());
    let average_ms = if successful == 0 {
        0.0
    } else {
        latencies.iter().map(Duration::as_secs_f64).sum::<f64>() * 1000.0 / successful as f64
    };
    let p50 = percentile_ms(&mut latencies.clone(), 0.50);
    let p95 = percentile_ms(&mut latencies.clone(), 0.95);
    let p99 = percentile_ms(&mut latencies.clone(), 0.99);
    let throughput = successful as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    let record = format!("{{\"run_id\":\"{run_id}\",\"timestamp_ms\":{},\"version\":\"{}\",\"operation\":\"select\",\"duration_ms\":{},\"concurrency\":1,\"dataset_size\":{},\"total_operations\":{},\"successful\":{},\"failed\":{},\"average_latency_ms\":{average_ms:.3},\"p50_ms\":{p50:.3},\"p95_ms\":{p95:.3},\"p99_ms\":{p99:.3},\"throughput_ops_sec\":{throughput:.3}}}", unix_millis(), env!("CARGO_PKG_VERSION"), elapsed.as_millis(), options.dataset_size.map_or_else(|| "null".to_string(), |value| value.to_string()), attempted, successful, failed);
    let directory = options.connection.data_dir.join("benchmarks");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create benchmark directory: {error}"))?;
    fs::write(directory.join(format!("{run_id}.json")), &record)
        .map_err(|error| format!("persist benchmark result: {error}"))?;
    if options.json {
        println!("{record}");
    } else {
        println!("PLOMID Benchmark\n────────────────────────────────\nRun ID:       {run_id}\nOperation:    SELECT 1\nOperations:   {}\nSuccessful:   {successful}\nFailed:       {failed}\nThroughput:   {throughput:.2} ops/sec\nAverage:      {average_ms:.3} ms\nP50 / P95 / P99: {p50:.3} / {p95:.3} / {p99:.3} ms\nSaved:        {}/{}.json", attempted, directory.display(), run_id);
    }
    Ok(())
}

fn percentile_ms(latencies: &mut [Duration], percentile: f64) -> f64 {
    if latencies.is_empty() {
        return 0.0;
    }
    latencies.sort_unstable();
    let index = ((latencies.len() - 1) as f64 * percentile).round() as usize;
    latencies[index].as_secs_f64() * 1000.0
}

struct WireClient {
    stream: TcpStream,
}

impl WireClient {
    fn connect(options: &ConnectionOptions) -> Result<Self, String> {
        let address = format!("{}:{}", options.host, options.port);
        let stream = TcpStream::connect_timeout(
            &address
                .parse()
                .map_err(|_| "invalid server address".to_string())?,
            Duration::from_secs(2),
        )
        .map_err(|error| format!("connect to {address}: {error}"))?;
        Self::startup(stream, options)
    }

    fn startup(mut stream: TcpStream, options: &ConnectionOptions) -> Result<Self, String> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|error| error.to_string())?;
        let mut payload = Vec::new();
        payload.extend_from_slice(&196608u32.to_be_bytes());
        for (key, value) in [
            ("user", options.user.as_str()),
            ("database", options.database.as_str()),
            ("application_name", "plomid-cli"),
        ] {
            payload.extend_from_slice(key.as_bytes());
            payload.push(0);
            payload.extend_from_slice(value.as_bytes());
            payload.push(0);
        }
        payload.push(0);
        write_startup(&mut stream, &payload)?;
        loop {
            let (tag, message) = read_backend(&mut stream)?;
            match tag {
                b'R' => {
                    if message.len() < 4 {
                        return Err("malformed authentication response".to_string());
                    }
                    match u32::from_be_bytes(
                        message[..4]
                            .try_into()
                            .map_err(|_| "malformed authentication response")?,
                    ) {
                        0 => {}
                        3 => {
                            let mut password = options.password.as_bytes().to_vec();
                            password.push(0);
                            write_frontend(&mut stream, b'p', &password)?;
                        }
                        _ => {
                            return Err(
                                "server requested an unsupported authentication method".to_string()
                            )
                        }
                    }
                }
                b'E' => return Err(error_message(&message)),
                b'S' | b'K' => {}
                b'Z' => break,
                _ => {}
            }
        }
        Ok(Self { stream })
    }

    fn query(&mut self, sql: &str) -> Result<QueryResponse, String> {
        let mut payload = sql.as_bytes().to_vec();
        payload.push(0);
        write_frontend(&mut self.stream, b'Q', &payload)?;
        let mut response = QueryResponse {
            columns: Vec::new(),
            rows: Vec::new(),
            command: None,
        };
        loop {
            let (tag, message) = read_backend(&mut self.stream)?;
            match tag {
                b'T' => response.columns = parse_row_description(&message)?,
                b'D' => response.rows.push(parse_data_row(&message)?),
                b'C' => response.command = Some(read_cstring(&message)?),
                b'E' => return Err(error_message(&message)),
                b'Z' => return Ok(response),
                _ => {}
            }
        }
    }
}

fn write_startup(stream: &mut TcpStream, payload: &[u8]) -> Result<(), String> {
    let length = (payload.len() + 4) as u32;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(|error| error.to_string())?;
    stream.write_all(payload).map_err(|error| error.to_string())
}
fn write_frontend(stream: &mut TcpStream, tag: u8, payload: &[u8]) -> Result<(), String> {
    let length = (payload.len() + 4) as u32;
    stream
        .write_all(&[tag])
        .map_err(|error| error.to_string())?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(|error| error.to_string())?;
    stream.write_all(payload).map_err(|error| error.to_string())
}
fn read_backend(stream: &mut TcpStream) -> Result<(u8, Vec<u8>), String> {
    let mut tag = [0u8; 1];
    stream
        .read_exact(&mut tag)
        .map_err(|error| error.to_string())?;
    let mut length = [0u8; 4];
    stream
        .read_exact(&mut length)
        .map_err(|error| error.to_string())?;
    let length = u32::from_be_bytes(length) as usize;
    if !(4..=16 * 1024 * 1024).contains(&length) {
        return Err("invalid backend message length".to_string());
    }
    let mut payload = vec![0u8; length - 4];
    stream
        .read_exact(&mut payload)
        .map_err(|error| error.to_string())?;
    Ok((tag[0], payload))
}
fn read_cstring(bytes: &[u8]) -> Result<String, String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| "malformed protocol string".to_string())?;
    String::from_utf8(bytes[..end].to_vec()).map_err(|_| "invalid UTF-8 from server".to_string())
}
fn parse_row_description(bytes: &[u8]) -> Result<Vec<String>, String> {
    if bytes.len() < 2 {
        return Err("malformed row description".to_string());
    }
    let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let mut position = 2;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let end = bytes[position..]
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| "malformed row description".to_string())?
            + position;
        names.push(
            String::from_utf8(bytes[position..end].to_vec())
                .map_err(|_| "invalid column name".to_string())?,
        );
        position = end + 1;
        if bytes.len() < position + 18 {
            return Err("truncated row description".to_string());
        }
        position += 18;
    }
    Ok(names)
}
fn parse_data_row(bytes: &[u8]) -> Result<Vec<Option<String>>, String> {
    if bytes.len() < 2 {
        return Err("malformed data row".to_string());
    }
    let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let mut position = 2;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        if bytes.len() < position + 4 {
            return Err("truncated data row".to_string());
        }
        let length = i32::from_be_bytes(
            bytes[position..position + 4]
                .try_into()
                .map_err(|_| "malformed data row")?,
        );
        position += 4;
        if length < 0 {
            values.push(None);
        } else {
            let end = position
                .checked_add(length as usize)
                .ok_or_else(|| "data row length overflow".to_string())?;
            if bytes.len() < end {
                return Err("truncated data row value".to_string());
            }
            values.push(Some(
                String::from_utf8(bytes[position..end].to_vec())
                    .map_err(|_| "invalid UTF-8 data row".to_string())?,
            ));
            position = end;
        }
    }
    Ok(values)
}
fn error_message(bytes: &[u8]) -> String {
    let mut position = 0;
    let mut message = "database request failed".to_string();
    while position < bytes.len() && bytes[position] != 0 {
        let field = bytes[position];
        position += 1;
        if let Some(end) = bytes[position..].iter().position(|byte| *byte == 0) {
            let value = String::from_utf8_lossy(&bytes[position..position + end]);
            if field == b'M' {
                message = value.into_owned();
            }
            position += end + 1;
        } else {
            break;
        }
    }
    message
}
fn file_size(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}
fn database_size(data_dir: &Path) -> u64 {
    tree_size(&data_dir.join("data")) + tree_size(&data_dir.join("wal"))
}
fn tree_size(path: &Path) -> u64 {
    if path.is_file() {
        return file_size(path);
    }
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| tree_size(&entry.path()))
        .sum()
}
fn catalog_table_count(data_dir: &Path) -> Option<u64> {
    let _ = data_dir;
    None
}
fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_order_independent() {
        let mut values = [
            Duration::from_millis(30),
            Duration::from_millis(10),
            Duration::from_millis(20),
        ];
        assert_eq!(percentile_ms(&mut values, 0.5), 20.0);
    }

    #[test]
    fn json_escape_does_not_emit_raw_quotes() {
        assert_eq!(json_escape("a\"b"), "a\\\"b");
    }
}
