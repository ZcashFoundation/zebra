//! Starting the experimental version 2 protocol: a QUIC endpoint that
//! accepts inbound connections, connections to the configured initial
//! version 2 peers, and, with discovery on, to the version 2 peers that the
//! version 2 address segment learns of.
//!
//! Everything here is off unless `v2_listen_addr` is configured. Each
//! successful handshake becomes a [`peer::Client`] on the same channel the
//! legacy listener and connector feed, so the peer set treats version 2
//! peers like any other.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::pin,
    sync::{Arc, OnceLock},
    time::Duration,
};

use futures::{sink::SinkExt, FutureExt};
use indexmap::{IndexMap, IndexSet};
use rand::RngExt;
use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use tower::Service;
use tracing::instrument;
use tracing_futures::Instrument;

use zebra_chain::{chain_tip::ChainTip, diagnostic::task::CheckForPanics, parameters::Network};

/// Dialling version 2 peers for a state-snapshot bootstrap, before the peer set exists.
pub(super) mod bootstrap;

pub use bootstrap::{v2_bootstrap_clients, BootstrapPeers};

use crate::{
    connection_metrics::{
        record_connection_attempt_finished, record_connection_attempt_started,
        record_inbound_connection_rejected, ConnectionDirection,
    },
    constants,
    meta_addr::{MetaAddr, MetaAddrChange},
    peer::{
        self,
        v2::{
            addr_relay::AddrRelay,
            connection::{
                global_block_range_permits, global_hashes_permits, global_object_permits,
                spawn_connection, ConnectionParams, ObjectSizes,
            },
            handshake::{handshake, LocalIdentity, Role},
            misbehavior::MisbehaviorScores,
            nonces::NonceRegistry,
        },
        ConnectedAddr, ConnectionInfo, MinimumPeerVersion,
    },
    peer_set::{
        initialize::{recent_by_ip, DiscoveredPeer},
        ActiveConnectionCounter, ConnectedPeers, ConnectionTracker, InventoryChange,
    },
    protocol::{
        external::{canonical_peer_addr, types::PeerServices, MAX_USER_AGENT_LENGTH},
        v2::types::ErrorCode,
    },
    quic::{self, ConnectError},
    sync_peer::{ConnectionId, SyncPeer, SyncPeersSender},
    v2_address_book::{self, Group, Provenance, V2BookHandle, V2BookStats},
    BanList, BoxError, Config, MisbehaviorReport, PeerSocketAddr, Request, Response,
};

/// The least time between DNS seed lookups for version 2 peers, which are only made while the
/// version 2 address segment has too few addresses to dial.
pub(super) const V2_DNS_RESEED_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// The remote addresses of the open inbound version 2 connections, each with its number of
/// open connections.
pub(super) type OpenInbound = HashMap<PeerSocketAddr, usize>;

/// What every version 2 connection attempt shares.
#[derive(Clone)]
pub(super) struct Shared<S, C> {
    network: Network,
    inbound_limit: usize,
    max_connections_per_ip: usize,
    /// The base delay before redialling a configured peer, before jitter.
    reconnection_delay: Duration,
    /// The delay between accepted inbound connections.
    inbound_interval: Duration,
    /// The open inbound connections, which the listener maintains, so the dialer does not dial
    /// a configured peer that is already connected to this node.
    open_inbound: watch::Receiver<OpenInbound>,
    endpoint: quinn::Endpoint,
    identity: LocalIdentity,
    nonces: NonceRegistry,
    minimum_peer_version: MinimumPeerVersion<C>,
    connection_params: ConnectionParams<S>,
    /// The version 2 address segment, which records this node's outbound connections.
    address_book: V2BookHandle,
    /// The peer set's connected peers, over either protocol, so the dialers do not dial a host
    /// whose connection the peer set would drop.
    connected_peers: ConnectedPeers,
    /// Where completed connections are registered for the checkpoint sync engine, if anywhere.
    sync_peers: Option<SyncPeersSender>,
}

impl<S, C> Shared<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    /// Returns what the connections of `endpoint` share, and the sender the listener keeps the
    /// open inbound connections up to date with.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        config: &Config,
        endpoint: quinn::Endpoint,
        identity: LocalIdentity,
        inbound_service: S,
        inv_collector: broadcast::Sender<InventoryChange>,
        latest_chain_tip: C,
        misbehavior_tx: mpsc::Sender<MisbehaviorReport>,
        bans_receiver: watch::Receiver<BanList>,
        reconnection_delay: Duration,
        inbound_interval: Duration,
        address_book: V2BookHandle,
        connected_peers: ConnectedPeers,
    ) -> (Self, watch::Sender<OpenInbound>) {
        let (open_inbound_tx, open_inbound) = watch::channel(OpenInbound::new());
        let shared = Shared {
            network: config.network.clone(),
            inbound_limit: config.v2_inbound_connection_limit(),
            max_connections_per_ip: config.max_connections_per_ip,
            reconnection_delay,
            inbound_interval,
            open_inbound,
            nonces: NonceRegistry::spawn(config.peerset_total_connection_limit()),
            minimum_peer_version: MinimumPeerVersion::new(latest_chain_tip, &config.network),
            connection_params: ConnectionParams {
                inbound_service,
                inv_collector,
                genesis_hash: config.network.genesis_hash(),
                misbehavior: MisbehaviorScores::spawn(
                    misbehavior_tx,
                    config.peerset_total_connection_limit(),
                ),
                address_book: Some(address_book.clone()),
                discovery: config.v2_peer_discovery,
                bans: bans_receiver,
                addr_relay: Some(AddrRelay::spawn(
                    config.v2_advertised_addr(),
                    config.network.clone(),
                )),
                serve_block_ranges: config.v2_serve_block_ranges,
                block_range_permits: global_block_range_permits(),
                hashes_permits: global_hashes_permits(),
                object_source: config.object_source.clone(),
                local_services: identity.services,
                object_permits: global_object_permits(),
            },
            endpoint,
            identity,
            address_book,
            connected_peers,
            sync_peers: config.v2_sync_peers.clone(),
        };
        (shared, open_inbound_tx)
    }
}

/// The version 2 endpoint this process opened, kept so that shutdown can close it.
///
/// A node opens at most one. Tests that open several keep only the first, and never close it
/// through [`close_v2_endpoint`].
pub(super) static OPEN_ENDPOINT: OnceLock<quinn::Endpoint> = OnceLock::new();

