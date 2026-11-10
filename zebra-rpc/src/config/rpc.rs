//! RPC config

use std::{fmt, net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

use zebra_chain::common::default_cache_dir;

/// A [`String`] whose `Debug` output is redacted, so secrets do not reach the logs.
///
/// Serialization is unaffected. Use [`RedactedString::as_str()`] to read the contents.
#[derive(Clone, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct RedactedString(String);

impl fmt::Debug for RedactedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("[REDACTED]")
    }
}

impl RedactedString {
    /// Returns the secret contents.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<S> From<S> for RedactedString
where
    S: Into<String>,
{
    fn from(value: S) -> Self {
        Self(value.into())
    }
}

/// The error returned when the static RPC credentials in [`Config`] are incomplete or unreadable.
#[derive(Debug, thiserror::Error)]
pub enum StaticCredentialsError {
    /// A password is configured without a user.
    #[error("rpc.rpc_password or rpc.rpc_password_file is set, but rpc.rpc_user is missing")]
    MissingUser,

    /// A user is configured without a password.
    #[error("rpc.rpc_user is set, but neither rpc.rpc_password nor rpc.rpc_password_file is")]
    MissingPassword,

    /// The configured user is an empty string.
    #[error("rpc.rpc_user must not be empty")]
    EmptyUser,

    /// The configured or loaded password is an empty string.
    #[error("the RPC password must not be empty")]
    EmptyPassword,

    /// The password file could not be read.
    #[error("reading rpc.rpc_password_file {path}: {source}")]
    ReadPasswordFile {
        /// The configured path.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
}

/// RPC configuration section.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// IP address and port for the RPC server.
    ///
    /// Note: The RPC server is disabled by default.
    /// To enable the RPC server, set a listen address in the config:
    /// ```toml
    /// [rpc]
    /// listen_addr = '127.0.0.1:8232'
    /// ```
    ///
    /// The recommended ports for the RPC server are:
    /// - Mainnet: 127.0.0.1:8232
    /// - Testnet: 127.0.0.1:18232
    ///
    /// # Security
    ///
    /// If you bind Zebra's RPC port to a public IP address,
    /// anyone on the internet can send transactions via your node.
    /// They can also query your node's state.
    pub listen_addr: Option<SocketAddr>,

    /// IP address and port for the indexer RPC server.
    ///
    /// Note: The indexer RPC server is disabled by default.
    /// To enable the indexer RPC server, compile `zebrad` with the
    /// `indexer` feature flag and set a listen address in the config:
    /// ```toml
    /// [rpc]
    /// indexer_listen_addr = '127.0.0.1:8230'
    /// ```
    ///
    /// # Security
    ///
    /// If you bind Zebra's indexer RPC port to a public IP address,
    /// anyone on the internet can query your node's state.
    pub indexer_listen_addr: Option<SocketAddr>,

    /// IP address and port for the lightwalletd-compatible gRPC server.
    ///
    /// Note: The lightwalletd gRPC server is disabled by default.
    /// To enable it, set a listen address in the config:
    /// ```toml
    /// [rpc]
    /// lightwalletd_listen_addr = '127.0.0.1:9067'
    /// ```
    ///
    /// This server implements the lightwalletd `CompactTxStreamer` service, so
    /// Zcash light clients can connect directly to Zebra.
    ///
    /// This server is experimental: its behaviour and configuration may change
    /// in any release, including in ways that break clients.
    ///
    /// # Security
    ///
    /// The port serves plaintext HTTP/2, with no TLS and no authentication —
    /// cookie authentication does not apply to it. Anyone who can reach the
    /// port can query your node's state and send transactions through it, and
    /// anyone on the network path can read and modify the traffic, including
    /// the addresses a light client is querying. Terminate TLS in a reverse
    /// proxy, and bind Zebra to a loopback or private address.
    ///
    /// Run this on infrastructure you operate, not on end users' machines: a
    /// loopback port is reachable by any local process, and by web pages
    /// loaded in a browser on that machine.
    pub lightwalletd_listen_addr: Option<SocketAddr>,

    /// The number of threads used to process RPC requests and responses.
    ///
    /// This field is deprecated and could be removed in a future release.
    /// We keep it just for backward compatibility but it actually do nothing.
    /// It was something configurable when the RPC server was based in the jsonrpc-core crate,
    /// not anymore since we migrated to jsonrpsee.
    // TODO: Prefix this field name with an underscore so it's clear that it's now unused, and
    //       use serde(rename) to continue successfully deserializing old configs.
    pub parallel_cpu_threads: usize,

    /// Test-only option that makes Zebra say it is at the chain tip,
    /// no matter what the estimated height or local clock is.
    pub debug_force_finished_sync: bool,

    /// The directory where Zebra stores RPC cookies.
    pub cookie_dir: PathBuf,

    /// Enable cookie-based authentication for RPCs.
    ///
    /// Ignored when static credentials are configured, see [`Self::rpc_user`].
    pub enable_cookie_auth: bool,

    /// Username for static HTTP Basic RPC authentication.
    ///
    /// Must be set together with `rpc_password` or `rpc_password_file`. When set,
    /// every request needs these credentials, and no cookie is written.
    pub rpc_user: Option<String>,

    /// Password for static HTTP Basic RPC authentication.
    ///
    /// Prefer `rpc_password_file`, which keeps the secret out of the config file.
    pub rpc_password: Option<RedactedString>,

