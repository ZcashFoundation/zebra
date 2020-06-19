use std::{
    collections::HashSet,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use chrono::Utc;
use futures::{channel::mpsc, prelude::*};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;
use tower::Service;
use tracing::{span, Level};
use tracing_futures::Instrument;

use zebra_chain::types::BlockHeight;

use crate::{
    constants,
    protocol::{
        external::{types::*, Codec, Message},
        internal::{Request, Response},
    },
    types::MetaAddr,
    BoxedStdError, Config,
};

use super::{Client, Connection, ErrorSlot, HandshakeError};

/// A [`Service`] that handshakes with a remote peer and constructs a
/// client/server pair.
pub struct Handshake<S> {
    config: Config,
    internal_service: S,
    timestamp_collector: mpsc::Sender<MetaAddr>,
    nonces: Arc<Mutex<HashSet<Nonce>>>,
}

impl<S: Clone> Clone for Handshake<S> {
    fn clone(&self) -> Self {
        Handshake {
            config: self.config.clone(),
            internal_service: self.internal_service.clone(),
            timestamp_collector: self.timestamp_collector.clone(),
            nonces: self.nonces.clone(),
        }
    }
}

impl<S> Handshake<S>
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
        Handshake {
            config,
            internal_service,
            timestamp_collector,
            nonces: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

impl<S> Service<(TcpStream, SocketAddr)> for Handshake<S>
where
    S: Service<Request, Response = Response, Error = BoxedStdError> + Clone + Send + 'static,
    S::Future: Send,
{
    type Response = Client;
    type Error = BoxedStdError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: (TcpStream, SocketAddr)) -> Self::Future {
        let (tcp_stream, addr) = req;

        let connector_span = span!(Level::INFO, "connector", addr = ?addr);
        // set parent: None for the peer connection span, as it should exist
        // independently of its creation source (inbound connection, crawler,
        // initial peer, ...)
        let connection_span = span!(parent: None, Level::INFO, "peer", addr = ?addr);

        // Clone these upfront, so they can be moved into the future.
        let nonces = self.nonces.clone();
        let internal_service = self.internal_service.clone();
        let timestamp_collector = self.timestamp_collector.clone();
        let user_agent = self.config.user_agent.clone();
        let network = self.config.network;

        let fut = async move {
            debug!("connecting to remote peer");

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
                address_from: (PeerServices::NODE_NETWORK, "0.0.0.0:9000".parse().unwrap()),
                nonce: local_nonce,
                user_agent,
                // XXX eventually the `PeerConnector` will need to have a handle
                // for a service that gets the current block height. Among other
                // things we need it to reject peers who don't know about the
                // current protocol epoch.
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
            let (remote_nonce, remote_services, remote_version) = if let Message::Version {
                nonce,
                services,
                version,
                ..
            } = remote_msg
            {
                (nonce, services, version)
            } else {
                return Err(HandshakeError::UnexpectedMessage(Box::new(remote_msg)));
            };

            // Check for nonce reuse, indicating self-connection.
            let nonce_reuse = {
                let mut locked_nonces = nonces.lock().expect("mutex should be unpoisoned");
                let nonce_reuse = locked_nonces.contains(&remote_nonce);
                // Regardless of whether we observed nonce reuse, clean up the nonce set.
                locked_nonces.remove(&local_nonce);
                nonce_reuse
            };
            if nonce_reuse {
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
                return Err(HandshakeError::UnexpectedMessage(Box::new(remote_msg)));
            }

            // XXX in zcashd remote peer can only send one version message and
            // we would disconnect here if it received a second one. Is it even possible
            // for that to happen to us here?

            if remote_version < constants::MIN_VERSION {
                // Disconnect if peer is using an obsolete version.
                return Err(HandshakeError::ObsoleteVersion(remote_version));
            }

            // TODO: Reject incoming connections from nodes that don't know about the current epoch.
            // zcashd does this:
            //  const Consensus::Params& consensusParams = chainparams.GetConsensus();
            //  auto currentEpoch = CurrentEpoch(GetHeight(), consensusParams);
            //  if (pfrom->nVersion < consensusParams.vUpgrades[currentEpoch].nProtocolVersion)

            // Set the connection's version to the minimum of the received version or our own.
            let negotiated_version = std::cmp::min(remote_version, constants::CURRENT_VERSION);

            // Reconfigure the codec to use the negotiated version.
            //
            // XXX The tokio documentation says not to do this while any frames are still being processed.
            // Since we don't know that here, another way might be to release the tcp
            // stream from the unversioned Framed wrapper and construct a new one with a versioned codec.
            let bare_codec = stream.codec_mut();
            bare_codec.reconfigure_version(negotiated_version);

            debug!("constructing client, spawning server");

            // These channels should not be cloned more than they are
            // in this block, see constants.rs for more.
            let (server_tx, server_rx) = mpsc::channel(0);
            let slot = ErrorSlot::default();

            let client = Client {
                span: connection_span.clone(),
                server_tx: server_tx.clone(),
                error_slot: slot.clone(),
            };

            let (peer_tx, peer_rx) = stream.split();

            // Instrument the peer's rx and tx streams.

            let peer_tx = peer_tx.with(move |msg: Message| {
                // Add a metric for outbound messages.
                // XXX add a dimension tagging message metrics by type
                metrics::counter!("peer.outbound_messages", 1, "addr" => addr.to_string());
                // We need to use future::ready rather than an async block here,
                // because we need the sink to be Unpin, and the With<Fut, ...>
                // returned by .with is Unpin only if Fut is Unpin, and the
                // futures generated by async blocks are not Unpin.
                future::ready(Ok(msg))
            });

            let peer_rx = peer_rx
                .then(move |msg| {
                    // Add a metric for inbound messages and fire a timestamp event.
                    let mut timestamp_collector = timestamp_collector.clone();
                    async move {
                        if msg.is_ok() {
                            // XXX add a dimension tagging message metrics by type
                            metrics::counter!(
                                "inbound_messages",
                                1,
                                "addr" => addr.to_string(),
                            );
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

            use super::connection;
            let server = Connection {
                state: connection::State::AwaitingRequest,
                svc: internal_service,
                client_rx: server_rx,
                error_slot: slot,
                peer_tx,
                request_timer: None,
            };

            tokio::spawn(server.run(peer_rx).instrument(connection_span).boxed());

            tokio::spawn(async move {
                use futures::channel::oneshot;

                use super::client::ClientRequest;

                let mut server_tx = server_tx;

                let mut interval_stream = tokio::time::interval(constants::HEARTBEAT_INTERVAL);

                loop {
                    interval_stream.tick().await;

                    // We discard the server handle because our
                    // heartbeat `Ping`s are a special case, and we
                    // don't actually care about the response here.
                    let (request_tx, _) = oneshot::channel();
                    let msg = ClientRequest(Request::Ping(Nonce::default()), request_tx);

                    if server_tx.send(msg).await.is_err() {
                        return;
                    }
                }
            });

            Ok(client)
        };

        // Spawn a new task to drive this handshake.
        tokio::spawn(fut.instrument(connector_span))
            // This is required to get error types to line up.
            // Probably there's a nicer way to express this using combinators.
            .map(|x| match x {
                Ok(Ok(client)) => Ok(client),
                Ok(Err(handshake_err)) => Err(handshake_err.into()),
                Err(join_err) => Err(join_err.into()),
            })
            .boxed()
    }
}
