//! User-configurable RPC parameters.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// RPC configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Turn on/off the RPC server
    pub listen: bool,

    /// IP address and port for the RPC server
    pub listen_addr: SocketAddr,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: false,
            listen_addr: "127.0.0.1:8232"
                .parse()
                .expect("Hardcoded address should be parseable"),
        }
    }
}
