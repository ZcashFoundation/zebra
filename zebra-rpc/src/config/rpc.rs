//! RPC config

use std::{net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

use zebra_chain::common::default_cache_dir;

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

    /// The cookie file name used in `cookie_dir`.
    #[serde(default = "default_cookie_file_name")]
    pub cookie_file_name: String,

    /// Enable cookie-based authentication for RPCs.
    pub enable_cookie_auth: bool,

    /// The maximum size of the response body in bytes.
    pub max_response_body_size: usize,

    /// Optional TLS configuration for this RPC listener.
    pub tls: Option<TlsConfig>,
}

/// TLS configuration for an RPC listener.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM certificate chain served to RPC clients.
    pub cert_file: PathBuf,

    /// PEM private key for the served certificate.
    pub key_file: PathBuf,
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

            // Use multiple threads, because we pause requests during getblocktemplate long polling
            parallel_cpu_threads: 0,

            // Debug options are always off by default.
            debug_force_finished_sync: false,

            // Use the default cache dir for the auth cookie.
            cookie_dir: default_cache_dir(),
            cookie_file_name: default_cookie_file_name(),

            // Enable cookie-based authentication by default.
            enable_cookie_auth: true,

            // 50 MiB
            max_response_body_size: 52_428_800,

            // Serve plain HTTP unless a caller wires TLS explicitly.
            tls: None,
        }
    }
}

fn default_cookie_file_name() -> String {
    ".cookie".to_string()
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn deserialize_defaults_cookie_file_name_when_missing() {
        let config: Config = toml::from_str(
            r#"
            listen_addr = "127.0.0.1:8232"
            "#,
        )
        .expect("partial rpc config should deserialize");

        assert_eq!(
            config.cookie_file_name,
            super::default_cookie_file_name(),
            "missing cookie file names should use the default value"
        );
    }
}
