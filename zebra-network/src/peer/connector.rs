use std::{
    collections::HashSet,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use chrono::Utc;
use futures::channel::mpsc;
use tokio::{codec::Framed, net::TcpStream, prelude::*, timer::Interval};
use tower::{Service, ServiceExt};
use tracing::{span, Level};
use tracing_futures::Instrument;

use zebra_chain::types::BlockHeight;

use crate::{
    constants,
    protocol::{codec::*, internal::*, message::*, types::*},
    types::MetaAddr,
    BoxedStdError, Config,
};

use super::{error::ErrorSlot, server::ServerState, HandshakeError, PeerClient, PeerServer};

/// A [`Service`] that connects to a remote peer and constructs a client/server pair.
pub struct PeerConnector<S> {
    config: Config,
    internal_service: S,
    timestamp_collector: mpsc::Sender<MetaAddr>,
    nonces: Arc<Mutex<HashSet<Nonce>>>,
}

impl<S> PeerConnector<S>
where
    S: Service<Request, Response = Response, Error = BoxedStdError> + Clone + Send + 'static,
    S::Future: Send,
{
    /// Construct a new `PeerConnector`.
    pub fn new(
        config: Config,
        internal_service: S,
        timestamp_collector: mpsc::Sender<MetaAddr>,
    ) -> Self {
        // XXX this function has too many parameters, but it's not clear how to
        // do a nice builder as all fields are mandatory. Could have Builder1,
        // Builder2, ..., with Builder1::with_config() -> Builder2;
        // Builder2::with_internal_service() -> ... or use Options in a single
        // Builder type or use the derive_builder crate.
        PeerConnector {
            config,
            internal_service,
            timestamp_collector,
            nonces: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

impl<S> Service<(TcpStream, SocketAddr)> for PeerConnector<S>
where
    S: Service<Request, Response = Response, Error = BoxedStdError> + Clone + Send + 'static,
    S::Future: Send,
{
    type Response = PeerClient;
    type Error = HandshakeError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: (TcpStream, SocketAddr)) -> Self::Future {
        let (tcp_stream, addr) = req;

        let connector_span = span!(Level::INFO, "connector", addr = ?addr);
        let connection_span = span!(Level::INFO, "peer", addr = ?addr);

        // Clone these upfront, so they can be moved into the future.
        let nonces = self.nonces.clone();
        let internal_service = self.internal_service.clone();
        let timestamp_collector = self.timestamp_collector.clone();
        let user_agent = self.config.user_agent.clone();
        let network = self.config.network.clone();

        let fut = async move {
            info!("connecting to remote peer");

            let mut stream =
                Framed::new(tcp_stream, Codec::builder().for_network(network).finish());

            let local_nonce = Nonce::default();
            nonces
                .lock()
                .expect("mutex should be unpoisoned")
                .insert(local_nonce);

            let version = Message::Version {
                version: constants::CURRENT_VERSION,
                services: PeerServices::NODE_NETWORK,
                timestamp: Utc::now(),
                address_recv: (PeerServices::NODE_NETWORK, addr),
                address_from: (
                    PeerServices::NODE_NETWORK,
                    "127.0.0.1:9000".parse().unwrap(),
                ),
                nonce: local_nonce,
                user_agent,
                // XXX eventually the `PeerConnector` will need to have a handle
                // for a service that gets the current block height.
                start_height: BlockHeight(0),
                relay: false,
            };

            debug!(?version, "sending initial version message");
            stream.send(version).await?;

            let remote_msg = stream
                .next()
                .await
                .ok_or_else(|| HandshakeError::ConnectionClosed)??;

            // Check that we got a Version and destructure its fields into the local scope.
            debug!(?remote_msg, "got message from remote peer");
            let (remote_nonce, remote_services) = if let Message::Version {
                nonce, services, ..
            } = remote_msg
            {
                (nonce, services)
            } else {
                return Err(HandshakeError::UnexpectedMessage(remote_msg));
            };

            // Check for nonce reuse, indicating self-connection.
            if {
                let mut locked_nonces = nonces.lock().expect("mutex should be unpoisoned");
                let nonce_reuse = locked_nonces.contains(&remote_nonce);
                // Regardless of whether we observed nonce reuse, clean up the nonce set.
                locked_nonces.remove(&local_nonce);
                nonce_reuse
            } {
                return Err(HandshakeError::NonceReuse);
            }

            stream.send(Message::Verack).await?;

            let remote_msg = stream
                .next()
                .await
                .ok_or_else(|| HandshakeError::ConnectionClosed)??;
            if let Message::Verack = remote_msg {
                debug!("got verack from remote peer");
            } else {
                return Err(HandshakeError::UnexpectedMessage(remote_msg));
            }

            // XXX here is where we would set the version to the minimum of the
            // two versions, etc. -- actually is it possible to edit the `Codec`
            // after using it to make a framed adapter?

            debug!("constructing PeerClient, spawning PeerServer");

            let (tx, rx) = mpsc::channel(0);
            let slot = ErrorSlot::default();

            let client = PeerClient {
                span: connection_span.clone(),
                server_tx: tx,
                error_slot: slot.clone(),
            };

            let (peer_tx, peer_rx) = stream.split();

            let server = PeerServer {
                state: ServerState::AwaitingRequest,
                svc: internal_service,
                client_rx: rx,
                error_slot: slot,
                peer_tx,
                request_timer: None,
            };

            let hooked_peer_rx = peer_rx
                .then(move |msg| {
                    let mut timestamp_collector = timestamp_collector.clone();
                    async move {
                        if let Ok(_) = msg {
                            use futures::sink::SinkExt;
                            let _ = timestamp_collector
                                .send(MetaAddr {
                                    addr,
                                    services: remote_services,
                                    last_seen: Utc::now(),
                                })
                                .await;
                        }
                        msg
                    }
                })
                .boxed();

            tokio::spawn(
                server
                    .run(hooked_peer_rx)
                    .instrument(connection_span)
                    .boxed(),
            );

            // client.ready().await?;

            tokio::spawn(
                Interval::new_interval(Duration::from_secs(60))
                    .for_each(|_| client.call(Request::Ping(Nonce::default()))),
            );

            Ok(client)
        };
        fut.instrument(connector_span).boxed()
    }
}