    /// Path to a file containing the password for static HTTP Basic RPC authentication.
    ///
    /// Read once at startup with trailing newlines trimmed. Takes precedence over `rpc_password`.
    pub rpc_password_file: Option<PathBuf>,

    /// The maximum size of the response body in bytes.
    pub max_response_body_size: u32,
}

impl Config {
    /// Returns `true` if any static credential field is set.
    pub fn uses_static_auth(&self) -> bool {
        self.rpc_user.is_some() || self.rpc_password.is_some() || self.rpc_password_file.is_some()
    }

    /// Returns the static user and password, `None` if they are not configured,
    /// or an error if the configuration is partial or the password file is unreadable.
    pub fn static_credentials(&self) -> Result<Option<(String, String)>, StaticCredentialsError> {
        if !self.uses_static_auth() {
            return Ok(None);
        }

        let user = self
            .rpc_user
            .clone()
            .ok_or(StaticCredentialsError::MissingUser)?;
        if user.is_empty() {
            return Err(StaticCredentialsError::EmptyUser);
        }

        let password = match (&self.rpc_password_file, &self.rpc_password) {
            (Some(path), _) => {
                let contents = std::fs::read_to_string(path).map_err(|source| {
                    StaticCredentialsError::ReadPasswordFile {
                        path: path.clone(),
                        source,
                    }
                })?;
                contents.trim_end_matches(['\r', '\n']).to_string()
            }
            (None, Some(password)) => password.as_str().to_string(),
            (None, None) => return Err(StaticCredentialsError::MissingPassword),
        };
        if password.is_empty() {
            return Err(StaticCredentialsError::EmptyPassword);
        }

        Ok(Some((user, password)))
    }
}

// This impl isn't derivable because it depends on features.
#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Self {
            // Disable RPCs by default.
            listen_addr: None,

            // Disable indexer RPCs by default.
            indexer_listen_addr: None,

            // Disable the lightwalletd gRPC server by default.
            lightwalletd_listen_addr: None,

            // Use multiple threads, because we pause requests during getblocktemplate long polling
            parallel_cpu_threads: 0,

            // Debug options are always off by default.
            debug_force_finished_sync: false,

            // Use the default cache dir for the auth cookie.
            cookie_dir: default_cache_dir(),

            // Enable cookie-based authentication by default.
            enable_cookie_auth: true,

            rpc_user: None,
            rpc_password: None,
            rpc_password_file: None,

            // 50 MiB
            max_response_body_size: 52_428_800,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::{Config, RedactedString, StaticCredentialsError};

    fn config_with(user: Option<&str>, password: Option<&str>) -> Config {
        Config {
            rpc_user: user.map(str::to_string),
            rpc_password: password.map(RedactedString::from),
            ..Config::default()
        }
    }

    #[test]
    fn no_static_credentials_by_default() {
        assert!(!Config::default().uses_static_auth());
        assert!(matches!(Config::default().static_credentials(), Ok(None)));
    }

    #[test]
    fn static_credentials_from_config_password() {
        let config = config_with(Some("user"), Some("hunter2"));
        assert!(config.uses_static_auth());
        assert_eq!(
            config.static_credentials().unwrap(),
            Some(("user".to_string(), "hunter2".to_string()))
        );
    }

    /// Zebra logs the whole config at startup, so the password must never appear in it.
    #[test]
    fn config_debug_does_not_contain_rpc_password() {
        let config = config_with(Some("user"), Some("hunter2"));
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
    }

    #[test]
    fn static_credentials_reject_partial_config() {
        let cases = [
            ("user without password", Some("user"), None),
            ("password without user", None, Some("hunter2")),
            ("empty user", Some(""), Some("hunter2")),
            ("empty password", Some("user"), Some("")),
        ];

        for (case, user, password) in cases {
            let result = config_with(user, password).static_credentials();
            assert!(result.is_err(), "{case} should be rejected, got {result:?}");
        }
    }

    #[test]
    fn static_credentials_from_password_file_take_precedence_and_trim_newline() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"from-file\n").unwrap();

        let config = Config {
            rpc_password_file: Some(file.path().to_path_buf()),
            ..config_with(Some("user"), Some("from-config"))
        };
        assert_eq!(
            config.static_credentials().unwrap(),
            Some(("user".to_string(), "from-file".to_string()))
        );
    }

    #[test]
    fn static_credentials_reject_missing_password_file() {
        let config = Config {
            rpc_password_file: Some("/nonexistent/rpc-password".into()),
            ..config_with(Some("user"), None)
        };
        assert!(matches!(
            config.static_credentials(),
            Err(StaticCredentialsError::ReadPasswordFile { .. })
        ));
    }

    #[test]
    fn static_credentials_deserialize_with_defaults() {
        let config: Config = serde_json::from_value(serde_json::json!({
            "listen_addr": "127.0.0.1:8232",
            "rpc_user": "user",
            "rpc_password_file": "/run/secrets/rpc_password",
        }))
        .expect("static credential fields should deserialize");
        assert_eq!(config.rpc_user.as_deref(), Some("user"));
        assert_eq!(
            config.rpc_password_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/rpc_password"))
        );
        assert!(config.enable_cookie_auth, "defaults are kept");
    }
}
