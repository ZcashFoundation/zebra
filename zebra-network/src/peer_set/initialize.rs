//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use futures::{
    channel::mpsc,
    future::{self, Future, FutureExt},
    sink::SinkExt,
    stream::{FuturesUnordered, StreamExt},
};
use tokio::net::{TcpListener, TcpStream};
use tower::{
    buffer::Buffer,
    discover::{Change, ServiceStream},
    layer::Layer,
    Service, ServiceExt,
};
use tower_load::{peak_ewma::PeakEwmaDiscover, NoInstrument};

use crate::{
    peer, timestamp_collector::TimestampCollector, AddressBook, BoxedStdError, Config, Request,
    Response,
};

use super::CandidateSet;
use super::PeerSet;

type PeerChange = Result<Change<SocketAddr, peer::Client>, BoxedStdError>;

/// Initialize a peer set with the given `config`, forwarding peer requests to the `inbound_service`.
pub async fn init<S>(
    config: Config,
    inbound_service: S,
) -> (
    impl Service<
            Request,
            Response = Response,
            Error = BoxedStdError,
            Future = impl Future<Output = Result<Response, BoxedStdError>> + Send,
        > + Send
        + Clone
        + 'static,
    Arc<Mutex<AddressBook>>,
)
where
    S: Service<Request, Response = Response, Error = BoxedStdError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let (address_book, timestamp_collector) = TimestampCollector::spawn();

    // Construct services that handle inbound handshakes and perform outbound
    // handshakes. These use the same handshake service internally to detect
    // self-connection attempts. Both are decorated with a tower TimeoutLayer to
    // enforce timeouts as specified in the Config.
    let (listener, connector) = {
        use tower::timeout::TimeoutLayer;
        let hs_timeout = TimeoutLayer::new(config.handshake_timeout);
        let hs = peer::Handshake::new(config.clone(), inbound_service, timestamp_collector);
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
            ServiceStream::new(
                // ServiceStream interprets an error as stream termination,
                // so discard any errored connections...
                peerset_rx.filter(|result| future::ready(result.is_ok())),
            ),
            config.ewma_default_rtt,
            config.ewma_decay_time,
            NoInstrument,
        ),
        demand_tx.clone(),
        handle_rx,
    );
    let peer_set = Buffer::new(peer_set, config.peerset_request_buffer_size);

    // Connect the tx end to the 3 peer sources:

    // 1. Initial peers, specified in the config.
    let add_guard = tokio::spawn(add_initial_peers(
        config.initial_peers(),
        connector.clone(),
        peerset_tx.clone(),
    ));

    // 2. Incoming peer connections, via a listener.
    let listen_guard = tokio::spawn(listen(config.listen_addr, listener, peerset_tx.clone()));

    // 3. Outgoing peers we connect to in response to load.
    let mut candidates = CandidateSet::new(address_book.clone(), peer_set.clone());

    // We need to await candidates.update() here, because Zcashd only sends one
    // `addr` message per connection, and if we only have one initial peer we
    // need to ensure that its `addr` message is used by the crawler.
    // XXX this should go in CandidateSet::new, but we need init() -> Result<_,_>
    let _ = candidates.update().await;

    info!("Sending initial request for peers");

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
) -> Result<(), BoxedStdError>
where
    S: Service<SocketAddr, Response = Change<SocketAddr, peer::Client>, Error = BoxedStdError>
        + Clone,
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
) -> Result<(), BoxedStdError>
where
    S: Service<(TcpStream, SocketAddr), Response = peer::Client, Error = BoxedStdError> + Clone,
    S::Future: Send + 'static,
{
    let mut listener = TcpListener::bind(addr).await?;
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
) -> Result<(), BoxedStdError>
where
    C: Service<SocketAddr, Response = Change<SocketAddr, peer::Client>, Error = BoxedStdError>
        + Clone,
    C::Future: Send + 'static,
    S: Service<Request, Response = Response, Error = BoxedStdError>,
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
        metrics::gauge!("crawler.in_flight_handshakes", handshakes.len() as i64 - 1);
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
                            .map_err(move |_| candidate)
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
                // in fact all changes are Insert so this branch is always taken
                if let Change::Insert(ref addr, _) = change {
                    debug!(candidate.addr = ?addr, "successfully dialed new peer");
                }
                success_tx.send(Ok(change)).await?;
            }
            Right((Some(Err(candidate)), _)) => {
                debug!(?candidate.addr, "failed to connect to peer");
                candidates.report_failed(candidate);
                // The demand signal that was taken out of the queue
                // to attempt to connect to the failed candidate never
                // turned into a connection, so add it back:
                let _ = demand_tx.try_send(());
            }
            // If we don't match one of these patterns, shutdown.
            _ => break,
        }
    }
    Ok(())
}