/// Closes this node's version 2 (QUIC) endpoint, if it opened one, and waits up to `grace` for
/// its peers to acknowledge.
///
/// Every version 2 peer then learns at once that this node is going away, with `NO_ERROR`,
/// instead of keeping a dead connection until its idle timeout. Without this, a peer would also
/// drop this node's first reconnection after a restart as a duplicate of the dead connection.
///
/// Call it during shutdown while the Tokio runtime is still running, because the endpoint's
/// driver task sends the close. It does nothing if no version 2 endpoint was opened.
pub async fn close_v2_endpoint(grace: Duration) {
    let Some(endpoint) = OPEN_ENDPOINT.get() else {
        return;
    };
    endpoint.close(quic::app_error_code(ErrorCode::NoError), b"shutting down");
    if tokio::time::timeout(grace, endpoint.wait_idle())
        .await
        .is_err()
    {
        tracing::debug!(
            ?grace,
            "version 2 peers did not acknowledge the close in time"
        );
    }
}

/// Opens the version 2 endpoint if it is configured, and spawns its listener
/// and initial peer dialer as one task.
///
/// # Panics
///
/// If the configured address cannot be bound, or the user agent is over 256
/// bytes: like the legacy listener, an explicitly configured endpoint that
/// silently failed would leave the operator without the peers they
/// configured.
#[allow(clippy::too_many_arguments)]
pub(super) fn start<S, C>(
    config: Config,
    local_services: PeerServices,
    inbound_service: S,
    inv_collector: broadcast::Sender<InventoryChange>,
    latest_chain_tip: C,
    user_agent: String,
    bans_receiver: watch::Receiver<BanList>,
    misbehavior_tx: mpsc::Sender<MisbehaviorReport>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    connected_peers: ConnectedPeers,
) -> Option<JoinHandle<Result<(), BoxError>>>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    start_with_intervals(
        config,
        local_services,
        inbound_service,
        inv_collector,
        latest_chain_tip,
        user_agent,
        bans_receiver,
        misbehavior_tx,
        peerset_tx,
        connected_peers,
        constants::MIN_PEER_RECONNECTION_DELAY,
        constants::MIN_INBOUND_PEER_CONNECTION_INTERVAL,
    )
}

/// [`start`], redialling configured peers after `reconnection_delay` plus jitter, and waiting
/// `inbound_interval` between accepted inbound connections: tests use short intervals.
#[allow(clippy::too_many_arguments)]
pub(super) fn start_with_intervals<S, C>(
    config: Config,
    local_services: PeerServices,
    inbound_service: S,
    inv_collector: broadcast::Sender<InventoryChange>,
    latest_chain_tip: C,
    user_agent: String,
    bans_receiver: watch::Receiver<BanList>,
    misbehavior_tx: mpsc::Sender<MisbehaviorReport>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    connected_peers: ConnectedPeers,
    reconnection_delay: Duration,
    inbound_interval: Duration,
) -> Option<JoinHandle<Result<(), BoxError>>>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    let listen_addr = config.v2_listen_addr?;

    // The one sender-side check of the draft's 256-byte `user_agent` bound ("Init Record"): the
    // record encoder relies on it.
    assert!(
        user_agent.len() <= MAX_USER_AGENT_LENGTH,
        "The version 2 user agent {user_agent:?} is {} bytes, over the protocol's limit of \
         {MAX_USER_AGENT_LENGTH} bytes.",
        user_agent.len(),
    );
    let identity = LocalIdentity {
        user_agent,
        services: config.v2_advertised_services(local_services),
        relay: true,
    };

    // A node that serves snapshot objects gets the larger send window.
    let windows = quic::Windows::for_services(identity.services);
    let endpoint = match quic::bind_with_windows(&config.network, listen_addr, windows) {
        Ok(endpoint) => endpoint,
        Err(error) => panic!(
            "Opening the version 2 (QUIC) Zcash protocol endpoint {listen_addr:?} failed: \
             {error:?}. Hint: check that the UDP port is free, that the network has a version 2 \
             ALPN identifier (custom testnets do not), and the network v2_listen_addr config.",
        ),
    };
    tracing::info!(
        local_addr = ?endpoint.local_addr().ok(),
        "opened experimental version 2 (QUIC) Zcash protocol endpoint"
    );
    // Only the first endpoint a process opens is kept for shutdown; a node opens one.
    let _ = OPEN_ENDPOINT.set(endpoint.clone());

    // Version 2 addresses are QUIC endpoints, so they have their own address book, which the
    // connections and the dialer share. Its actor stops once they have all stopped.
    let local_listener = config.v2_advertised_addr().unwrap_or(listen_addr);
    let (address_book, address_book_stats, _address_book_task) = v2_address_book::spawn(
        &config,
        &config.network,
        local_listener,
        endpoint_is_ipv4(&endpoint),
        bans_receiver.clone(),
    );

    // The peer set's own limit includes both of these, so it has room for every v2 connection.
    // Configured peers have the first claim on the outbound slots: discovered peers get at most
    // the slots the configured hosts cannot all use, and no more than the discovery target.
    let outbound_limit = config.v2_outbound_connection_limit();
    let discovery = config.v2_peer_discovery;
    let discovered_limit = if discovery {
        config
            .v2_outbound_target()
            .min(outbound_limit.saturating_sub(config.initial_v2_peers.len()))
    } else {
        0
    };
    let (shared, open_inbound_tx) = Shared::new(
        &config,
        endpoint,
        identity,
        inbound_service,
        inv_collector,
        latest_chain_tip,
        misbehavior_tx,
        bans_receiver.clone(),
        reconnection_delay,
        inbound_interval,
        address_book.clone(),
        connected_peers,
    );

    // One task for all of them. The listener runs for the life of the peer set, so its exit is
    // fatal, like the legacy listener's; the peer set aborts the task on shutdown. The configured
    // peer dialer returns once there is nothing to keep connected (no configured peers), and the
    // others run on. With discovery off, the discovered peer dialer and the cache updater return
    // at once.
    let (configured_tx, configured_rx) = watch::channel(HashSet::new());
    let discovered = Discovery {
        book: address_book.clone(),
        stats: address_book_stats,
        limit: discovered_limit,
        target: config.v2_outbound_target(),
        configured: configured_rx,
        await_configured: !config.initial_v2_peers.is_empty(),
        open_inbound: shared.open_inbound.clone(),
        dial_interval: constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL,
        retry_interval: constants::MIN_PEER_RECONNECTION_DELAY,
    };
    let initial_v2_peers = config.initial_v2_peers.clone();
    Some(tokio::spawn(
        async move {
            let mut listener = pin!(accept_connections(
                shared.clone(),
                peerset_tx.clone(),
                bans_receiver.clone(),
                open_inbound_tx,
            ));
            let mut dialer = pin!(keep_initial_peers_connected(
                shared.clone(),
                initial_v2_peers,
                outbound_limit - discovered_limit,
                bans_receiver.clone(),
                peerset_tx.clone(),
                configured_tx,
            )
            .fuse());
            let mut discovered = pin!(async {
                if discovery {
                    v2_address_book::load_v2_peer_cache(&config, &address_book).await;
                    keep_discovered_peers_connected_with(
                        shared,
                        discovered,
                        bans_receiver,
                        peerset_tx,
                        &config,
                    )
                    .await;
                }
            }
            .fuse());
            let mut cache_updater = pin!(async {
                if discovery {
                    v2_address_book::v2_peer_cache_updater(config.clone(), address_book.clone())
                        .await;
                }
            }
            .fuse());
            loop {
                tokio::select! {
                    result = &mut listener => return result,
                    // A finished `Fuse` stays pending, so from here on the others are awaited.
                    () = &mut dialer => {}
                    () = &mut discovered => {}
                    () = &mut cache_updater => {}
                }
            }
        }
        .in_current_span(),
    ))
}

