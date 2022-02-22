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
    /// The recommended ports for the RPC server are:
    /// - Mainnet: Some(127.0.0.1:8232)
    /// - Testnet: Some(127.0.0.1:18232)
    ///
    /// # Security
    ///
    /// If you bind Zebra's RPC port to a public IP address,
    /// anyone on the internet can send transactions via your node.
    /// They can also query your node's state.
    pub listen_addr: Option<SocketAddr>,
}
