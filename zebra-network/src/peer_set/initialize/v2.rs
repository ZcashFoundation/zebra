//! Starting the experimental version 2 protocol: a QUIC endpoint that
//! accepts inbound connections, and connections to the configured initial
//! version 2 peers.
//!
//! Everything here is off unless `v2_listen_addr` is configured. Each
//! successful handshake becomes a [`peer::Client`] on the same channel the
//! legacy listener and connector feed, so the peer set treats version 2
//! peers like any other.

use std::{collections::HashMap, pin::pin, sync::OnceLock, time::Duration};

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

use crate::{
    connection_metrics::{
        record_connection_attempt_finished, record_connection_attempt_started,
        record_inbound_connection_rejected, ConnectionDirection,
    },
    constants,
    peer::{
        self,
        v2::{
            connection::{spawn_connection, ConnectionParams},
            handshake::{handshake, LocalIdentity, Role},
            misbehavior::MisbehaviorScores,
            nonces::NonceRegistry,
        },
        ConnectedAddr, MinimumPeerVersion,
    },
    peer_set::{
        initialize::{recent_by_ip, DiscoveredPeer},
        ActiveConnectionCounter, ConnectionTracker, InventoryChange,
    },
    protocol::{
        external::{canonical_peer_addr, types::PeerServices, MAX_USER_AGENT_LENGTH},
        v2::types::ErrorCode,
    },
    quic::{self, ConnectError},
    BanList, BoxError, Config, MisbehaviorReport, PeerSocketAddr, Request, Response,
};

/// The remote addresses of the open inbound version 2 connections, each with its number of
/// open connections.
type OpenInbound = HashMap<PeerSocketAddr, usize>;

/// What every version 2 connection attempt shares.
#[derive(Clone)]
struct Shared<S, C> {
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
    inbound_service: S,
    inv_collector: broadcast::Sender<InventoryChange>,
    latest_chain_tip: C,
    user_agent: String,
    bans_receiver: watch::Receiver<BanList>,
    misbehavior_tx: mpsc::Sender<MisbehaviorReport>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
) -> Option<JoinHandle<Result<(), BoxError>>>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    start_with_intervals(
        config,
        inbound_service,
        inv_collector,
        latest_chain_tip,
        user_agent,
        bans_receiver,
        misbehavior_tx,
        peerset_tx,
        constants::MIN_PEER_RECONNECTION_DELAY,
        constants::MIN_INBOUND_PEER_CONNECTION_INTERVAL,
    )
}