/// Accepts inbound version 2 connections, handshakes with them, and sends
/// the resulting clients to the peer set.
///
/// Applies the same ban list and per-IP rate limit as the legacy listener,
/// and this transport's share of the inbound connection limit.
///
/// Keeps `open_inbound` up to date with the open inbound connections, so the
/// dialer can skip configured peers that are already connected to this node.
#[instrument(skip_all, fields(listener_addr = ?shared.endpoint.local_addr().ok()))]
async fn accept_connections<S, C>(
    shared: Shared<S, C>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    bans_receiver: watch::Receiver<BanList>,
    open_inbound: watch::Sender<OpenInbound>,
) -> Result<(), BoxError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    let mut recent_inbound_connections =
        recent_by_ip::RecentByIp::new(None, Some(shared.max_connections_per_ip));
    let mut active_inbound_connections =
        ActiveConnectionCounter::new_counter_with(shared.inbound_limit, "Inbound v2 Connections");
    let mut handshakes: JoinSet<()> = JoinSet::new();

    loop {
        let incoming = tokio::select! {
            biased;
            finished = handshakes.join_next(), if !handshakes.is_empty() => {
                let _ = finished
                    .expect("the handshake set is not empty")
                    .panic_if_task_has_panicked();
                continue;
            }
            incoming = shared.endpoint.accept() => incoming,
        };
        // `accept` only returns `None` when the endpoint is closed.
        let Some(incoming) = incoming else {
            tracing::info!("version 2 endpoint closed, stopping its listener");
            return Ok(());
        };

        // # Security
        //
        // Validate the peer's address with a Retry before committing any state to the
        // connection (draft, "QUIC Transport"): it costs the peer a round trip and defeats
        // address spoofing and amplification. A flood of such packets must not starve other
        // tasks, so every path through this loop yields.
        if !incoming.remote_address_validated() {
            incoming
                .retry()
                .expect("an unvalidated address can be retried");
            tokio::task::yield_now().await;
            continue;
        }

        let addr: PeerSocketAddr = canonical_peer_addr(incoming.remote_address());
        record_connection_attempt_started(&shared.network, ConnectionDirection::Inbound, addr);

        if bans_receiver.borrow().is_banned(addr.ip()) {
            tracing::debug!(?addr, "banned inbound v2 connection attempt");
            record_inbound_connection_rejected(&shared.network, addr, "banned");
            incoming.refuse();
            tokio::task::yield_now().await;
            continue;
        }

        if active_inbound_connections.update_count() >= shared.inbound_limit
            || recent_inbound_connections.is_past_limit_or_add(addr.ip())
        {
            record_inbound_connection_rejected(&shared.network, addr, "capacity_or_rate_limited");
            incoming.refuse();
            tokio::time::sleep(constants::MIN_INBOUND_PEER_FAILED_CONNECTION_INTERVAL).await;
            continue;
        }

        let connection_tracker = active_inbound_connections.track_connection();
        // The transport handshake has already started; the application deadline runs from here,
        // which is as close to the connection's establishment as this loop can get.
        let deadline = Instant::now() + constants::HANDSHAKE_TIMEOUT;
        let inbound_interval = shared.inbound_interval;
        let shared = shared.clone();
        let peerset_tx = peerset_tx.clone();
        let open_inbound = open_inbound.clone();

        // The task lives as long as the connection, so it can track it in `open_inbound`. The
        // connection tracker bounds these tasks by the inbound limit.
        handshakes.spawn(
            async move {
                let network = shared.network.clone();
                let (exit_tx, exit_rx) = oneshot::channel();
                let result = async {
                    // The same error type as outbound connects, so metrics classify both alike.
                    let connecting = incoming.accept().map_err(ConnectError::Handshake)?;
                    let connection = tokio::time::timeout_at(deadline, connecting)
                        .await
                        .map_err(|_elapsed| ConnectError::Timeout)?
                        .map_err(ConnectError::Handshake)?;
                    finish_connection(
                        shared,
                        connection,
                        ConnectedAddr::new_inbound_direct(addr),
                        Role::Responder,
                        connection_tracker,
                        deadline,
                        peerset_tx,
                        Some(exit_tx),
                    )
                    .await
                }
                .await;
                if let Err(error) = &result {
                    tracing::debug!(?addr, %error, "inbound v2 connection failed");
                }
                record_connection_attempt_finished(
                    &network,
                    ConnectionDirection::Inbound,
                    addr,
                    result.as_ref().err(),
                );

                if result.is_ok() {
                    open_inbound.send_modify(|open| *open.entry(addr).or_default() += 1);
                    // Wait for the connection task to end, however it ends: the peer set
                    // dropping a duplicate connection ends it too.
                    let _ = exit_rx.await;
                    open_inbound.send_modify(|open| {
                        if let Some(count) = open.get_mut(&addr) {
                            *count -= 1;
                            if *count == 0 {
                                open.remove(&addr);
                            }
                        }
                    });
                }
            }
            .in_current_span(),
        );

        // Rate-limit inbound connections, like the legacy listener.
        tokio::time::sleep(inbound_interval).await;
    }
}

