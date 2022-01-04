use std::{
    cmp::min,
    collections::HashSet,
    fmt,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use chrono::{TimeZone, Utc};
use futures::{channel::oneshot, future, FutureExt, SinkExt, StreamExt};
use tokio::{
    net::TcpStream,
    sync::broadcast,
    task::JoinError,
    time::{timeout, Instant},
};
use tokio_stream::wrappers::IntervalStream;
use tokio_util::codec::Framed;
use tower::Service;
use tracing::{span, Level, Span};
use tracing_futures::Instrument;

use zebra_chain::{
    block,
    chain_tip::{ChainTip, NoChainTip},
    parameters::Network,
};

use crate::{
    constants,
    meta_addr::MetaAddrChange,
    peer::{
        CancelHeartbeatTask, Client, ClientRequest, Connection, ErrorSlot, HandshakeError,
        MinimumPeerVersion, PeerError,
    },
    peer_set::ConnectionTracker,
    protocol::{
        external::{types::*, AddrInVersion, Codec, InventoryHash, Message},
        internal::{Request, Response},
    },
    types::MetaAddr,
    BoxError, Config,
};

/// A [`Service`] that handshakes with a remote peer and constructs a
/// client/server pair.
///
/// CORRECTNESS
///
/// To avoid hangs, each handshake (or its connector) should be:
/// - launched in a separate task, and
/// - wrapped in a timeout.
#[derive(Clone)]
pub struct Handshake<S, C = NoChainTip> {
    config: Config,
    inbound_service: S,
    address_book_updater: tokio::sync::mpsc::Sender<MetaAddrChange>,
    inv_collector: broadcast::Sender<(InventoryHash, SocketAddr)>,
    nonces: Arc<futures::lock::Mutex<HashSet<Nonce>>>,
    user_agent: String,
    our_services: PeerServices,
    relay: bool,
    parent_span: Span,
    minimum_peer_version: MinimumPeerVersion<C>,
}

/// The peer address that we are handshaking with.
///
/// Typically, we can rely on outbound addresses, but inbound addresses don't
/// give us enough information to reconnect to that peer.
#[derive(Copy, Clone, PartialEq)]
pub enum ConnectedAddr {
    /// The address we used to make a direct outbound connection.
    ///
    /// In an honest network, a Zcash peer is listening on this exact address
    /// and port.
    OutboundDirect { addr: SocketAddr },

    /// The address we received from the OS, when a remote peer directly
    /// connected to our Zcash listener port.
    ///
    /// In an honest network, a Zcash peer might be listening on this address,
    /// if its outbound address is the same as its listener address. But the port
    /// is an ephemeral outbound TCP port, not a listener port.
    InboundDirect {
        maybe_ip: IpAddr,
        transient_port: u16,
    },

    /// The proxy address we used to make an outbound connection.
    ///
    /// The proxy address can be used by many connections, but our own ephemeral
    /// outbound address and port can be used as an identifier for the duration
    /// of this connection.
    OutboundProxy {
        proxy_addr: SocketAddr,
        transient_local_addr: SocketAddr,
    },

    /// The address we received from the OS, when a remote peer connected via an
    /// inbound proxy.
    ///
    /// The proxy's ephemeral outbound address can be used as an identifier for
    /// the duration of this connection.
    InboundProxy { transient_addr: SocketAddr },

    /// An isolated connection, where we deliberately don't have any connection metadata.
    Isolated,
    //
    // TODO: handle Tor onion addresses
}

/// Get an unspecified IPv4 address for `network`
pub fn get_unspecified_ipv4_addr(network: Network) -> SocketAddr {
    (Ipv4Addr::UNSPECIFIED, network.default_port()).into()
}

use ConnectedAddr::*;

impl ConnectedAddr {
    /// Returns a new outbound directly connected addr.
    pub fn new_outbound_direct(addr: SocketAddr) -> ConnectedAddr {
        OutboundDirect { addr }
    }

    /// Returns a new inbound directly connected addr.
    pub fn new_inbound_direct(addr: SocketAddr) -> ConnectedAddr {
        InboundDirect {
            maybe_ip: addr.ip(),
            transient_port: addr.port(),
        }
    }

    /// Returns a new outbound connected addr via `proxy`.
    ///
    /// `local_addr` is the ephemeral local address of the connection.
    #[allow(unused)]
    pub fn new_outbound_proxy(proxy: SocketAddr, local_addr: SocketAddr) -> ConnectedAddr {
        OutboundProxy {
            proxy_addr: proxy,
            transient_local_addr: local_addr,
        }
    }

    /// Returns a new inbound connected addr from `proxy`.
    //
    // TODO: distinguish between direct listeners and proxy listeners in the
    //       rest of zebra-network
    #[allow(unused)]
    pub fn new_inbound_proxy(proxy: SocketAddr) -> ConnectedAddr {
        InboundProxy {
            transient_addr: proxy,
        }
    }

    /// Returns a new isolated connected addr, with no metadata.
    pub fn new_isolated() -> ConnectedAddr {
        Isolated
    }

    /// Returns a `SocketAddr` that can be used to track this connection in the
    /// `AddressBook`.
    ///
    /// `None` for inbound connections, proxy connections, and isolated
    /// connections.
    ///
    /// # Correctness
    ///
    /// This address can be used for reconnection attempts, or as a permanent
    /// identifier.
    ///
    /// # Security
    ///
    /// This address must not depend on the canonical address from the `Version`
    /// message. Otherwise, malicious peers could interfere with other peers
    /// `AddressBook` state.
    pub fn get_address_book_addr(&self) -> Option<SocketAddr> {
        match self {
            OutboundDirect { addr } => Some(*addr),
            // TODO: consider using the canonical address of the peer to track
            //       outbound proxy connections
            InboundDirect { .. } | OutboundProxy { .. } | InboundProxy { .. } | Isolated => None,
        }
    }

    /// Returns a `SocketAddr` that can be used to temporarily identify a
    /// connection.
    ///
    /// Isolated connections must not change Zebra's peer set or address book
    /// state, so they do not have an identifier.
    ///
    /// # Correctness
    ///
    /// The returned address is only valid while the original connection is
    /// open. It must not be used in the `AddressBook`, for outbound connection
    /// attempts, or as a permanent identifier.
    ///
    /// # Security
    ///
    /// This address must not depend on the canonical address from the `Version`
    /// message. Otherwise, malicious peers could interfere with other peers'
    /// `PeerSet` state.
    pub fn get_transient_addr(&self) -> Option<SocketAddr> {
        match self {
            OutboundDirect { addr } => Some(*addr),
            InboundDirect {
                maybe_ip,
                transient_port,
            } => Some(SocketAddr::new(*maybe_ip, *transient_port)),
            OutboundProxy {
                transient_local_addr,
                ..
            } => Some(*transient_local_addr),
            InboundProxy { transient_addr } => Some(*transient_addr),
            Isolated => None,
        }
    }

    /// Returns the metrics label for this connection's address.
    pub fn get_transient_addr_label(&self) -> String {
        self.get_transient_addr()
            .map_or_else(|| "isolated".to_string(), |addr| addr.to_string())
    }

    /// Returns a short label for the kind of connection.
    pub fn get_short_kind_label(&self) -> &'static str {
        match self {
            OutboundDirect { .. } => "Out",
            InboundDirect { .. } => "In",
            OutboundProxy { .. } => "ProxOut",
            InboundProxy { .. } => "ProxIn",
            Isolated => "Isol",
        }
    }

    /// Returns a list of alternate remote peer addresses, which can be used for
    /// reconnection attempts.
    ///
    /// Uses the connected address, and the remote canonical address.
    ///
    /// Skips duplicates. If this is an outbound connection, also skips the
    /// remote address that we're currently connected to.
    pub fn get_alternate_addrs(
        &self,
        mut canonical_remote: SocketAddr,
    ) -> impl Iterator<Item = SocketAddr> {
        let addrs = match self {
            OutboundDirect { addr } => {
                // Fixup unspecified addresses and ports using known good data
                if canonical_remote.ip().is_unspecified() {
                    canonical_remote.set_ip(addr.ip());
                }
                if canonical_remote.port() == 0 {
                    canonical_remote.set_port(addr.port());
                }

                // Try the canonical remote address, if it is different from the
                // outbound address (which we already have in our address book)
                if &canonical_remote != addr {
                    vec![canonical_remote]
                } else {
                    // we didn't learn a new address from the handshake:
                    // it's the same as the outbound address, which is already in our address book
                    Vec::new()
                }
            }

            InboundDirect { maybe_ip, .. } => {
                // Use the IP from the TCP connection, and the port the peer told us
                let maybe_addr = SocketAddr::new(*maybe_ip, canonical_remote.port());

                // Try both addresses, but remove one duplicate if they match
                if canonical_remote != maybe_addr {
                    vec![canonical_remote, maybe_addr]
                } else {
                    vec![canonical_remote]
                }
            }

            // Proxy addresses can't be used for reconnection attempts, but we
            // can try the canonical remote address
            OutboundProxy { .. } | InboundProxy { .. } => vec![canonical_remote],

            // Hide all metadata for isolated connections
            Isolated => Vec::new(),
        };

        addrs.into_iter()
    }
}

