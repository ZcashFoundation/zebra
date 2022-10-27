//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

use std::{
    collections::{BTreeMap, HashSet},
    net::SocketAddr,
    sync::Arc,
};

use futures::{
    future::{self, FutureExt},
    sink::SinkExt,
    stream::{FuturesUnordered, StreamExt, TryStreamExt},
    TryFutureExt,
};
use rand::seq::SliceRandom;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::broadcast,
    time::{sleep, Instant},
};
use tokio_stream::wrappers::IntervalStream;
use tower::{
    buffer::Buffer, discover::Change, layer::Layer, util::BoxService, Service, ServiceExt,
};
use tracing_futures::Instrument;

use zebra_chain::{chain_tip::ChainTip, parameters::Network};

use crate::{
    address_book_updater::AddressBookUpdater,
    constants,
    meta_addr::{MetaAddr, MetaAddrChange},
    peer::{self, HandshakeRequest, MinimumPeerVersion, OutboundConnectorRequest, PeerPreference},
    peer_set::{set::MorePeers, ActiveConnectionCounter, CandidateSet, ConnectionTracker, PeerSet},
    AddressBook, BoxError, Config, Request, Response,
};

#[cfg(test)]
mod tests;

/// The result of an outbound peer connection attempt or inbound connection
/// handshake.
///
/// This result comes from the `Handshaker`.
type DiscoveredPeer = Result<(SocketAddr, peer::Client), BoxError>;

