//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

mod discover;
mod set;
mod unready_service;

pub use discover::PeerDiscover;
pub use set::PeerSet;

use std::{net::SocketAddr, pin::Pin};

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
    config::Config,
    peer::{HandshakeError, PeerClient, PeerConnector},
    protocol::internal::{Request, Response},
    timestamp_collector::TimestampCollector,
    BoxedStdError,
};

type BoxedZebraService = Box<
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
pub fn init<S>(config: Config, inbound_service: S) -> (BoxedZebraService, TimestampCollector)
where
    S: Service<Request, Response = Response, Error = BoxedStdError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let timestamp_collector = TimestampCollector::new();
    let peer_connector = Buffer::new(
        PeerConnector::new(config.clone(), inbound_service, &timestamp_collector),
        1,
    );

    // Create an mpsc channel for peer changes, with a generous buffer.
    let (peerset_tx, peerset_rx) = mpsc::channel::<PeerChange>(100);

    // Connect the rx end to a PeerSet, wrapping new peers in load instruments.
    let peer_set = PeerSet::new(PeakEwmaDiscover::new(
        ServiceStream::new(
            // ServiceStream interprets an error as stream termination,
            // so discard any errored connections...
            peerset_rx.filter(|result| future::ready(result.is_ok())),
        ),
        config.ewma_default_rtt,
        config.ewma_decay_time,
        NoInstrument,
    ));

    // Connect the tx end to the 3 peer sources:

    // 1. Initial peers, specified in the config.
    tokio::spawn(add_initial_peers(
        config.initial_peers.clone(),
        peer_connector.clone(),
        peerset_tx.clone(),
    ));

    // 2. Incoming peer connections, via a listener.
    tokio::spawn(
        listen(config.listen_addr, peer_connector, peerset_tx).map(|result| {
            if let Err(e) = result {
                error!(%e);
            }
        }),
    );

    // 3. Outgoing peers we connect to in response to load.

    (Box::new(peer_set), timestamp_collector)
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