/// The addresses of the configured initial version 2 peers, by host, and which of them hold
/// the dialer's outbound slots.
///
/// Each free slot goes to a host with the fewest addresses in slots, and hosts with as many take
/// turns, so a host that resolves to many addresses cannot keep the other configured hosts from
/// being dialled. A host's addresses also take turns, so the slot an address frees after its
/// connection failed or ended can go to another address of the same host.
///
/// An address that is offered a slot but not dialled, because it is banned or already connected
/// to this node, gives the slot back at once, so another address can use it, and is not offered
/// a slot again until its deferral passed, so it is not checked every time a slot is free.
///
/// A slot is kept free for each configured host that has not resolved yet, as long as only a
/// host that already holds a slot could take it, so a host that resolves late still gets a slot
/// without ending a live connection. Connections that stay up keep their slots, so when there
/// are more hosts than slots, the hosts that resolve first get them, and the others wait.
#[derive(Debug)]
pub(super) struct InitialPeerSlots {
    /// The number of slots.
    limit: usize,
    /// The addresses of each host that resolved, in resolution order, each with the index of the
    /// address the host offers next.
    hosts: Vec<(IndexSet<PeerSocketAddr>, usize)>,
    /// The addresses that hold a slot, each with the index of the host it holds the slot for.
    in_slots: IndexMap<PeerSocketAddr, usize>,
    /// The addresses that are not offered a slot before the given time, because they were not
    /// dialled when they were last offered one.
    deferred: HashMap<PeerSocketAddr, Instant>,
    /// The host whose turn it is among the hosts with the fewest addresses in slots, modulo the
    /// number of hosts.
    next_host: usize,
    /// The number of configured hosts that have not resolved yet, each of which keeps a slot
    /// free from the hosts that already hold one.
    unresolved_hosts: usize,
}

impl InitialPeerSlots {
    /// Returns `limit` empty slots, for no hosts.
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            hosts: Vec::new(),
            in_slots: IndexMap::new(),
            deferred: HashMap::new(),
            next_host: 0,
            unresolved_hosts: 0,
        }
    }

    /// Keeps a slot free for each of `count` configured hosts that have not resolved yet.
    pub(super) fn set_unresolved_hosts(&mut self, count: usize) {
        self.unresolved_hosts = count;
    }

    /// Returns `true` if `addr` waits for a free slot: it neither holds one nor is deferred.
    pub(super) fn waits_for_slot(&self, addr: &PeerSocketAddr) -> bool {
        !self.in_slots.contains_key(addr) && !self.deferred.contains_key(addr)
    }

    /// Adds a resolved host's `addrs`, after the hosts that already resolved.
    pub(super) fn add_host(&mut self, addrs: impl IntoIterator<Item = PeerSocketAddr>) {
        let addrs: IndexSet<PeerSocketAddr> = addrs.into_iter().collect();
        if !addrs.is_empty() {
            self.hosts.push((addrs, 0));
        }
    }

    /// Frees the slot that `addr` holds, if any, so any address can take it.
    pub(super) fn free(&mut self, addr: &PeerSocketAddr) {
        self.in_slots.shift_remove(addr);
    }

    /// Frees the slot that `addr` holds, if any, and does not offer `addr` a slot again before
    /// `until`: the address was not dialled, so another address can use the slot meanwhile.
    pub(super) fn defer(&mut self, addr: PeerSocketAddr, until: Instant) {
        self.in_slots.shift_remove(&addr);
        self.deferred.insert(addr, until);
    }

    /// Returns the earliest time a deferred address is offered a slot again, if any address is
    /// deferred.
    pub(super) fn next_deferral_end(&self) -> Option<Instant> {
        self.deferred.values().min().copied()
    }

    /// Gives a free slot to the next address to dial and returns it, or returns `None` if every
    /// slot is taken, every address already holds one or is deferred at `now`, or the free slots
    /// are kept for the hosts that have not resolved yet.
    pub(super) fn take_next(&mut self, now: Instant) -> Option<PeerSocketAddr> {
        // Always, so that a passed deferral does not keep its address out of the next slot that
        // frees, and `next_deferral_end` does not return a time that has passed.
        self.deferred.retain(|_, until| *until > now);

        if self.in_slots.len() >= self.limit || self.hosts.is_empty() {
            return None;
        }

        let mut host_slots = vec![0_usize; self.hosts.len()];
        for &host in self.in_slots.values() {
            host_slots[host] += 1;
        }

        // The first host in turn order with the fewest addresses in slots, among the hosts that
        // have an address without one that is not deferred, and that host's next such address.
        let mut chosen: Option<(usize, usize)> = None;
        for offset in 0..self.hosts.len() {
            let host = (self.next_host + offset) % self.hosts.len();
            if chosen.is_some_and(|(chosen, _)| host_slots[chosen] <= host_slots[host]) {
                continue;
            }
            let (addrs, next_addr) = &self.hosts[host];
            if let Some(index) = (0..addrs.len())
                .map(|offset| (next_addr + offset) % addrs.len())
                .find(|&index| {
                    let addr = &addrs[index];
                    !self.in_slots.contains_key(addr) && !self.deferred.contains_key(addr)
                })
            {
                chosen = Some((host, index));
            }
        }
        let (host, index) = chosen?;

        // The chosen host has the fewest slots, so if it holds one, every host that could take
        // this slot does.
        let free_slots = self.limit - self.in_slots.len();
        if host_slots[host] > 0 && free_slots <= self.unresolved_hosts {
            return None;
        }

        let (addrs, next_addr) = &mut self.hosts[host];
        let addr = addrs[index];
        *next_addr = (index + 1) % addrs.len();
        // Taken modulo the number of hosts when it is used, so a host added after this one, which
        // has had no turn yet, is next.
        self.next_host = host + 1;
        self.in_slots.insert(addr, host);

        Some(addr)
    }
}