/// Initialize a peer set, using a network `config`, `inbound_service`,
/// and `latest_chain_tip`.
///
/// The peer set abstracts away peer management to provide a
/// [`tower::Service`] representing "the network" that load-balances requests
/// over available peers.  The peer set automatically crawls the network to
/// find more peer addresses and opportunistically connects to new peers.
///
/// Each peer connection's message handling is isolated from other
/// connections, unlike in `zcashd`.  The peer connection first attempts to
/// interpret inbound messages as part of a response to a previously-issued
/// request.  Otherwise, inbound messages are interpreted as requests and sent
/// to the supplied `inbound_service`.
///
/// Wrapping the `inbound_service` in [`tower::load_shed`] middleware will
/// cause the peer set to shrink when the inbound service is unable to keep up
/// with the volume of inbound requests.
///
/// Use [`NoChainTip`][1] to explicitly provide no chain tip receiver.
///
/// In addition to returning a service for outbound requests, this method
/// returns a shared [`AddressBook`] updated with last-seen timestamps for
/// connected peers. The shared address book should be accessed using a
/// [blocking thread](https://docs.rs/tokio/1.15.0/tokio/task/index.html#blocking-and-yielding),
/// to avoid async task deadlocks.
///
/// # Panics
///
/// If `config.config.peerset_initial_target_size` is zero.
/// (zebra-network expects to be able to connect to at least one peer.)
///
/// [1]: zebra_chain::chain_tip::NoChainTip
pub async fn init<S, C>(
    config: Config,
    inbound_service: S,
    latest_chain_tip: C,
) -> (
    Buffer<BoxService<Request, Response, BoxError>, Request>,
    Arc<std::sync::Mutex<AddressBook>>,
)
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + 'static,
{
    // If we want Zebra to operate with no network,
    // we should implement a `zebrad` command that doesn't use `zebra-network`.
    assert!(
        config.peerset_initial_target_size > 0,
        "Zebra must be allowed to connect to at least one peer"
    );

    let (tcp_listener, listen_addr) = open_listener(&config.clone()).await;

    let (address_book, address_book_updater, address_metrics, address_book_updater_guard) =
        AddressBookUpdater::spawn(&config, listen_addr);

    // Create a broadcast channel for peer inventory advertisements.
    // If it reaches capacity, this channel drops older inventory advertisements.
    //
    // When Zebra is at the chain tip with an up-to-date mempool,
    // we expect to have at most 1 new transaction per connected peer,
    // and 1-2 new blocks across the entire network.
    // (The block syncer and mempool crawler handle bulk fetches of blocks and transactions.)
    let (inv_sender, inv_receiver) = broadcast::channel(config.peerset_total_connection_limit());

    // Construct services that handle inbound handshakes and perform outbound
    // handshakes. These use the same handshake service internally to detect
    // self-connection attempts. Both are decorated with a tower TimeoutLayer to
    // enforce timeouts as specified in the Config.
    let (listen_handshaker, outbound_connector) = {
        use tower::timeout::TimeoutLayer;
        let hs_timeout = TimeoutLayer::new(constants::HANDSHAKE_TIMEOUT);
        use crate::protocol::external::types::PeerServices;
        let hs = peer::Handshake::builder()
            .with_config(config.clone())
            .with_inbound_service(inbound_service)
            .with_inventory_collector(inv_sender)
            .with_address_book_updater(address_book_updater.clone())
            .with_advertised_services(PeerServices::NODE_NETWORK)
            .with_user_agent(crate::constants::USER_AGENT.to_string())
            .with_latest_chain_tip(latest_chain_tip.clone())
            .want_transactions(true)
            .finish()
            .expect("configured all required parameters");
        (
            hs_timeout.layer(hs.clone()),
            hs_timeout.layer(peer::Connector::new(hs)),
        )
    };

    // Create an mpsc channel for peer changes,
    // based on the maximum number of inbound and outbound peers.
    let (peerset_tx, peerset_rx) =
        futures::channel::mpsc::channel::<DiscoveredPeer>(config.peerset_total_connection_limit());

    let discovered_peers = peerset_rx
        // Discover interprets an error as stream termination,
        // so discard any errored connections...
        .filter(|result| future::ready(result.is_ok()))
        .map_ok(|(address, client)| Change::Insert(address, client.into()));

    // Create an mpsc channel for peerset demand signaling,
    // based on the maximum number of outbound peers.
    let (mut demand_tx, demand_rx) =
        futures::channel::mpsc::channel::<MorePeers>(config.peerset_outbound_connection_limit());

    // Create a oneshot to send background task JoinHandles to the peer set
    let (handle_tx, handle_rx) = tokio::sync::oneshot::channel();

    // Connect the rx end to a PeerSet, wrapping new peers in load instruments.
    let peer_set = PeerSet::new(
        &config,
        discovered_peers,
        demand_tx.clone(),
        handle_rx,
        inv_receiver,
        address_metrics,
        MinimumPeerVersion::new(latest_chain_tip, config.network),
    );
    let peer_set = Buffer::new(BoxService::new(peer_set), constants::PEERSET_BUFFER_SIZE);

    // Connect peerset_tx to the 3 peer sources:
    //
    // 1. Incoming peer connections, via a listener.
    let listen_fut = accept_inbound_connections(
        config.clone(),
        tcp_listener,
        listen_handshaker,
        peerset_tx.clone(),
    );
    let listen_guard = tokio::spawn(listen_fut.in_current_span());

    // 2. Initial peers, specified in the config.
    let initial_peers_fut = add_initial_peers(
        config.clone(),
        outbound_connector.clone(),
        peerset_tx.clone(),
        address_book_updater,
    );
    let initial_peers_join = tokio::spawn(initial_peers_fut.in_current_span());

    // 3. Outgoing peers we connect to in response to load.
    let mut candidates = CandidateSet::new(address_book.clone(), peer_set.clone());

    // Wait for the initial seed peer count
    let mut active_outbound_connections = initial_peers_join
        .await
        .expect("unexpected panic in spawned initial peers task")
        .expect("unexpected error connecting to initial peers");
    let active_initial_peer_count = active_outbound_connections.update_count();

    // We need to await candidates.update() here,
    // because zcashd rate-limits `addr`/`addrv2` messages per connection,
    // and if we only have one initial peer,
    // we need to ensure that its `Response::Addr` is used by the crawler.
    info!(
        ?active_initial_peer_count,
        "sending initial request for peers"
    );
    let _ = candidates.update_initial(active_initial_peer_count).await;

    // Compute remaining connections to open.
    let demand_count = config
        .peerset_initial_target_size
        .saturating_sub(active_outbound_connections.update_count());

    for _ in 0..demand_count {
        let _ = demand_tx.try_send(MorePeers);
    }

    let crawl_fut = crawl_and_dial(
        config,
        demand_tx,
        demand_rx,
        candidates,
        outbound_connector,
        peerset_tx,
        active_outbound_connections,
    );
    let crawl_guard = tokio::spawn(crawl_fut.in_current_span());

    handle_tx
        .send(vec![listen_guard, crawl_guard, address_book_updater_guard])
        .unwrap();

    (peer_set, address_book)
}

