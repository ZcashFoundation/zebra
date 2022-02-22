//! User-configurable RPC parameters.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// RPC configuration section.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// IP address and port for the RPC server.
    ///
    /// Note: RPC server will not start if this is `None`.
    ///
    // The recommended ports for the RPC server is:
    /// - Mainnet: 8232
    /// - Testnet: 83232
    pub listen_addr: Option<SocketAddr>,
}
