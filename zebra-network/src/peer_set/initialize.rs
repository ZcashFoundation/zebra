//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

use std::{cmp::max, net::SocketAddr, sync::Arc};

use futures::{
    channel::mpsc,
    future::{self, FutureExt},
    sink::SinkExt,
    stream::{FuturesUnordered, StreamExt},
};
use tokio::{
    net::TcpListener,
    sync::{broadcast, oneshot},
    time::Instant,
};
use tower::{
    buffer::Buffer, discover::Change, layer::Layer, load::peak_ewma::PeakEwmaDiscover,
    util::BoxService, Service, ServiceExt,
};
use tracing::Span;
use tracing_futures::Instrument;

use crate::{
    constants,
    meta_addr::{MetaAddr, MetaAddrChange},
    peer,
    timestamp_collector::TimestampCollector,
    types::PeerServices,
    AddressBook, BoxError, Config, Request, Response,
};

use zebra_chain::parameters::Network;

use super::CandidateSet;
use super::PeerSet;

use CrawlerAction::*;

type PeerChange = Result<Change<SocketAddr, peer::Client>, BoxError>;

/// Initialize a peer set.
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
/// In addition to returning a service for outbound requests, this method
/// returns a shared [`AddressBook`] updated with last-seen timestamps for
/// connected peers.
pub async fn init<S>(
    config: Config,
    inbound_service: S,
) -> (
    Buffer<BoxService<Request, Response, BoxError>, Request>,
    Arc<std::sync::Mutex<AddressBook>>,
)
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let (address_book, timestamp_collector) = TimestampCollector::spawn(config.clone());
    let (inv_sender, inv_receiver) = broadcast::channel(100);

    // Construct services that handle inbound handshakes and perform outbound
    // handshakes. These use the same handshake service internally to detect
    // self-connection attempts. Both are decorated with a tower TimeoutLayer to
    // enforce timeouts as specified in the Config.
    let (listen_handshaker, outbound_connector) = {
        use tower::timeout::TimeoutLayer;
        let hs_timeout = TimeoutLayer::new(constants::HANDSHAKE_TIMEOUT);
        let hs = peer::Handshake::builder()
            .with_config(config.clone())
            .with_inbound_service(inbound_service)
            .with_inventory_collector(inv_sender)
            .with_timestamp_collector(timestamp_collector.clone())
            .with_advertised_services(PeerServices::NODE_NETWORK)
            .with_user_agent(crate::constants::USER_AGENT.to_string())
            .want_transactions(true)
            .finish()
            .expect("configured all required parameters");
        (
            hs_timeout.layer(hs.clone()),
            hs_timeout.layer(peer::Connector::new(hs)),
        )
    };

    // Create an mpsc channel for peer changes, with a generous buffer.
    let (peerset_tx, peerset_rx) = mpsc::channel::<PeerChange>(100);
    // Create an mpsc channel for peerset demand signaling.
    let (mut demand_tx, demand_rx) = mpsc::channel::<()>(100);
    let (handle_tx, handle_rx) = oneshot::channel();

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

    // Connect the tx end to the 3 peer sources:
    //
    // 1. Incoming peer connections, via a listener.

    // Warn if we're configured using the wrong network port.
    use Network::*;
    let wrong_net = match config.network {
        Mainnet => Testnet,
        Testnet => Mainnet,
    };
    if config.listen_addr.port() == wrong_net.default_port() {
        warn!(
            "We are configured with port {} for {:?}, but that port is the default port for {:?}",
            config.listen_addr.port(),
            config.network,
            wrong_net
        );
    }

    let listen_guard = tokio::spawn(
        listen(config.listen_addr, listen_handshaker, peerset_tx.clone())
            .instrument(Span::current()),
    );

    // 2. Initial peer connections, as specified in the config.
    let initial_peers_fut = {
        let config = config.clone();
        async move {
            let initial_peers = config.initial_peers().await;
            add_initial_peers(initial_peers, timestamp_collector).await
        }
        .boxed()
    };

    let initial_peers_guard = tokio::spawn(initial_peers_fut.instrument(Span::current()));

    // 3. Outgoing peers we connect to in response to load.
    let candidate_set = CandidateSet::new(address_book.clone(), peer_set.clone());

    for _ in 0..config.peerset_initial_target_size {
        let _ = demand_tx.try_send(());
    }

    // Wait for the initial peers to be sent to the address book.
    // This a workaround for zcashd's per-connection `addr` message rate limit.
    info!("waiting for initial seed peer info");
    initial_peers_guard
        .await
        .expect("unexpected panic in initial peers task")
        .expect("unexpected error sending initial peers");

    let (initial_crawl_tx, initial_crawl_rx) = oneshot::channel();
    let crawl_guard = tokio::spawn(
        crawl_and_dial(
            config.crawl_new_peer_interval,
            config.peerset_initial_target_size,
            demand_tx,
            demand_rx,
            candidate_set,
            outbound_connector,
            peerset_tx,
            initial_crawl_tx,
        )
        .instrument(Span::current()),
    );

    // Wait for the initial crawls from the crawler.
    // This a workaround for zcashd's per-connection `addr` message rate limit.
    info!("waiting for initial seed peer getaddr crawls");
    initial_crawl_rx
        .await
        .expect("initial crawl oneshot unexpectedly dropped");

    handle_tx.send(vec![listen_guard, crawl_guard]).unwrap();

    (peer_set, address_book)
}