/// Use the provided `outbound_connector` to connect to the configured initial peers,
/// then send the resulting peer connections over `peerset_tx`.
///
/// Also sends every initial peer address to the `address_book_updater`.
#[instrument(skip(config, outbound_connector, peerset_tx, address_book_updater))]
async fn add_initial_peers<S>(
    config: Config,
    outbound_connector: S,
    mut peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    address_book_updater: tokio::sync::mpsc::Sender<MetaAddrChange>,
) -> Result<ActiveConnectionCounter, BoxError>
where
    S: Service<OutboundConnectorRequest, Response = (SocketAddr, peer::Client), Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    let initial_peers = limit_initial_peers(&config, address_book_updater).await;

    let mut handshake_success_total: usize = 0;
    let mut handshake_error_total: usize = 0;

    let mut active_outbound_connections = ActiveConnectionCounter::new_counter();

    info!(
        initial_peer_count = ?initial_peers.len(),
        ?initial_peers,
        "connecting to initial peer set"
    );

    // # Security
    //
    // Resists distributed denial of service attacks by making sure that
    // new peer connections are initiated at least
    // [`MIN_PEER_CONNECTION_INTERVAL`][constants::MIN_PEER_CONNECTION_INTERVAL]
    // apart.
    //
    // # Correctness
    //
    // Each `FuturesUnordered` can hold one `Buffer` or `Batch` reservation for
    // an indefinite period. We can use `FuturesUnordered` without filling
    // the underlying network buffers, because we immediately drive this
    // single `FuturesUnordered` to completion, and handshakes have a short timeout.
    let mut handshakes: FuturesUnordered<_> = initial_peers
        .into_iter()
        .enumerate()
        .map(|(i, addr)| {
            let connection_tracker = active_outbound_connections.track_connection();
            let req = OutboundConnectorRequest {
                addr,
                connection_tracker,
            };
            let outbound_connector = outbound_connector.clone();

            // Spawn a new task to make the outbound connection.
            tokio::spawn(
                async move {
                    // Only spawn one outbound connector per `MIN_PEER_CONNECTION_INTERVAL`,
                    // sleeping for an interval according to its index in the list.
                    sleep(constants::MIN_PEER_CONNECTION_INTERVAL.saturating_mul(i as u32)).await;

                    // As soon as we create the connector future,
                    // the handshake starts running as a spawned task.
                    outbound_connector
                        .oneshot(req)
                        .map_err(move |e| (addr, e))
                        .await
                }
                .in_current_span(),
            )
        })
        .collect();

    while let Some(handshake_result) = handshakes.next().await {
        let handshake_result =
            handshake_result.expect("unexpected panic in initial peer handshake");
        match handshake_result {
            Ok(ref change) => {
                handshake_success_total += 1;
                debug!(
                    ?handshake_success_total,
                    ?handshake_error_total,
                    ?change,
                    "an initial peer handshake succeeded"
                );
            }
            Err((addr, ref e)) => {
                handshake_error_total += 1;

                // this is verbose, but it's better than just hanging with no output when there are errors
                let mut expected_error = false;
                if let Some(io_error) = e.downcast_ref::<tokio::io::Error>() {
                    // Some systems only have IPv4, or only have IPv6,
                    // so these errors are not particularly interesting.
                    if io_error.kind() == tokio::io::ErrorKind::AddrNotAvailable {
                        expected_error = true;
                    }
                }

                if expected_error {
                    debug!(
                        successes = ?handshake_success_total,
                        errors = ?handshake_error_total,
                        ?addr,
                        ?e,
                        "an initial peer connection failed"
                    );
                } else {
                    info!(
                        successes = ?handshake_success_total,
                        errors = ?handshake_error_total,
                        ?addr,
                        %e,
                        "an initial peer connection failed"
                    );
                }
            }
        }

        peerset_tx
            .send(handshake_result.map_err(|(_addr, e)| e))
            .await?;

        // Security: Let other tasks run after each connection is processed.
        //
        // Avoids remote peers starving other Zebra tasks using initial connection successes or errors.
        tokio::task::yield_now().await;
    }

    let outbound_connections = active_outbound_connections.update_count();
    info!(
        ?handshake_success_total,
        ?handshake_error_total,
        ?outbound_connections,
        "finished connecting to initial seed peers"
    );

    Ok(active_outbound_connections)
}

