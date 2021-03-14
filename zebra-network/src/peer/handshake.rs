use std::{
    collections::HashSet,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use chrono::{TimeZone, Utc};
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use tokio::{net::TcpStream, sync::broadcast};
use tokio_util::codec::Framed;
use tower::Service;
use tracing::{span, Level, Span};
use tracing_futures::Instrument;

use zebra_chain::block;

use crate::{
    constants,
    protocol::{
        external::{types::*, Codec, InventoryHash, Message},
        internal::{Request, Response},
    },
    types::MetaAddr,
    BoxError, Config, PeerAddrState,
};

use super::{Client, Connection, ErrorSlot, HandshakeError, PeerError};

/// A [`Service`] that handshakes with a remote peer and constructs a
/// client/server pair.
#[derive(Clone)]
pub struct Handshake<S> {
    config: Config,
    inbound_service: S,
    timestamp_collector: mpsc::Sender<MetaAddr>,
    inv_collector: broadcast::Sender<(InventoryHash, SocketAddr)>,
    nonces: Arc<Mutex<HashSet<Nonce>>>,
    user_agent: String,
    our_services: PeerServices,
    relay: bool,
    parent_span: Span,
}

pub struct Builder<S> {
    config: Option<Config>,
    inbound_service: Option<S>,
    timestamp_collector: Option<mpsc::Sender<MetaAddr>>,
    our_services: Option<PeerServices>,
    user_agent: Option<String>,
    relay: Option<bool>,
    inv_collector: Option<broadcast::Sender<(InventoryHash, SocketAddr)>>,
}

impl<S> Builder<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
{
    /// Provide a config.  Mandatory.
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    /// Provide a service to handle inbound requests. Mandatory.
    pub fn with_inbound_service(mut self, inbound_service: S) -> Self {
        self.inbound_service = Some(inbound_service);
        self
    }

    /// Provide a channel for registering inventory advertisements. Optional.
    pub fn with_inventory_collector(
        mut self,
        inv_collector: broadcast::Sender<(InventoryHash, SocketAddr)>,
    ) -> Self {
        self.inv_collector = Some(inv_collector);
        self
    }

    /// Provide a hook for timestamp collection. Optional.
    ///
    /// If this is unset, timestamps will not be collected.
    pub fn with_timestamp_collector(mut self, timestamp_collector: mpsc::Sender<MetaAddr>) -> Self {
        self.timestamp_collector = Some(timestamp_collector);
        self
    }

    /// Provide the services this node advertises to other peers.  Optional.
    ///
    /// If this is unset, the node will advertise itself as a client.
    pub fn with_advertised_services(mut self, services: PeerServices) -> Self {
        self.our_services = Some(services);
        self
    }

    /// Provide this node's user agent.  Optional.
    ///
    /// This must be a valid BIP14 string.  If it is unset, the user-agent will be empty.
    pub fn with_user_agent(mut self, user_agent: String) -> Self {
        self.user_agent = Some(user_agent);
        self
    }

    /// Whether to request that peers relay transactions to our node.  Optional.
    ///
    /// If this is unset, the node will not request transactions.
    pub fn want_transactions(mut self, relay: bool) -> Self {
        self.relay = Some(relay);
        self
    }

    /// Consume this builder and produce a [`Handshake`].
    ///
    /// Returns an error only if any mandatory field was unset.
    pub fn finish(self) -> Result<Handshake<S>, &'static str> {
        let config = self.config.ok_or("did not specify config")?;
        let inbound_service = self
            .inbound_service
            .ok_or("did not specify inbound service")?;
        let inv_collector = self.inv_collector.unwrap_or_else(|| {
            let (tx, _) = broadcast::channel(100);
            tx
        });
        let timestamp_collector = self.timestamp_collector.unwrap_or_else(|| {
            // No timestamp collector was passed, so create a stub channel.
            // Dropping the receiver means sends will fail, but we don't care.
            let (tx, _rx) = mpsc::channel(1);
            tx
        });
        let nonces = Arc::new(Mutex::new(HashSet::new()));
        let user_agent = self.user_agent.unwrap_or_else(|| "".to_string());
        let our_services = self.our_services.unwrap_or_else(PeerServices::empty);
        let relay = self.relay.unwrap_or(false);

        Ok(Handshake {
            config,
            inbound_service,
            inv_collector,
            timestamp_collector,
            nonces,
            user_agent,
            our_services,
            relay,
            parent_span: Span::current(),
        })
    }
}

impl<S> Handshake<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
{
    /// Create a builder that configures a [`Handshake`] service.
    pub fn builder() -> Builder<S> {
        // We don't derive `Default` because the derive inserts a `where S:
        // Default` bound even though `Option<S>` implements `Default` even if
        // `S` does not.
        Builder {
            config: None,
            inbound_service: None,
            timestamp_collector: None,
            user_agent: None,
            our_services: None,
            relay: None,
            inv_collector: None,
        }
    }
}

