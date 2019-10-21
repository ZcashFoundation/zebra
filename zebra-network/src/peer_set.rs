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
pub fn init<S>(
    config: Config,
    inbound_service: S,
) -> (
    impl Service<Request, Response = Response, Error = BoxedStdError> + Send + Clone + 'static,
    Arc<Mutex<AddressBook>>,
)
where
    S: Service<Request, Response = Response, Error = BoxedStdError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let (address_book, timestamp_collector) = TimestampCollector::spawn();
    let peer_connector = Buffer::new(
        PeerConnector::new(config.clone(), inbound_service, timestamp_collector),
        1,
    );

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
    tokio::spawn(
        crawl_and_dial(
            demand_rx,
            peer_set.clone(),
            address_book.clone(),
            peer_connector,
            peerset_tx,
        )
        .map(|result| {
            if let Err(e) = result {
                error!(%e);
            }
        }),
    );

    (Box::new(peer_set), address_book)
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
/// ```ascii,no_run
///                         ┌─────────────────┐
///                         │     PeerSet     │
///                         │GetPeers Requests│
///                         └─────────────────┘
///                                  │
///                                  │
///                                  │
///                                  │
///                                  ▼
///    ┌─────────────┐   filter by   Λ     filter by
///    │   PeerSet   │!contains_addr╱ ╲ !contains_addr
/// ┌──│ AddressBook │────────────▶▕   ▏◀───────────────────┐
/// │  └─────────────┘              ╲ ╱                     │
/// │         │                      V                      │
/// │         │disconnected_peers    │                      │
/// │         ▼                      │                      │
/// │         Λ     filter by        │                      │
/// │        ╱ ╲ !contains_addr      │                      │
/// │       ▕   ▏◀───────────────────┼──────────────────────┤
/// │        ╲ ╱                     │                      │
/// │         V                      │                      │
/// │         │                      │                      │
/// │┌────────┼──────────────────────┼──────────────────────┼────────┐
/// ││        ▼                      ▼                      │        │
/// ││ ┌─────────────┐        ┌─────────────┐        ┌─────────────┐ │
/// ││ │Disconnected │        │  Gossiped   │        │Failed Peers │ │
/// ││ │    Peers    │        │    Peers    │        │ AddressBook │◀┼┐
/// ││ │ AddressBook │        │ AddressBook │        │             │ ││
/// ││ └─────────────┘        └─────────────┘        └─────────────┘ ││
/// ││        │                      │                      │        ││
/// ││ #1 drain_oldest        #2 drain_newest        #3 drain_oldest ││
/// ││        │                      │                      │        ││
/// ││        ├──────────────────────┴──────────────────────┘        ││
/// ││        │           disjoint candidate sets                    ││
/// │└────────┼──────────────────────────────────────────────────────┘│
/// │         ▼                                                       │
/// │         Λ                                                       │
/// │        ╱ ╲         filter by                                    │
/// └──────▶▕   ▏!is_potentially_connected                            │
///          ╲ ╱                                                      │
///           V                                                       │
///           │                                                       │
///           │                                                       │
///           ▼                                                       │
///           Λ                                                       │
///          ╱ ╲                                                      │
///         ▕   ▏─────────────────────────────────────────────────────┘
///          ╲ ╱   connection failed, update last_seen to now()
///           V
///           │
///           │
///           ▼
///    ┌────────────┐
///    │    send    │
///    │ PeerClient │
///    │to Discover │
///    └────────────┘
/// ```
#[instrument(skip(
    demand_signal,
    peer_set_service,
    peer_set_address_book,
    peer_connector,
    success_tx
))]
async fn crawl_and_dial<C, S>(
    mut demand_signal: mpsc::Receiver<()>,
    peer_set_service: S,
    peer_set_address_book: Arc<Mutex<AddressBook>>,
    mut peer_connector: C,
    mut success_tx: mpsc::Sender<PeerChange>,
) -> Result<(), BoxedStdError>
where
    C: Service<(TcpStream, SocketAddr), Response = PeerClient, Error = BoxedStdError> + Clone,
    C::Future: Send + 'static,
    S: Service<Request, Response = Response, Error = BoxedStdError>,
    S::Future: Send + 'static,
{
    let mut candidates = CandidateSet {
        disconnected: AddressBook::default(),
        gossiped: AddressBook::default(),
        failed: AddressBook::default(),
        peer_set: peer_set_address_book.clone(),
        peer_service: peer_set_service,
    };

    // XXX instead of just responding to demand, we could respond to demand *or*
    // to a interval timer (to continuously grow the peer set).
    while let Some(()) = demand_signal.next().await {
        debug!("Got demand signal from peer set");
        loop {
            candidates.update().await?;
            // If we were unable to get a candidate, keep looping to crawl more.
            let addr = match candidates.next() {
                Some(candidate) => candidate.addr,
                None => continue,
            };

            // Check that we have not connected to the candidate since it was
            // pulled into the candidate set.
            if peer_set_address_book
                .lock()
                .unwrap()
                .is_potentially_connected(&addr)
            {
                continue;
            };

            if let Ok(stream) = TcpStream::connect(addr).await {
                peer_connector.ready().await?;
                if let Ok(client) = peer_connector.call((stream, addr)).await {
                    debug!("Successfully dialed new peer, sending to peerset");
                    success_tx.send(Ok(Change::Insert(addr, client))).await?;
                    break;
                }
            }
        }
    }
    Ok(())
}
