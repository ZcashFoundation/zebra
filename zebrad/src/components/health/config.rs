use std::{net::SocketAddr, time::Duration};

use serde::{Deserialize, Serialize};

/// Health server configuration.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Address to bind the health server to.
    ///
    /// The server is disabled when this is `None`.
    pub listen_addr: Option<SocketAddr>,
    /// Minimum number of recently live peers to consider the node healthy.
    ///
    /// Used by `/healthy`.
    pub min_connected_peers: usize,
    /// Maximum allowed estimated blocks behind the network tip for readiness.
    ///
    /// Used by `/ready`. Negative estimates are treated as 0.
    pub ready_max_blocks_behind: i64,
    /// Enforce readiness checks on test networks.
    ///
    /// If `false`, `/ready` always returns 200 on regtest and testnets.
    pub enforce_on_test_networks: bool,
    /// Maximum age of the last committed block before readiness fails.
    #[serde(with = "humantime_serde")]
    pub ready_max_tip_age: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: None,
            min_connected_peers: 1,
            ready_max_blocks_behind: 2,
            enforce_on_test_networks: false,
            ready_max_tip_age: DEFAULT_READY_MAX_TIP_AGE,
        }
    }
}

const DEFAULT_READY_MAX_TIP_AGE: Duration = Duration::from_secs(5 * 60);
