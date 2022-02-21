//! User-configurable RPC parameters.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// RPC configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// IP address and port for the RPC server.
    ///
    /// Note: RPC server will not start if this is `None`.
    pub listen_addr: Option<SocketAddr>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: Some(
                "127.0.0.1:8232"
                    .parse()
                    .expect("Hardcoded address should be parseable"),
            ),
        }
    }
}
