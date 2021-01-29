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
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::broadcast,
};
use tower::{
    buffer::Buffer, discover::Change, layer::Layer, load::peak_ewma::PeakEwmaDiscover,
    util::BoxService, Service, ServiceExt,
};

use crate::{
    constants, peer, timestamp_collector::TimestampCollector, AddressBook, BoxError, Config,
    Request, Response,
};

use zebra_chain::parameters::Network;

use super::CandidateSet;
use super::PeerSet;

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

    let listen_guard = tokio::spawn(listen(config.listen_addr, listener, peerset_tx.clone()));

    let initial_peers_fut = {
        let initial_peers = config.initial_peers();
        let connector = connector.clone();
        let tx = peerset_tx.clone();

        // Connect the tx end to the 3 peer sources:
        add_initial_peers(initial_peers, connector, tx)
    };

    // 2. Initial peers, specified in the config.
    let add_guard = tokio::spawn(initial_peers_fut);

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

    let crawl_guard = tokio::spawn(crawl_and_dial(
        config.new_peer_interval,
        demand_tx,
        demand_rx,
        candidates,
        connector,
        peerset_tx,
    ));

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
    info!(?initial_peers, "Connecting to initial peer set");
    use tower::util::CallAllUnordered;
    let addr_stream = futures::stream::iter(initial_peers.into_iter());
    let mut handshakes = CallAllUnordered::new(connector, addr_stream);

    while let Some(handshake_result) = handshakes.next().await {
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

/// Given a channel that signals a need for new peers, try to connect to a peer
/// and send the resulting `peer::Client` through a channel.
#[instrument(skip(
    new_peer_interval,
    demand_tx,
    demand_rx,
    candidates,
    connector,
    success_tx
))]
async fn crawl_and_dial<C, S>(
    new_peer_interval: std::time::Duration,
    mut demand_tx: mpsc::Sender<()>,
    mut demand_rx: mpsc::Receiver<()>,
    mut candidates: CandidateSet<S>,
    mut connector: C,
    mut success_tx: mpsc::Sender<PeerChange>,
) -> Result<(), BoxError>
where
    C: Service<SocketAddr, Response = Change<SocketAddr, peer::Client>, Error = BoxError> + Clone,
    C::Future: Send + 'static,
    S: Service<Request, Response = Response, Error = BoxError>,
    S::Future: Send + 'static,
{
    use futures::{
        future::{
            select,
            Either::{Left, Right},
        },
        TryFutureExt,
    };

    let mut handshakes = FuturesUnordered::new();
    // <FuturesUnordered as Stream> returns None when empty.
    // Keeping an unresolved future in the pool means the stream
    // never terminates.
    handshakes.push(future::pending().boxed());

    let mut crawl_timer = tokio::time::interval(new_peer_interval);

    loop {
        metrics::gauge!(
            "crawler.in_flight_handshakes",
            handshakes
                .len()
                .checked_sub(1)
                .expect("the pool always contains an unresolved future") as f64
        );
        // This is a little awkward because there's no select3.
        match select(
            select(demand_rx.next(), crawl_timer.next()),
            handshakes.next(),
        )
        .await
        {
            Left((Left((Some(_demand), _)), _)) => {
                if handshakes.len() > 50 {
                    // This is set to trace level because when the peerset is
                    // congested it can generate a lot of demand signal very rapidly.
                    trace!("too many in-flight handshakes, dropping demand signal");
                    continue;
                }
                if let Some(candidate) = candidates.next() {
                    debug!(?candidate.addr, "attempting outbound connection in response to demand");
                    connector.ready_and().await?;
                    handshakes.push(
                        connector
                            .call(candidate.addr)
                            .map_err(move |e| {
                                debug!(?candidate.addr, ?e, "failed to connect to candidate");
                                candidate
                            })
                            .boxed(),
                    );
                } else {
                    debug!("demand for peers but no available candidates");
                    candidates.update().await?;
                    // Try to connect to a new peer.
                    let _ = demand_tx.try_send(());
                }
            }
            // did a drill sergeant write this? no there's just no Either3
            Left((Right((Some(_timer), _)), _)) => {
                debug!("crawling for more peers");
                candidates.update().await?;
                // Try to connect to a new peer.
                let _ = demand_tx.try_send(());
            }
            Right((Some(Ok(change)), _)) => {
                if let Change::Insert(ref addr, _) = change {
                    debug!(candidate.addr = ?addr, "successfully dialed new peer");
                } else {
                    unreachable!("unexpected handshake result: all changes should be Insert");
                }
                success_tx.send(Ok(change)).await?;
            }
            Right((Some(Err(candidate)), _)) => {
                debug!(?candidate.addr, "marking candidate as failed");
                candidates.report_failed(candidate);
                // The demand signal that was taken out of the queue
                // to attempt to connect to the failed candidate never
                // turned into a connection, so add it back:
                let _ = demand_tx.try_send(());
            }
            // We don't expect to see these patterns during normal operation
            Left((Left((None, _)), _)) => {
                unreachable!("demand_rx never fails, because we hold demand_tx");
            }
            Left((Right((None, _)), _)) => {
                unreachable!("crawl_timer never terminates");
            }
            Right((None, _)) => {
                unreachable!(
                    "handshakes never terminates, because it contains a future that never resolves"
                );
            }
        }
    }
}
