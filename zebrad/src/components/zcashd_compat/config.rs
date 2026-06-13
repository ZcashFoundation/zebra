use std::{net::SocketAddr, path::PathBuf, time::Duration};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use zebra_chain::common::default_cache_dir;

/// Source selector for Zebra-managed `zcashd` execution.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ZcashdBinarySource {
    /// Resolve `zcashd` from Zebra's embedded managed release manifest.
    #[default]
    Managed,
    /// Resolve `zcashd` from a local executable path.
    Path,
}

/// Configuration for Zebra zcashd-compat mode.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Enables zcashd-compat mode.
    ///
    /// zcashd-compat mode configures Zebra RPC defaults for a local `zcashd -zebra-compat` process.
    pub enabled: bool,

    /// Whether Zebra should spawn and supervise a `zcashd -zebra-compat` child process.
    ///
    /// Set this to `false` if `zcashd` is managed externally.
    pub manage_zcashd: bool,

    /// Preferred source for the `zcashd` binary.
    ///
    /// If `zcashd_path` is set, that explicit local path overrides this value.
    pub zcashd_source: ZcashdBinarySource,

    /// Optional explicit path to a local `zcashd` binary with zcashd-compat support.
    ///
    /// When set, Zebra uses this path directly and skips managed downloads.
    pub zcashd_path: Option<PathBuf>,

    /// Optional `zcashd` datadir path.
    ///
    /// If unset, Zebra uses a subdirectory in `state.cache_dir`.
    pub zcashd_datadir: Option<PathBuf>,

    /// Extra command-line arguments passed to `zcashd`.
    ///
    /// This can be provided as:
    /// - a TOML array: `zcashd_extra_args = ["-debug=1"]`
    /// - a JSON array string (useful for environment variable overrides):
    ///   `ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-conf=/path/to/zcash.conf","-debug=1"]'`
    ///
    /// Zebra passes these arguments through unchanged. For first-start bootstrap,
    /// Zebra only infers path overrides from the first valid `-conf=/path` or
    /// `-datadir=/path` form, and logs warnings for paired, empty, or duplicate
    /// path options.
    ///
    /// Zebra always includes `-printtoconsole` automatically.
    #[serde(default, deserialize_with = "deserialize_zcashd_extra_args")]
    pub zcashd_extra_args: Vec<String>,

    /// Dedicated RPC listen address used by `zcashd -zebra-compat`.
    ///
    /// If unset, zcashd-compat startup defaults it to `127.0.0.1:28232`.
    ///
    /// Backward compatibility: this field also accepts the legacy `rpc_url` key and env var.
    #[serde(
        default,
        alias = "rpc_url",
        deserialize_with = "deserialize_listen_addr_or_rpc_url"
    )]
    pub listen_addr: Option<SocketAddr>,

    /// The directory where Zebra stores zcashd-compat RPC cookies.
    ///
    /// By default this reuses Zebra's standard cache directory.
    pub cookie_dir: PathBuf,

    /// The zcashd-compat RPC cookie file name.
    ///
    /// This is separate from the standard RPC cookie filename to avoid
    /// conflicts when both RPC servers share the same `cookie_dir`.
    #[serde(default = "default_cookie_file_name")]
    pub cookie_file_name: String,

    /// Enable cookie-based authentication on the dedicated zcashd-compat RPC listener.
    ///
    /// This defaults to true. Disabling it is only allowed when the zcashd-compat
    /// listener is configured for TLS, so external access control can protect the
    /// unauthenticated HTTPS endpoint.
    pub enable_cookie_auth: bool,

    /// TLS certificate chain for the dedicated zcashd-compat RPC listener.
    pub tls_cert_file: Option<PathBuf>,

    /// TLS private key for the dedicated zcashd-compat RPC listener.
    pub tls_key_file: Option<PathBuf>,

    /// CA certificate file passed to supervised zcashd so it can verify Zebra's TLS certificate.
    pub tls_ca_file: Option<PathBuf>,

    /// Allow a non-loopback zcashd-compat RPC listener without TLS.
    ///
    /// By default, non-loopback `listen_addr` values require TLS because cookie
    /// credentials would otherwise cross the network in cleartext. Set this only
    /// when another layer secures the listener, such as a container or private
    /// network boundary. This mirrors zcashd's `-zebra-compat-allow-remote-http`
    /// client-side escape hatch.
    pub unsafe_allow_remote_http: bool,

    /// Delay before the first `zcashd` spawn attempt.
    #[serde(with = "humantime_serde")]
    pub startup_delay: Duration,

    /// Delay between supervisor restart attempts.
    ///
    /// This is the base delay for exponential restart backoff.
    #[serde(with = "humantime_serde")]
    pub restart_backoff: Duration,

    /// Maximum delay between supervisor restart attempts.
    ///
    /// This caps exponential restart backoff while retries continue indefinitely.
    #[serde(with = "humantime_serde")]
    pub restart_backoff_max: Duration,

    /// Child uptime that resets the supervisor's consecutive restart count.
    #[serde(with = "humantime_serde")]
    pub restart_reset_after: Duration,

    /// Grace period for a clean shutdown after sending SIGTERM.
    #[serde(with = "humantime_serde")]
    pub shutdown_grace_period: Duration,
}

