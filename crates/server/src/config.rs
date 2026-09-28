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
//! Server configuration for the PLOMID database server.
//!
//! Configuration is provided via command-line arguments and environment
//! variables. The design is intentionally simple for V1 and can be
//! extended with configuration files in future releases.

use clap::{Arg, ArgAction};
use std::path::PathBuf;

/// TLS configuration for the PLOMID server.
///
/// When disabled the server answers every `SSLRequest` with an `N` (SSL not
/// available) response, which conforms to the PostgreSQL protocol: clients
/// then fall back to a plaintext connection. When enabled the certificate and
/// private key must both be supplied.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    pub enabled: bool,
    pub cert: PathBuf,
    pub key: PathBuf,
    /// If true, reject plaintext connections and require TLS (PostgreSQL
    /// `ssl = require` semantics).
    pub required: bool,
}

/// Configuration for the PLOMID server process.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: std::net::IpAddr,
    pub port: u16,
    pub data_dir: PathBuf,
    pub username: String,
    pub password: String,
    pub auth_method: String,
    pub tls: TlsConfig,
    /// Explicit operator opt-in that permits a non-loopback bind **without**
    /// TLS.
    ///
    /// PLOMID fails closed here: a non-loopback listener would put the
    /// single configured credential pair on the network, where clients send it
    /// unencrypted. Containers and trusted LAN deployments cannot use TLS
    /// (not implemented in this release) yet must bind `0.0.0.0` to be
    /// reachable, so they opt in explicitly and the server logs a warning on
    /// every start. Authentication is still required and cannot be disabled
    /// remotely, so this never yields an unauthenticated listener.
    pub allow_remote_plaintext: bool,
    pub log_level: Option<String>,
    /// Whether the automatic WAL checkpoint policy is active.
    pub checkpoint_enabled: bool,
    /// WAL bytes appended since the last checkpoint that make one due.
    pub checkpoint_wal_bytes: u64,
    /// Retained WAL segments that make a checkpoint due (backstop signal).
    pub checkpoint_segments: u64,
    /// Maximum seconds between checkpoints (`0` disables the interval signal).
    pub checkpoint_interval_secs: u64,
}

impl ServerConfig {
    /// Parses the process arguments (`PLOMID_*` environment variables are
    /// applied by clap).
    pub fn parse() -> Result<Self, String> {
        Self::parse_from(std::env::args())
    }