impl fmt::Debug for ConnectedAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = self.get_short_kind_label();
        let addr = self.get_transient_addr_label();

        if matches!(self, Isolated) {
            f.write_str(kind)
        } else {
            f.debug_tuple(kind).field(&addr).finish()
        }
    }
}

/// A builder for `Handshake`.
pub struct Builder<S, C = NoChainTip> {
    config: Option<Config>,
    inbound_service: Option<S>,
    address_book_updater: Option<tokio::sync::mpsc::Sender<MetaAddrChange>>,
    our_services: Option<PeerServices>,
    user_agent: Option<String>,
    relay: Option<bool>,
    inv_collector: Option<broadcast::Sender<(InventoryHash, SocketAddr)>>,
    latest_chain_tip: C,
}

impl<S, C> Builder<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
    C: ChainTip,
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
    ///
    /// This channel takes transient remote addresses, which the `PeerSet` uses
    /// to look up peers that have specific inventory.
    pub fn with_inventory_collector(
        mut self,
        inv_collector: broadcast::Sender<(InventoryHash, SocketAddr)>,
    ) -> Self {
        self.inv_collector = Some(inv_collector);
        self
    }

    /// Provide a hook for timestamp collection. Optional.
    ///
    /// This channel takes `MetaAddr`s, permanent addresses which can be used to
    /// make outbound connections to peers.
    pub fn with_address_book_updater(
        mut self,
        address_book_updater: tokio::sync::mpsc::Sender<MetaAddrChange>,
    ) -> Self {
        self.address_book_updater = Some(address_book_updater);
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

    /// Provide a realtime endpoint to obtain the current best chain tip block height. Optional.
    ///
    /// If this is unset, the minimum accepted protocol version for peer connections is kept
    /// constant over network upgrade activations.
    ///
    /// Use [`NoChainTip`] to explicitly provide no chain tip.
    pub fn with_latest_chain_tip<NewC>(self, latest_chain_tip: NewC) -> Builder<S, NewC> {
        Builder {
            latest_chain_tip,
            // TODO: Until Rust RFC 2528 reaches stable, we can't do `..self`
            config: self.config,
            inbound_service: self.inbound_service,
            address_book_updater: self.address_book_updater,
            our_services: self.our_services,
            user_agent: self.user_agent,
            relay: self.relay,
            inv_collector: self.inv_collector,
        }
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
    pub fn finish(self) -> Result<Handshake<S, C>, &'static str> {
        let config = self.config.ok_or("did not specify config")?;
        let inbound_service = self
            .inbound_service
            .ok_or("did not specify inbound service")?;
        let inv_collector = self.inv_collector.unwrap_or_else(|| {
            let (tx, _) = broadcast::channel(100);
            tx
        });
        let address_book_updater = self.address_book_updater.unwrap_or_else(|| {
            // No `AddressBookUpdater` for timestamp collection was passed, so create a stub
            // channel. Dropping the receiver means sends will fail, but we don't care.
            let (tx, _rx) = tokio::sync::mpsc::channel(1);
            tx
        });
        let nonces = Arc::new(futures::lock::Mutex::new(HashSet::new()));
        let user_agent = self.user_agent.unwrap_or_else(|| "".to_string());
        let our_services = self.our_services.unwrap_or_else(PeerServices::empty);
        let relay = self.relay.unwrap_or(false);
        let network = config.network;
        let minimum_peer_version = MinimumPeerVersion::new(self.latest_chain_tip, network);

        Ok(Handshake {
            config,
            inbound_service,
            inv_collector,
            address_book_updater,
            nonces,
            user_agent,
            our_services,
            relay,
            parent_span: Span::current(),
            minimum_peer_version,
        })
    }
}