impl<S> Service<(TcpStream, SocketAddr)> for Handshake<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
{
    type Response = Client;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: (TcpStream, SocketAddr)) -> Self::Future {
        let (tcp_stream, addr) = req;

        let connector_span = span!(Level::INFO, "connector", addr = ?addr);
        // set the peer connection span's parent to the global span, as it
        // should exist independently of its creation source (inbound
        // connection, crawler, initial peer, ...)
        let connection_span = span!(parent: &self.parent_span, Level::INFO, "peer", addr = ?addr);

        // Clone these upfront, so they can be moved into the future.
        let nonces = self.nonces.clone();
        let inbound_service = self.inbound_service.clone();
        let timestamp_collector = self.timestamp_collector.clone();
        let inv_collector = self.inv_collector.clone();
        let network = self.config.network;
        let our_addr = self.config.listen_addr;
        let user_agent = self.user_agent.clone();
        let our_services = self.our_services;
        let relay = self.relay;

        let fut = async move {
            debug!("connecting to remote peer");

            let mut stream = Framed::new(
                tcp_stream,
                Codec::builder()
                    .for_network(network)
                    .with_metrics_label(addr.ip().to_string())
                    .finish(),
            );

            let local_nonce = Nonce::default();
            nonces
                .lock()
                .expect("mutex should be unpoisoned")
                .insert(local_nonce);

            // Don't leak our exact clock skew to our peers. On the other hand,
            // we can't deviate too much, or zcashd will get confused.
            // Inspection of the zcashd source code reveals that the timestamp
            // is only ever used at the end of parsing the version message, in
            //
            // pfrom->nTimeOffset = timeWarning.AddTimeData(pfrom->addr, nTime, GetTime());
            //
            // AddTimeData is defined in src/timedata.cpp and is a no-op as long
            // as the difference between the specified timestamp and the
            // zcashd's local time is less than TIMEDATA_WARNING_THRESHOLD, set
            // to 10 * 60 seconds (10 minutes).
            //
            // nTimeOffset is peer metadata that is never used, except for
            // statistics.
            //
            // To try to stay within the range where zcashd will ignore our clock skew,
            // truncate the timestamp to the nearest 5 minutes.
            let now = Utc::now().timestamp();
            let timestamp = Utc.timestamp(now - now.rem_euclid(5 * 60), 0);

            let version = Message::Version {
                version: constants::CURRENT_VERSION,
                services: our_services,
                timestamp,
                address_recv: (PeerServices::NODE_NETWORK, addr),
                address_from: (our_services, our_addr),
                nonce: local_nonce,
                user_agent,
                // The protocol works fine if we don't reveal our current block height,
                // and not sending it means we don't need to be connected to the chain state.
                start_height: block::Height(0),
                relay,
            };

            debug!(?version, "sending initial version message");
            stream.send(version).await?;

            let remote_msg = stream
                .next()
                .await
                .ok_or(HandshakeError::ConnectionClosed)??;

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
                .ok_or(HandshakeError::ConnectionClosed)??;
            if let Message::Verack = remote_msg {
                debug!("got verack from remote peer");
            } else {
                return Err(HandshakeError::UnexpectedMessage(Box::new(remote_msg)));
            }

            // XXX in zcashd remote peer can only send one version message and
            // we would disconnect here if it received a second one. Is it even possible
            // for that to happen to us here?

            // TODO: Reject incoming connections from nodes that don't know about the current epoch.
            // zcashd does this:
            //  const Consensus::Params& consensusParams = chainparams.GetConsensus();
            //  auto currentEpoch = CurrentEpoch(GetHeight(), consensusParams);
            //  if (pfrom->nVersion < consensusParams.vUpgrades[currentEpoch].nProtocolVersion)
            //
            // For approximately 1.5 days before a network upgrade, zcashd also:
            //  - avoids old peers, and
            //  - prefers updated peers.
            // We haven't decided if we need this behaviour in Zebra yet (see #706).
            //
            // At the network upgrade, we also need to disconnect from old peers (see #1334).
            //
            // TODO: replace min_for_upgrade(network, MIN_NETWORK_UPGRADE) with
            //       current_min(network, height) where network is the
            //       configured network, and height is the best tip's block
            //       height.

            if remote_version < Version::min_for_upgrade(network, constants::MIN_NETWORK_UPGRADE) {
                // Disconnect if peer is using an obsolete version.
                return Err(HandshakeError::ObsoleteVersion(remote_version));
            }

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
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let slot = ErrorSlot::default();

            let client = Client {
                shutdown_tx: Some(shutdown_tx),
                server_tx: server_tx.clone(),
                error_slot: slot.clone(),
            };

            let (peer_tx, peer_rx) = stream.split();

            // Instrument the peer's rx and tx streams.

            let peer_tx = peer_tx.with(move |msg: Message| {
                // Add a metric for outbound messages.
                // XXX add a dimension tagging message metrics by type
                metrics::counter!("zcash.net.out.messages", 1, "addr" => addr.to_string());
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
                                "zcash.net.in.messages",
                                1,
                                "addr" => addr.to_string(),
                            );
                            use futures::sink::SinkExt;
                            let _ = timestamp_collector
                                .send(MetaAddr {
                                    addr,
                                    services: remote_services,
                                    last_seen: Utc::now(),
                                    last_connection_state: PeerAddrState::Responded,
                                })
                                .await;
                        }
                        msg
                    }
                })
                .then(move |msg| {
                    let inv_collector = inv_collector.clone();
                    let span = debug_span!("inventory_filter");
                    async move {
                        if let Ok(Message::Inv(hashes)) = &msg {
                            // We ignore inventory messages with more than one
                            // block, because they are most likely replies to a
                            // query, rather than a newly gossiped block.
                            //
                            // (We process inventory messages with any number of
                            // transactions.)
                            //
                            // https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html#inventory-monitoring
                            match hashes.as_slice() {
                                [hash @ InventoryHash::Block(_)] => {
                                    let _ = inv_collector.send((*hash, addr));
                                }
                                [hashes @ ..] => {
                                    for hash in hashes {
                                        if matches!(hash, InventoryHash::Tx(_)) {
                                            debug!(?hash, "registering Tx inventory hash");
                                            let _ = inv_collector.send((*hash, addr));
                                        } else {
                                            trace!(?hash, "ignoring non Tx inventory hash")
                                        }
                                    }
                                }
                            }
                        }
                        msg
                    }
                    .instrument(span)
                })
                .boxed();

            use super::connection;
            let server = Connection {
                state: connection::State::AwaitingRequest,
                svc: inbound_service,
                client_rx: server_rx.into(),
                error_slot: slot,
                peer_tx,
                request_timer: None,
            };

            tokio::spawn(
                server
                    .run(peer_rx)
                    .instrument(connection_span.clone())
                    .boxed(),
            );

            let heartbeat_span = tracing::debug_span!(parent: connection_span, "heartbeat");
            tokio::spawn(
                async move {
                    use super::ClientRequest;
                    use futures::future::Either;

                    let mut shutdown_rx = shutdown_rx;
                    let mut server_tx = server_tx;
                    let mut interval_stream = tokio::time::interval(constants::HEARTBEAT_INTERVAL);
                    loop {
                        let shutdown_rx_ref = Pin::new(&mut shutdown_rx);
                        match future::select(interval_stream.next(), shutdown_rx_ref).await {
                            Either::Left(_) => {
                                let (tx, rx) = oneshot::channel();
                                let request = Request::Ping(Nonce::default());
                                tracing::trace!(?request, "queueing heartbeat request");
                                match server_tx.try_send(ClientRequest {
                                    request,
                                    tx,
                                    span: tracing::Span::current(),
                                }) {
                                    Ok(()) => {
                                        match server_tx.flush().await {
                                            Ok(()) => {}
                                            Err(e) => {
                                                // We can't get the client request for this failure,
                                                // so we can't send an error back here. But that's ok,
                                                // because:
                                                //   - this error never happens (or it's very rare)
                                                //   - if the flush() fails, the server hasn't
                                                //     received the request
                                                tracing::warn!(
                                                    "flushing client request failed: {:?}",
                                                    e
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::trace!(
                                            ?e,
                                            "error sending heartbeat request, shutting down"
                                        );
                                        if e.is_disconnected() {
                                            let ClientRequest { tx, .. } = e.into_inner();
                                            let _ =
                                                tx.send(Err(PeerError::ConnectionClosed.into()));
                                        } else if e.is_full() {
                                            // TODO: wait for the sink to be ready, or wait for a timeout,
                                            // then close the connection with an overloaded error (#1551)
                                            let ClientRequest { tx, .. } = e.into_inner();
                                            let _ = tx.send(Err(PeerError::Overloaded.into()));
                                        } else {
                                            // we need to map unexpected error types to PeerErrors
                                            panic!("unexpected try_send error: {:?}", e);
                                        }
                                        return;
                                    }
                                }
                                // Heartbeats are checked internally to the
                                // connection logic, but we need to wait on the
                                // response to avoid canceling the request.
                                match rx.await {
                                    Ok(_) => tracing::trace!("got heartbeat response"),
                                    Err(_) => {
                                        tracing::trace!(
                                            "error awaiting heartbeat response, shutting down"
                                        );
                                        return;
                                    }
                                }
                            }
                            Either::Right(_) => return, // got shutdown signal
                        }
                    }
                }
                .instrument(heartbeat_span)
                .boxed(),
            );

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