/// Limit the number of `initial_peers` addresses entries to the configured
/// `peerset_initial_target_size`.
///
/// Returns randomly chosen entries from the provided set of addresses,
/// in a random order.
///
/// Also sends every initial peer to the `address_book_updater`.
async fn limit_initial_peers(
    config: &Config,
    address_book_updater: tokio::sync::mpsc::Sender<MetaAddrChange>,
) -> HashSet<SocketAddr> {
    let all_peers: HashSet<SocketAddr> = config.initial_peers().await;
    let mut preferred_peers: BTreeMap<PeerPreference, Vec<SocketAddr>> = BTreeMap::new();

    let all_peers_count = all_peers.len();
    if all_peers_count > config.peerset_initial_target_size {
        info!(
            "limiting the initial peers list from {} to {}",
            all_peers_count, config.peerset_initial_target_size,
        );
    }

    // Filter out invalid initial peers, and prioritise valid peers for initial connections.
    // (This treats initial peers the same way we treat gossiped peers.)
    for peer_addr in all_peers {
        let preference = PeerPreference::new(&peer_addr, config.network);

        match preference {
            Ok(preference) => preferred_peers
                .entry(preference)
                .or_default()
                .push(peer_addr),
            Err(error) => warn!(
                ?peer_addr,
                ?error,
                "invalid initial peer from DNS seeder or configured IP address",
            ),
        }
    }

    // Send every initial peer to the address book, in preferred order.
    // (This treats initial peers the same way we treat gossiped peers.)
    for peer in preferred_peers.values().flatten() {
        let peer_addr = MetaAddr::new_initial_peer(*peer);
        // `send` only waits when the channel is full.
        // The address book updater runs in its own thread, so we will only wait for a short time.
        let _ = address_book_updater.send(peer_addr).await;
    }

    // Split out the `initial_peers` that will be shuffled and returned,
    // choosing preferred peers first.
    let mut initial_peers: HashSet<SocketAddr> = HashSet::new();
    for better_peers in preferred_peers.values() {
        let mut better_peers = better_peers.clone();
        let (chosen_peers, _unused_peers) = better_peers.partial_shuffle(
            &mut rand::thread_rng(),
            config.peerset_initial_target_size - initial_peers.len(),
        );

        initial_peers.extend(chosen_peers.iter());

        if initial_peers.len() >= config.peerset_initial_target_size {
            break;
        }
    }

    initial_peers
}

/// Open a peer connection listener on `config.listen_addr`,
/// returning the opened [`TcpListener`], and the address it is bound to.
///
/// If the listener is configured to use an automatically chosen port (port `0`),
/// then the returned address will contain the actual port.
///
/// # Panics
///
/// If opening the listener fails.
#[instrument(skip(config), fields(addr = ?config.listen_addr))]
pub(crate) async fn open_listener(config: &Config) -> (TcpListener, SocketAddr) {
    // Warn if we're configured using the wrong network port.
    use Network::*;
    let wrong_net = match config.network {
        Mainnet => Testnet,
        Testnet => Mainnet,
    };
    if config.listen_addr.port() == wrong_net.default_port() {
        warn!(
            "We are configured with port {} for {:?}, but that port is the default port for {:?}. The default port for {:?} is {}.",
            config.listen_addr.port(),
            config.network,
            wrong_net,
            config.network,
            config.network.default_port(),
        );
    }

    info!(
        "Trying to open Zcash protocol endpoint at {}...",
        config.listen_addr
    );
    let listener_result = TcpListener::bind(config.listen_addr).await;

    let listener = match listener_result {
        Ok(l) => l,
        Err(e) => panic!(
            "Opening Zcash network protocol listener {:?} failed: {e:?}. \
             Hint: Check if another zebrad or zcashd process is running. \
             Try changing the network listen_addr in the Zebra config.",
            config.listen_addr,
        ),
    };

    let local_addr = listener
        .local_addr()
        .expect("unexpected missing local addr for open listener");
    info!("Opened Zcash protocol endpoint at {}", local_addr);

    (listener, local_addr)
}