/// Use the provided `outbound_connector` to connect to `initial_peers`, then
/// send the results over `peerset_tx`.
///
/// Also adds those peers to the [`AddressBook`] using `timestamp_collector`,
/// and updates `success_count_tx` with the number of successful peers.
///
/// Stops trying peers once we've had `peerset_initial_target_size` successful
/// handshakes.
#[instrument(skip(
    initial_peers,
    timestamp_collector,
), fields(initial_peers_len = %initial_peers.len()))]
async fn add_initial_peers(
    initial_peers: std::collections::HashSet<SocketAddr>,
    mut timestamp_collector: mpsc::Sender<MetaAddrChange>,
) -> Result<(), BoxError> {
    info!(?initial_peers, "sending seed peers to the address book");

    // Note: these address book updates are sent to a channel, so they might be
    // applied after updates from concurrent tasks.
    let seed_changes = initial_peers.into_iter().map(MetaAddr::new_seed);
    let mut seed_changes = futures::stream::iter(seed_changes).map(Result::Ok);
    timestamp_collector.send_all(&mut seed_changes).await?;

    Ok(())
}

/// Listens for peer connections on `addr`, then sets up each connection as a
/// Zcash peer.
///
/// Uses `handshaker` to perform a Zcash network protocol handshake, and sends
/// the [`peer::Client`] result over `peerset_tx`.
#[instrument(skip(peerset_tx, handshaker))]
async fn listen<S>(
    addr: SocketAddr,
    mut handshaker: S,
    peerset_tx: mpsc::Sender<PeerChange>,
) -> Result<(), BoxError>
where
    S: Service<peer::HandshakeRequest, Response = peer::Client, Error = BoxError> + Clone,
    S::Future: Send + 'static,
{
    info!("Trying to open Zcash protocol endpoint at {}...", addr);
    let listener_result = TcpListener::bind(addr).await;

    let listener = match listener_result {
        Ok(l) => l,
        Err(e) => panic!(
            "Opening Zcash network protocol listener {:?} failed: {:?}. \
             Hint: Check if another zebrad or zcashd process is running. \
             Try changing the network listen_addr in the Zebra config.",
            addr, e,
        ),
    };

    let local_addr = listener.local_addr()?;
    info!("Opened Zcash protocol endpoint at {}", local_addr);
    loop {
        if let Ok((tcp_stream, addr)) = listener.accept().await {
            let connected_addr = peer::ConnectedAddr::new_inbound_direct(addr);
            let accept_span = info_span!("listen_accept", peer = ?connected_addr);
            let _guard = accept_span.enter();

            debug!("got incoming connection");
            handshaker.ready_and().await?;
            // TODO: distinguish between proxied listeners and direct listeners
            let handshaker_span = info_span!("listen_handshaker", peer = ?connected_addr);
            // Construct a handshake future but do not drive it yet....
            let handshake = handshaker.call((tcp_stream, connected_addr));
            // ... instead, spawn a new task to handle this connection
            let mut peerset_tx2 = peerset_tx.clone();
            tokio::spawn(
                async move {
                    if let Ok(client) = handshake.await {
                        let _ = peerset_tx2.send(Ok(Change::Insert(addr, client))).await;
                    }
                }
                .instrument(handshaker_span),
            );
        }
    }
}