/// [`start`], redialling configured peers after `reconnection_delay` plus jitter, and waiting
/// `inbound_interval` between accepted inbound connections: tests use short intervals.
#[allow(clippy::too_many_arguments)]
pub(super) fn start_with_intervals<S, C>(
    config: Config,
    inbound_service: S,
    inv_collector: broadcast::Sender<InventoryChange>,
    latest_chain_tip: C,
    user_agent: String,
    bans_receiver: watch::Receiver<BanList>,
    misbehavior_tx: mpsc::Sender<MisbehaviorReport>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
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
        services: PeerServices::NODE_NETWORK,
        relay: true,
    };

    let endpoint = match quic::bind(&config.network, listen_addr) {
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

    // The peer set's own limit includes both of these, so it has room for every v2 connection.
    let outbound_limit = config.v2_outbound_connection_limit();
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
        // TODO: report version 2 peers to the address book once it records each address's
        // transport; today the legacy crawler would dial such an entry over TCP.
        connection_params: ConnectionParams {
            inbound_service,
            inv_collector,
            genesis_hash: config.network.genesis_hash(),
            misbehavior: MisbehaviorScores::spawn(
                misbehavior_tx,
                config.peerset_total_connection_limit(),
            ),
        },
        endpoint,
        identity,
    };

    // One task for both. The listener runs for the life of the peer set, so its exit is fatal,
    // like the legacy listener's; the peer set aborts the task on shutdown. The dialer returns once
    // there is nothing to keep connected (no configured peers), and the listener runs on.
    let initial_v2_peers = config.initial_v2_peers;
    Some(tokio::spawn(
        async move {
            let mut listener = pin!(accept_connections(
                shared.clone(),
                peerset_tx.clone(),
                bans_receiver.clone(),
                open_inbound_tx,
            ));
            let mut dialer = pin!(keep_initial_peers_connected(
                shared,
                initial_v2_peers,
                outbound_limit,
                bans_receiver,
                peerset_tx
            )
            .fuse());
            loop {
                tokio::select! {
                    result = &mut listener => return result,
                    // A finished `Fuse` stays pending, so from here on only the listener is awaited.
                    () = &mut dialer => {}
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
            next_host: 0,
            unresolved_hosts: 0,
        }
    }

    /// Keeps a slot free for each of `count` configured hosts that have not resolved yet.
    pub(super) fn set_unresolved_hosts(&mut self, count: usize) {
        self.unresolved_hosts = count;
    }

    /// Returns `true` if `addr` holds a slot.
    pub(super) fn holds_slot(&self, addr: &PeerSocketAddr) -> bool {
        self.in_slots.contains_key(addr)
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

    /// Gives a free slot to the next address to dial and returns it, or returns `None` if every
    /// slot is taken, every address already holds one, or the free slots are kept for the hosts
    /// that have not resolved yet.
    pub(super) fn take_next(&mut self) -> Option<PeerSocketAddr> {
        if self.in_slots.len() >= self.limit || self.hosts.is_empty() {
            return None;
        }

        let mut host_slots = vec![0_usize; self.hosts.len()];
        for &host in self.in_slots.values() {
            host_slots[host] += 1;
        }

        // The first host in turn order with the fewest addresses in slots, among the hosts that
        // have an address without one, and that host's next address without one.
        let mut chosen: Option<(usize, usize)> = None;
        for offset in 0..self.hosts.len() {
            let host = (self.next_host + offset) % self.hosts.len();
            if chosen.is_some_and(|(chosen, _)| host_slots[chosen] <= host_slots[host]) {
                continue;
            }
            let (addrs, next_addr) = &self.hosts[host];
            if let Some(index) = (0..addrs.len())
                .map(|offset| (next_addr + offset) % addrs.len())
                .find(|&index| !self.in_slots.contains_key(&addrs[index]))
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
/// connection failed or ended and the reconnection delay passed.
///
/// Resolution runs here rather than in `start`, so a slow or failing DNS
/// lookup does not delay peer set initialization. Returns once there are no
/// peers to keep connected and no hosts left to resolve.
///
/// A configured peer whose IP is in `bans_receiver` is not dialled until its
/// ban lapses.
#[instrument(skip_all)]
async fn keep_initial_peers_connected<S, C>(
    shared: Shared<S, C>,
    hosts: IndexSet<String>,
    outbound_limit: usize,
    bans_receiver: watch::Receiver<BanList>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
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
    // quinn refuses to send to an IPv6 address from an IPv4 socket, so such a peer could never be
    // reached; an IPv6 endpoint sends to IPv4 peers as IPv4-mapped addresses.
    let endpoint_is_ipv4 = shared
        .endpoint
        .local_addr()
        .is_ok_and(|local_addr| local_addr.is_ipv4());
    // Resolve straight away, then retry whatever did not resolve after the reconnection delay.
    let mut resolve_at = pin!(tokio::time::sleep(Duration::ZERO));

    loop {
        tokio::select! {
            () = &mut resolve_at, if !unresolved.is_empty() => {
                let (resolved, still_unresolved) =
                    Config::resolve_initial_v2_peers(&unresolved).await;
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
                    newly_resolved.push((host, addrs));
                }
                resolve_at
                    .as_mut()
                    .reset(Instant::now() + shared.reconnection_delay);
            }
            // An attempt returns after its connection ended, or failed, and the reconnection
            // delay has passed, so its slot is free for the next address, which can be the same.
            Some(finished) = attempts.join_next(), if !attempts.is_empty() => {
                // Drain the finished attempt's notifications.
                active_outbound_connections.update_count();
                if let Ok(addr) = finished.panic_if_task_has_panicked() {
                    slots.free(&addr);
                }
            }
            else => return,
        }

        while let Some(addr) = slots.take_next() {
            attempts.spawn(
                connect_once(
                    shared.clone(),
                    addr,
                    active_outbound_connections.track_connection(),
                    bans_receiver.clone(),
                    peerset_tx.clone(),
                )
                .in_current_span(),
            );
            active_outbound_connections.update_count();
            // Space out connection attempts, like the legacy initial peer dialer.
            tokio::time::sleep(constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL).await;
        }

        // Each host resolves successfully once, so this logs once per host.
        for (host, addrs) in newly_resolved.drain(..) {
            let waiting: Vec<_> = addrs
                .iter()
                .filter(|addr| !slots.holds_slot(addr))
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

/// Connects to one configured peer and waits until the connection ends, or
/// the attempt fails, then waits the reconnection delay plus jitter and
/// returns the address, so its slot can go to the next address to dial.
///
/// If the peer already has an inbound connection to this node, does not dial
/// it: version 2 connections leave from the listening port, so both
/// connections would have the same peer set key and per-IP slot, and the peer
/// set would drop one of them, at each end independently. Waits for that
/// connection to end, or the reconnection delay, instead.
///
/// If the peer's IP is banned, does not dial it either, so a misbehaving
/// configured peer is not reconnected: waits the reconnection delay and
/// returns, and the ban is checked again on the next attempt.
async fn connect_once<S, C>(
    shared: Shared<S, C>,
    addr: PeerSocketAddr,
    tracker: ConnectionTracker,
    bans_receiver: watch::Receiver<BanList>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
) -> PeerSocketAddr
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    let network = shared.network.clone();
    let reconnection_delay = shared.reconnection_delay;

    // `is_banned` checks the ban's age when it is called, so a lapsed ban is seen on the next
    // attempt without waiting for the ban list to change.
    if bans_receiver.borrow().is_banned(addr.ip()) {
        tracing::debug!(?addr, "initial v2 peer is banned, not dialling it");
        // No connection is attempted, so its slot is free while this waits.
        drop(tracker);
        tokio::time::sleep(with_jitter(reconnection_delay)).await;
        return addr;
    }

    let mut open_inbound = shared.open_inbound.clone();
    if open_inbound.borrow().contains_key(&addr) {
        tracing::debug!(
            ?addr,
            "initial v2 peer is already connected to us, not dialling it"
        );
        // No connection is attempted, so its slot is free while this waits.
        drop(tracker);
        // An error means the listener has exited, which ends the whole task anyway.
        let _ = tokio::time::timeout(
            reconnection_delay,
            open_inbound.wait_for(|open| !open.contains_key(&addr)),
        )
        .await;
        tokio::time::sleep(with_jitter(reconnection_delay)).await;
        return addr;
    }

    let (exit_tx, exit_rx) = oneshot::channel();
    record_connection_attempt_started(&network, ConnectionDirection::Outbound, addr);
    let result: Result<(), BoxError> = async move {
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
        Ok(()) => {
            tracing::info!(?addr, "connected to initial v2 peer");
            // Wait for the connection task to end, however it ends.
            let _ = exit_rx.await;
            tracing::info!(?addr, "initial v2 peer disconnected, will reconnect");
        }
        Err(error) => {
            tracing::info!(?addr, %error, "initial v2 peer connection failed, will retry");
        }
    }

    tokio::time::sleep(with_jitter(reconnection_delay)).await;
    addr
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
) -> Result<(), BoxError>
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
    let client: peer::Client = spawn_connection(
        connection,
        completed,
        connection_tracker,
        shared.connection_params,
        exit_notifier,
    );

    // The peer set closed: nothing more to do, the client is dropped and the connection ends.
    peerset_tx
        .send((addr, client))
        .await
        .map_err(|_| "peer set channel closed")?;
    Ok(())
}
