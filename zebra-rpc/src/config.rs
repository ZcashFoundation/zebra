//! User-configurable RPC settings.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// RPC configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
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

    /// The number of threads used to process RPC requests and responses.
    ///
    /// Zebra's RPC server has a separate thread pool and a `tokio` executor for each thread.
    /// State queries are run concurrently using the shared thread pool controlled by
    /// the [`SyncSection.parallel_cpu_threads`](https://doc.zebra.zfnd.org/zebrad/config/struct.SyncSection.html#structfield.parallel_cpu_threads) config.
    ///
    /// We recommend setting both configs to `0` (automatic scaling) for the best performance.
    /// This uses one thread per available CPU core.
    ///
    /// Set to `1` by default, which runs all RPC queries on a single thread, and detects RPC
    /// port conflicts from multiple Zebra or `zcashd` instances.
    ///
    /// For details, see [the `jsonrpc_http_server` documentation](https://docs.rs/jsonrpc-http-server/latest/jsonrpc_http_server/struct.ServerBuilder.html#method.threads).
    ///
    /// ## Warning
    ///
    /// Changing this config disables RPC port conflict detection.
    /// This can allow multiple Zebra instances to share the same RPC port.
    ///
    /// If some of those instances are outdated or failed, RPC queries can be slow or inconsistent.
    pub parallel_cpu_threads: usize,

    /// Test-only option that makes Zebra say it is at the chain tip,
    /// no matter what the estimated height or local clock is.
    pub debug_force_finished_sync: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Disable RPCs by default.
            listen_addr: None,

            // Use a single thread, so we can detect RPC port conflicts.
            parallel_cpu_threads: 1,

            // Debug options are always off by default.
            debug_force_finished_sync: false,
        }
    }
}
