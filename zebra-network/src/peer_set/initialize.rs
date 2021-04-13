//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use futures::{
    channel::mpsc,
    future::{self, FutureExt},
    sink::SinkExt,
    stream::{FuturesUnordered, StreamExt},
    TryFutureExt,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::broadcast,
    time::Instant,
};
use tower::{
    buffer::Buffer, discover::Change, layer::Layer, load::peak_ewma::PeakEwmaDiscover,
    util::BoxService, Service, ServiceExt,
};
use tracing::Span;
use tracing_futures::Instrument;

use crate::{
    constants, meta_addr::MetaAddr, peer, timestamp_collector::TimestampCollector, AddressBook,
    BoxError, Config, Request, Response,
};

use zebra_chain::parameters::Network;

use super::CandidateSet;
use super::PeerSet;
use peer::Client;

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
    Arc<Mutex<AddressBook>>,
)
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let (address_book, timestamp_collector) = TimestampCollector::spawn();
    let (inv_sender, inv_receiver) = broadcast::channel(100);

    // Construct services that handle inbound handshakes and perform outbound
    // handshakes. These use the same handshake service internally to detect
    // self-connection attempts. Both are decorated with a tower TimeoutLayer to
    // enforce timeouts as specified in the Config.
    let (listener, connector) = {
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

    // 1. Incoming peer connections, via a listener.

    // Warn if we're configured using the wrong network port.
    // TODO: use the right port if the port is unspecified
    //       split the address and port configs?
    let (wrong_net, wrong_net_port) = match config.network {
        Network::Mainnet => (Network::Testnet, 18233),
        Network::Testnet => (Network::Mainnet, 8233),
    };
    if config.listen_addr.port() == wrong_net_port {
        warn!(
            "We are configured with port {} for {:?}, but that port is the default port for {:?}",
            config.listen_addr.port(),
            config.network,
            wrong_net
        );
    }

    let listen_guard = tokio::spawn(
        listen(config.listen_addr, listener, peerset_tx.clone()).instrument(Span::current()),
    );

    // 2. Initial peers, specified in the config.
    let initial_peers_fut = {
        let config = config.clone();
        let connector = connector.clone();
        let peerset_tx = peerset_tx.clone();
        async move {
            let initial_peers = config.initial_peers().await;
            // Connect the tx end to the 3 peer sources:
            add_initial_peers(initial_peers, connector, peerset_tx).await
        }
        .boxed()
    };

    let add_guard = tokio::spawn(initial_peers_fut.instrument(Span::current()));

    // 3. Outgoing peers we connect to in response to load.
    let mut candidates = CandidateSet::new(address_book.clone(), peer_set.clone());

    // We need to await candidates.update() here, because zcashd only sends one
    // `addr` message per connection, and if we only have one initial peer we
    // need to ensure that its `addr` message is used by the crawler.
    // XXX this should go in CandidateSet::new, but we need init() -> Result<_,_>

    info!("Sending initial request for peers");
    let _ = candidates.update().await;

    for _ in 0..config.peerset_initial_target_size {
        let _ = demand_tx.try_send(());
    }

    let crawl_guard = tokio::spawn(
        crawl_and_dial(
            config.crawl_new_peer_interval,
            demand_tx,
            demand_rx,
            candidates,
            connector,
            peerset_tx,
        )
        .instrument(Span::current()),
    );

    handle_tx
        .send(vec![add_guard, listen_guard, crawl_guard])
        .unwrap();

    (peer_set, address_book)
}

/// Use the provided `handshaker` to connect to `initial_peers`, then send
/// the results over `tx`.
#[instrument(skip(initial_peers, connector, tx))]
async fn add_initial_peers<S>(
    initial_peers: std::collections::HashSet<SocketAddr>,
    connector: S,
    mut tx: mpsc::Sender<PeerChange>,
) -> Result<(), BoxError>
where
    S: Service<SocketAddr, Response = Change<SocketAddr, peer::Client>, Error = BoxError> + Clone,
    S::Future: Send + 'static,
{
    info!(?initial_peers, "connecting to initial peer set");
    // ## Correctness:
    //
    // Each `CallAll` can hold one `Buffer` or `Batch` reservation for
    // an indefinite period. We can use `CallAllUnordered` without filling
    // the underlying `Inbound` buffer, because we immediately drive this
    // single `CallAll` to completion, and handshakes have a short timeout.
    use tower::util::CallAllUnordered;
    let addr_stream = futures::stream::iter(initial_peers.into_iter());
    let mut handshakes = CallAllUnordered::new(connector, addr_stream);

    while let Some(handshake_result) = handshakes.next().await {
        // this is verbose, but it's better than just hanging with no output
        if let Err(ref e) = handshake_result {
            info!(?e, "an initial peer connection failed");
        }
        tx.send(handshake_result).await?;
    }

    Ok(())
}