    /// Parses an explicit argument list.
    ///
    /// Split out from [`Self::parse`] so tests can exercise flags without
    /// mutating the process environment.
    pub fn parse_from<I, T>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let matches = clap::Command::new("plomid-server")
            .version(env!("CARGO_PKG_VERSION"))
            .about("PLOMID database server")
            .arg(
                Arg::new("host")
                    .long("host")
                    .short('H')
                    .env("PLOMID_HOST")
                    .value_name("ADDR")
                    .default_value("127.0.0.1")
                    .help("IP address to listen on"),
            )
            .arg(
                Arg::new("port")
                    .long("port")
                    .short('p')
                    .env("PLOMID_PORT")
                    .value_name("PORT")
                    .default_value("5432")
                    .help("TCP port to listen on"),
            )
            .arg(
                Arg::new("data")
                    .long("data")
                    .short('D')
                    .env("PLOMID_DATA_DIR")
                    .value_name("DIR")
                    .default_value("./data")
                    .help("Data directory for storage and WAL"),
            )
            .arg(
                Arg::new("username")
                    .long("username")
                    .short('U')
                    .env("PLOMID_USER")
                    .value_name("USER")
                    .default_value("plomid")
                    .help("Required username for authentication"),
            )
            .arg(
                Arg::new("password")
                    .long("password")
                    .short('W')
                    .env("PLOMID_PASSWORD")
                    .value_name("PASS")
                    .default_value("plomid")
                    .help("Password for authentication"),
            )
            .arg(
                Arg::new("log-level")
                    .long("log-level")
                    .value_name("LEVEL")
                    .action(ArgAction::Set)
                    .help("Log level (trace, debug, info, warn, error)"),
            )
            .arg(
                Arg::new("auth")
                    .long("auth")
                    .env("PLOMID_AUTH")
                    .value_name("METHOD")
                    .default_value("scram")
                    .help("Authentication method: scram, password, md5, none"),
            )
            .arg(
                Arg::new("tls")
                    .long("tls")
                    .help("Enable TLS (requires --tls-cert and --tls-key)")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("tls-cert")
                    .long("tls-cert")
                    .env("PLOMID_TLS_CERT")
                    .value_name("FILE")
                    .default_value("")
                    .help("Path to the TLS certificate (PEM)"),
            )
            .arg(
                Arg::new("tls-key")
                    .long("tls-key")
                    .env("PLOMID_TLS_KEY")
                    .value_name("FILE")
                    .default_value("")
                    .help("Path to the TLS private key (PEM)"),
            )
            .arg(
                Arg::new("tls-required")
                    .long("tls-required")
                    .help("Refuse plaintext connections when TLS is enabled")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("checkpoint-wal-bytes")
                    .long("checkpoint-wal-bytes")
                    .env("PLOMID_CHECKPOINT_WAL_BYTES")
                    .value_name("BYTES")
                    .default_value("67108864")
                    .help(
                        "WAL bytes since the last checkpoint that make one due \
                         (0 disables this signal; default 64 MiB)",
                    ),
            )
            .arg(
                Arg::new("checkpoint-segments")
                    .long("checkpoint-segments")
                    .env("PLOMID_CHECKPOINT_SEGMENTS")
                    .value_name("N")
                    .default_value("16")
                    .help(
                        "Retained WAL segment count that makes a checkpoint due \
                         (0 disables this signal; default 16)",
                    ),
            )
            .arg(
                Arg::new("checkpoint-interval-secs")
                    .long("checkpoint-interval-secs")
                    .env("PLOMID_CHECKPOINT_INTERVAL_SECS")
                    .value_name("SECONDS")
                    .default_value("900")
                    .help(
                        "Maximum seconds between automatic checkpoints \
                         (0 disables this signal; default 900)",
                    ),
            )
            .arg(
                Arg::new("allow-remote-plaintext")
                    .long("allow-remote-plaintext")
                    .env("PLOMID_ALLOW_INSECURE_REMOTE")
                    .help(
                        "Allow a non-loopback bind without TLS (container / trusted-network \
                         deployments). Authentication is still required; only the transport is \
                         unencrypted. Prefer TLS once it is available",
                    )
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("no-auto-checkpoint")
                    .long("no-auto-checkpoint")
                    .help(
                        "Disable automatic checkpointing entirely; WAL is then \
                         reclaimed only by an explicit checkpoint",
                    )
                    .action(ArgAction::SetTrue),
            )
            .get_matches_from(args);

        let host = matches
            .get_one::<String>("host")
            .expect("host has default")
            .parse()
            .map_err(|e| format!("invalid host address: {e}"))?;

        let port = matches
            .get_one::<String>("port")
            .expect("port has default")
            .parse()
            .map_err(|e| format!("invalid port: {e}"))?;

        let data_dir = matches
            .get_one::<String>("data")
            .expect("data has default")
            .into();

        let username = matches
            .get_one::<String>("username")
            .expect("username has default")
            .clone();

        let password = matches
            .get_one::<String>("password")
            .expect("password has default")
            .clone();

        let log_level = matches.get_one::<String>("log-level").cloned();

        let auth_method = matches
            .get_one::<String>("auth")
            .expect("auth has default")
            .to_ascii_lowercase();