impl Default for Config {
    /// Returns conservative zcashd-compat defaults suitable for local supervision.
    ///
    /// Defaults keep zcashd-compat disabled unless explicitly requested, and use a
    /// short restart/backoff policy for child-process recovery. Shutdowns allow
    /// enough time for `zcashd` to flush wallet and chainstate data after SIGTERM.
    fn default() -> Self {
        Self {
            enabled: false,
            manage_zcashd: true,
            zcashd_source: ZcashdBinarySource::Managed,
            zcashd_path: None,
            zcashd_datadir: None,
            zcashd_extra_args: Vec::new(),
            listen_addr: None,
            cookie_dir: default_cache_dir(),
            cookie_file_name: default_cookie_file_name(),
            enable_cookie_auth: true,
            tls_cert_file: None,
            tls_key_file: None,
            tls_ca_file: None,
            unsafe_allow_remote_http: false,
            startup_delay: Duration::from_secs(1),
            restart_backoff: Duration::from_secs(2),
            restart_backoff_max: Duration::from_secs(5 * 60),
            restart_reset_after: Duration::from_secs(60 * 60),
            shutdown_grace_period: Duration::from_secs(300),
        }
    }
}

impl Config {
    /// Returns true when the dedicated zcashd-compat RPC listener should use TLS.
    pub fn tls_enabled(&self) -> bool {
        self.tls_cert_file.is_some() || self.tls_key_file.is_some()
    }
}

fn default_cookie_file_name() -> String {
    ".zcashd-compat.cookie".to_string()
}

/// Deserializes the compat listen address from either `listen_addr` (`127.0.0.1:28232`)
/// or legacy `rpc_url` (`http://127.0.0.1:28232`) formats.
fn deserialize_listen_addr_or_rpc_url<'de, D>(
    deserializer: D,
) -> Result<Option<SocketAddr>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ListenAddrValue {
        SocketAddr(SocketAddr),
        String(String),
    }

    let value = Option::<ListenAddrValue>::deserialize(deserializer)?;
    value
        .map(|value| match value {
            ListenAddrValue::SocketAddr(addr) => Ok(addr),
            ListenAddrValue::String(raw) => {
                if let Ok(addr) = raw.parse::<SocketAddr>() {
                    return Ok(addr);
                }

                let stripped = raw
                    .strip_prefix("http://")
                    .or_else(|| raw.strip_prefix("https://"))
                    .unwrap_or(&raw)
                    .trim_end_matches('/');

                stripped.parse::<SocketAddr>().map_err(|error| {
                    D::Error::custom(format!(
                        "listen_addr / rpc_url must be a socket address like \
                         127.0.0.1:28232 or URL like http://127.0.0.1:28232, got {raw:?}: {error}"
                    ))
                })
            }
        })
        .transpose()
}

