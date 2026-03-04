//! Configuration for the crosslink (TFL) service.

use serde::{Deserialize, Serialize};

/// Configuration for the crosslink TFL service.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Local address, e.g. "/ip4/0.0.0.0/udp/24834/quic-v1"
    pub listen_address: Option<String>,
    /// Public address for this node, e.g. "/ip4/127.0.0.1/udp/24834/quic-v1" if testing
    /// internally, or the public IP address if using externally.
    pub public_address: Option<String>,
    /// temp seed for private/public key pair
    pub insecure_user_name: Option<String>,
    /// List of public IP addresses for peers, in the same format as `public_address`.
    pub malachite_peers: Vec<String>,
    /// Do not manipulate config
    pub do_not_manipulate_config: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_address: None,
            public_address: None,
            insecure_user_name: None,
            malachite_peers: Vec::new(),
            do_not_manipulate_config: false,
        }
    }
}