/// Listens for peer connections on `addr`, then sets up each connection as a
/// Zcash peer.
///
/// Uses `handshaker` to perform a Zcash network protocol handshake, and sends
/// the [`peer::Client`] result over `peerset_tx`.
///
/// Limit the number of active inbound connections based on `config`.
#[instrument(skip(config, listener, handshaker, peerset_tx), fields(listener_addr = ?listener.local_addr()))]
async fn accept_inbound_connections<S>(
    config: Config,
    listener: TcpListener,
    mut handshaker: S,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
) -> Result<(), BoxError>
where
    S: Service<peer::HandshakeRequest<TcpStream>, Response = peer::Client, Error = BoxError>
        + Clone,
    S::Future: Send + 'static,
{
    let mut active_inbound_connections = ActiveConnectionCounter::new_counter();

    let mut handshakes = FuturesUnordered::new();
    // Keeping an unresolved future in the pool means the stream never terminates.
    handshakes.push(future::pending().boxed());

    loop {
        // Check for panics in finished tasks, before accepting new connections
        let inbound_result = tokio::select! {
            biased;
            next_handshake_res = handshakes.next() => match next_handshake_res {
                // The task has already sent the peer change to the peer set.
                Some(Ok(_)) => continue,
                Some(Err(task_panic)) => panic!("panic in inbound handshake task: {task_panic:?}"),
                None => unreachable!("handshakes never terminates, because it contains a future that never resolves"),
            },

            inbound_result = listener.accept() => inbound_result,
        };

        if let Ok((tcp_stream, addr)) = inbound_result {
            if active_inbound_connections.update_count()
                >= config.peerset_inbound_connection_limit()
            {
                // Too many open inbound connections or pending handshakes already.
                // Close the connection.
                std::mem::drop(tcp_stream);
                continue;
            }

            // The peer already opened a connection to us.
            // So we want to increment the connection count as soon as possible.
            let connection_tracker = active_inbound_connections.track_connection();
            debug!(
                inbound_connections = ?active_inbound_connections.update_count(),
                "handshaking on an open inbound peer connection"
            );

            let connected_addr = peer::ConnectedAddr::new_inbound_direct(addr);
            let accept_span = info_span!("listen_accept", peer = ?connected_addr);
            let _guard = accept_span.enter();

            debug!("got incoming connection");
            handshaker.ready().await?;
            // TODO: distinguish between proxied listeners and direct listeners
            let handshaker_span = info_span!("listen_handshaker", peer = ?connected_addr);

            // Construct a handshake future but do not drive it yet....
            let handshake = handshaker.call(HandshakeRequest {
                data_stream: tcp_stream,
                connected_addr,
                connection_tracker,
            });
            // ... instead, spawn a new task to handle this connection
            {
                let mut peerset_tx = peerset_tx.clone();

                let handshake_task = tokio::spawn(
                    async move {
                        let handshake_result = handshake.await;

                        if let Ok(client) = handshake_result {
                            let _ = peerset_tx.send(Ok((addr, client))).await;
                        } else {
                            debug!(?handshake_result, "error handshaking with inbound peer");
                        }
                    }
                    .instrument(handshaker_span),
                );

                handshakes.push(Box::pin(handshake_task));
            }

            // Only spawn one inbound connection handshake per `MIN_PEER_CONNECTION_INTERVAL`.
            // But clear out failed connections as fast as possible.
            //
            // If there is a flood of connections,
            // this stops Zebra overloading the network with handshake data.
            //
            // Zebra can't control how many queued connections are waiting,
            // but most OSes also limit the number of queued inbound connections on a listener port.
            tokio::time::sleep(constants::MIN_PEER_CONNECTION_INTERVAL).await;
        } else {
            debug!(?inbound_result, "error accepting inbound connection");
        }

        // Security: Let other tasks run after each connection is processed.
        //
        // Avoids remote peers starving other Zebra tasks using inbound connection successes or errors.
        tokio::task::yield_now().await;
    }
}

/// An action that the peer crawler can take.
#[allow(dead_code)]
enum CrawlerAction {
    /// Drop the demand signal because there are too many pending handshakes.
    DemandDrop,
    /// Initiate a handshake to `candidate` in response to demand.
    DemandHandshake { candidate: MetaAddr },
    /// Crawl existing peers for more peers in response to demand, because there
    /// are no available candidates.
    DemandCrawl,
    /// Crawl existing peers for more peers in response to a timer `tick`.
    TimerCrawl { tick: Instant },
    /// Handle a successfully connected handshake `peer_set_change`.
    HandshakeConnected {
        address: SocketAddr,
        client: peer::Client,
    },
    /// Handle a handshake failure to `failed_addr`.
    HandshakeFailed { failed_addr: MetaAddr },
}

