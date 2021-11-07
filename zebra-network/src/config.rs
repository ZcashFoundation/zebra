use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    string::String,
    time::Duration,
};

use serde::{de, Deserialize, Deserializer};

use zebra_chain::parameters::Network;

use crate::{constants, protocol::external::canonical_socket_addr, BoxError};

#[cfg(test)]
mod tests;

/// The number of times Zebra will retry each initial peer's DNS resolution,
/// before checking if any other initial peers have returned addresses.
const MAX_SINGLE_PEER_RETRIES: usize = 2;

/// Configuration for networking code.
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The address on which this node should listen for connections.
    ///
    /// Can be `address:port` or just `address`. If there is no configured
    /// port, Zebra will use the default port for the configured `network`.
    ///
    /// `address` can be an IP address or a DNS name. DNS names are
    /// only resolved once, when Zebra starts up.
    ///
    /// If a specific listener address is configured, Zebra will advertise
    /// it to other nodes. But by default, Zebra uses an unspecified address
    /// ("0.0.0.0" or "[::]"), which is not advertised to other nodes.
    ///
    /// Zebra does not currently support:
    /// - [Advertising a different external IP address #1890](https://github.com/ZcashFoundation/zebra/issues/1890), or
    /// - [Auto-discovering its own external IP address #1893](https://github.com/ZcashFoundation/zebra/issues/1893).
    ///
    /// However, other Zebra instances compensate for unspecified or incorrect
    /// listener addresses by adding the external IP addresses of peers to
    /// their address books.
    pub listen_addr: SocketAddr,

    /// The network to connect to.
    pub network: Network,

    /// A list of initial peers for the peerset when operating on
    /// mainnet.
    pub initial_mainnet_peers: HashSet<String>,

    /// A list of initial peers for the peerset when operating on
    /// testnet.
    pub initial_testnet_peers: HashSet<String>,

    /// The initial target size for the peer set.
    ///
    /// Also used to limit the number of inbound and outbound connections made by Zebra.
    ///
    /// If you have a slow network connection, and Zebra is having trouble
    /// syncing, try reducing the peer set size. You can also reduce the peer
    /// set size to reduce Zebra's bandwidth usage.
    pub peerset_initial_target_size: usize,

    /// How frequently we attempt to crawl the network to discover new peer
    /// addresses.
    ///
    /// Zebra asks its connected peers for more peer addresses:
    /// - regularly, every time `crawl_new_peer_interval` elapses, and
    /// - if the peer set is busy, and there aren't any peer addresses for the
    ///   next connection attempt.
    //
    // Note: Durations become a TOML table, so they must be the final item in the config
    //       We'll replace them with a more user-friendly format in #2847
    pub crawl_new_peer_interval: Duration,
}

impl Config {
    /// The maximum number of outbound connections that Zebra will open at the same time.
    /// When this limit is reached, Zebra stops opening outbound connections.
    ///
    /// # Security
    ///
    /// This is larger than the inbound connection limit,
    /// so Zebra is more likely to be connected to peers that it has selected.
    pub fn peerset_outbound_connection_limit(&self) -> usize {
        let inbound_limit = self.peerset_inbound_connection_limit();

        inbound_limit + inbound_limit / constants::OUTBOUND_PEER_BIAS_DENOMINATOR
    }

    /// The maximum number of inbound connections that Zebra will accept at the same time.
    /// When this limit is reached, Zebra drops new inbound connections without handshaking on them.
    pub fn peerset_inbound_connection_limit(&self) -> usize {
        self.peerset_initial_target_size
    }

    /// The maximum number of inbound and outbound connections that Zebra will have at the same time.
    pub fn peerset_total_connection_limit(&self) -> usize {
        self.peerset_outbound_connection_limit() + self.peerset_inbound_connection_limit()
    }

    /// Get the initial seed peers based on the configured network.
    pub async fn initial_peers(&self) -> HashSet<SocketAddr> {
        match self.network {
            Network::Mainnet => Config::resolve_peers(&self.initial_mainnet_peers).await,
            Network::Testnet => Config::resolve_peers(&self.initial_testnet_peers).await,
        }
    }

    /// Concurrently resolves `peers` into zero or more IP addresses, with a
    /// timeout of a few seconds on each DNS request.
    ///
    /// If DNS resolution fails or times out for all peers, continues retrying
    /// until at least one peer is found.
    async fn resolve_peers(peers: &HashSet<String>) -> HashSet<SocketAddr> {
        use futures::stream::StreamExt;

        if peers.is_empty() {
            warn!(
                "no initial peers in the network config. \
                 Hint: you must configure at least one peer IP or DNS seeder to run Zebra, \
                 or make sure Zebra's listener port gets inbound connections."
            );
            return HashSet::new();
        }

        loop {
            // We retry each peer individually, as well as retrying if there are
            // no peers in the combined list. DNS failures are correlated, so all
            // peers can fail DNS, leaving Zebra with a small list of custom IP
            // address peers. Individual retries avoid this issue.
            let peer_addresses = peers
                .iter()
                .map(|s| Config::resolve_host(s, MAX_SINGLE_PEER_RETRIES))
                .collect::<futures::stream::FuturesUnordered<_>>()
                .concat()
                .await;

            if peer_addresses.is_empty() {
                tracing::info!(
                    ?peers,
                    ?peer_addresses,
                    "empty peer list after DNS resolution, retrying after {} seconds",
                    crate::constants::DNS_LOOKUP_TIMEOUT.as_secs()
                );
                tokio::time::sleep(crate::constants::DNS_LOOKUP_TIMEOUT).await;
            } else {
                return peer_addresses;
            }
        }
    }