/// Resolves the configured initial version 2 peers and keeps connections to
/// them for the life of the peer set, redialling after a disconnection. A
/// host that does not resolve is retried after the reconnection delay, for
/// as long as the peer set runs.
///
/// The outbound slots are shared between the configured hosts as
/// [`InitialPeerSlots`] describes, and an address frees its slot once its
/// connection failed or ended and the reconnection delay passed. An address
/// that is not dialled, because it is banned, already connected to this
/// node over either protocol, or its IP is at the per-IP limit in
/// the peer set, takes no slot, and is offered one again after the
/// reconnection delay.
///
/// Resolution runs here rather than in `start`, so a slow or failing DNS
/// lookup does not delay peer set initialization. Returns once there are no
/// peers to keep connected and no hosts left to resolve.
///
/// A configured peer whose IP is in `bans_receiver` is not dialled until its
/// ban lapses.
#[instrument(skip_all)]
pub(super) async fn keep_initial_peers_connected<S, C>(
    shared: Shared<S, C>,
    hosts: IndexSet<String>,
    outbound_limit: usize,
    bans_receiver: watch::Receiver<BanList>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    configured: watch::Sender<HashSet<PeerSocketAddr>>,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    // `Config::v2_outbound_connection_limit` is at least one when the endpoint is configured, so
    // this only guards a change there: with no slot, every configured peer would wait for a
    // connection to fail or end that never comes, and the dialer would exit without a word.
    if outbound_limit == 0 {
        if !hosts.is_empty() {
            tracing::warn!(
                ?hosts,
                "the outbound v2 connection limit is zero, so the initial v2 peers are never \
                 dialled"
            );
        }
        return;
    }

    // An address holds its slot from its dial until its attempt returns, so admission is on
    // `slots`. Each connection attempt gets a fresh tracker from this counter, which is drained
    // on every attempt, for the progress bar and logs only.
    let mut active_outbound_connections =
        ActiveConnectionCounter::new_counter_with(outbound_limit, "Outbound v2 Connections");
    let mut attempts: JoinSet<PeerSocketAddr> = JoinSet::new();
    let mut slots = InitialPeerSlots::new(outbound_limit);
    slots.set_unresolved_hosts(hosts.len());
    let mut unresolved = hosts;
    // The hosts that resolved in this round, with their addresses, to log any that wait for a slot.
    let mut newly_resolved: Vec<(String, IndexSet<PeerSocketAddr>)> = Vec::new();
    let endpoint_is_ipv4 = endpoint_is_ipv4(&shared.endpoint);
    // Resolve straight away, then retry whatever did not resolve after the reconnection delay.
    let mut resolve_at = pin!(tokio::time::sleep(Duration::ZERO));
    // Fires when the earliest deferral ends, so the address is offered a slot again then. It is
    // reset after every round of dials, and only polled while an address is deferred.
    let mut deferral_ends = pin!(tokio::time::sleep(Duration::ZERO));

    loop {
        tokio::select! {
            () = &mut resolve_at, if !unresolved.is_empty() => {
                let (resolved, still_unresolved) =
                    Config::resolve_initial_v2_peers(&unresolved).await;
                let mut round: Vec<PeerSocketAddr> = Vec::new();
                unresolved = still_unresolved;
                slots.set_unresolved_hosts(unresolved.len());
                for (host, addrs) in resolved {
                    // Each host resolves successfully once, so this warns once per address.
                    let addrs: IndexSet<PeerSocketAddr> = addrs
                        .into_iter()
                        .filter(|addr| {
                            let unreachable = endpoint_is_ipv4 && addr.is_ipv6();
                            if unreachable {
                                tracing::warn!(
                                    ?addr,
                                    "the version 2 endpoint is bound to an IPv4 address, so it \
                                     cannot dial this IPv6 initial v2 peer, skipping it. Hint: \
                                     set the network v2_listen_addr config to an IPv6 address \
                                     such as [::]:8233 to reach IPv6 peers"
                                );
                            }
                            !unreachable
                        })
                        .collect();
                    // Each host's addresses are added together, so they share its turns.
                    slots.add_host(addrs.iter().copied());
                    // Recorded in the version 2 address segment, so the configured peers that
                    // answer are served and cached.
                    shared
                        .address_book
                        .seed(addrs.iter().copied().collect(), Provenance::Configured)
                        .await;
                    round.extend(addrs.iter().copied());
                    newly_resolved.push((host, addrs));
                }
                // Kept out of discovered dialling. Sent once per round, even when nothing
                // resolved, because the discovered peer dialer waits for the end of the first
                // round: until then, a cached configured peer could be dialled as discovered.
                configured.send_modify(|configured| configured.extend(round));
                resolve_at
                    .as_mut()
                    .reset(Instant::now() + shared.reconnection_delay);
            }
            // An attempt returns after its connection ended, or failed, and the reconnection
            // delay has passed, so its slot is free for the next address, which can be the same.
            Some(finished) = attempts.join_next(), if !attempts.is_empty() => {
                // Drain the finished attempt's notifications.
                active_outbound_connections.update_count();
                // A cancelled attempt has no address to free, and the dialer cancels none.
                if let Ok(addr) = finished.panic_if_task_has_panicked() {
                    slots.free(&addr);
                }
            }
            // A deferred address is offered a slot again once its delay has passed.
            () = &mut deferral_ends, if slots.next_deferral_end().is_some() => {}
            else => return,
        }

        while let Some(addr) = slots.take_next(Instant::now()) {
            // Checked here, before an attempt is spawned, so an address that is not dialled
            // holds no slot, and the addresses that wait for one are logged right below.
            let not_dialled = not_dialled(&shared, &bans_receiver, addr);
            if let Some(reason) = not_dialled {
                tracing::debug!(
                    ?addr,
                    reason,
                    "initial v2 peer is not dialled, deferring it"
                );
                record_dial("configured", "skipped");
                // At least the dial interval, so a deferral cannot end within the round of dials
                // that made it and offer the address a slot again in it: no caller passes such a
                // short delay, so this only guards against one.
                let deferral = with_jitter(shared.reconnection_delay)
                    .max(constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL);
                slots.defer(addr, Instant::now() + deferral);
                continue;
            }

            attempts.spawn(
                connect_once(
                    shared.clone(),
                    addr,
                    active_outbound_connections.track_connection(),
                    peerset_tx.clone(),
                )
                .in_current_span(),
            );
            active_outbound_connections.update_count();
            // Space out connection attempts, like the legacy initial peer dialer.
            tokio::time::sleep(constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL).await;
        }
        if let Some(ends_at) = slots.next_deferral_end() {
            deferral_ends.as_mut().reset(ends_at);
        }

        // Each host resolves successfully once, so this logs once per host.
        for (host, addrs) in newly_resolved.drain(..) {
            let waiting: Vec<_> = addrs
                .iter()
                .filter(|addr| slots.waits_for_slot(addr))
                .collect();
            if !waiting.is_empty() {
                tracing::info!(
                    %host,
                    ?waiting,
                    limit = outbound_limit,
                    "the outbound v2 connection slots are taken, so these initial v2 peer \
                     addresses wait for a connection to fail or end before they are dialled"
                );
            }
        }
    }
}