/// Given a channel `demand_rx` that signals a need for new peers, try to find
/// and connect to new peers, and send the resulting `peer::Client`s through the
/// `peerset_tx` channel.
///
/// Crawl for new peers every `config.crawl_new_peer_interval`.
/// Also crawl whenever there is demand, but no new peers in `candidates`.
/// After crawling, try to connect to one new peer using `outbound_connector`.
///
/// If a handshake fails, restore the unused demand signal by sending it to
/// `demand_tx`.
///
/// The crawler terminates when `candidates.update()` or `peerset_tx` returns a
/// permanent internal error. Transient errors and individual peer errors should
/// be handled within the crawler.
///
/// Uses `active_outbound_connections` to limit the number of active outbound connections
/// across both the initial peers and crawler. The limit is based on `config`.
#[instrument(
    skip(
        config,
        demand_tx,
        demand_rx,
        candidates,
        outbound_connector,
        peerset_tx,
        active_outbound_connections,
    ),
    fields(
        new_peer_interval = ?config.crawl_new_peer_interval,
    )
)]
async fn crawl_and_dial<C, S>(
    config: Config,
    mut demand_tx: futures::channel::mpsc::Sender<MorePeers>,
    mut demand_rx: futures::channel::mpsc::Receiver<MorePeers>,
    mut candidates: CandidateSet<S>,
    outbound_connector: C,
    mut peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    mut active_outbound_connections: ActiveConnectionCounter,
) -> Result<(), BoxError>
where
    C: Service<OutboundConnectorRequest, Response = (SocketAddr, peer::Client), Error = BoxError>
        + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
    S: Service<Request, Response = Response, Error = BoxError>,
    S::Future: Send + 'static,
{
    use CrawlerAction::*;

    // CORRECTNESS
    //
    // To avoid hangs and starvation, the crawler must:
    // - spawn a separate task for each crawl and handshake, so they can make
    //   progress independently (and avoid deadlocking each other)
    // - use the `select!` macro for all actions, because the `select` function
    //   is biased towards the first ready future

    info!(
        crawl_new_peer_interval = ?config.crawl_new_peer_interval,
        outbound_connections = ?active_outbound_connections.update_count(),
        "starting the peer address crawler",
    );

    let mut handshakes = FuturesUnordered::new();
    // <FuturesUnordered as Stream> returns None when empty.
    // Keeping an unresolved future in the pool means the stream
    // never terminates.
    // We could use StreamExt::select_next_some and StreamExt::fuse, but `fuse`
    // prevents us from adding items to the stream and checking its length.
    handshakes.push(future::pending().boxed());

    let mut crawl_timer = tokio::time::interval(config.crawl_new_peer_interval);
    // If the crawl is delayed, also delay all future crawls.
    // (Shorter intervals just add load, without any benefit.)
    crawl_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut crawl_timer = IntervalStream::new(crawl_timer).map(|tick| TimerCrawl { tick });

    loop {
        metrics::gauge!(
            "crawler.in_flight_handshakes",
            handshakes
                .len()
                .checked_sub(1)
                .expect("the pool always contains an unresolved future") as f64,
        );

        let crawler_action = tokio::select! {
            next_handshake_res = handshakes.next() => next_handshake_res.expect(
                "handshakes never terminates, because it contains a future that never resolves"
            ),
            next_timer = crawl_timer.next() => next_timer.expect("timers never terminate"),
            // turn the demand into an action, based on the crawler's current state
            _ = demand_rx.next() => {
                if active_outbound_connections.update_count() >= config.peerset_outbound_connection_limit() {
                    // Too many open outbound connections or pending handshakes already
                    DemandDrop
                } else if let Some(candidate) = candidates.next().await {
                    // candidates.next has a short delay, and briefly holds the address
                    // book lock, so it shouldn't hang
                    DemandHandshake { candidate }
                } else {
                    DemandCrawl
                }
            }
        };

        match crawler_action {
            DemandDrop => {
                // This is set to trace level because when the peerset is
                // congested it can generate a lot of demand signal very
                // rapidly.
                trace!("too many open connections or in-flight handshakes, dropping demand signal");
            }
            DemandHandshake { candidate } => {
                // Increment the connection count before we spawn the connection.
                let outbound_connection_tracker = active_outbound_connections.track_connection();
                debug!(
                    outbound_connections = ?active_outbound_connections.update_count(),
                    "opening an outbound peer connection"
                );

                // Spawn each handshake into an independent task, so it can make
                // progress independently of the crawls.
                let hs_join = tokio::spawn(dial(
                    candidate,
                    outbound_connector.clone(),
                    outbound_connection_tracker,
                ))
                .map(move |res| match res {
                    Ok(crawler_action) => crawler_action,
                    Err(e) => {
                        panic!("panic during handshaking with {candidate:?}: {e:?} ");
                    }
                })
                .in_current_span();

                handshakes.push(Box::pin(hs_join));
            }
            DemandCrawl => {
                debug!("demand for peers but no available candidates");
                // update has timeouts, and briefly holds the address book
                // lock, so it shouldn't hang
                //
                // TODO: refactor candidates into a buffered service, so we can
                //       spawn independent tasks to avoid deadlocks
                let more_peers = candidates.update().await?;

                // If we got more peers, try to connect to a new peer.
                //
                // # Security
                //
                // Update attempts are rate-limited by the candidate set.
                //
                // We only try peers if there was actually an update.
                // So if all peers have had a recent attempt,
                // and there was recent update with no peers,
                // the channel will drain.
                // This prevents useless update attempt loops.
                if let Some(more_peers) = more_peers {
                    let _ = demand_tx.try_send(more_peers);
                }
            }
            TimerCrawl { tick } => {
                debug!(
                    ?tick,
                    "crawling for more peers in response to the crawl timer"
                );
                // TODO: spawn independent tasks to avoid deadlocks
                candidates.update().await?;
                // Try to connect to a new peer.
                let _ = demand_tx.try_send(MorePeers);
            }
            HandshakeConnected { address, client } => {
                debug!(candidate.addr = ?address, "successfully dialed new peer");
                // successes are handled by an independent task, except for `candidates.update` in
                // this task, which has a timeout, so they shouldn't hang
                peerset_tx.send(Ok((address, client))).await?;
            }
            HandshakeFailed { failed_addr } => {
                // The connection was never opened, or it failed the handshake and was dropped.

                debug!(?failed_addr.addr, "marking candidate as failed");
                candidates.report_failed(&failed_addr).await;
                // The demand signal that was taken out of the queue
                // to attempt to connect to the failed candidate never
                // turned into a connection, so add it back:
                //
                // Security: handshake failures are rate-limited by peer attempt timeouts.
                let _ = demand_tx.try_send(MorePeers);
            }
        }

        // Security: Let other tasks run after each crawler action is processed.
        //
        // Avoids remote peers starving other Zebra tasks using outbound connection errors.
        tokio::task::yield_now().await;
    }
}

