use std::{
    net::{SocketAddr, ToSocketAddrs},
    time::Duration,
};

use crate::network::Network;

/// Configuration for networking code.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The address on which this node should listen for connections.
    pub listen_addr: SocketAddr,

    /// The network to connect to.
    pub network: Network,

    /// The user-agent to advertise.
    pub user_agent: String,

    /// A list of initial peers for the peerset when operating on
    /// mainnet.
    ///
    /// XXX this should be replaced with DNS names, not SocketAddrs
    pub initial_mainnet_peers: Vec<SocketAddr>,

    /// A list of initial peers for the peerset when operating on
    /// testnet.
    pub initial_testnet_peers: Vec<SocketAddr>,

    /// The outgoing request buffer size for the peer set.
    pub peerset_request_buffer_size: usize,

    // Note: due to the way this is rendered by the toml
    // serializer, the Duration fields should come last.
    /// The default RTT estimate for peer responses, used in load-balancing.
    pub ewma_default_rtt: Duration,

    /// The decay time for the exponentially-weighted moving average response time.
    pub ewma_decay_time: Duration,

    /// The timeout for peer handshakes.
    pub handshake_timeout: Duration,

    /// How frequently we attempt to connect to a new peer.
    pub new_peer_interval: Duration,
}

impl Config {
    fn parse_peers<S: ToSocketAddrs>(peers: Vec<S>) -> Vec<SocketAddr> {
        peers
            .iter()
            .flat_map(|s| s.to_socket_addrs())
            .flatten()
            .collect::<Vec<SocketAddr>>()
    }

    /// Get the initial seed peers based on the configured network.
    pub fn initial_peers(&self) -> Vec<SocketAddr> {
        match self.network {
            Network::Mainnet => self.initial_mainnet_peers.clone(),
            Network::Testnet => self.initial_testnet_peers.clone(),
        }
    }
}

impl Default for Config {
    fn default() -> Config {
        Config {
            listen_addr: "127.0.0.1:28233"
                .parse()
                .expect("Hardcoded address should be parseable"),
            user_agent: crate::constants::USER_AGENT.to_owned(),
            network: Network::Mainnet,
            initial_mainnet_peers: Config::parse_peers(vec![
                "dnsseed.z.cash:8233",
                "dnsseed.str4d.xyz:8233",
                "dnsseed.znodes.org:8233",
            ]),
            initial_testnet_peers: Config::parse_peers(vec!["dnsseed.testnet.z.cash:18233"]),
            ewma_default_rtt: Duration::from_secs(1),
            ewma_decay_time: Duration::from_secs(60),
            peerset_request_buffer_size: 1,
            handshake_timeout: Duration::from_secs(4),
            new_peer_interval: Duration::from_secs(120),
        }
    }
}