/// Deserializes `zcashd_extra_args` from either a sequence or a JSON-array string.
fn deserialize_zcashd_extra_args<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ExtraArgsField {
        Sequence(Vec<String>),
        JsonString(String),
    }

    match ExtraArgsField::deserialize(deserializer)? {
        ExtraArgsField::Sequence(args) => Ok(args),
        ExtraArgsField::JsonString(args) => {
            serde_json::from_str(&args).map_err(|error| {
                D::Error::custom(format!(
                    "zcashd_extra_args must be a sequence or a JSON string array, got: {args:?}. parse error: {error}"
                ))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::{Config, ZcashdBinarySource};

    #[test]
    fn defaults_to_managed_source_without_explicit_path() {
        let config = Config::default();
        assert_eq!(config.zcashd_source, ZcashdBinarySource::Managed);
        assert_eq!(config.zcashd_path, None);
        assert_eq!(config.listen_addr, None);
        assert_eq!(config.cookie_dir, super::default_cache_dir());
        assert_eq!(config.cookie_file_name, super::default_cookie_file_name());
        assert!(config.enable_cookie_auth);
        assert!(!config.tls_enabled());
        assert_eq!(
            config.restart_reset_after,
            std::time::Duration::from_secs(60 * 60)
        );
        assert_eq!(
            config.restart_backoff_max,
            std::time::Duration::from_secs(5 * 60)
        );
    }

    #[test]
    fn default_shutdown_grace_period_allows_zcashd_to_flush_state() {
        let config = Config::default();

        assert_eq!(
            config.shutdown_grace_period,
            std::time::Duration::from_secs(300)
        );
    }

    #[test]
    fn deserialize_restart_reset_after_duration() {
        let config: Config = toml::from_str(
            r#"
            restart_reset_after = "30m"
            "#,
        )
        .expect("restart reset duration should deserialize");

        assert_eq!(
            config.restart_reset_after,
            std::time::Duration::from_secs(30 * 60)
        );
    }

    #[test]
    fn deserialize_restart_backoff_max_duration() {
        let config: Config = toml::from_str(
            r#"
            restart_backoff_max = "10m"
            "#,
        )
        .expect("restart backoff cap duration should deserialize");

        assert_eq!(
            config.restart_backoff_max,
            std::time::Duration::from_secs(10 * 60)
        );
    }

    #[test]
    fn deserialize_defaults_cookie_file_name_when_missing() {
        let config: Config = toml::from_str(
            r#"
            cookie_dir = "/tmp/zcashd-compat-cookie-dir"
            "#,
        )
        .expect("partial zcashd-compat config should deserialize");

        assert_eq!(
            config.cookie_file_name,
            super::default_cookie_file_name(),
            "missing cookie file names should use the default value"
        );
    }

    #[test]
    fn deserialize_legacy_rpc_url_into_listen_addr() {
        let config: Config = toml::from_str(
            r#"
            rpc_url = "http://127.0.0.1:28232"
            "#,
        )
        .expect("legacy rpc_url should deserialize");

        assert_eq!(
            config.listen_addr,
            Some(SocketAddr::from(([127, 0, 0, 1], 28232)))
        );
    }

    #[test]
    fn deserialize_tls_and_cookie_auth_settings() {
        let config: Config = toml::from_str(
            r#"
            enable_cookie_auth = false
            tls_cert_file = "/tmp/zebra.crt"
            tls_key_file = "/tmp/zebra.key"
            tls_ca_file = "/tmp/ca.pem"
            "#,
        )
        .expect("TLS zcashd-compat config should deserialize");

        assert!(!config.enable_cookie_auth);
        assert!(config.tls_enabled());
        assert_eq!(config.tls_cert_file, Some("/tmp/zebra.crt".into()));
        assert_eq!(config.tls_key_file, Some("/tmp/zebra.key".into()));
        assert_eq!(config.tls_ca_file, Some("/tmp/ca.pem".into()));
    }

    #[test]
    fn deserialize_extra_args_from_sequence() {
        let config: Config = toml::from_str(
            r#"
            zcashd_extra_args = ["-conf=/tmp/zcash.conf", "-debug=1"]
            "#,
        )
        .expect("valid sequence should deserialize");

        assert_eq!(
            config.zcashd_extra_args,
            vec!["-conf=/tmp/zcash.conf".to_string(), "-debug=1".to_string()]
        );
    }

    #[test]
    fn deserialize_extra_args_from_json_string() {
        let config: Config = toml::from_str(
            r#"
            zcashd_extra_args = "[\"-conf=/tmp/zcash.conf\",\"-debug=1\"]"
            "#,
        )
        .expect("valid JSON string array should deserialize");

        assert_eq!(
            config.zcashd_extra_args,
            vec!["-conf=/tmp/zcash.conf".to_string(), "-debug=1".to_string()]
        );
    }

    #[test]
    fn reject_non_array_string_extra_args() {
        let error = toml::from_str::<Config>(
            r#"
            zcashd_extra_args = "-debug=1"
            "#,
        )
        .expect_err("plain strings should be rejected");

        let error_message = error.to_string();
        assert!(
            error_message.contains("JSON string array"),
            "error should explain expected format: {error_message}"
        );
    }
}