/// Bind to `addr`, listen for peers using `handshaker`, then send the
/// results over `tx`.
#[instrument(skip(tx, handshaker))]
async fn listen<S>(
    addr: SocketAddr,
    mut handshaker: S,
    tx: mpsc::Sender<PeerChange>,
) -> Result<(), BoxError>
where
    S: Service<(TcpStream, SocketAddr), Response = peer::Client, Error = BoxError> + Clone,
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
            debug!(?addr, "got incoming connection");
            handshaker.ready_and().await?;
            // Construct a handshake future but do not drive it yet....
            let handshake = handshaker.call((tcp_stream, addr));
            // ... instead, spawn a new task to handle this connection
            let mut tx2 = tx.clone();
            tokio::spawn(async move {
                if let Ok(client) = handshake.await {
                    let _ = tx2.send(Ok(Change::Insert(addr, client))).await;
                }
            });
        }
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
        peer_set_change: Change<SocketAddr, Client>,
    },
    /// Handle a handshake failure to `failed_addr`.
    HandshakeFailed { failed_addr: MetaAddr },
}

/// Given a channel `demand_rx` that signals a need for new peers, try to find
/// and connect to new peers, and send the resulting `peer::Client`s through the
/// `success_tx` channel.
///
/// Crawl for new peers every `crawl_new_peer_interval`, and whenever there is
/// demand, but no new peers in `candidates`. After crawling, try to connect to
/// one new peer using `connector`.
///
/// If a handshake fails, restore the unused demand signal by sending it to
/// `demand_tx`.
///
/// The crawler terminates when `candidates.update()` or `success_tx` returns a
/// permanent internal error. Transient errors and individual peer errors should
/// be handled within the crawler.
#[instrument(skip(demand_tx, demand_rx, candidates, connector, success_tx))]
async fn crawl_and_dial<C, S>(
    crawl_new_peer_interval: std::time::Duration,
    mut demand_tx: mpsc::Sender<()>,
    mut demand_rx: mpsc::Receiver<()>,
    mut candidates: CandidateSet<S>,
    connector: C,
    mut success_tx: mpsc::Sender<PeerChange>,
) -> Result<(), BoxError>
where
    C: Service<SocketAddr, Response = Change<SocketAddr, peer::Client>, Error = BoxError>
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

    let mut handshakes = FuturesUnordered::new();
    // <FuturesUnordered as Stream> returns None when empty.
    // Keeping an unresolved future in the pool means the stream
    // never terminates.
    // We could use StreamExt::select_next_some and StreamExt::fuse, but `fuse`
    // prevents us from adding items to the stream and checking its length.
    handshakes.push(future::pending().boxed());

    let mut crawl_timer =
        tokio::time::interval(crawl_new_peer_interval).map(|tick| TimerCrawl { tick });

    loop {
        metrics::gauge!(
            "crawler.in_flight_handshakes",
            handshakes
                .len()
                .checked_sub(1)
                .expect("the pool always contains an unresolved future") as f64
        );

        let crawler_action = tokio::select! {
            a = handshakes.next() => a.expect("handshakes never terminates, because it contains a future that never resolves"),
            a = crawl_timer.next() => a.expect("timers never terminate"),
            // turn the demand into an action, based on the crawler's current state
            _ = demand_rx.next() => {
                if handshakes.len() > 50 {
                    // Too many pending handshakes already
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
                trace!("too many in-flight handshakes, dropping demand signal");
                continue;
            }
            DemandHandshake { candidate } => {
                // spawn each handshake into an independent task, so it can make
                // progress independently of the crawls
                let hs_join =
                    tokio::spawn(dial(candidate, connector.clone())).map(move |res| match res {
                        Ok(crawler_action) => crawler_action,
                        Err(e) => {
                            panic!("panic during handshaking with {:?}: {:?} ", candidate, e);
                        }
                    });
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
                let _ = demand_tx.try_send(());
            }
            TimerCrawl { tick } => {
                debug!(
                    ?tick,
                    "crawling for more peers in response to the crawl timer"
                );
                // TODO: spawn independent tasks to avoid deadlocks
                candidates.update().await?;
                // Try to connect to a new peer.
                let _ = demand_tx.try_send(());
            }
            HandshakeConnected { peer_set_change } => {
                if let Change::Insert(ref addr, _) = peer_set_change {
                    debug!(candidate.addr = ?addr, "successfully dialed new peer");
                } else {
                    unreachable!("unexpected handshake result: all changes should be Insert");
                }
                // successes are handled by an independent task, so they
                // shouldn't hang
                success_tx.send(Ok(peer_set_change)).await?;
            }
            HandshakeFailed { failed_addr } => {
                debug!(?failed_addr.addr, "marking candidate as failed");
                candidates.report_failed(&failed_addr);
                // The demand signal that was taken out of the queue
                // to attempt to connect to the failed candidate never
                // turned into a connection, so add it back:
                let _ = demand_tx.try_send(());
            }
        }
    }
}

/// Try to connect to `candidate` using `connector`.
///
/// Returns a `HandshakeConnected` action on success, and a
/// `HandshakeFailed` action on error.
#[instrument(skip(connector,))]
async fn dial<C>(candidate: MetaAddr, mut connector: C) -> CrawlerAction
where
    C: Service<SocketAddr, Response = Change<SocketAddr, peer::Client>, Error = BoxError>
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
    let connector = connector.ready_and().await.expect("connector never errors");

    // the handshake has timeouts, so it shouldn't hang
    connector
        .call(candidate.addr)
        .map_err(|e| (candidate, e))
        .map(Into::into)
        .await
}

impl From<Result<Change<SocketAddr, Client>, (MetaAddr, BoxError)>> for CrawlerAction {
    fn from(dial_result: Result<Change<SocketAddr, Client>, (MetaAddr, BoxError)>) -> Self {
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
