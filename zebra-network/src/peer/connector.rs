use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use chrono::Utc;
use failure::Error;
use futures::channel::mpsc;
use tokio::{codec::Framed, net::TcpStream, prelude::*};
use tower::Service;
use tracing::{span, Level};
use tracing_futures::Instrument;

use zebra_chain::types::BlockHeight;

use crate::{
    constants,
    protocol::{codec::*, internal::*, message::*, types::*},
    timestamp_collector::{PeerLastSeen, TimestampCollector},
    BoxedStdError, Config, Network,
};

use super::{
    client::PeerClient,
    server::{ErrorSlot, PeerServer, ServerState},
};

/// A [`Service`] that connects to a remote peer and constructs a client/server pair.
pub struct PeerConnector<S> {
    config: Config,
    network: Network,
    internal_service: S,
    sender: mpsc::Sender<PeerLastSeen>,
}

impl<S> PeerConnector<S>
where
    S: Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: Into<BoxedStdError>,
{
    /// Construct a new `PeerConnector`.
    pub fn new(
        config: Config,
        network: Network,
        internal_service: S,
        collector: &TimestampCollector,
    ) -> Self {
        // XXX this function has too many parameters, but it's not clear how to
        // do a nice builder as all fields are mandatory. Could have Builder1,
        // Builder2, ..., with Builder1::with_config() -> Builder2;
        // Builder2::with_internal_service() -> ... or use Options in a single
        // Builder type or use the derive_builder crate.
        let sender = collector.sender_handle();
        PeerConnector {
            config,
            network,
            internal_service,
            sender,
        }
    }
}

impl<S> Service<SocketAddr> for PeerConnector<S>
where
    S: Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: Send + Into<BoxedStdError>,
{
    type Response = PeerClient;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // XXX when this asks a second service for
        // an address to connect to, it should call inner.ready
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, addr: SocketAddr) -> Self::Future {
        let connector_span = span!(Level::INFO, "connector", addr = ?addr);
        let connection_span = span!(Level::INFO, "peer", addr = ?addr);

        // Clone these upfront, so they can be moved into the future.
        let network = self.network.clone();
        let internal_service = self.internal_service.clone();
        let sender = self.sender.clone();
        let user_agent = self.config.user_agent.clone();

        let fut = async move {
            info!("beginning connection");
            let mut stream = Framed::new(
                TcpStream::connect(addr).await?,
                Codec::builder().for_network(network).finish(),
            );

            // XXX construct the Version message from a config
            let version = Message::Version {
                version: constants::CURRENT_VERSION,
                services: PeerServices::NODE_NETWORK,
                timestamp: Utc::now(),
                address_recv: (PeerServices::NODE_NETWORK, addr),
                address_from: (
                    PeerServices::NODE_NETWORK,
                    "127.0.0.1:9000".parse().unwrap(),
                ),
                nonce: Nonce::default(),
                user_agent,
                // XXX eventually the `PeerConnector` will need to have a handle
                // for a service that gets the current block height.
                start_height: BlockHeight(0),
                relay: false,
            };

            stream.send(version).await?;

            let _remote_version = stream
                .next()
                .await
                .ok_or_else(|| format_err!("stream closed during handshake"))??;

            stream.send(Message::Verack).await?;
            let _remote_verack = stream
                .next()
                .await
                .ok_or_else(|| format_err!("stream closed during handshake"))??;

            // XXX here is where we would set the version to the minimum of the
            // two versions, etc. -- actually is it possible to edit the `Codec`
            // after using it to make a framed adapter?

            // Construct a PeerClient, PeerServer pair

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
            };

            let hooked_peer_rx = peer_rx
                .then(move |msg| {
                    let mut sender = sender.clone();
                    async move {
                        if let Ok(_) = msg {
                            use futures::sink::SinkExt;
                            let _ = sender.send((addr, Utc::now())).await;
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

            Ok(client)
        };
        fut.instrument(connector_span).boxed()
    }
}