/// Returns why `addr` is not dialled now, or `None` if it can be.
fn not_dialled<S, C>(
    shared: &Shared<S, C>,
    bans_receiver: &watch::Receiver<BanList>,
    addr: PeerSocketAddr,
) -> Option<&'static str> {
    // `is_banned` checks the ban's age when it is called, so a lapsed ban is seen when
    // the address is offered a slot again, without waiting for the ban list to change.
    // Version 2 connections leave from the listening port, so an outbound connection to
    // a peer that is connected to this node would have the same peer set key and per-IP
    // slot as the inbound one, and the peer set would drop one of them, at each end
    // independently.
    if bans_receiver.borrow().is_banned(addr.ip()) {
        Some("banned")
    } else if shared.open_inbound.borrow().contains_key(&addr) {
        Some("already connected to us")
    } else {
        // The peer set would drop this connection, because the address has a legacy
        // connection, or its IP is at the per-IP limit. An existing legacy connection
        // is kept, rather than replaced by a version 2 one.
        shared.connected_peers.blocks_dial(addr)
    }
}

/// Connects to one configured peer and waits until the connection ends, or
/// the attempt fails, then waits the reconnection delay plus jitter and
/// returns the address, so its slot can go to the next address to dial.
///
/// The dialer checks that the peer is not banned, and not already connected
/// to this node, before it spawns an attempt.
async fn connect_once<S, C>(
    shared: Shared<S, C>,
    addr: PeerSocketAddr,
    tracker: ConnectionTracker,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
) -> PeerSocketAddr
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    let network = shared.network.clone();
    let reconnection_delay = shared.reconnection_delay;

    let address_book = shared.address_book.clone();

    let (exit_tx, exit_rx) = oneshot::channel();
    address_book.change(MetaAddr::new_reconnect(addr)).await;
    record_connection_attempt_started(&network, ConnectionDirection::Outbound, addr);
    let result: Result<Arc<ConnectionInfo>, BoxError> = async move {
        // The QUIC handshake has its own timeout inside `connect`; the application handshake's
        // deadline runs from the connection's establishment, as the draft asks.
        let connection = quic::connect(&shared.endpoint, *addr).await?;
        let deadline = Instant::now() + constants::HANDSHAKE_TIMEOUT;
        finish_connection(
            shared,
            connection,
            ConnectedAddr::new_outbound_direct(addr),
            Role::Initiator,
            tracker,
            deadline,
            peerset_tx,
            Some(exit_tx),
        )
        .await
    }
    .await;
    record_connection_attempt_finished(
        &network,
        ConnectionDirection::Outbound,
        addr,
        result.as_ref().err(),
    );

    match result {
        Ok(connection_info) => {
            tracing::info!(?addr, "connected to initial v2 peer");
            record_dial("configured", "ok");
            address_book
                .change(connected_change(addr, &connection_info))
                .await;
            wait_for_disconnect(&address_book, addr, exit_rx).await;
            address_book.change(MetaAddr::new_shutdown(addr)).await;
            tracing::info!(?addr, "initial v2 peer disconnected, will reconnect");
        }
        Err(error) => {
            tracing::info!(?addr, %error, "initial v2 peer connection failed, will retry");
            record_dial("configured", "failed");
            address_book.change(MetaAddr::new_errored(addr, None)).await;
        }
    }

    tokio::time::sleep(with_jitter(reconnection_delay)).await;
    addr
}

/// Waits for the outbound connection to `addr` to end, however it ends, which `exit` reports, and
/// tells `book` that the peer responded every [`constants::HEARTBEAT_INTERVAL`] until then.
///
/// The segment's record of a peer ages from its last response, like the legacy address book's,
/// which its heartbeats refresh. Without this, a peer that stays connected would leave the
/// `get-addr` answers and the peer cache [`constants::MAX_PEER_ACTIVE_FOR_GOSSIP`] after its
/// handshake, and once it disconnected, it would not be dialled again if it had been connected
/// for more than [`constants::MAX_RECENT_PEER_AGE`].
pub(super) async fn wait_for_disconnect(
    book: &V2BookHandle,
    addr: PeerSocketAddr,
    mut exit: oneshot::Receiver<()>,
) {
    let mut responded = tokio::time::interval_at(
        Instant::now() + constants::HEARTBEAT_INTERVAL,
        constants::HEARTBEAT_INTERVAL,
    );
    responded.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut exit => return,
            _ = responded.tick() => book.change(MetaAddr::new_responded(addr, None)).await,
        }
    }
}

/// Returns `delay` plus a random jitter of up to a quarter of it, so two nodes that dropped each
/// other's connections do not redial in lockstep.
pub(super) fn with_jitter(delay: Duration) -> Duration {
    // `random` returns a value in `[0, 1)`, so the product is finite and non-negative.
    delay + (delay / 4).mul_f64(rand::rng().random::<f64>())
}