impl<S> Handshake<S, NoChainTip>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
{
    /// Create a builder that configures a [`Handshake`] service.
    pub fn builder() -> Builder<S, NoChainTip> {
        // We don't derive `Default` because the derive inserts a `where S:
        // Default` bound even though `Option<S>` implements `Default` even if
        // `S` does not.
        Builder {
            config: None,
            inbound_service: None,
            address_book_updater: None,
            user_agent: None,
            our_services: None,
            relay: None,
            inv_collector: None,
            latest_chain_tip: NoChainTip,
        }
    }
}

/// Negotiate the Zcash network protocol version with the remote peer
/// at `connected_addr`, using the connection `peer_conn`.
///
/// We split `Handshake` into its components before calling this function,
/// to avoid infectious `Sync` bounds on the returned future.
#[allow(clippy::too_many_arguments)]
pub async fn negotiate_version(
    peer_conn: &mut Framed<TcpStream, Codec>,
    connected_addr: &ConnectedAddr,
    config: Config,
    nonces: Arc<futures::lock::Mutex<HashSet<Nonce>>>,
    user_agent: String,
    our_services: PeerServices,
    relay: bool,
    mut minimum_peer_version: MinimumPeerVersion<impl ChainTip>,
) -> Result<(Version, PeerServices, SocketAddr), HandshakeError> {
    // Create a random nonce for this connection
    let local_nonce = Nonce::default();
    // # Correctness
    //
    // It is ok to wait for the lock here, because handshakes have a short
    // timeout, and the async mutex will be released when the task times
    // out.
    nonces.lock().await.insert(local_nonce);

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

    let (their_addr, our_services, our_listen_addr) = match connected_addr {
        // Version messages require an address, so we use
        // an unspecified address for Isolated connections
        Isolated => {
            let unspec_ipv4 = get_unspecified_ipv4_addr(config.network);
            (unspec_ipv4, PeerServices::empty(), unspec_ipv4)
        }
        _ => {
            let their_addr = connected_addr
                .get_transient_addr()
                .expect("non-Isolated connections have a remote addr");
            (their_addr, our_services, config.listen_addr)
        }
    };

    let our_version = Message::Version {
        version: constants::CURRENT_NETWORK_PROTOCOL_VERSION,
        services: our_services,
        timestamp,
        address_recv: AddrInVersion::new(their_addr, PeerServices::NODE_NETWORK),
        // TODO: detect external address (#1893)
        address_from: AddrInVersion::new(our_listen_addr, our_services),
        nonce: local_nonce,
        user_agent: user_agent.clone(),
        // The protocol works fine if we don't reveal our current block height,
        // and not sending it means we don't need to be connected to the chain state.
        start_height: block::Height(0),
        relay,
    };

    debug!(?our_version, "sending initial version message");
    peer_conn.send(our_version).await?;

    let remote_msg = peer_conn
        .next()
        .await
        .ok_or(HandshakeError::ConnectionClosed)??;

    // Check that we got a Version and destructure its fields into the local scope.
    debug!(?remote_msg, "got message from remote peer");
    let (remote_nonce, remote_services, remote_version, remote_canonical_addr, user_agent) =
        if let Message::Version {
            version,
            services,
            address_from,
            nonce,
            user_agent,
            ..
        } = remote_msg
        {
            let canonical_addr = address_from.addr();
            let address_services = address_from.untrusted_services();
            if address_services != services {
                info!(
                    ?services,
                    ?address_services,
                    "peer with inconsistent version services and version address services"
                );
            }

            (nonce, services, version, canonical_addr, user_agent)
        } else {
            Err(HandshakeError::UnexpectedMessage(Box::new(remote_msg)))?
        };

    // Check for nonce reuse, indicating self-connection
    //
    // # Correctness
    //
    // We must wait for the lock before we continue with the connection, to avoid
    // self-connection. If the connection times out, the async lock will be
    // released.
    let nonce_reuse = {
        let mut locked_nonces = nonces.lock().await;
        let nonce_reuse = locked_nonces.contains(&remote_nonce);
        // Regardless of whether we observed nonce reuse, clean up the nonce set.
        locked_nonces.remove(&local_nonce);
        nonce_reuse
    };
    if nonce_reuse {
        Err(HandshakeError::NonceReuse)?;
    }

    // SECURITY: Reject connections to peers on old versions, because they might not know about all
    // network upgrades and could lead to chain forks or slower block propagation.
    let min_version = minimum_peer_version.current();
    if remote_version < min_version {
        debug!(
            remote_ip = ?their_addr,
            ?remote_version,
            ?min_version,
            "disconnecting from peer with obsolete network protocol version"
        );

        // the value is the number of rejected handshakes, by peer IP and protocol version
        metrics::counter!(
            "zcash.net.peers.obsolete",
            1,
            "remote_ip" => their_addr.to_string(),
            "remote_version" => remote_version.to_string(),
            "min_version" => min_version.to_string(),
            "user_agent" => user_agent,
        );

        // the value is the remote version of the most recent rejected handshake from each peer
        metrics::gauge!(
            "zcash.net.peers.version.obsolete",
            remote_version.0.into(),
            "remote_ip" => their_addr.to_string(),
        );

        // Disconnect if peer is using an obsolete version.
        Err(HandshakeError::ObsoleteVersion(remote_version))?;
    } else {
        let negotiated_version = min(constants::CURRENT_NETWORK_PROTOCOL_VERSION, remote_version);

        debug!(
            remote_ip = ?their_addr,
            ?remote_version,
            ?negotiated_version,
            ?min_version,
            "negotiated network protocol version with peer"
        );

        // the value is the number of connected handshakes, by peer IP and protocol version
        metrics::counter!(
            "zcash.net.peers.connected",
            1,
            "remote_ip" => their_addr.to_string(),
            "remote_version" => remote_version.to_string(),
            "negotiated_version" => negotiated_version.to_string(),
            "min_version" => min_version.to_string(),
            "user_agent" => user_agent,
        );

        // the value is the remote version of the most recent connected handshake from each peer
        metrics::gauge!(
            "zcash.net.peers.version.connected",
            remote_version.0.into(),
            "remote_ip" => their_addr.to_string(),
        );
    }

    peer_conn.send(Message::Verack).await?;

    let remote_msg = peer_conn
        .next()
        .await
        .ok_or(HandshakeError::ConnectionClosed)??;
    if let Message::Verack = remote_msg {
        debug!("got verack from remote peer");
    } else {
        Err(HandshakeError::UnexpectedMessage(Box::new(remote_msg)))?;
    }

    Ok((remote_version, remote_services, remote_canonical_addr))
}

