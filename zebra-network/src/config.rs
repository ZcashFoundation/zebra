//! Configuration for Zebra's network communication.

use std::{
    collections::HashSet,
    ffi::OsString,
    io::{self, ErrorKind},
    net::{IpAddr, SocketAddr},
    string::String,
    time::Duration,
};

use indexmap::IndexSet;
use serde::{de, Deserialize, Deserializer};
use tempfile::NamedTempFile;
use tokio::{fs, io::AsyncWriteExt};

use zebra_chain::parameters::Network;

use crate::{
    constants::{
        DEFAULT_CRAWL_NEW_PEER_INTERVAL, DNS_LOOKUP_TIMEOUT, INBOUND_PEER_LIMIT_MULTIPLIER,
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
    /// ("0.0.0.0" or "\[::\]"), which is not advertised to other nodes.
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
    pub fn initial_peer_hostnames(&self) -> &IndexSet<String> {
        match self.network {
            Network::Mainnet => &self.initial_mainnet_peers,
            Network::Testnet => &self.initial_testnet_peers,
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

        // Ignore disk errors because the cache is optional and the method already logs them.
        let disk_peers = self.load_peer_cache().await.unwrap_or_default();

        dns_peers
            .into_iter()
            .chain(disk_peers.into_iter())
            .collect()
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

                // if we're logging at debug level,
                // the full list of IP addresses will be shown in the log message
                let debug_span = debug_span!("", remote_ip_addrs = ?ip_addrs);
                let _span_guard = debug_span.enter();

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
                        1,
                        "seed" => host.to_string(),
                        "remote_ip" => ip.to_string()
                    );
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
        let Some(peer_cache_file) = self.cache_dir.peer_cache_file_path(self.network) else {
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
                1,
                "cache" => peer_cache_file.display().to_string(),
                "remote_ip" => ip.to_string()
            );
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
        let Some(peer_cache_file) = self.cache_dir.peer_cache_file_path(self.network) else {
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

        // Write to a temporary file, so the cache is not corrupted if Zebra shuts down or crashes
        // at the same time.
        //
        // # Concurrency
        //
        // We want to use async code to avoid blocking the tokio executor on filesystem operations,
        // but `tempfile` is implemented using non-asyc methods. So we wrap its filesystem
        // operations in `tokio::spawn_blocking()`.
        //
        // TODO: split this out into an atomic_write_to_tmp_file() method if we need to re-use it

        // Create the peer cache directory if needed
        let peer_cache_dir = peer_cache_file
            .parent()
            .expect("cache path always has a network directory")
            .to_owned();
        tokio::fs::create_dir_all(&peer_cache_dir).await?;

        // Give the temporary file a similar name to the permanent cache file,
        // but hide it in directory listings.
        let mut tmp_peer_cache_prefix: OsString = ".tmp.".into();
        tmp_peer_cache_prefix.push(
            peer_cache_file
                .file_name()
                .expect("cache file always has a file name"),
        );

        // Create the temporary file.
        // Do blocking filesystem operations on a dedicated thread.
        let tmp_peer_cache_file = tokio::task::spawn_blocking(move || {
            // Put the temporary file in the same directory as the permanent file,
            // so atomic filesystem operations are possible.
            tempfile::Builder::new()
                .prefix(&tmp_peer_cache_prefix)
                .tempfile_in(peer_cache_dir)
        })
        .await
        .expect("unexpected panic creating temporary peer cache file")?;

        // Write the list to the file asynchronously, by extracting the inner file, using it,
        // then combining it back into a type that will correctly drop the file on error.
        let (tmp_peer_cache_file, tmp_peer_cache_path) = tmp_peer_cache_file.into_parts();
        let mut tmp_peer_cache_file = tokio::fs::File::from_std(tmp_peer_cache_file);
        tmp_peer_cache_file.write_all(peer_data.as_bytes()).await?;

        let tmp_peer_cache_file =
            NamedTempFile::from_parts(tmp_peer_cache_file, tmp_peer_cache_path);

        // Atomically replace the current cache with the temporary cache.
        // Do blocking filesystem operations on a dedicated thread.
        tokio::task::spawn_blocking(move || {
            let result = tmp_peer_cache_file.persist(&peer_cache_file);

            // Drops the temp file if needed
            match result {
                Ok(_temp_file) => {
                    info!(
                        cached_ip_count = ?peer_list.len(),
                        ?peer_cache_file,
                        "updated cached peer IP addresses"
                    );

                    for ip in &peer_list {
                        metrics::counter!(
                            "zcash.net.peers.cache",
                            1,
                            "cache" => peer_cache_file.display().to_string(),
                            "remote_ip" => ip.to_string()
                        );
                    }

                    Ok(())
                }
                Err(error) => Err(error.error),
            }
        })
        .await
        .expect("unexpected panic making temporary peer cache file permanent")
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
            cache_dir: CacheDir::default(),
            crawl_new_peer_interval: DEFAULT_CRAWL_NEW_PEER_INTERVAL,

            // # Security
            //
            // The default peerset target size should be large enough to ensure
            // nodes have a reliable set of peers.
            //
            // But Zebra should only make a small number of initial outbound connections,
            // so that idle peers don't use too many connection slots.
            peerset_initial_target_size: 25,
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
            initial_mainnet_peers: IndexSet<String>,
            initial_testnet_peers: IndexSet<String>,
            cache_dir: CacheDir,
            peerset_initial_target_size: usize,
            #[serde(alias = "new_peer_interval", with = "humantime_serde")]
            crawl_new_peer_interval: Duration,
        }

        impl Default for DConfig {
            fn default() -> Self {
                let config = Config::default();
                Self {
                    listen_addr: "0.0.0.0".to_string(),
                    network: config.network,
                    initial_mainnet_peers: config.initial_mainnet_peers,
                    initial_testnet_peers: config.initial_testnet_peers,
                    cache_dir: config.cache_dir,
                    peerset_initial_target_size: config.peerset_initial_target_size,
                    crawl_new_peer_interval: config.crawl_new_peer_interval,
                }
            }
        }

        let config = DConfig::deserialize(deserializer)?;

        let listen_addr = match config.listen_addr.parse::<SocketAddr>() {
            Ok(socket) => Ok(socket),
            Err(_) => match config.listen_addr.parse::<IpAddr>() {
                Ok(ip) => Ok(SocketAddr::new(ip, config.network.default_port())),
                Err(err) => Err(de::Error::custom(format!(
                    "{err}; Hint: addresses can be a IPv4, IPv6 (with brackets), or a DNS name, the port is optional"
                ))),
            },
        }?;

        Ok(Config {
            listen_addr: canonical_socket_addr(listen_addr),
            network: config.network,
            initial_mainnet_peers: config.initial_mainnet_peers,
            initial_testnet_peers: config.initial_testnet_peers,
            cache_dir: config.cache_dir,
            peerset_initial_target_size: config.peerset_initial_target_size,
            crawl_new_peer_interval: config.crawl_new_peer_interval,
        })
    }
}
