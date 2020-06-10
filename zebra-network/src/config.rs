use std::{
    collections::HashSet,
    net::{SocketAddr, ToSocketAddrs},
    string::String,
    time::Duration,
};

use zebra_chain::Network;

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
    pub initial_mainnet_peers: HashSet<String>,

    /// A list of initial peers for the peerset when operating on
    /// testnet.
    pub initial_testnet_peers: HashSet<String>,

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

    /// The initial target size for the peer set.
    pub peerset_initial_target_size: usize,
}

impl Config {
    fn parse_peers<S: ToSocketAddrs>(peers: HashSet<S>) -> HashSet<SocketAddr> {
        peers
            .iter()
            .flat_map(|s| s.to_socket_addrs())
            .flatten()
            .collect()
    }

    /// Get the initial seed peers based on the configured network.
    pub fn initial_peers(&self) -> HashSet<SocketAddr> {
        match self.network {
            Network::Mainnet => Config::parse_peers(self.initial_mainnet_peers.clone()),
            Network::Testnet => Config::parse_peers(self.initial_testnet_peers.clone()),
        }
    }
}

impl Default for Config {
    fn default() -> Config {
        let mainnet_peers = [
            "dnsseed.z.cash:8233",
            "dnsseed.str4d.xyz:8233",
            "mainnet.seeder.zfnd.org:8233",
            "mainnet.seeder.yolo.money:8233",
        ]
        .iter()
        .map(|&s| String::from(s))
        .collect();

        let testnet_peers = [
            "dnsseed.testnet.z.cash:18233",
            "testnet.seeder.zfnd.org:18233",
            "testnet.seeder.yolo.money:18233",
        ]
        .iter()
        .map(|&s| String::from(s))
        .collect();

        Config {
            listen_addr: "127.0.0.1:8233"
                .parse()
                .expect("Hardcoded address should be parseable"),
            user_agent: crate::constants::USER_AGENT.to_owned(),
            network: Network::Mainnet,
            initial_mainnet_peers: mainnet_peers,
            initial_testnet_peers: testnet_peers,
            ewma_default_rtt: Duration::from_secs(1),
            ewma_decay_time: Duration::from_secs(60),
            peerset_request_buffer_size: 10,
            handshake_timeout: Duration::from_secs(4),
            new_peer_interval: Duration::from_secs(60),
            peerset_initial_target_size: 50,
        }
    }
}