/// A handshake request.
/// Contains the information needed to handshake with the peer.
pub struct HandshakeRequest {
    /// The TCP connection to the peer.
    pub tcp_stream: TcpStream,

    /// The address of the peer, and other related information.
    pub connected_addr: ConnectedAddr,

    /// A connection tracker that reduces the open connection count when dropped.
    ///
    /// Used to limit the number of open connections in Zebra.
    pub connection_tracker: ConnectionTracker,
}

impl<S, C> Service<HandshakeRequest> for Handshake<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
    C: ChainTip + Clone + Send + 'static,
{
    type Response = Client;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: HandshakeRequest) -> Self::Future {
        let HandshakeRequest {
            tcp_stream,
            connected_addr,
            connection_tracker,
        } = req;

        let negotiator_span = debug_span!("negotiator", peer = ?connected_addr);
        // set the peer connection span's parent to the global span, as it
        // should exist independently of its creation source (inbound
        // connection, crawler, initial peer, ...)
        let connection_span =
            span!(parent: &self.parent_span, Level::INFO, "", peer = ?connected_addr);

        // Clone these upfront, so they can be moved into the future.
        let nonces = self.nonces.clone();
        let inbound_service = self.inbound_service.clone();
        let address_book_updater = self.address_book_updater.clone();
        let inv_collector = self.inv_collector.clone();
        let config = self.config.clone();
        let user_agent = self.user_agent.clone();
        let our_services = self.our_services;
        let relay = self.relay;
        let minimum_peer_version = self.minimum_peer_version.clone();

        let fut = async move {
            debug!(
                addr = ?connected_addr,
                "negotiating protocol version with remote peer"
            );

            // CORRECTNESS
            //
            // As a defence-in-depth against hangs, every send or next on stream
            // should be wrapped in a timeout.
            let mut peer_conn = Framed::new(
                tcp_stream,
                Codec::builder()
                    .for_network(config.network)
                    .with_metrics_addr_label(connected_addr.get_transient_addr_label())
                    .finish(),
            );

            // Wrap the entire initial connection setup in a timeout.
            let (remote_version, remote_services, remote_canonical_addr) = timeout(
                constants::HANDSHAKE_TIMEOUT,
                negotiate_version(
                    &mut peer_conn,
                    &connected_addr,
                    config,
                    nonces,
                    user_agent,
                    our_services,
                    relay,
                    minimum_peer_version,
                ),
            )
            .await??;

            // If we've learned potential peer addresses from an inbound
            // connection or handshake, add those addresses to our address book.
            //
            // # Security
            //
            // We must handle alternate addresses separately from connected
            // addresses. Otherwise, malicious peers could interfere with the
            // address book state of other peers by providing their addresses in
            // `Version` messages.
            let alternate_addrs = connected_addr.get_alternate_addrs(remote_canonical_addr);
            for alt_addr in alternate_addrs {
                let alt_addr = MetaAddr::new_alternate(&alt_addr, &remote_services);
                // awaiting a local task won't hang
                let _ = address_book_updater.send(alt_addr).await;
            }

            // Set the connection's version to the minimum of the received version or our own.
            let negotiated_version =
                std::cmp::min(remote_version, constants::CURRENT_NETWORK_PROTOCOL_VERSION);

            // Reconfigure the codec to use the negotiated version.
            //
            // XXX The tokio documentation says not to do this while any frames are still being processed.
            // Since we don't know that here, another way might be to release the tcp
            // stream from the unversioned Framed wrapper and construct a new one with a versioned codec.
            let bare_codec = peer_conn.codec_mut();
            bare_codec.reconfigure_version(negotiated_version);

            debug!("constructing client, spawning server");

            // These channels should not be cloned more than they are
            // in this block, see constants.rs for more.
            let (server_tx, server_rx) = futures::channel::mpsc::channel(0);
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let error_slot = ErrorSlot::default();

            let (peer_tx, peer_rx) = peer_conn.split();

            // Instrument the peer's rx and tx streams.

            let inner_conn_span = connection_span.clone();
            let peer_tx = peer_tx.with(move |msg: Message| {
                let span = debug_span!(parent: inner_conn_span.clone(), "outbound_metric");
                // Add a metric for outbound messages.
                metrics::counter!(
                    "zcash.net.out.messages",
                    1,
                    "command" => msg.command(),
                    "addr" => connected_addr.get_transient_addr_label(),
                );
                // We need to use future::ready rather than an async block here,
                // because we need the sink to be Unpin, and the With<Fut, ...>
                // returned by .with is Unpin only if Fut is Unpin, and the
                // futures generated by async blocks are not Unpin.
                future::ready(Ok(msg)).instrument(span)
            });

            // CORRECTNESS
            //
            // Every message and error must update the peer address state via
            // the inbound_ts_collector.
            let inbound_ts_collector = address_book_updater.clone();
            let inv_collector = inv_collector.clone();
            let ts_inner_conn_span = connection_span.clone();
            let inv_inner_conn_span = connection_span.clone();
            let peer_rx = peer_rx
                .then(move |msg| {
                    // Add a metric for inbound messages and errors.
                    // Fire a timestamp or failure event.
                    let inbound_ts_collector = inbound_ts_collector.clone();
                    let span =
                        debug_span!(parent: ts_inner_conn_span.clone(), "inbound_ts_collector");
                    async move {
                        match &msg {
                            Ok(msg) => {
                                metrics::counter!(
                                    "zcash.net.in.messages",
                                    1,
                                    "command" => msg.command(),
                                    "addr" => connected_addr.get_transient_addr_label(),
                                );

                                if let Some(book_addr) = connected_addr.get_address_book_addr() {
                                    // the collector doesn't depend on network activity,
                                    // so this await should not hang
                                    let _ = inbound_ts_collector
                                        .send(MetaAddr::new_responded(&book_addr, &remote_services))
                                        .await;
                                }
                            }
                            Err(err) => {
                                metrics::counter!(
                                    "zebra.net.in.errors",
                                    1,
                                    "error" => err.to_string(),
                                    "addr" => connected_addr.get_transient_addr_label(),
                                );

                                if let Some(book_addr) = connected_addr.get_address_book_addr() {
                                    let _ = inbound_ts_collector
                                        .send(MetaAddr::new_errored(&book_addr, remote_services))
                                        .await;
                                }
                            }
                        }
                        msg
                    }
                    .instrument(span)
                })
                .then(move |msg| {
                    let inv_collector = inv_collector.clone();
                    let span = debug_span!(parent: inv_inner_conn_span.clone(), "inventory_filter");
                    async move {
                        if let (Ok(Message::Inv(hashes)), Some(transient_addr)) =
                            (&msg, connected_addr.get_transient_addr())
                        {
                            // We ignore inventory messages with more than one
                            // block, because they are most likely replies to a
                            // query, rather than a newly gossiped block.
                            //
                            // (We process inventory messages with any number of
                            // transactions.)
                            //
                            // https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html#inventory-monitoring
                            //
                            // TODO: zcashd has a bug where it merges queued inv messages of
                            // the same or different types. So Zebra should split small
                            // merged inv messages into separate inv messages. (#1768)
                            match hashes.as_slice() {
                                [hash @ InventoryHash::Block(_)] => {
                                    debug!(?hash, "registering gossiped block inventory for peer");
                                    let _ = inv_collector.send((*hash, transient_addr));
                                }
                                [hashes @ ..] => {
                                    for hash in hashes {
                                        if let Some(unmined_tx_id) = hash.unmined_tx_id() {
                                            debug!(?unmined_tx_id, "registering unmined transaction inventory for peer");
                                            // The peer set and inv collector use the peer's remote
                                            // address as an identifier
                                            let _ = inv_collector.send((*hash, transient_addr));
                                        } else {
                                            trace!(?hash, "ignoring non-transaction inventory hash in multi-hash list")
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
                request_timer: None,
                svc: inbound_service,
                client_rx: server_rx.into(),
                error_slot: error_slot.clone(),
                peer_tx,
                connection_tracker,
                metrics_label: connected_addr.get_transient_addr_label(),
                last_metrics_state: None,
            };

            let connection_task = tokio::spawn(
                server
                    .run(peer_rx)
                    .instrument(connection_span.clone())
                    .boxed(),
            );

            let heartbeat_task = tokio::spawn(
                send_periodic_heartbeats(
                    connected_addr,
                    remote_services,
                    shutdown_rx,
                    server_tx.clone(),
                    address_book_updater.clone(),
                )
                .instrument(tracing::debug_span!(parent: connection_span, "heartbeat")),
            );

            let client = Client {
                shutdown_tx: Some(shutdown_tx),
                server_tx,
                error_slot,
                version: remote_version,
                connection_task,
                heartbeat_task,
            };

            Ok(client)
        };

        // Spawn a new task to drive this handshake.
        tokio::spawn(fut.instrument(negotiator_span))
            .map(|x: Result<Result<Client, HandshakeError>, JoinError>| Ok(x??))
            .boxed()
    }
}

/// Send periodical heartbeats to `server_tx`, and update the peer status through
/// `heartbeat_ts_collector`.
///
/// # Correctness
///
/// To prevent hangs:
/// - every await that depends on the network must have a timeout (or interval)
/// - every error/shutdown must update the address book state and return
///
/// The address book state can be updated via `ClientRequest.tx`, or the
/// heartbeat_ts_collector.
///
/// Returning from this function terminates the connection's heartbeat task.
async fn send_periodic_heartbeats(
    connected_addr: ConnectedAddr,
    remote_services: PeerServices,
    mut shutdown_rx: oneshot::Receiver<CancelHeartbeatTask>,
    mut server_tx: futures::channel::mpsc::Sender<ClientRequest>,
    mut heartbeat_ts_collector: tokio::sync::mpsc::Sender<MetaAddrChange>,
) {
    use futures::future::Either;

    // Don't send the first heartbeat immediately - we've just completed the handshake!
    let mut interval = tokio::time::interval_at(
        Instant::now() + constants::HEARTBEAT_INTERVAL,
        constants::HEARTBEAT_INTERVAL,
    );
    // If the heartbeat is delayed, also delay all future heartbeats.
    // (Shorter heartbeat intervals just add load, without any benefit.)
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut interval_stream = IntervalStream::new(interval);

    loop {
        let shutdown_rx_ref = Pin::new(&mut shutdown_rx);

        // CORRECTNESS
        //
        // Currently, select prefers the first future if multiple
        // futures are ready.
        //
        // Starvation is impossible here, because interval has a
        // slow rate, and shutdown is a oneshot. If both futures
        // are ready, we want the shutdown to take priority over
        // sending a useless heartbeat.
        if matches!(
            future::select(shutdown_rx_ref, interval_stream.next()).await,
            Either::Left(_)
        ) {
            tracing::trace!("shutting down due to Client shut down");
            if let Some(book_addr) = connected_addr.get_address_book_addr() {
                // awaiting a local task won't hang
                let _ = heartbeat_ts_collector
                    .send(MetaAddr::new_shutdown(&book_addr, remote_services))
                    .await;
            }
            return;
        }

        // We've reached another heartbeat interval without
        // shutting down, so do a heartbeat request.
        //
        // TODO: await heartbeat and shutdown (#3254)
        let heartbeat = send_one_heartbeat(&mut server_tx);
        if heartbeat_timeout(
            heartbeat,
            &mut heartbeat_ts_collector,
            &connected_addr,
            &remote_services,
        )
        .await
        .is_err()
        {
            return;
        }
    }
}

/// Send one heartbeat using `server_tx`.
async fn send_one_heartbeat(
    server_tx: &mut futures::channel::mpsc::Sender<ClientRequest>,
) -> Result<(), BoxError> {
    // We just reached a heartbeat interval, so start sending
    // a heartbeat.
    let (tx, rx) = oneshot::channel();

    // Try to send the heartbeat request
    let request = Request::Ping(Nonce::default());
    tracing::trace!(?request, "queueing heartbeat request");
    match server_tx.try_send(ClientRequest {
        request,
        tx,
        span: tracing::Span::current(),
    }) {
        Ok(()) => {}
        Err(e) => {
            if e.is_disconnected() {
                Err(PeerError::ConnectionClosed)?;
            } else if e.is_full() {
                // Send the message when the Client becomes ready.
                // If sending takes too long, the heartbeat timeout will elapse
                // and close the connection, reducing our load to busy peers.
                server_tx.send(e.into_inner()).await?;
            } else {
                // we need to map unexpected error types to PeerErrors
                warn!(?e, "unexpected try_send error");
                Err(e)?;
            };
        }
    }

    // Flush the heartbeat request from the queue
    server_tx.flush().await?;
    tracing::trace!("sent heartbeat request");

    // Heartbeats are checked internally to the
    // connection logic, but we need to wait on the
    // response to avoid canceling the request.
    rx.await??;
    tracing::trace!("got heartbeat response");

    Ok(())
}

/// Wrap `fut` in a timeout, handing any inner or outer errors using
/// `handle_heartbeat_error`.
async fn heartbeat_timeout<F, T>(
    fut: F,
    address_book_updater: &mut tokio::sync::mpsc::Sender<MetaAddrChange>,
    connected_addr: &ConnectedAddr,
    remote_services: &PeerServices,
) -> Result<T, BoxError>
where
    F: Future<Output = Result<T, BoxError>>,
{
    let t = match timeout(constants::HEARTBEAT_INTERVAL, fut).await {
        Ok(inner_result) => {
            handle_heartbeat_error(
                inner_result,
                address_book_updater,
                connected_addr,
                remote_services,
            )
            .await?
        }
        Err(elapsed) => {
            handle_heartbeat_error(
                Err(elapsed),
                address_book_updater,
                connected_addr,
                remote_services,
            )
            .await?
        }
    };

    Ok(t)
}

/// If `result.is_err()`, mark `connected_addr` as failed using `address_book_updater`.
async fn handle_heartbeat_error<T, E>(
    result: Result<T, E>,
    address_book_updater: &mut tokio::sync::mpsc::Sender<MetaAddrChange>,
    connected_addr: &ConnectedAddr,
    remote_services: &PeerServices,
) -> Result<T, E>
where
    E: std::fmt::Debug,
{
    match result {
        Ok(t) => Ok(t),
        Err(err) => {
            tracing::debug!(?err, "heartbeat error, shutting down");

            if let Some(book_addr) = connected_addr.get_address_book_addr() {
                let _ = address_book_updater
                    .send(MetaAddr::new_errored(&book_addr, *remote_services))
                    .await;
            }
            Err(err)
        }
    }
}
