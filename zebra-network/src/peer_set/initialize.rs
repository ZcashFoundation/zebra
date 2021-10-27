//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

use std::{collections::HashSet, net::SocketAddr, sync::Arc};

use futures::{
    channel::mpsc,
    future::{self, FutureExt},
    sink::SinkExt,
    stream::{FuturesUnordered, StreamExt},
    TryFutureExt,
};
use rand::seq::SliceRandom;
use tokio::{
    net::TcpListener,
    sync::broadcast,
    time::{sleep, Instant},
};
use tower::{
    buffer::Buffer, discover::Change, layer::Layer, load::peak_ewma::PeakEwmaDiscover,
    util::BoxService, Service, ServiceExt,
};
use tracing::Span;
use tracing_futures::Instrument;

use zebra_chain::{chain_tip::ChainTip, parameters::Network};

use crate::{
    constants,
    meta_addr::MetaAddr,
    peer::{self, HandshakeRequest, OutboundConnectorRequest},
    peer_set::{set::MorePeers, ActiveConnectionCounter, CandidateSet, ConnectionTracker, PeerSet},
    timestamp_collector::TimestampCollector,
    AddressBook, BoxError, Config, Request, Response,
};

#[cfg(test)]
mod tests;

/// The result of an outbound peer connection attempt or inbound connection handshake.
///
/// This result comes from the [`Handshaker`].
type PeerChange = Result<Change<SocketAddr, peer::Client>, BoxError>;

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
/// Use [`NoChainTip`] to explicitly provide no chain tip receiver.
///
/// In addition to returning a service for outbound requests, this method
/// returns a shared [`AddressBook`] updated with last-seen timestamps for
/// connected peers.
///
/// # Panics
///
/// If `config.config.peerset_initial_target_size` is zero.
/// (zebra-network expects to be able to connect to at least one peer.)
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

    let (address_book, timestamp_collector) = TimestampCollector::spawn(listen_addr);

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
            .with_timestamp_collector(timestamp_collector)
            .with_advertised_services(PeerServices::NODE_NETWORK)
            .with_user_agent(crate::constants::USER_AGENT.to_string())
            .with_latest_chain_tip(latest_chain_tip)
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
        mpsc::channel::<PeerChange>(config.peerset_total_connection_limit());
    // Create an mpsc channel for peerset demand signaling,
    // based on the maximum number of outbound peers.
    let (mut demand_tx, demand_rx) =
        mpsc::channel::<MorePeers>(config.peerset_outbound_connection_limit());

    // Create a oneshot to send background task JoinHandles to the peer set
    let (handle_tx, handle_rx) = tokio::sync::oneshot::channel();

    // Connect the rx end to a PeerSet, wrapping new peers in load instruments.
    let peer_set = PeerSet::new(
        PeakEwmaDiscover::new(
            // Discover interprets an error as stream termination,
            // so discard any errored connections...
            peerset_rx.filter(|result| future::ready(result.is_ok())),
            constants::EWMA_DEFAULT_RTT,
            constants::EWMA_DECAY_TIME,
            tower::load::CompleteOnResponse::default(),
        ),
        demand_tx.clone(),
        handle_rx,
        inv_receiver,
        address_book.clone(),
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
    let listen_guard = tokio::spawn(listen_fut.instrument(Span::current()));

    // 2. Initial peers, specified in the config.
    let initial_peers_fut = add_initial_peers(
        config.clone(),
        outbound_connector.clone(),
        peerset_tx.clone(),
    );
    let initial_peers_join = tokio::spawn(initial_peers_fut.instrument(Span::current()));

    // 3. Outgoing peers we connect to in response to load.
    let mut candidates = CandidateSet::new(address_book.clone(), peer_set.clone());

    // Wait for the initial seed peer count
    let mut active_outbound_connections = initial_peers_join
        .await
        .expect("unexpected panic in spawned initial peers task")
        .expect("unexpected error connecting to initial peers");
    let active_initial_peer_count = active_outbound_connections.update_count();

    // We need to await candidates.update() here, because zcashd only sends one
    // `addr` message per connection, and if we only have one initial peer we
    // need to ensure that its `addr` message is used by the crawler.

    info!(
        ?active_initial_peer_count,
        "sending initial request for peers"
    );
    let _ = candidates.update_initial(active_initial_peer_count).await;

    // TODO: reduce demand by `active_outbound_connections.update_count()` (#2902)
    for _ in 0..config.peerset_initial_target_size {
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
    let crawl_guard = tokio::spawn(crawl_fut.instrument(Span::current()));

    handle_tx.send(vec![listen_guard, crawl_guard]).unwrap();

    (peer_set, address_book)
}

/// Use the provided `outbound_connector` to connect to the configured initial peers,
/// then send the resulting peer connections over `peerset_tx`.
#[instrument(skip(config, outbound_connector, peerset_tx))]
async fn add_initial_peers<S>(
    config: Config,
    outbound_connector: S,
    mut peerset_tx: mpsc::Sender<PeerChange>,
) -> Result<ActiveConnectionCounter, BoxError>
where
    S: Service<
            OutboundConnectorRequest,
            Response = Change<SocketAddr, peer::Client>,
            Error = BoxError,
        > + Clone,
    S::Future: Send + 'static,
{
    let initial_peers = limit_initial_peers(&config).await;

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
            async move {
                // Rate-limit the connection, sleeping for an interval according
                // to its index in the list.
                sleep(constants::MIN_PEER_CONNECTION_INTERVAL.saturating_mul(i as u32)).await;
                outbound_connector
                    .oneshot(req)
                    .map_err(move |e| (addr, e))
                    .await
            }
        })
        .collect();

    while let Some(handshake_result) = handshakes.next().await {
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
/// The result is randomly chosen entries from the provided set of addresses.
async fn limit_initial_peers(config: &Config) -> HashSet<SocketAddr> {
    let initial_peers = config.initial_peers().await;
    let initial_peer_count = initial_peers.len();

    // Limit the number of initial peers to `config.peerset_initial_target_size`
    if initial_peer_count > config.peerset_initial_target_size {
        info!(
            "Limiting the initial peers list from {} to {}",
            initial_peer_count, config.peerset_initial_target_size
        );
    }

    let initial_peers_vect: Vec<SocketAddr> = initial_peers.iter().copied().collect();

    // TODO: add unused peers to the AddressBook (#2931)
    //       https://docs.rs/rand/0.8.4/rand/seq/trait.SliceRandom.html#tymethod.partial_shuffle
    initial_peers_vect
        .choose_multiple(&mut rand::thread_rng(), config.peerset_initial_target_size)
        .copied()
        .collect()
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
async fn open_listener(config: &Config) -> (TcpListener, SocketAddr) {
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
            "Opening Zcash network protocol listener {:?} failed: {:?}. \
             Hint: Check if another zebrad or zcashd process is running. \
             Try changing the network listen_addr in the Zebra config.",
            config.listen_addr, e,
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
    peerset_tx: mpsc::Sender<PeerChange>,
) -> Result<(), BoxError>
where
    S: Service<peer::HandshakeRequest, Response = peer::Client, Error = BoxError> + Clone,
    S::Future: Send + 'static,
{
    let mut active_inbound_connections = ActiveConnectionCounter::new_counter();

    loop {
        let inbound_result = listener.accept().await;
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
            handshaker.ready_and().await?;
            // TODO: distinguish between proxied listeners and direct listeners
            let handshaker_span = info_span!("listen_handshaker", peer = ?connected_addr);

            // Construct a handshake future but do not drive it yet....
            let handshake = handshaker.call(HandshakeRequest {
                tcp_stream,
                connected_addr,
                connection_tracker,
            });
            // ... instead, spawn a new task to handle this connection
            {
                let mut peerset_tx = peerset_tx.clone();
                tokio::spawn(
                    async move {
                        if let Ok(client) = handshake.await {
                            let _ = peerset_tx.send(Ok(Change::Insert(addr, client))).await;
                        }
                    }
                    .instrument(handshaker_span),
                );
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
        peer_set_change: Change<SocketAddr, peer::Client>,
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
#[instrument(skip(
    config,
    demand_tx,
    demand_rx,
    candidates,
    outbound_connector,
    peerset_tx,
    active_outbound_connections,
))]
async fn crawl_and_dial<C, S>(
    config: Config,
    mut demand_tx: mpsc::Sender<MorePeers>,
    mut demand_rx: mpsc::Receiver<MorePeers>,
    mut candidates: CandidateSet<S>,
    outbound_connector: C,
    mut peerset_tx: mpsc::Sender<PeerChange>,
    mut active_outbound_connections: ActiveConnectionCounter,
) -> Result<(), BoxError>
where
    C: Service<
            OutboundConnectorRequest,
            Response = Change<SocketAddr, peer::Client>,
            Error = BoxError,
        > + Clone
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

    let mut handshakes = FuturesUnordered::new();
    // <FuturesUnordered as Stream> returns None when empty.
    // Keeping an unresolved future in the pool means the stream
    // never terminates.
    // We could use StreamExt::select_next_some and StreamExt::fuse, but `fuse`
    // prevents us from adding items to the stream and checking its length.
    handshakes.push(future::pending().boxed());

    let mut crawl_timer =
        tokio::time::interval(config.crawl_new_peer_interval).map(|tick| TimerCrawl { tick });

    loop {
        metrics::gauge!(
            "crawler.in_flight_handshakes",
            handshakes
                .len()
                .checked_sub(1)
                .expect("the pool always contains an unresolved future") as f64
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
                continue;
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
                        panic!("panic during handshaking with {:?}: {:?} ", candidate, e);
                    }
                })
                .instrument(Span::current());
                handshakes.push(Box::pin(hs_join));
            }
            DemandCrawl => {
                debug!("demand for peers but no available candidates");
                // update has timeouts, and briefly holds the address book
                // lock, so it shouldn't hang
                //
                // TODO: refactor candidates into a buffered service, so we can
                //       spawn independent tasks to avoid deadlocks
                candidates.update().await?;
                // Try to connect to a new peer.
                let _ = demand_tx.try_send(MorePeers);
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
            HandshakeConnected { peer_set_change } => {
                if let Change::Insert(ref addr, _) = peer_set_change {
                    debug!(candidate.addr = ?addr, "successfully dialed new peer");
                } else {
                    unreachable!("unexpected handshake result: all changes should be Insert");
                }
                // successes are handled by an independent task, so they
                // shouldn't hang
                peerset_tx.send(Ok(peer_set_change)).await?;
            }
            HandshakeFailed { failed_addr } => {
                // The connection was never opened, or it failed the handshake and was dropped.

                debug!(?failed_addr.addr, "marking candidate as failed");
                candidates.report_failed(&failed_addr);
                // The demand signal that was taken out of the queue
                // to attempt to connect to the failed candidate never
                // turned into a connection, so add it back:
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
    C: Service<
            OutboundConnectorRequest,
            Response = Change<SocketAddr, peer::Client>,
            Error = BoxError,
        > + Clone
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
        .ready_and()
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

impl From<Result<Change<SocketAddr, peer::Client>, (MetaAddr, BoxError)>> for CrawlerAction {
    fn from(dial_result: Result<Change<SocketAddr, peer::Client>, (MetaAddr, BoxError)>) -> Self {
        use CrawlerAction::*;
        match dial_result {
            Ok(peer_set_change) => HandshakeConnected { peer_set_change },
            Err((candidate, e)) => {
                debug!(?candidate.addr, ?e, "failed to connect to candidate");
                HandshakeFailed {
                    failed_addr: candidate,
                }
            }
        }
    }
}
