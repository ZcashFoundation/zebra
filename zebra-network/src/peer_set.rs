//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

use std::{
    net::SocketAddr,
    pin::Pin,
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
    timeout::Timeout,
    Service, ServiceExt,
};
use tower_load::{peak_ewma::PeakEwmaDiscover, NoInstrument};
use tracing::Level;
use tracing_futures::Instrument;

use crate::{
    peer::{HandshakeError, PeerClient, PeerConnector},
    timestamp_collector::TimestampCollector,
    AddressBook, BoxedStdError, Config, Request, Response,
};

mod candidate_set;
mod discover;
mod set;
mod unready_service;

use candidate_set::CandidateSet;
pub use discover::PeerDiscover;
pub use set::PeerSet;

/// A type alias for a boxed [`tower::Service`] used to process [`Request`]s into [`Response`]s.
pub type BoxedZebraService = Box<
    dyn Service<
            Request,
            Response = Response,
            Error = BoxedStdError,
            Future = Pin<Box<dyn Future<Output = Result<Response, BoxedStdError>> + Send>>,
        > + Send
        + 'static,
>;

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
    let peer_connector = Buffer::new(
        Timeout::new(
            PeerConnector::new(config.clone(), inbound_service, timestamp_collector),
            config.handshake_timeout,
        ),
        1,
    );

    // Create an mpsc channel for peer changes, with a generous buffer.
    let (peerset_tx, peerset_rx) = mpsc::channel::<PeerChange>(100);
    // Create an mpsc channel for peerset demand signaling.
    let (demand_tx, demand_rx) = mpsc::channel::<()>(100);

    // Connect the rx end to a PeerSet, wrapping new peers in load instruments.
    let mut peer_set = Buffer::new(
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
        config.initial_peers.clone(),
        peer_connector.clone(),
        peerset_tx.clone(),
    ));

    // 2. Incoming peer connections, via a listener.
    tokio::spawn(
        listen(
            config.listen_addr,
            peer_connector.clone(),
            peerset_tx.clone(),
        )
        .map(|result| {
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
        crawl_and_dial(demand_rx, candidates, peer_connector, peerset_tx).map(|result| {
            if let Err(e) = result {
                error!(%e);
            }
        }),
    );

    (peer_set, address_book)
}

/// Use the provided `peer_connector` to connect to `initial_peers`, then send
/// the results over `tx`.
#[instrument(skip(initial_peers, tx, peer_connector))]
async fn add_initial_peers<S>(
    initial_peers: Vec<SocketAddr>,
    peer_connector: S,
    mut tx: mpsc::Sender<PeerChange>,
) where
    S: Service<(TcpStream, SocketAddr), Response = PeerClient, Error = BoxedStdError> + Clone,
    S::Future: Send + 'static,
{
    info!(?initial_peers, "Connecting to initial peer set");
    let mut handshakes = initial_peers
        .into_iter()
        .map(|addr| {
            let mut pc = peer_connector.clone();
            async move {
                let stream = TcpStream::connect(addr).await?;
                pc.ready().await?;
                let client = pc.call((stream, addr)).await?;
                Ok::<_, BoxedStdError>(Change::Insert(addr, client))
            }
        })
        .collect::<FuturesUnordered<_>>();
    while let Some(handshake_result) = handshakes.next().await {
        let _ = tx.send(handshake_result).await;
    }
}

/// Bind to `addr`, listen for peers using `peer_connector`, then send the
/// results over `tx`.
#[instrument(skip(tx, peer_connector))]
async fn listen<S>(
    addr: SocketAddr,
    mut peer_connector: S,
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
            peer_connector.ready().await?;
            // Construct a handshake future but do not drive it yet....
            let handshake = peer_connector.call((tcp_stream, addr));
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
#[instrument(skip(demand_signal, candidates, peer_connector, success_tx))]
async fn crawl_and_dial<C, S>(
    mut demand_signal: mpsc::Receiver<()>,
    mut candidates: CandidateSet<S>,
    peer_connector: C,
    mut success_tx: mpsc::Sender<PeerChange>,
) -> Result<(), BoxedStdError>
where
    C: Service<(TcpStream, SocketAddr), Response = PeerClient, Error = BoxedStdError> + Clone,
    C::Future: Send + 'static,
    S: Service<Request, Response = Response, Error = BoxedStdError>,
    S::Future: Send + 'static,
{
    // XXX this kind of boilerplate didn't exist before we made PeerConnector
    // take (TcpStream, SocketAddr), which made it so that we could share code
    // between inbound and outbound handshakes. Probably the cleanest way to
    // make it go away again is to rename "Connector" to "Handshake" (since it
    // is really responsible just for the handshake) and to have a "Connector"
    // Service wrapper around "Handshake" that opens a TCP stream.
    // We could also probably make the Handshake service `Clone` directly,
    // which might be more efficient than using a Buffer wrapper.
    use crate::types::MetaAddr;
    use futures::TryFutureExt;
    let try_connect = |candidate: MetaAddr| {
        let mut pc = peer_connector.clone();
        async move {
            let stream = TcpStream::connect(candidate.addr).await?;
            pc.ready().await?;
            pc.call((stream, candidate.addr))
                .await
                .map(|client| Change::Insert(candidate.addr, client))
        }
            // Use map_err to tag failed connections with the MetaAddr,
            // so they can be reported to the CandidateSet.
            .map_err(move |_| candidate)
    };

    // On creation, we are likely to have very few peers, so try to get more
    // connections quickly by concurrently connecting to a large number of
    // candidates.
    let mut handshakes = FuturesUnordered::new();
    for _ in 0..50usize {
        if let Some(candidate) = candidates.next() {
            handshakes.push(try_connect(candidate))
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

    // XXX instead of just responding to demand, we could respond to demand *or*
    // to a interval timer (to continuously grow the peer set).
    while let Some(()) = demand_signal.next().await {
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

            match try_connect(candidate).await {
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