/// An action that the peer crawler can take.
enum CrawlerAction {
    /// Drop the demand signal because there are too many pending handshakes.
    DemandDrop,
    /// Initiate a handshake to the next candidate in response to demand.
    DemandHandshake,
    /// Crawl existing peers for more peers in response to demand, because there
    /// are no available candidates.
    DemandCrawl,
    /// Crawl existing peers for more peers in response to a timer `tick`.
    TimerCrawl { tick: Instant },
    /// Handle a successfully connected handshake to `addr`.
    HandshakeConnected { addr: SocketAddr },
    /// Handle a handshake failure to `failed_addr`.
    HandshakeFailed {
        failed_addr: MetaAddr,
        error: BoxError,
    },
    /// Handle a completed crawl that produced `new_candidates`, based on a
    /// `fanout_limit`.
    CrawlCompleted {
        new_candidates: usize,
        fanout_limit: usize,
    },
}

/// Given a channel `demand_rx` that signals a need for new peers, try to find
/// and connect to new peers, and send the resulting [`peer::Client`]s through the
/// `peerset_tx` channel.
///
/// Crawl for new peers every `crawl_new_peer_interval`, and whenever there is
/// demand, but no new peers in `candidate_set`. After crawling, try to connect to
/// one new peer using `outbound_connector`.
///
/// If a handshake fails, restore the unused demand signal by sending it to
/// `demand_tx`.
///
/// When the seed peer crawls are completed, notifies `initial_crawl_rx`.
///
/// The crawler terminates when [`CandidateSet::update`] or `peerset_tx` returns a
/// permanent internal error. Transient errors and individual peer errors should
/// be handled within the crawler.
#[instrument(skip(
    demand_tx,
    demand_rx,
    candidate_set,
    outbound_connector,
    peerset_tx,
    initial_crawl_tx
))]
#[allow(clippy::too_many_arguments)]
async fn crawl_and_dial<C, P>(
    crawl_new_peer_interval: std::time::Duration,
    peerset_initial_target_size: usize,
    mut demand_tx: mpsc::Sender<()>,
    mut demand_rx: mpsc::Receiver<()>,
    mut candidate_set: CandidateSet<P>,
    outbound_connector: C,
    peerset_tx: mpsc::Sender<PeerChange>,
    initial_crawl_tx: oneshot::Sender<()>,
) -> Result<(), BoxError>
where
    C: Service<SocketAddr, Response = Change<SocketAddr, peer::Client>, Error = BoxError>
        + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
    P: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    P::Future: Send + 'static,
{
    // CORRECTNESS
    //
    // To avoid hangs and starvation, the crawler must:
    // - spawn a separate task for each crawl and handshake, so they can make
    //   progress independently (and avoid deadlocking each other)
    // - use the `select!` macro for all actions, because the `select` function
    //   is biased towards its first argument if multiple futures are ready
    // - perform all `await`s in the `select!` or in spawned tasks
    // - perform all threaded mutex locks in spawned tasks
    //   TODO: replace threaded mutexes with services (#1976)

    let mut recently_live_peers = candidate_set.recently_live_peer_count().await;

    // These variables aren't updated, so drop them before the loop
    {
        let candidate_peers = candidate_set.candidate_peer_count().await;
        let recently_used_peers = candidate_set.recently_used_peer_count().await;
        info!(
            ?candidate_peers,
            ?recently_used_peers,
            ?recently_live_peers,
            "starting the peer crawler"
        );
    }

    let mut handshakes = FuturesUnordered::new();
    let mut crawls = FuturesUnordered::new();

    let crawl_start = Instant::now() + crawl_new_peer_interval;
    let mut crawl_timer = tokio::time::interval_at(crawl_start, crawl_new_peer_interval)
        .map(|tick| TimerCrawl { tick })
        .fuse();

    // Zebra has just started, and we're the only task that makes connections,
    // so the number of recently live peers must be zero here.
    //
    // TODO: replace with a live peers watch channel
    //       include inbound live peers using the peer set (#1552)
    //       remove this check when recently live peers uses the PeerSet,
    //       so we don't panic on early inbound connections
    assert_eq!(
        recently_live_peers, 0,
        "Unexpected recently live peers: the crawler should be the only task that makes outbound connections"
    );

    // Start by doing a crawl after every successful handshake.
    //
    // If we only have a small number of initial peers, we need to ensure their
    // `Addrs` responses used by the crawler. (zcashd has a per-connection
    // `GetAddr` response rate limit.)
    let mut initial_crawl_tx = Some(initial_crawl_tx);
    let mut initial_crawls_left: usize = 2;

    loop {
        // Do we need a crawl() after processing this message?
        let mut needs_crawl = false;

        metrics::gauge!("crawler.in_flight_handshakes", handshakes.len() as f64);
        metrics::gauge!("crawler.in_flight_crawls", crawls.len() as f64);
        metrics::gauge!("crawler.initial_crawls_left", initial_crawls_left as f64);

        // If multiple futures are ready, the `select!` macro chooses one at random.
        //
        // TODO: after we upgrade to tokio 1.6, use `select { biased; ... }`
        // to prioritise handshakes, then crawls, then demand if multiple futures
        // are ready.
        // (Handshakes enable crawls, and both stop when there is no demand.)
        let crawler_action = futures::select! {
            next_handshake = handshakes.select_next_some() => next_handshake,
            next_crawl = crawls.select_next_some() => next_crawl,
            next_timer = crawl_timer.select_next_some() => next_timer,
            // turn the demand into an action, based on the crawler's current state
            _ = demand_rx.next() => {
                // Too many pending handshakes and crawls already
                if handshakes.len() + crawls.len() >= peerset_initial_target_size {
                    DemandDrop
                } else  {
                    DemandHandshake
                }
            }
        };

        match crawler_action {
            DemandDrop => {
                // This is set to trace level because when the peerset is
                // congested it can generate a lot of demand signal very
                // rapidly.
                trace!("too many in-flight handshakes and crawls, dropping demand signal");
            }
            DemandHandshake => {
                // spawn each handshake into an independent task, so it can make
                // progress independently of the crawls
                let hs_join = tokio::spawn(dial(
                    candidate_set.clone(),
                    outbound_connector.clone(),
                    peerset_tx.clone(),
                ))
                .map(move |res| match res {
                    Ok(crawler_action) => crawler_action,
                    Err(e) => {
                        panic!("panic during handshake attempt, error: {:?}", e);
                    }
                })
                .instrument(Span::current());
                handshakes.push(Box::pin(hs_join));
            }
            DemandCrawl => {
                debug!("demand for peers but no available candidates");

                // Make sure we have some live peers before crawling
                recently_live_peers = candidate_set.recently_live_peer_count().await;
                needs_crawl = true;

                // Try to connect to a new peer after the crawl.
                let _ = demand_tx.try_send(());
            }
            TimerCrawl { tick } => {
                let address_metrics = candidate_set.address_metrics().await;
                info!(
                    ?address_metrics,
                    "crawling for more peers in response to the crawl timer"
                );
                debug!(?tick, "crawl timer value");

                recently_live_peers = address_metrics.recently_live;
                needs_crawl = true;

                let _ = demand_tx.try_send(());
            }
            HandshakeConnected { addr } => {
                trace!(?addr, "crawler cleared successful handshake");

                // Assume an update for the peer that just connected is waiting
                // in the channel.
                recently_live_peers = candidate_set.recently_live_peer_count().await + 1;

                // Do a crawl after the first few handshakes, to handle low peer numbers
                needs_crawl = initial_crawls_left > 0;
            }
            HandshakeFailed { failed_addr, error } => {
                trace!(?failed_addr, ?error, "crawler cleared failed handshake");
                // The demand signal that was taken out of the queue
                // to attempt to connect to the failed candidate never
                // turned into a connection, so add it back:
                let _ = demand_tx.try_send(());
            }
            CrawlCompleted {
                new_candidates,
                fanout_limit,
            } => {
                if initial_crawls_left > 0 {
                    info!(?new_candidates, ?fanout_limit, "crawler completed crawl");
                } else {
                    debug!(?new_candidates, ?fanout_limit, "crawler completed crawl");
                }

                if new_candidates > 0 {
                    if let Some(initial_crawl_tx) = initial_crawl_tx.take() {
                        initial_crawls_left = 0;
                        // We don't care if it has been dropped
                        let _ = initial_crawl_tx.send(());
                    }
                }
            }
        }

        // Only run one crawl at a time, to avoid peer set congestion
        if needs_crawl && crawls.is_empty() && recently_live_peers > 0 {
            // limit our fanout, because zcashd has a response rate-limit
            let fanout_limit = max(
                recently_live_peers / constants::GET_ADDR_FANOUT_LIVE_PEERS_DIVISOR,
                1,
            );

            // spawn each crawl into an independent task, so it can make
            // progress in parallel with the handshakes
            let crawl_join = tokio::spawn(crawl(candidate_set.clone(), fanout_limit))
                .map(move |res| match res {
                    Ok(crawler_action) => crawler_action,
                    Err(e) => {
                        panic!(
                            "panic during crawl, recently live peers: {:?}, error: {:?}",
                            recently_live_peers, e
                        );
                    }
                })
                .instrument(Span::current());
            crawls.push(Box::pin(crawl_join));

            initial_crawls_left = initial_crawls_left.saturating_sub(1);
            if initial_crawls_left == 0 {
                if let Some(initial_crawl_tx) = initial_crawl_tx.take() {
                    let _ = initial_crawl_tx.send(());
                }
            }
        }
    }
}

