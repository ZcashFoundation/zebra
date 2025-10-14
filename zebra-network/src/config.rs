//! Configuration for Zebra's network communication.

use std::{
    collections::HashSet,
    io::{self, ErrorKind},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use indexmap::IndexSet;
use serde::{de, Deserialize, Deserializer};
use tokio::fs;

use tracing::Span;
use zebra_chain::{
    common::atomic_write,
    parameters::{
        testnet::{
            self, ConfiguredActivationHeights, ConfiguredCheckpoints, ConfiguredFundingStreams,
            ConfiguredLockboxDisbursement, RegtestParameters,
        },
        Magic, Network, NetworkKind,
    },
    work::difficulty::U256,
};

use crate::{
    constants::{
        DEFAULT_CRAWL_NEW_PEER_INTERVAL, DEFAULT_MAX_CONNS_PER_IP,
        DEFAULT_PEERSET_INITIAL_TARGET_SIZE, DNS_LOOKUP_TIMEOUT, INBOUND_PEER_LIMIT_MULTIPLIER,
        MAX_PEER_DISK_CACHE_SIZE, OUTBOUND_PEER_LIMIT_MULTIPLIER,
    },
    protocol::external::{canonical_peer_addr, canonical_socket_addr},
    BoxError, PeerSocketAddr,
};

mod cache_dir;

#[cfg(test)]
mod tests;

pub use cache_dir::CacheDir;

/// The number of times Zebra will retry each initial peer's DNS resolution,
/// before checking if any other initial peers have returned addresses.
///
/// After doing this number of retries of a failed single peer, Zebra will
/// check if it has enough peer addresses from other seed peers. If it has
/// enough addresses, it won't retry this peer again.
///
/// If the number of retries is `0`, other peers are checked after every successful
/// or failed DNS attempt.
const MAX_SINGLE_SEED_PEER_DNS_RETRIES: usize = 0;

/// Configuration for networking code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, default, into = "DConfig")]
pub struct Config {
    /// The address on which this node should listen for connections.
    ///
    /// Can be `address:port` or just `address`. If there is no configured
    /// port, Zebra will use the default port for the configured `network`.
    ///
    /// `address` can be an IP address or a DNS name. DNS names are
    /// only resolved once, when Zebra starts up.
    ///
    /// By default, Zebra listens on `[::]` (all IPv6 and IPv4 addresses).
    /// This enables dual-stack support, accepting both IPv4 and IPv6 connections.
    ///
    /// If a specific listener address is configured, Zebra will advertise
    /// it to other nodes. But by default, Zebra uses an unspecified address
    /// ("\[::\]:port"), which is not advertised to other nodes.
    ///
    /// Zebra does not currently support:
    /// - [Advertising a different external IP address #1890](https://github.com/ZcashFoundation/zebra/issues/1890), or
    /// - [Auto-discovering its own external IP address #1893](https://github.com/ZcashFoundation/zebra/issues/1893).
    ///
    /// However, other Zebra instances compensate for unspecified or incorrect
    /// listener addresses by adding the external IP addresses of peers to
    /// their address books.
    pub listen_addr: SocketAddr,

    /// The external address of this node if any.
    ///
    /// Zebra bind to `listen_addr` but this can be an internal address if the node
    /// is behind a firewall, load balancer or NAT. This field can be used to
    /// advertise a different address to peers making it possible to receive inbound
    /// connections and contribute to the P2P network from behind a firewall, load balancer, or NAT.
    pub external_addr: Option<SocketAddr>,

    /// The network to connect to.
    pub network: Network,

    /// A list of initial peers for the peerset when operating on
    /// mainnet.
    pub initial_mainnet_peers: IndexSet<String>,

    /// A list of initial peers for the peerset when operating on
    /// testnet.
    pub initial_testnet_peers: IndexSet<String>,

    /// An optional root directory for storing cached peer address data.
    ///
    /// # Configuration
    ///
    /// Set to:
    /// - `true` to read and write peer addresses to disk using the default cache path,
    /// - `false` to disable reading and writing peer addresses to disk,
    /// - `'/custom/cache/directory'` to read and write peer addresses to a custom directory.
    ///
    /// By default, all Zebra instances run by the same user will share a single peer cache.
    /// If you use a custom cache path, you might also want to change `state.cache_dir`.
    ///
    /// # Functionality
    ///
    /// The peer cache is a list of the addresses of some recently useful peers.
    ///
    /// For privacy reasons, the cache does *not* include any other information about peers,
    /// such as when they were connected to the node.
    ///
    /// Deleting or modifying the peer cache can impact your node's:
    /// - reliability: if DNS or the Zcash DNS seeders are unavailable or broken
    /// - security: if DNS is compromised with malicious peers
    ///
    /// If you delete it, Zebra will replace it with a fresh set of peers from the DNS seeders.
    ///
    /// # Defaults
    ///
    /// The default directory is platform dependent, based on
    /// [`dirs::cache_dir()`](https://docs.rs/dirs/3.0.1/dirs/fn.cache_dir.html):
    ///
    /// |Platform | Value                                           | Example                              |
    /// | ------- | ----------------------------------------------- | ------------------------------------ |
    /// | Linux   | `$XDG_CACHE_HOME/zebra` or `$HOME/.cache/zebra` | `/home/alice/.cache/zebra`           |
    /// | macOS   | `$HOME/Library/Caches/zebra`                    | `/Users/Alice/Library/Caches/zebra`  |
    /// | Windows | `{FOLDERID_LocalAppData}\zebra`                 | `C:\Users\Alice\AppData\Local\zebra` |
    /// | Other   | `std::env::current_dir()/cache/zebra`           | `/cache/zebra`                       |
    ///
    /// # Security
    ///
    /// If you are running Zebra with elevated permissions ("root"), create the
    /// directory for this file before running Zebra, and make sure the Zebra user
    /// account has exclusive access to that directory, and other users can't modify
    /// its parent directories.
    ///
    /// # Implementation Details
    ///
    /// Each network has a separate peer list, which is updated regularly from the current
    /// address book. These lists are stored in `network/mainnet.peers` and
    /// `network/testnet.peers` files, underneath the `cache_dir` path.
    ///
    /// Previous peer lists are automatically loaded at startup, and used to populate the
    /// initial peer set and address book.
    pub cache_dir: CacheDir,

    /// The initial target size for the peer set.
    ///
    /// Also used to limit the number of inbound and outbound connections made by Zebra,
    /// and the size of the cached peer list.
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
    #[serde(with = "humantime_serde")]
    pub crawl_new_peer_interval: Duration,

    /// The maximum number of peer connections Zebra will keep for a given IP address
    /// before it drops any additional peer connections with that IP.
    ///
    /// The default and minimum value are 1.
    ///
    /// # Security
    ///
    /// Increasing this config above 1 reduces Zebra's network security.
    ///
    /// If this config is greater than 1, Zebra can initiate multiple outbound handshakes to the same
    /// IP address.
    ///
    /// This config does not currently limit the number of inbound connections that Zebra will accept
    /// from the same IP address.
    ///
    /// If Zebra makes multiple inbound or outbound connections to the same IP, they will be dropped
    /// after the handshake, but before adding them to the peer set. The total numbers of inbound and
    /// outbound connections are also limited to a multiple of `peerset_initial_target_size`.
    pub max_connections_per_ip: usize,
}

impl Config {
    /// The maximum number of outbound connections that Zebra will open at the same time.
    /// When this limit is reached, Zebra stops opening outbound connections.
    ///
    /// # Security
    ///
    /// See the note at [`INBOUND_PEER_LIMIT_MULTIPLIER`].
    ///
    /// # Performance
    ///
    /// Zebra's peer set should be limited to a reasonable size,
    /// to avoid queueing too many in-flight block downloads.
    /// A large queue of in-flight block downloads can choke a
    /// constrained local network connection.
    ///
    /// We assume that Zebra nodes have at least 10 Mbps bandwidth.
    /// Therefore, a maximum-sized block can take up to 2 seconds to
    /// download. So the initial outbound peer set adds up to 100 seconds worth
    /// of blocks to the queue. If Zebra has reached its outbound peer limit,
    /// that adds an extra 200 seconds of queued blocks.
    ///
    /// But the peer set for slow nodes is typically much smaller, due to
    /// the handshake RTT timeout. And Zebra responds to inbound request
    /// overloads by dropping peer connections.
    pub fn peerset_outbound_connection_limit(&self) -> usize {
        self.peerset_initial_target_size * OUTBOUND_PEER_LIMIT_MULTIPLIER
    }

    /// The maximum number of inbound connections that Zebra will accept at the same time.
    /// When this limit is reached, Zebra drops new inbound connections,
    /// without handshaking on them.
    ///
    /// # Security
    ///
    /// See the note at [`INBOUND_PEER_LIMIT_MULTIPLIER`].
    pub fn peerset_inbound_connection_limit(&self) -> usize {
        self.peerset_initial_target_size * INBOUND_PEER_LIMIT_MULTIPLIER
    }

    /// The maximum number of inbound and outbound connections that Zebra will have
    /// at the same time.
    pub fn peerset_total_connection_limit(&self) -> usize {
        self.peerset_outbound_connection_limit() + self.peerset_inbound_connection_limit()
    }

    /// Returns the initial seed peer hostnames for the configured network.
    pub fn initial_peer_hostnames(&self) -> IndexSet<String> {
        match &self.network {
            Network::Mainnet => self.initial_mainnet_peers.clone(),
            Network::Testnet(_params) => self.initial_testnet_peers.clone(),
        }
    }

    /// Resolve initial seed peer IP addresses, based on the configured network,
    /// and load cached peers from disk, if available.
    ///
    /// # Panics
    ///
    /// If a configured address is an invalid [`SocketAddr`] or DNS name.
    pub async fn initial_peers(&self) -> HashSet<PeerSocketAddr> {
        // TODO: do DNS and disk in parallel if startup speed becomes important
        let dns_peers =
            Config::resolve_peers(&self.initial_peer_hostnames().iter().cloned().collect()).await;

        if self.network.is_regtest() {
            // Only return local peer addresses and skip loading the peer cache on Regtest.
            dns_peers
                .into_iter()
                .filter(PeerSocketAddr::is_localhost)
                .collect()
        } else {
            // Ignore disk errors because the cache is optional and the method already logs them.
            let disk_peers = self.load_peer_cache().await.unwrap_or_default();

            dns_peers.into_iter().chain(disk_peers).collect()
        }
    }

    /// Concurrently resolves `peers` into zero or more IP addresses, with a
    /// timeout of a few seconds on each DNS request.
    ///
    /// If DNS resolution fails or times out for all peers, continues retrying
    /// until at least one peer is found.
    async fn resolve_peers(peers: &HashSet<String>) -> HashSet<PeerSocketAddr> {
        use futures::stream::StreamExt;

        if peers.is_empty() {
            warn!(
                "no initial peers in the network config. \
                 Hint: you must configure at least one peer IP or DNS seeder to run Zebra, \
                 give it some previously cached peer IP addresses on disk, \
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
                .map(|s| Config::resolve_host(s, MAX_SINGLE_SEED_PEER_DNS_RETRIES))
                .collect::<futures::stream::FuturesUnordered<_>>()
                .concat()
                .await;

            if peer_addresses.is_empty() {
                tracing::info!(
                    ?peers,
                    ?peer_addresses,
                    "empty peer list after DNS resolution, retrying after {} seconds",
                    DNS_LOOKUP_TIMEOUT.as_secs(),
                );
                tokio::time::sleep(DNS_LOOKUP_TIMEOUT).await;
            } else {
                return peer_addresses;
            }
        }
    }

    /// Resolves `host` into zero or more IP addresses, retrying up to
    /// `max_retries` times.
    ///
    /// If DNS continues to fail, returns an empty list of addresses.
    ///
    /// # Panics
    ///
    /// If a configured address is an invalid [`SocketAddr`] or DNS name.
    async fn resolve_host(host: &str, max_retries: usize) -> HashSet<PeerSocketAddr> {
        for retries in 0..=max_retries {
            if let Ok(addresses) = Config::resolve_host_once(host).await {
                return addresses;
            }

            if retries < max_retries {
                tracing::info!(
                    ?host,
                    previous_attempts = ?(retries + 1),
                    "Waiting {DNS_LOOKUP_TIMEOUT:?} to retry seed peer DNS resolution",
                );
                tokio::time::sleep(DNS_LOOKUP_TIMEOUT).await;
            } else {
                tracing::info!(
                    ?host,
                    attempts = ?(retries + 1),
                    "Seed peer DNS resolution failed, checking for addresses from other seed peers",
                );
            }
        }

        HashSet::new()
    }

    /// Resolves `host` into zero or more IP addresses.
    ///
    /// If `host` is a DNS name, performs DNS resolution with a timeout of a few seconds.
    /// If DNS resolution fails or times out, returns an error.
    ///
    /// # Panics
    ///
    /// If a configured address is an invalid [`SocketAddr`] or DNS name.
    async fn resolve_host_once(host: &str) -> Result<HashSet<PeerSocketAddr>, BoxError> {
        let fut = tokio::net::lookup_host(host);
        let fut = tokio::time::timeout(DNS_LOOKUP_TIMEOUT, fut);

        match fut.await {
            Ok(Ok(ip_addrs)) => {
                let ip_addrs: Vec<PeerSocketAddr> = ip_addrs.map(canonical_peer_addr).collect();

                // This log is needed for user debugging, but it's annoying during tests.
                #[cfg(not(test))]
                info!(seed = ?host, remote_ip_count = ?ip_addrs.len(), "resolved seed peer IP addresses");
                #[cfg(test)]
                debug!(seed = ?host, remote_ip_count = ?ip_addrs.len(), "resolved seed peer IP addresses");

                for ip in &ip_addrs {
                    // Count each initial peer, recording the seed config and resolved IP address.
                    //
                    // If an IP is returned by multiple seeds,
                    // each duplicate adds 1 to the initial peer count.
                    // (But we only make one initial connection attempt to each IP.)
                    metrics::counter!(
                        "zcash.net.peers.initial",
                        "seed" => host.to_string(),
                        "remote_ip" => ip.to_string()
                    )
                    .increment(1);
                }

                Ok(ip_addrs.into_iter().collect())
            }
            Ok(Err(e)) if e.kind() == ErrorKind::InvalidInput => {
                // TODO: add testnet/mainnet ports, like we do with the listener address
                panic!(
                    "Invalid peer IP address in Zebra config: addresses must have ports:\n\
                     resolving {host:?} returned {e:?}"
                );
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

    /// Returns the addresses in the peer list cache file, if available.
    pub async fn load_peer_cache(&self) -> io::Result<HashSet<PeerSocketAddr>> {
        let Some(peer_cache_file) = self.cache_dir.peer_cache_file_path(&self.network) else {
            return Ok(HashSet::new());
        };

        let peer_list = match fs::read_to_string(&peer_cache_file).await {
            Ok(peer_list) => peer_list,
            Err(peer_list_error) => {
                // We expect that the cache will be missing for new Zebra installs
                if peer_list_error.kind() == ErrorKind::NotFound {
                    return Ok(HashSet::new());
                } else {
                    info!(
                        ?peer_list_error,
                        "could not load cached peer list, using default seed peers"
                    );
                    return Err(peer_list_error);
                }
            }
        };

        // Skip and log addresses that don't parse, and automatically deduplicate using the HashSet.
        // (These issues shouldn't happen unless users modify the file.)
        let peer_list: HashSet<PeerSocketAddr> = peer_list
            .lines()
            .filter_map(|peer| {
                peer.parse()
                    .map_err(|peer_parse_error| {
                        info!(
                            ?peer_parse_error,
                            "invalid peer address in cached peer list, skipping"
                        );
                        peer_parse_error
                    })
                    .ok()
            })
            .collect();

        // This log is needed for user debugging, but it's annoying during tests.
        #[cfg(not(test))]
        info!(
            cached_ip_count = ?peer_list.len(),
            ?peer_cache_file,
            "loaded cached peer IP addresses"
        );
        #[cfg(test)]
        debug!(
            cached_ip_count = ?peer_list.len(),
            ?peer_cache_file,
            "loaded cached peer IP addresses"
        );

        for ip in &peer_list {
            // Count each initial peer, recording the cache file and loaded IP address.
            //
            // If an IP is returned by DNS seeders and the cache,
            // each duplicate adds 1 to the initial peer count.
            // (But we only make one initial connection attempt to each IP.)
            metrics::counter!(
                "zcash.net.peers.initial",
                "cache" => peer_cache_file.display().to_string(),
                "remote_ip" => ip.to_string()
            )
            .increment(1);
        }

        Ok(peer_list)
    }

    /// Atomically writes a new `peer_list` to the peer list cache file, if configured.
    /// If the list is empty, keeps the previous cache file.
    ///
    /// Also creates the peer cache directory, if it doesn't already exist.
    ///
    /// Atomic writes avoid corrupting the cache if Zebra panics or crashes, or if multiple Zebra
    /// instances try to read and write the same cache file.
    pub async fn update_peer_cache(&self, peer_list: HashSet<PeerSocketAddr>) -> io::Result<()> {
        let Some(peer_cache_file) = self.cache_dir.peer_cache_file_path(&self.network) else {
            return Ok(());
        };

        if peer_list.is_empty() {
            info!(
                ?peer_cache_file,
                "cacheable peer list was empty, keeping previous cache"
            );
            return Ok(());
        }

        // Turn IP addresses into strings
        let mut peer_list: Vec<String> = peer_list
            .iter()
            .take(MAX_PEER_DISK_CACHE_SIZE)
            .map(|redacted_peer| redacted_peer.remove_socket_addr_privacy().to_string())
            .collect();
        // # Privacy
        //
        // Sort to destroy any peer order, which could leak peer connection times.
        // (Currently the HashSet argument does this as well.)
        peer_list.sort();
        // Make a newline-separated list
        let peer_data = peer_list.join("\n");

        // Write the peer cache file atomically so the cache is not corrupted if Zebra shuts down
        // or crashes.
        let span = Span::current();
        let write_result = tokio::task::spawn_blocking(move || {
            span.in_scope(move || atomic_write(peer_cache_file, peer_data.as_bytes()))
        })
        .await
        .expect("could not write the peer cache file")?;

        match write_result {
            Ok(peer_cache_file) => {
                info!(
                    cached_ip_count = ?peer_list.len(),
                    ?peer_cache_file,
                    "updated cached peer IP addresses"
                );

                for ip in &peer_list {
                    metrics::counter!(
                        "zcash.net.peers.cache",
                        "cache" => peer_cache_file.display().to_string(),
                        "remote_ip" => ip.to_string()
                    )
                    .increment(1);
                }

                Ok(())
            }
            Err(error) => Err(error.error),
        }
    }
}

impl Default for Config {
    fn default() -> Config {
        let mainnet_peers = [
            "dnsseed.z.cash:8233",
            "dnsseed.str4d.xyz:8233",
            "mainnet.seeder.zfnd.org:8233",
        ]
        .iter()
        .map(|&s| String::from(s))
        .collect();

        let testnet_peers = [
            "dnsseed.testnet.z.cash:18233",
            "testnet.seeder.zfnd.org:18233",
        ]
        .iter()
        .map(|&s| String::from(s))
        .collect();

        Config {
            listen_addr: "[::]:8233"
                .parse()
                .expect("Hardcoded address should be parseable"),
            external_addr: None,
            network: Network::Mainnet,
            initial_mainnet_peers: mainnet_peers,
            initial_testnet_peers: testnet_peers,
            cache_dir: CacheDir::default(),
            crawl_new_peer_interval: DEFAULT_CRAWL_NEW_PEER_INTERVAL,

            // # Security
            //
            // The default peerset target size should be large enough to ensure
            // nodes have a reliable set of peers.
            //
            // But Zebra should only make a small number of initial outbound connections,
            // so that idle peers don't use too many connection slots.
            peerset_initial_target_size: DEFAULT_PEERSET_INITIAL_TARGET_SIZE,
            max_connections_per_ip: DEFAULT_MAX_CONNS_PER_IP,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DTestnetParameters {
    network_name: Option<String>,
    network_magic: Option<[u8; 4]>,
    slow_start_interval: Option<u32>,
    target_difficulty_limit: Option<String>,
    disable_pow: Option<bool>,
    genesis_hash: Option<String>,
    activation_heights: Option<ConfiguredActivationHeights>,
    pre_nu6_funding_streams: Option<ConfiguredFundingStreams>,
    post_nu6_funding_streams: Option<ConfiguredFundingStreams>,
    funding_streams: Option<Vec<ConfiguredFundingStreams>>,
    pre_blossom_halving_interval: Option<u32>,
    lockbox_disbursements: Option<Vec<ConfiguredLockboxDisbursement>>,
    #[serde(default)]
    checkpoints: ConfiguredCheckpoints,
    /// If `true`, automatically repeats configured funding stream addresses to fill
    /// all required periods.
    extend_funding_stream_addresses_as_required: Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct DConfig {
    listen_addr: String,
    external_addr: Option<String>,
    network: NetworkKind,
    testnet_parameters: Option<DTestnetParameters>,
    initial_mainnet_peers: IndexSet<String>,
    initial_testnet_peers: IndexSet<String>,
    cache_dir: CacheDir,
    peerset_initial_target_size: usize,
    #[serde(alias = "new_peer_interval", with = "humantime_serde")]
    crawl_new_peer_interval: Duration,
    max_connections_per_ip: Option<usize>,
}

impl Default for DConfig {
    fn default() -> Self {
        let config = Config::default();
        Self {
            listen_addr: "[::]".to_string(),
            external_addr: None,
            network: Default::default(),
            testnet_parameters: None,
            initial_mainnet_peers: config.initial_mainnet_peers,
            initial_testnet_peers: config.initial_testnet_peers,
            cache_dir: config.cache_dir,
            peerset_initial_target_size: config.peerset_initial_target_size,
            crawl_new_peer_interval: config.crawl_new_peer_interval,
            max_connections_per_ip: Some(config.max_connections_per_ip),
        }
    }
}

impl From<Arc<testnet::Parameters>> for DTestnetParameters {
    fn from(params: Arc<testnet::Parameters>) -> Self {
        Self {
            network_name: Some(params.network_name().to_string()),
            network_magic: Some(params.network_magic().0),
            slow_start_interval: Some(params.slow_start_interval().0),
            target_difficulty_limit: Some(params.target_difficulty_limit().to_string()),
            disable_pow: Some(params.disable_pow()),
            genesis_hash: Some(params.genesis_hash().to_string()),
            activation_heights: Some(params.activation_heights().into()),
            pre_nu6_funding_streams: None,
            post_nu6_funding_streams: None,
            funding_streams: Some(params.funding_streams().iter().map(Into::into).collect()),
            pre_blossom_halving_interval: Some(
                params
                    .pre_blossom_halving_interval()
                    .try_into()
                    .expect("should convert"),
            ),
            lockbox_disbursements: Some(
                params
                    .lockbox_disbursements()
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            ),
            checkpoints: if params.checkpoints() == testnet::Parameters::default().checkpoints() {
                ConfiguredCheckpoints::Default(true)
            } else {
                params.checkpoints().into()
            },
            extend_funding_stream_addresses_as_required: None,
        }
    }
}

impl From<Config> for DConfig {
    fn from(
        Config {
            listen_addr,
            external_addr,
            network,
            initial_mainnet_peers,
            initial_testnet_peers,
            cache_dir,
            peerset_initial_target_size,
            crawl_new_peer_interval,
            max_connections_per_ip,
        }: Config,
    ) -> Self {
        let testnet_parameters = network
            .parameters()
            .filter(|params| !params.is_default_testnet())
            .map(Into::into);

        DConfig {
            listen_addr: listen_addr.to_string(),
            external_addr: external_addr.map(|addr| addr.to_string()),
            network: network.into(),
            testnet_parameters,
            initial_mainnet_peers,
            initial_testnet_peers,
            cache_dir,
            peerset_initial_target_size,
            crawl_new_peer_interval,
            max_connections_per_ip: Some(max_connections_per_ip),
        }
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let DConfig {
            listen_addr,
            external_addr,
            network: network_kind,
            testnet_parameters,
            initial_mainnet_peers,
            initial_testnet_peers,
            cache_dir,
            peerset_initial_target_size,
            crawl_new_peer_interval,
            max_connections_per_ip,
        } = DConfig::deserialize(deserializer)?;

        /// Accepts an [`IndexSet`] of initial peers,
        ///
        /// Returns true if any of them are the default Testnet or Mainnet initial peers.
        fn contains_default_initial_peers(initial_peers: &IndexSet<String>) -> bool {
            let Config {
                initial_mainnet_peers: mut default_initial_peers,
                initial_testnet_peers: default_initial_testnet_peers,
                ..
            } = Config::default();
            default_initial_peers.extend(default_initial_testnet_peers);

            initial_peers
                .intersection(&default_initial_peers)
                .next()
                .is_some()
        }

        let network = match (network_kind, testnet_parameters) {
            (NetworkKind::Mainnet, _) => Network::Mainnet,
            (NetworkKind::Testnet, None) => Network::new_default_testnet(),
            (NetworkKind::Regtest, testnet_parameters) => {
                let params = testnet_parameters
                    .map(
                        |DTestnetParameters {
                             activation_heights,
                             pre_nu6_funding_streams,
                             post_nu6_funding_streams,
                             funding_streams,
                             lockbox_disbursements,
                             checkpoints,
                             extend_funding_stream_addresses_as_required,
                             ..
                         }| {
                            let mut funding_streams_vec = funding_streams.unwrap_or_default();
                            if let Some(funding_streams) = post_nu6_funding_streams {
                                funding_streams_vec.insert(0, funding_streams);
                            }
                            if let Some(funding_streams) = pre_nu6_funding_streams {
                                funding_streams_vec.insert(0, funding_streams);
                            }

                            RegtestParameters {
                                activation_heights: activation_heights.unwrap_or_default(),
                                funding_streams: Some(funding_streams_vec),
                                lockbox_disbursements,
                                checkpoints: Some(checkpoints),
                                extend_funding_stream_addresses_as_required,
                            }
                        },
                    )
                    .unwrap_or_default();

                Network::new_regtest(params)
            }
            (
                NetworkKind::Testnet,
                Some(DTestnetParameters {
                    network_name,
                    network_magic,
                    slow_start_interval,
                    target_difficulty_limit,
                    disable_pow,
                    genesis_hash,
                    activation_heights,
                    pre_nu6_funding_streams,
                    post_nu6_funding_streams,
                    funding_streams,
                    pre_blossom_halving_interval,
                    lockbox_disbursements,
                    checkpoints,
                    extend_funding_stream_addresses_as_required,
                }),
            ) => {
                let mut params_builder = testnet::Parameters::build();

                if let Some(network_name) = network_name.clone() {
                    params_builder = params_builder.with_network_name(network_name)
                }

                if let Some(network_magic) = network_magic {
                    params_builder = params_builder.with_network_magic(Magic(network_magic));
                }

                if let Some(genesis_hash) = genesis_hash {
                    params_builder = params_builder.with_genesis_hash(genesis_hash);
                }

                if let Some(slow_start_interval) = slow_start_interval {
                    params_builder = params_builder.with_slow_start_interval(
                        slow_start_interval.try_into().map_err(de::Error::custom)?,
                    );
                }

                if let Some(target_difficulty_limit) = target_difficulty_limit.clone() {
                    params_builder = params_builder.with_target_difficulty_limit(
                        target_difficulty_limit
                            .parse::<U256>()
                            .map_err(de::Error::custom)?,
                    );
                }

                if let Some(disable_pow) = disable_pow {
                    params_builder = params_builder.with_disable_pow(disable_pow);
                }

                // Retain default Testnet activation heights unless there's an empty [testnet_parameters.activation_heights] section.
                if let Some(activation_heights) = activation_heights {
                    params_builder = params_builder.with_activation_heights(activation_heights)
                }

                if let Some(halving_interval) = pre_blossom_halving_interval {
                    params_builder = params_builder.with_halving_interval(halving_interval.into())
                }

                // Set configured funding streams after setting any parameters that affect the funding stream address period.
                let mut funding_streams_vec = funding_streams.unwrap_or_default();

                if let Some(funding_streams) = post_nu6_funding_streams {
                    funding_streams_vec.insert(0, funding_streams);
                }

                if let Some(funding_streams) = pre_nu6_funding_streams {
                    funding_streams_vec.insert(0, funding_streams);
                }

                if !funding_streams_vec.is_empty() {
                    params_builder = params_builder.with_funding_streams(funding_streams_vec);
                }

                if let Some(lockbox_disbursements) = lockbox_disbursements {
                    params_builder =
                        params_builder.with_lockbox_disbursements(lockbox_disbursements);
                }

                params_builder = params_builder.with_checkpoints(checkpoints);

                if let Some(should_extend_funding_stream_addresses) =
                    extend_funding_stream_addresses_as_required
                {
                    if should_extend_funding_stream_addresses {
                        params_builder = params_builder.extend_funding_streams();
                    }
                }

                // Return an error if the initial testnet peers includes any of the default initial Mainnet or Testnet
                // peers and the configured network parameters are incompatible with the default public Testnet.
                if !params_builder.is_compatible_with_default_parameters()
                    && contains_default_initial_peers(&initial_testnet_peers)
                {
                    return Err(de::Error::custom(
                        "cannot use default initials peers with incompatible testnet",
                    ));
                };

                // Return the default Testnet if no network name was configured and all parameters match the default Testnet
                if network_name.is_none() && params_builder == testnet::Parameters::build() {
                    Network::new_default_testnet()
                } else {
                    params_builder.to_network()
                }
            }
        };

        let listen_addr = match listen_addr.parse::<SocketAddr>().or_else(|_| format!("{listen_addr}:{}", network.default_port()).parse()) {
            Ok(socket) => Ok(socket),
            Err(_) => match listen_addr.parse::<IpAddr>() {
                Ok(ip) => Ok(SocketAddr::new(ip, network.default_port())),
                Err(err) => Err(de::Error::custom(format!(
                    "{err}; Hint: addresses can be a IPv4, IPv6 (with brackets), or a DNS name, the port is optional"
                ))),
            },
        }?;

        let external_socket_addr = if let Some(address) = &external_addr {
            match address.parse::<SocketAddr>().or_else(|_| format!("{address}:{}", network.default_port()).parse()) {
                Ok(socket) => Ok(Some(socket)),
                Err(_) => match address.parse::<IpAddr>() {
                    Ok(ip) => Ok(Some(SocketAddr::new(ip, network.default_port()))),
                    Err(err) => Err(de::Error::custom(format!(
                        "{err}; Hint: addresses can be a IPv4, IPv6 (with brackets), or a DNS name, the port is optional"
                    ))),
                },
            }?
        } else {
            None
        };

        let [max_connections_per_ip, peerset_initial_target_size] = [
            ("max_connections_per_ip", max_connections_per_ip, DEFAULT_MAX_CONNS_PER_IP),
            // If we want Zebra to operate with no network,
            // we should implement a `zebrad` command that doesn't use `zebra-network`.
            ("peerset_initial_target_size", Some(peerset_initial_target_size), DEFAULT_PEERSET_INITIAL_TARGET_SIZE)
        ].map(|(field_name, non_zero_config_field, default_config_value)| {
            if non_zero_config_field == Some(0) {
                warn!(
                    ?field_name,
                    ?non_zero_config_field,
                    "{field_name} should be greater than 0, using default value of {default_config_value} instead"
                );
            }

            non_zero_config_field.filter(|config_value| config_value > &0).unwrap_or(default_config_value)
        });

        Ok(Config {
            listen_addr: canonical_socket_addr(listen_addr),
            external_addr: external_socket_addr,
            network,
            initial_mainnet_peers,
            initial_testnet_peers,
            cache_dir,
            peerset_initial_target_size,
            crawl_new_peer_interval,
            max_connections_per_ip,
        })
    }
}