        let checkpoint_enabled = !matches.get_flag("no-auto-checkpoint");
        let allow_remote_plaintext = matches.get_flag("allow-remote-plaintext");
        let checkpoint_wal_bytes = matches
            .get_one::<String>("checkpoint-wal-bytes")
            .expect("checkpoint-wal-bytes has default")
            .parse()
            .map_err(|e| format!("invalid --checkpoint-wal-bytes: {e}"))?;
        let checkpoint_segments = matches
            .get_one::<String>("checkpoint-segments")
            .expect("checkpoint-segments has default")
            .parse()
            .map_err(|e| format!("invalid --checkpoint-segments: {e}"))?;
        let checkpoint_interval_secs = matches
            .get_one::<String>("checkpoint-interval-secs")
            .expect("checkpoint-interval-secs has default")
            .parse()
            .map_err(|e| format!("invalid --checkpoint-interval-secs: {e}"))?;

        let tls_enabled = matches.get_flag("tls");
        let tls_required = matches.get_flag("tls-required");
        let tls = TlsConfig {
            enabled: tls_enabled,
            cert: matches
                .get_one::<String>("tls-cert")
                .expect("tls-cert has default")
                .clone()
                .into(),
            key: matches
                .get_one::<String>("tls-key")
                .expect("tls-key has default")
                .clone()
                .into(),
            required: tls_required,
        };