/// Runs the version 2 handshake on an established QUIC connection, spawns
/// its connection task, and sends the client to the peer set.
#[allow(clippy::too_many_arguments)]
async fn finish_connection<S, C>(
    mut shared: Shared<S, C>,
    connection: quinn::Connection,
    connected_addr: ConnectedAddr,
    role: Role,
    mut connection_tracker: ConnectionTracker,
    deadline: Instant,
    mut peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    exit_notifier: Option<oneshot::Sender<()>>,
) -> Result<Arc<ConnectionInfo>, BoxError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    let completed = handshake(
        &connection,
        connected_addr,
        role,
        &shared.identity,
        &mut shared.minimum_peer_version,
        &shared.nonces,
        deadline,
    )
    .await?;
    connection_tracker.mark_open();

    let addr = connected_addr
        .get_transient_addr()
        .expect("direct connections have a transient address");
    let services = completed.connection_info.remote.services();
    let remote_tip_height = completed.connection_info.remote.start_height();
    let misbehavior = shared.connection_params.misbehavior.clone();
    // The connection task and its `SyncPeer` share one size task, so both request paths check
    // `get-object` sizes against the same earlier answers.
    let object_sizes = ObjectSizes::spawn();
    let (client, closer): (peer::Client, _) = spawn_connection(
        connection.clone(),
        completed,
        connection_tracker,
        shared.connection_params,
        object_sizes.clone(),
        exit_notifier,
    );
    let connection_info = client.connection_info.clone();
    if let Some(sync_peers) = &shared.sync_peers {
        sync_peers.connected(SyncPeer::new(
            ConnectionId::next(),
            addr,
            services,
            remote_tip_height,
            connection,
            misbehavior,
            closer,
            object_sizes,
        ));
    }

    // The peer set closed: nothing more to do, the client is dropped and the connection ends.
    peerset_tx
        .send((addr, client))
        .await
        .map_err(|_| "peer set channel closed")?;
    Ok(connection_info)
}

/// Returns whether `endpoint` is bound to an IPv4 address.
///
/// quinn refuses to send to an IPv6 address from an IPv4 socket, so such a peer could never be
/// reached; an IPv6 endpoint sends to IPv4 peers as IPv4-mapped addresses.
fn endpoint_is_ipv4(endpoint: &quinn::Endpoint) -> bool {
    endpoint
        .local_addr()
        .is_ok_and(|local_addr| local_addr.is_ipv4())
}

/// The address segment change for an outbound connection to `addr` whose handshake completed.
fn connected_change(addr: PeerSocketAddr, connection_info: &ConnectionInfo) -> MetaAddrChange {
    MetaAddr::new_connected(
        addr,
        &connection_info.remote.services(),
        false,
        connection_info.remote.user_agent().to_string(),
        connection_info.negotiated_version,
        connection_info.remote.start_height(),
    )
}

/// Counts an outbound version 2 connection attempt to a peer from `source`, `configured` or
/// `discovered`, by its `outcome`: `ok`, `failed`, or `skipped` before it was dialled.
fn record_dial(source: &'static str, outcome: &'static str) {
    metrics::counter!("zcash.net.v2.dials", "source" => source, "outcome" => outcome).increment(1);
}

/// What the discovered peer dialer works with.
pub(super) struct Discovery {
    /// The version 2 address segment, which supplies the candidates and records the attempts.
    pub book: V2BookHandle,

    /// The segment's statistics, which wake the dialer when they change.
    pub stats: watch::Receiver<V2BookStats>,

    /// The most discovered peers dialled or connected at once.
    pub limit: usize,

    /// The number of dialable addresses below which the DNS seeders are asked for more.
    pub target: usize,

    /// The configured peers' addresses, which the configured peer dialer owns, and updates at
    /// the end of each round of host resolution.
    pub configured: watch::Receiver<HashSet<PeerSocketAddr>>,

    /// Whether to wait for the first update of `configured` before dialling, because there are
    /// configured hosts. The version 2 peer cache has the configured peers that answered last
    /// time, so a candidate from it could otherwise be a configured peer that has not resolved
    /// yet, which the configured peer dialer would then dial again.
    pub await_configured: bool,

    /// The open inbound connections.
    pub open_inbound: watch::Receiver<OpenInbound>,

    /// The least time between two dials.
    pub dial_interval: Duration,

    /// How often the dialer looks for candidates without a change in the segment, for addresses
    /// whose reconnection delay passed.
    pub retry_interval: Duration,
}

/// [`keep_discovered_peers_connected`] with this node's version 2 handshake, and the DNS seeders
/// of `config`.
async fn keep_discovered_peers_connected_with<S, C>(
    shared: Shared<S, C>,
    discovery: Discovery,
    bans_receiver: watch::Receiver<BanList>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    config: &Config,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    let mut counter = ActiveConnectionCounter::new_counter_with(
        discovery.limit,
        "Outbound v2 Discovered Connections",
    );
    let dial_shared = shared.clone();
    let dial = move |addr: PeerSocketAddr| {
        let tracker = counter.track_connection();
        counter.update_count();
        dial_discovered(dial_shared.clone(), addr, tracker, peerset_tx.clone()).in_current_span()
    };
    let not_dialled = move |addr: PeerSocketAddr| not_dialled(&shared, &bans_receiver, addr);
    let resolve_seeds = move || {
        let config = config.clone();
        async move { config.resolve_v2_dns_seeds().await }
    };

    keep_discovered_peers_connected(discovery, not_dialled, dial, resolve_seeds).await
}

/// Dials the discovered version 2 peer at `addr`, with the same handshake as a configured peer.
///
/// Returns the segment change for the completed handshake, and a receiver that ends when the
/// connection ends.
async fn dial_discovered<S, C>(
    shared: Shared<S, C>,
    addr: PeerSocketAddr,
    tracker: ConnectionTracker,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
) -> Result<(MetaAddrChange, oneshot::Receiver<()>), BoxError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    let network = shared.network.clone();
    let (exit_tx, exit_rx) = oneshot::channel();
    record_connection_attempt_started(&network, ConnectionDirection::Outbound, addr);
    let result = async move {
        let connection = quic::connect(&shared.endpoint, *addr).await?;
        let deadline = Instant::now() + constants::HANDSHAKE_TIMEOUT;
        finish_connection(
            shared,
            connection,
            ConnectedAddr::new_outbound_direct(addr),
            Role::Initiator,
            tracker,
            deadline,
            peerset_tx,
            Some(exit_tx),
        )
        .await
    }
    .await;
    record_connection_attempt_finished(
        &network,
        ConnectionDirection::Outbound,
        addr,
        result.as_ref().err(),
    );

    let connection_info = result?;
    Ok((connected_change(addr, &connection_info), exit_rx))
}