/// Try to update `candidate_set` with a `fanout_limit`.
///
/// Returns an [`UpdateCompleted`] action containing the number of new candidate
/// peers.
#[instrument(skip(candidate_set))]
async fn crawl<P>(mut candidate_set: CandidateSet<P>, fanout_limit: usize) -> CrawlerAction
where
    P: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    P::Future: Send + 'static,
{
    // CORRECTNESS
    //
    // To avoid hangs, the crawler must only await:
    // - functions that return immediately, or
    // - functions that have a reasonable timeout

    debug!(?fanout_limit, "attempting peer set address crawl");

    // CandidateSet::update has timeouts, so it shouldn't hang
    //
    // TODO: refactor CandidateSet into a buffered service, to avoid
    // AddressBook threaded mutex deadlocks and contention (#1976)
    let new_candidates = candidate_set.update(fanout_limit).await;

    CrawlCompleted {
        new_candidates,
        fanout_limit,
    }
}

/// Try to connect to a candidate from `candidate_set` using
/// `outbound_connector`, and send successful handshakes to `peerset_tx`.
///
/// Returns:
/// - [`DemandCrawl`] if there are no candidates for outbound connection,
/// - [`HandshakeConnected`] for successful handshakes, and
/// - [`HandshakeFailed`] for failed handshakes.
#[instrument(skip(candidate_set, outbound_connector, peerset_tx))]
async fn dial<C, P>(
    mut candidate_set: CandidateSet<P>,
    mut outbound_connector: C,
    mut peerset_tx: mpsc::Sender<PeerChange>,
) -> CrawlerAction
where
    C: Service<SocketAddr, Response = Change<SocketAddr, peer::Client>, Error = BoxError>
        + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
    P: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    P::Future: Send + 'static,
{
    // CORRECTNESS
    //
    // To avoid hangs, the dialer must only await:
    // - functions that return immediately, or
    // - functions that have a reasonable timeout

    debug!("trying to get the next candidate address");

    // candidate_set.next has a short delay, and briefly holds the address
    // book lock, so it shouldn't hang
    let candidate = match candidate_set.next().await {
        Some(candidate) => candidate,
        None => return DemandCrawl,
    };

    debug!(?candidate.addr, "attempting outbound connection in response to demand");

    // the connector is always ready, so this can't hang
    let outbound_connector = outbound_connector
        .ready_and()
        .await
        .expect("outbound connector never errors");

    // the handshake has timeouts, so it shouldn't hang
    let handshake_result = outbound_connector.call(candidate.addr).await;

    match handshake_result {
        Ok(peer_set_change) => {
            if let Change::Insert(ref addr, _) = peer_set_change {
                debug!(?addr, "successfully dialed new peer");
            } else {
                unreachable!("unexpected handshake result: all changes should be Insert");
            }

            // the peer set is handled by an independent task, so this send
            // shouldn't hang
            peerset_tx
                .send(Ok(peer_set_change))
                .await
                .expect("peer set never errors");

            HandshakeConnected {
                addr: candidate.addr,
            }
        }
        Err(error) => {
            debug!(addr = ?candidate.addr, ?error, "marking candidate as failed");
            // The handshaker sends its errors to the timestamp collector.
            // But we also need to record errors in the connector.
            // (And any other layers in the service stack.)
            //
            // TODO: replace this with the timestamp collector?
            candidate_set.report_failed(candidate.addr).await;

            HandshakeFailed {
                failed_addr: candidate,
                error,
            }
        }
    }
}