        let config = Self {
            host,
            port,
            data_dir,
            username,
            password,
            auth_method,
            tls,
            log_level,
            allow_remote_plaintext,
            checkpoint_enabled,
            checkpoint_wal_bytes,
            checkpoint_segments,
            checkpoint_interval_secs,
        };
        config.validate_security()?;
        Ok(config)
    }

    /// Builds the automatic checkpoint policy from the configured thresholds.
    ///
    /// A threshold of `0` (or an interval of `0` seconds) disables that signal,
    /// which lets an operator keep, for example, byte-triggered checkpointing
    /// while disabling the interval. `--no-auto-checkpoint` disables the whole
    /// policy; explicit checkpoints remain available either way, so the
    /// durability boundary itself is never configurable.
    #[must_use]
    pub fn checkpoint_policy(&self) -> plomid_txn::CheckpointPolicy {
        if !self.checkpoint_enabled {
            return plomid_txn::CheckpointPolicy::disabled();
        }
        let interval = match self.checkpoint_interval_secs {
            0 => None,
            seconds => Some(std::time::Duration::from_secs(seconds)),
        };
        plomid_txn::CheckpointPolicy::new(
            self.checkpoint_wal_bytes,
            self.checkpoint_segments,
            interval,
        )
    }

    fn validate_security(&self) -> Result<(), String> {
        if self.tls.required && !self.tls.enabled {
            return Err("--tls-required requires --tls".to_string());
        }
        let auth_disabled = matches!(self.auth_method.as_str(), "none" | "disabled");
        if auth_disabled && !self.host.is_loopback() {
            return Err("authentication cannot be disabled on a non-loopback bind address".into());
        }
        if !self.host.is_loopback() && !self.tls.enabled && !self.allow_remote_plaintext {
            return Err(
                "non-loopback binds require TLS; TLS is not implemented in this release. \
                 Pass --allow-remote-plaintext (or set PLOMID_ALLOW_INSECURE_REMOTE=1) only on \
                 a trusted network to accept unencrypted remote clients"
                    .into(),
            );
        }
        // V1 has exactly one configured credential pair and defaults to a
        // loopback listener. Keep local single-user development usable with
        // the built-in pair; remote deployment is rejected above unless TLS
        // is enabled.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ServerConfig, TlsConfig};

    fn config(host: &str, auth_method: &str, password: &str) -> ServerConfig {
        ServerConfig {
            host: host.parse().unwrap(),
            port: 5432,
            data_dir: "./data".into(),
            username: "plomid".into(),
            password: password.into(),
            auth_method: auth_method.into(),
            tls: TlsConfig {
                enabled: false,
                cert: "".into(),
                key: "".into(),
                required: false,
            },
            log_level: None,
            allow_remote_plaintext: false,
            checkpoint_enabled: true,
            checkpoint_wal_bytes: plomid_txn::CheckpointPolicy::DEFAULT_WAL_BYTES,
            checkpoint_segments: plomid_txn::CheckpointPolicy::DEFAULT_WAL_SEGMENTS,
            checkpoint_interval_secs: plomid_txn::CheckpointPolicy::DEFAULT_MAX_INTERVAL.as_secs(),
        }
    }

    #[test]
    fn default_checkpoint_policy_matches_the_engine_defaults() {
        let policy = config("127.0.0.1", "scram", "plomid").checkpoint_policy();
        assert!(policy.is_enabled());
        assert_eq!(
            policy.wal_bytes(),
            plomid_txn::CheckpointPolicy::DEFAULT_WAL_BYTES
        );
        assert_eq!(
            policy.wal_segments(),
            plomid_txn::CheckpointPolicy::DEFAULT_WAL_SEGMENTS
        );
        assert_eq!(
            policy.max_interval(),
            Some(plomid_txn::CheckpointPolicy::DEFAULT_MAX_INTERVAL)
        );
    }

    #[test]
    fn checkpoint_thresholds_are_operator_configurable() {
        let mut cfg = config("127.0.0.1", "scram", "plomid");
        cfg.checkpoint_enabled = false;
        assert!(!cfg.checkpoint_policy().is_enabled());

        cfg.checkpoint_enabled = true;
        cfg.checkpoint_wal_bytes = 1024;
        cfg.checkpoint_segments = 0;
        cfg.checkpoint_interval_secs = 0;
        let policy = cfg.checkpoint_policy();
        assert_eq!(policy.wal_bytes(), 1024);
        assert_eq!(policy.wal_segments(), 0);
        assert_eq!(policy.max_interval(), None);
    }

    #[test]
    fn unsafe_remote_defaults_fail_closed() {
        assert!(config("0.0.0.0", "scram", "secret")
            .validate_security()
            .is_err());
        assert!(config("0.0.0.0", "none", "secret")
            .validate_security()
            .is_err());
        // The opt-in relaxes the transport rule only: remote listeners still
        // require authentication.
        let mut opted_in = config("0.0.0.0", "scram", "secret");
        opted_in.allow_remote_plaintext = true;
        assert!(opted_in.validate_security().is_ok());
        opted_in.auth_method = "none".into();
        assert!(opted_in.validate_security().is_err());
        // TLS-required still wins over the opt-in.
        let mut tls_required = config("0.0.0.0", "scram", "secret");
        tls_required.allow_remote_plaintext = true;
        tls_required.tls.required = true;
        assert!(tls_required.validate_security().is_err());
    }

    #[test]
    fn remote_plaintext_opt_in_is_required_to_bind_beyond_loopback() {
        let base = ["plomid-server", "--host", "0.0.0.0", "--password", "secret"];
        let err = ServerConfig::parse_from(base)
            .expect_err("a non-loopback bind must fail closed by default");
        assert!(err.contains("non-loopback binds require TLS"), "got: {err}");

        let cfg = ServerConfig::parse_from(base.into_iter().chain(["--allow-remote-plaintext"]))
            .expect("the explicit opt-in must permit the bind");
        assert!(cfg.allow_remote_plaintext);
        assert!(!cfg.host.is_loopback());

        // Loopback never needs the opt-in.
        let local = ServerConfig::parse_from(["plomid-server", "--host", "127.0.0.1"])
            .expect("loopback binds stay valid without the opt-in");
        assert!(!local.allow_remote_plaintext);
    }

    #[test]
    fn loopback_single_user_accepts_the_default_pair() {
        assert!(config("127.0.0.1", "scram", "plomid")
            .validate_security()
            .is_ok());
        assert!(config("127.0.0.1", "scram", "secret")
            .validate_security()
            .is_ok());
    }
}
