//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

// XXX these imports should go in a peer_set::initialize submodule

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
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
    peer::{PeerClient, PeerConnector, PeerHandshake},
    timestamp_collector::TimestampCollector,
    AddressBook, BoxedStdError, Config, Request, Response,
};

mod candidate_set;
mod set;
mod unready_service;

use candidate_set::CandidateSet;
use set::PeerSet;

type PeerChange = Result<Change<SocketAddr, PeerClient>, BoxedStdError>;

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
        let hs = PeerHandshake::new(config.clone(), inbound_service, timestamp_collector);
        (
            hs_timeout.layer(hs.clone()),
            hs_timeout.layer(PeerConnector::new(hs)),
        )
    };

    // Create an mpsc channel for peer changes, with a generous buffer.
    let (peerset_tx, peerset_rx) = mpsc::channel::<PeerChange>(100);
    // Create an mpsc channel for peerset demand signaling.
    let (demand_tx, demand_rx) = mpsc::channel::<()>(100);

    // Connect the rx end to a PeerSet, wrapping new peers in load instruments.
    let peer_set = Buffer::new(
        PeerSet::new(
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
            demand_tx,
        ),
        config.peerset_request_buffer_size,
    );

    // Connect the tx end to the 3 peer sources:

    // 1. Initial peers, specified in the config.
    tokio::spawn(add_initial_peers(
        config.initial_peers(),
        connector.clone(),
        peerset_tx.clone(),
    ));

    // 2. Incoming peer connections, via a listener.
    tokio::spawn(
        listen(config.listen_addr, listener, peerset_tx.clone()).map(|result| {
            if let Err(e) = result {
                error!(%e);
            }
        }),
    );

    // 3. Outgoing peers we connect to in response to load.

    let mut candidates = CandidateSet::new(address_book.clone(), peer_set.clone());

    // We need to await candidates.update() here, because Zcashd only sends one
    // `addr` message per connection, and if we only have one initial peer we
    // need to ensure that its `addr` message is used by the crawler.
    // XXX this should go in CandidateSet::new, but we need init() -> Result<_,_>
    let _ = candidates.update().await;

    info!("Sending initial request for peers");
    tokio::spawn(
        crawl_and_dial(
            config.new_peer_interval,
            demand_rx,
            candidates,
            connector,
            peerset_tx,
        )
        .map(|result| {
            if let Err(e) = result {
                error!(%e);
            }
        }),
    );

    (peer_set, address_book)
}

/// Use the provided `handshaker` to connect to `initial_peers`, then send
/// the results over `tx`.
#[instrument(skip(initial_peers, connector, tx))]
async fn add_initial_peers<S>(
    initial_peers: Vec<SocketAddr>,
    connector: S,
    mut tx: mpsc::Sender<PeerChange>,
) where
    S: Service<SocketAddr, Response = Change<SocketAddr, PeerClient>, Error = BoxedStdError>
        + Clone,
    S::Future: Send + 'static,
{
    info!(?initial_peers, "Connecting to initial peer set");
    use tower::util::CallAllUnordered;
    let addr_stream = futures::stream::iter(initial_peers.into_iter());
    let mut handshakes = CallAllUnordered::new(connector, addr_stream);
    while let Some(handshake_result) = handshakes.next().await {
        let _ = tx.send(handshake_result).await;
    }
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
    S: Service<(TcpStream, SocketAddr), Response = PeerClient, Error = BoxedStdError> + Clone,
    S::Future: Send + 'static,
{
    let mut listener = TcpListener::bind(addr).await?;
    loop {
        if let Ok((tcp_stream, addr)) = listener.accept().await {
            debug!(?addr, "got incoming connection");
            handshaker.ready().await?;
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
/// and send the resulting `PeerClient` through a channel.
///
#[instrument(skip(new_peer_interval, demand_signal, candidates, connector, success_tx))]
async fn crawl_and_dial<C, S>(
    new_peer_interval: Duration,
    demand_signal: mpsc::Receiver<()>,
    mut candidates: CandidateSet<S>,
    mut connector: C,
    mut success_tx: mpsc::Sender<PeerChange>,
) -> Result<(), BoxedStdError>
where
    C: Service<SocketAddr, Response = Change<SocketAddr, PeerClient>, Error = BoxedStdError>
        + Clone,
    C::Future: Send + 'static,
    S: Service<Request, Response = Response, Error = BoxedStdError>,
    S::Future: Send + 'static,
{
    use futures::TryFutureExt;

    // On creation, we are likely to have very few peers, so try to get more
    // connections quickly by concurrently connecting to a large number of
    // candidates.
    let mut handshakes = FuturesUnordered::new();
    for _ in 0..50usize {
        if let Some(candidate) = candidates.next() {
            connector.ready().await?;
            handshakes.push(
                connector
                    .call(candidate.addr)
                    // Use map_err to tag failed connections with the MetaAddr,
                    // so they can be reported to the CandidateSet.
                    .map_err(move |_| candidate),
            )
        }
    }
    while let Some(handshake) = handshakes.next().await {
        match handshake {
            Ok(change) => {
                debug!("Successfully dialed new peer, sending to peerset");
                success_tx.send(Ok(change)).await?;
            }
            Err(candidate) => {
                debug!(?candidate.addr, "marking address as failed");
                candidates.report_failed(candidate);
            }
        }
    }

    use tokio::timer::Interval;
    let mut connect_signal = futures::stream::select(
        Interval::new_interval(new_peer_interval).map(|_| ()),
        demand_signal,
    );
    while let Some(()) = connect_signal.next().await {
        debug!("got demand signal from peer set, updating candidates");
        candidates.update().await?;
        loop {
            let candidate = match candidates.next() {
                Some(candidate) => candidate,
                None => {
                    warn!("got demand for more peers but no available candidates");
                    break;
                }
            };

            connector.ready().await?;
            match connector
                .call(candidate.addr)
                .map_err(move |_| candidate)
                .await
            {
                Ok(change) => {
                    debug!("Successfully dialed new peer, sending to peerset");
                    success_tx.send(Ok(change)).await?;
                    break;
                }
                Err(candidate) => {
                    debug!(?candidate.addr, "marking address as failed");
                    candidates.report_failed(candidate);
                }
            }
        }
    }
    Ok(())
}
