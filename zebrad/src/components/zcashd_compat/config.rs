use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

/// Source selector for supervised `zcashd` execution.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ZcashdBinarySource {
    /// Resolve `zcashd` from a local executable path.
    #[default]
    Path,
    /// Resolve `zcashd` from Zebra's embedded release manifest.
    Embedded,
}

/// Configuration for Zebra zcashd-compat mode.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Enables zcashd-compat mode.
    ///
    /// zcashd-compat mode supervises or validates a P2P sidecar `zcashd` process
    /// that syncs chain data from Zebra over the legacy Zcash P2P protocol.
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
    /// When set, Zebra uses this path directly and skips embedded downloads.
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
    /// Supervised zcashd runs always include `-printtoconsole` automatically.
    #[serde(default, deserialize_with = "deserialize_zcashd_extra_args")]
    pub zcashd_extra_args: Vec<String>,

    /// The Zebra legacy P2P address supervised zcashd connects to via `-connect`.
    ///
    /// If unset, Zebra derives it from its own bound legacy P2P listener
    /// (`network.listen_addr`), substituting `127.0.0.1` when the listener is
    /// bound to an unspecified address. Set this only when zcashd must reach
    /// Zebra through a different address, such as across containers.
    pub p2p_connect_addr: Option<SocketAddr>,

    /// Inbound sidecar peer IPs that must always receive block inventory broadcasts.
    ///
    /// If empty while zcashd-compat is enabled, Zebra defaults this list to
    /// loopback addresses. This setting is rejected when zcashd-compat is
    /// disabled.
    ///
    /// # Security
    ///
    /// This is a trusted-peer privilege boundary, not just a gossip list.
    /// Inbound peers connecting from these IPs also:
    /// - use a reserved inbound connection pool of one slot per listed IP
    ///   (shared across the list, without per-IP accounting), reducing the
    ///   public inbound pool by the same amount; when the reserved pool is
    ///   full, further connections from listed IPs compete for public slots
    ///   like any other peer,
    /// - bypass the recent-IP reconnection rate limit while a reserved slot
    ///   is free (fallback connections are rate limited normally), and
    /// - are exempt from the `FindBlocks`/`FindHeaders` sync stall detector.
    ///
    /// Only list addresses where every process is trusted, like the loopback
    /// addresses of a host that runs nothing but Zebra and its sidecar.
    /// Never list IPs that untrusted machines can connect from.
    ///
    /// Because environment values cannot express TOML arrays, this also accepts
    /// a JSON array string, e.g.
    /// `ZEBRA_ZCASHD_COMPAT__BLOCK_GOSSIP_PEER_IPS='["10.0.0.5"]'`.
    #[serde(default, deserialize_with = "deserialize_block_gossip_peer_ips")]
    pub block_gossip_peer_ips: Vec<IpAddr>,

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
    /// Returns conservative zcashd-compat defaults for an externally managed zcashd.
    ///
    /// Defaults keep zcashd-compat disabled unless explicitly requested, and do
    /// not spawn `zcashd` unless supervision is explicitly enabled. The
    /// restart/shutdown settings apply only to supervised `zcashd` children.
    fn default() -> Self {
        Self {
            enabled: false,
            manage_zcashd: false,
            zcashd_source: ZcashdBinarySource::Path,
            zcashd_path: None,
            zcashd_datadir: None,
            zcashd_extra_args: Vec::new(),
            p2p_connect_addr: None,
            block_gossip_peer_ips: Vec::new(),
            startup_delay: Duration::from_secs(1),
            restart_backoff: Duration::from_secs(2),
            restart_backoff_max: Duration::from_secs(5 * 60),
            restart_reset_after: Duration::from_secs(60 * 60),
            shutdown_grace_period: Duration::from_secs(300),
        }
    }
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

/// Deserializes `block_gossip_peer_ips` from either a sequence or a JSON-array
/// string, so it can be set through the environment (which cannot express TOML
/// arrays) in cross-container deployments where the sidecar's source IP is not
/// loopback.
fn deserialize_block_gossip_peer_ips<'de, D>(deserializer: D) -> Result<Vec<IpAddr>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum PeerIpsField {
        Sequence(Vec<IpAddr>),
        JsonString(String),
    }

    match PeerIpsField::deserialize(deserializer)? {
        PeerIpsField::Sequence(ips) => Ok(ips),
        PeerIpsField::JsonString(ips) => serde_json::from_str(&ips).map_err(|error| {
            D::Error::custom(format!(
                "block_gossip_peer_ips must be a sequence or a JSON string array, got: {ips:?}. parse error: {error}"
            ))
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::{Config, ZcashdBinarySource};

    #[test]
    fn defaults_to_unsupervised_path_source_without_explicit_path() {
        let config = Config::default();
        assert!(!config.manage_zcashd);
        assert_eq!(config.zcashd_source, ZcashdBinarySource::Path);
        assert_eq!(config.zcashd_path, None);
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
    fn deserialize_block_gossip_peer_ips() {
        let config: Config = toml::from_str(
            r#"
            block_gossip_peer_ips = ["127.0.0.1"]
            "#,
        )
        .expect("sidecar block gossip peer IPs should deserialize");

        assert_eq!(
            config.block_gossip_peer_ips,
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]
        );
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