    /// Resolves `host` into zero or more IP addresses, retrying up to
    /// `max_retries` times.
    ///
    /// If DNS continues to fail, returns an empty list of addresses.
    async fn resolve_host(host: &str, max_retries: usize) -> HashSet<SocketAddr> {
        for retry_count in 1..=max_retries {
            match Config::resolve_host_once(host).await {
                Ok(addresses) => return addresses,
                Err(_) => tracing::info!(?host, ?retry_count, "Retrying peer DNS resolution"),
            };
            tokio::time::sleep(crate::constants::DNS_LOOKUP_TIMEOUT).await;
        }

        HashSet::new()
    }

    /// Resolves `host` into zero or more IP addresses.
    ///
    /// If `host` is a DNS name, performs DNS resolution with a timeout of a few seconds.
    /// If DNS resolution fails or times out, returns an error.
    async fn resolve_host_once(host: &str) -> Result<HashSet<SocketAddr>, BoxError> {
        let fut = tokio::net::lookup_host(host);
        let fut = tokio::time::timeout(crate::constants::DNS_LOOKUP_TIMEOUT, fut);

        match fut.await {
            Ok(Ok(ip_addrs)) => {
                let ip_addrs: Vec<SocketAddr> = ip_addrs.map(canonical_socket_addr).collect();

                // if we're logging at debug level,
                // the full list of IP addresses will be shown in the log message
                let debug_span = debug_span!("", remote_ip_addrs = ?ip_addrs);
                let _span_guard = debug_span.enter();
                info!(seed = ?host, remote_ip_count = ?ip_addrs.len(), "resolved seed peer IP addresses");

                for ip in &ip_addrs {
                    // Count each initial peer, recording the seed config and resolved IP address.
                    //
                    // If an IP is returned by multiple seeds,
                    // each duplicate adds 1 to the initial peer count.
                    // (But we only make one initial connection attempt to each IP.)
                    metrics::counter!(
                        "zcash.net.peers.initial",
                        1,
                        "seed" => host.to_string(),
                        "remote_ip" => ip.to_string()
                    );
                }

                Ok(ip_addrs.into_iter().collect())
            }
            Ok(Err(e)) => {
                tracing::info!(?host, ?e, "DNS error resolving peer IP addresses");
                Err(e.into())
            }
            Err(e) => {
                tracing::info!(?host, ?e, "DNS timeout resolving peer IP addresses");
                Err(e.into())
            }
        }
    }
}

impl Default for Config {
    fn default() -> Config {
        let mainnet_peers = [
            "dnsseed.z.cash:8233",
            "dnsseed.str4d.xyz:8233",
            "mainnet.seeder.zfnd.org:8233",
            "mainnet.is.yolo.money:8233",
        ]
        .iter()
        .map(|&s| String::from(s))
        .collect();

        let testnet_peers = [
            "dnsseed.testnet.z.cash:18233",
            "testnet.seeder.zfnd.org:18233",
            "testnet.is.yolo.money:18233",
        ]
        .iter()
        .map(|&s| String::from(s))
        .collect();

        Config {
            listen_addr: "0.0.0.0:8233"
                .parse()
                .expect("Hardcoded address should be parseable"),
            network: Network::Mainnet,
            initial_mainnet_peers: mainnet_peers,
            initial_testnet_peers: testnet_peers,
            crawl_new_peer_interval: Duration::from_secs(60),

            // The default peerset target size should be large enough to ensure
            // nodes have a reliable set of peers. But it should also be limited
            // to a reasonable size, to avoid queueing too many in-flight block
            // downloads. A large queue of in-flight block downloads can choke a
            // constrained local network connection.
            //
            // We assume that Zebra nodes have at least 10 Mbps bandwidth.
            // Therefore, a maximum-sized block can take up to 2 seconds to
            // download. So a full default peer set adds up to 100 seconds worth
            // of blocks to the queue.
            //
            // But the peer set for slow nodes is typically much smaller, due to
            // the handshake RTT timeout.
            peerset_initial_target_size: 50,
        }
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, default)]
        struct DConfig {
            listen_addr: String,
            network: Network,
            initial_mainnet_peers: HashSet<String>,
            initial_testnet_peers: HashSet<String>,
            peerset_initial_target_size: usize,
            #[serde(alias = "new_peer_interval")]
            crawl_new_peer_interval: Duration,
        }

        impl Default for DConfig {
            fn default() -> Self {
                let config = Config::default();
                Self {
                    listen_addr: config.listen_addr.to_string(),
                    network: config.network,
                    initial_mainnet_peers: config.initial_mainnet_peers,
                    initial_testnet_peers: config.initial_testnet_peers,
                    peerset_initial_target_size: config.peerset_initial_target_size,
                    crawl_new_peer_interval: config.crawl_new_peer_interval,
                }
            }
        }

        let config = DConfig::deserialize(deserializer)?;

        // TODO: perform listener DNS lookups asynchronously with a timeout (#1631)
        let listen_addr = match config.listen_addr.parse::<SocketAddr>() {
            Ok(socket) => Ok(socket),
            Err(_) => match config.listen_addr.parse::<IpAddr>() {
                Ok(ip) => Ok(SocketAddr::new(ip, config.network.default_port())),
                Err(err) => Err(de::Error::custom(format!(
                    "{}; Hint: addresses can be a IPv4, IPv6 (with brackets), or a DNS name, the port is optional",
                    err
                ))),
            },
        }?;

        Ok(Config {
            listen_addr: canonical_socket_addr(listen_addr),
            network: config.network,
            initial_mainnet_peers: config.initial_mainnet_peers,
            initial_testnet_peers: config.initial_testnet_peers,
            peerset_initial_target_size: config.peerset_initial_target_size,
            crawl_new_peer_interval: config.crawl_new_peer_interval,
        })
    }
}