/// Keeps up to `discovery.limit` connections to version 2 peers that the version 2 address
/// segment discovered, for as long as the peer set runs.
///
/// Candidates come from the segment, and are marked as attempted there when they are dialled.
/// They are never configured peers or peers connected to this node, and at most one at a time
/// comes from each address group (draft, "Address Book": diversify outbound connections).
/// `not_dialled` skips candidates on the same grounds as configured peers, without recording a
/// failure, so a skipped candidate is offered again in the next round of dials. If
/// `discovery.await_configured`, nothing is dialled until the configured peer dialer has finished
/// its first round of host resolution, so a cached configured peer is not dialled as discovered.
/// `dial` makes the connection, and returns the segment change for its handshake and a receiver
/// that ends with the connection, which keeps its entry fresh until then. A slot is free again as
/// soon as its connection fails or ends: the segment's reconnection delays space out redials.
///
/// When the segment runs out of candidates and has fewer than `discovery.target` dialable
/// addresses, the DNS seeders are asked for more with `resolve_seeds`, at most once per
/// [`V2_DNS_RESEED_INTERVAL`].
pub(super) async fn keep_discovered_peers_connected<N, D, DF, R, RF>(
    mut discovery: Discovery,
    mut not_dialled: N,
    mut dial: D,
    mut resolve_seeds: R,
) where
    N: FnMut(PeerSocketAddr) -> Option<&'static str>,
    D: FnMut(PeerSocketAddr) -> DF,
    DF: Future<Output = Result<(MetaAddrChange, oneshot::Receiver<()>), BoxError>> + Send + 'static,
    R: FnMut() -> RF,
    RF: Future<Output = Vec<PeerSocketAddr>>,
{
    if discovery.limit == 0 {
        tracing::debug!("no outbound v2 slots for discovered peers");
        return;
    }

    // DNS lookups have a timeout, so the first round ends soon. An error means the configured
    // peer dialer has stopped, so there is nothing to wait for.
    if discovery.await_configured {
        let _ = discovery.configured.changed().await;
    }

    let book = discovery.book.clone();
    let mut attempts: JoinSet<PeerSocketAddr> = JoinSet::new();
    // The candidates that were not dialled in this round of dials. They are left as they are in
    // the segment, so they are offered again in the next round.
    let mut skipped: HashSet<PeerSocketAddr> = HashSet::new();
    // The discovered peers that hold a slot, with their groups.
    let mut in_slots: HashMap<PeerSocketAddr, Group> = HashMap::new();
    let mut last_dns_seed: Option<Instant> = None;
    let mut retry_at = pin!(tokio::time::sleep(Duration::ZERO));

    loop {
        // Fill the free slots.
        let mut out_of_candidates = false;
        skipped.clear();
        while in_slots.len() < discovery.limit {
            let mut exclude: HashSet<PeerSocketAddr> = discovery.configured.borrow().clone();
            exclude.extend(discovery.open_inbound.borrow().keys().copied());
            exclude.extend(in_slots.keys().copied());
            exclude.extend(skipped.iter().copied());
            let busy_groups: HashSet<Group> = in_slots.values().copied().collect();

            let Some(candidate) = book.next_candidate(exclude, busy_groups).await else {
                out_of_candidates = true;
                break;
            };
            let addr = candidate.addr;

            if let Some(reason) = not_dialled(addr) {
                tracing::debug!(?addr, reason, "discovered v2 peer is not dialled");
                record_dial("discovered", "skipped");
                // Not a failure of the peer, so the segment is not told: a trusted entry that
                // failed would never be a candidate again, because it has no last-seen time.
                skipped.insert(addr);
                continue;
            }

            book.change(MetaAddr::new_reconnect(addr)).await;
            in_slots.insert(addr, Group::of(addr.ip()));
            let connecting = dial(addr);
            let book = book.clone();
            attempts.spawn(
                async move {
                    match connecting.await {
                        Ok((connected, exit)) => {
                            tracing::debug!(?addr, "connected to discovered v2 peer");
                            record_dial("discovered", "ok");
                            book.change(connected).await;
                            wait_for_disconnect(&book, addr, exit).await;
                            book.change(MetaAddr::new_shutdown(addr)).await;
                            tracing::debug!(?addr, "discovered v2 peer disconnected");
                        }
                        Err(error) => {
                            tracing::debug!(?addr, %error, "discovered v2 peer connection failed");
                            record_dial("discovered", "failed");
                            book.change(MetaAddr::new_errored(addr, None)).await;
                        }
                    }
                    addr
                }
                .in_current_span(),
            );

            // Space out connection attempts, like the configured peer dialer.
            tokio::time::sleep(discovery.dial_interval).await;
        }

        // Ask the DNS seeders when the segment has too few addresses to dial.
        let short = discovery.stats.borrow().dialable < discovery.target;
        let reseed_due =
            last_dns_seed.is_none_or(|seeded_at| seeded_at.elapsed() >= V2_DNS_RESEED_INTERVAL);
        if out_of_candidates && short && reseed_due {
            last_dns_seed = Some(Instant::now());
            let seeds = resolve_seeds().await;
            tracing::debug!(count = seeds.len(), "seeding v2 peers from the DNS seeders");
            // The cast is safe: there are far fewer seed addresses than 2^64.
            metrics::counter!("zcash.net.v2.dns_seed.addresses").increment(seeds.len() as u64);
            if !seeds.is_empty() {
                book.seed(seeds, Provenance::DnsSeed).await;
                continue;
            }
        }

        // Wait for a free slot, a change in the segment, or for reconnection delays to pass.
        retry_at
            .as_mut()
            .reset(Instant::now() + discovery.retry_interval);
        tokio::select! {
            Some(finished) = attempts.join_next(), if !attempts.is_empty() => {
                // A cancelled attempt has no address to free, and the dialer cancels none.
                if let Ok(addr) = finished.panic_if_task_has_panicked() {
                    in_slots.remove(&addr);
                }
            }
            changed = discovery.stats.changed() => {
                if changed.is_err() {
                    // This dialer holds a handle, so the segment outlives it.
                    tracing::warn!(
                        "the v2 address segment stopped, so discovered v2 peers are not dialled"
                    );
                    return;
                }
            }
            () = &mut retry_at => {}
        }
    }
}