/// Try to connect to `candidate` using `outbound_connector`.
/// Uses `outbound_connection_tracker` to track the active connection count.
///
/// Returns a `HandshakeConnected` action on success, and a
/// `HandshakeFailed` action on error.
#[instrument(skip(outbound_connector, outbound_connection_tracker))]
async fn dial<C>(
    candidate: MetaAddr,
    mut outbound_connector: C,
    outbound_connection_tracker: ConnectionTracker,
) -> CrawlerAction
where
    C: Service<OutboundConnectorRequest, Response = (SocketAddr, peer::Client), Error = BoxError>
        + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
{
    // CORRECTNESS
    //
    // To avoid hangs, the dialer must only await:
    // - functions that return immediately, or
    // - functions that have a reasonable timeout

    debug!(?candidate.addr, "attempting outbound connection in response to demand");

    // the connector is always ready, so this can't hang
    let outbound_connector = outbound_connector
        .ready()
        .await
        .expect("outbound connector never errors");

    let req = OutboundConnectorRequest {
        addr: candidate.addr,
        connection_tracker: outbound_connection_tracker,
    };

    // the handshake has timeouts, so it shouldn't hang
    outbound_connector
        .call(req)
        .map_err(|e| (candidate, e))
        .map(Into::into)
        .await
}

impl From<Result<(SocketAddr, peer::Client), (MetaAddr, BoxError)>> for CrawlerAction {
    fn from(dial_result: Result<(SocketAddr, peer::Client), (MetaAddr, BoxError)>) -> Self {
        use CrawlerAction::*;
        match dial_result {
            Ok((address, client)) => HandshakeConnected { address, client },
            Err((candidate, e)) => {
                debug!(?candidate.addr, ?e, "failed to connect to candidate");
                HandshakeFailed {
                    failed_addr: candidate,
                }
            }
        }
    }
}
