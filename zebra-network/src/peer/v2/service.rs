//! A [`Service`] that performs version 2 handshakes on established QUIC
//! connections and constructs peer [`Client`]s.
//!
//! This is the version 2 analog of the legacy
//! [`Handshake`](crate::peer::Handshake) service: it produces the same
//! [`Client`] type, so v2 peers plug into the peer set alongside legacy
//! peers.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{channel::oneshot, FutureExt};
use tokio::time::timeout;
use tower::Service;
use tracing_futures::Instrument;

use zebra_chain::chain_tip::{ChainTip, NoChainTip};

use crate::{
    constants,
    meta_addr::MetaAddr,
    peer::{
        handshake::{
            report_handshake_success, send_periodic_heartbeats_with_shutdown_handle,
            HandshakeNonces,
        },
        v2::connection,
        v2::handshake,
        Client, ConnectedAddr, ConnectionInfo, MinimumPeerVersion, RemoteHandshake,
    },
    peer_set::{ConnectionTracker, InventoryChange},
    protocol::{
        external::{
            addr::v2::AddrV2,
            types::{Nonce, PeerServices},
        },
        v2::{
            constants::{
                MAX_HIGH_BANDWIDTH_PEERS, MIN_V2_PROTOCOL_VERSION, SELF_ADDR_ANNOUNCEMENT_INTERVAL,
            },
            init::InitRecord,
            types::StreamType,
        },
    },
    BoxError, Config, PeerSocketAddr, Request, Response,
};

/// A request to perform a version 2 handshake on an established QUIC
/// connection.
pub struct HandshakeRequest {
    /// The established QUIC connection, with ALPN already checked.
    pub connection: quinn::Connection,

    /// The address of the remote peer, and how the connection was made.
    pub connected_addr: ConnectedAddr,

    /// A connection tracker that reduces the open connection count when
    /// dropped.
    pub connection_tracker: ConnectionTracker,
}

/// A [`Service`] that performs version 2 handshakes and constructs peer
/// [`Client`]s.
#[derive(Clone)]
pub struct Handshake<S, C = NoChainTip> {
    /// The shared network configuration.
    config: Config,

    /// The user agent advertised in `init` records.
    user_agent: String,

    /// The services advertised in `init` records.
    our_services: PeerServices,

    /// Whether this node requests transaction relay from its peers.
    relay: bool,

    /// The service that handles requests from remote peers.
    inbound_service: S,

    /// Sends peer status updates to the address book.
    address_book_updater: crate::peer_book::ChangeSender,

    /// Sends misbehavior scores for peers that violate the protocol, so they
    /// are banned once they reach the ban threshold.
    misbehavior_tx: tokio::sync::mpsc::Sender<(PeerSocketAddr, u32)>,

    /// Registers inventory advertised by remote peers.
    inv_collector: tokio::sync::broadcast::Sender<InventoryChange>,

    /// The minimum peer protocol version for the current network epoch.
    minimum_peer_version: MinimumPeerVersion<C>,

    /// The local listener address, announced to peers on each connection's
    /// address announcement stream.
    local_listener: std::net::SocketAddr,

    /// The nonces recently sent in `init` records, shared between v2
    /// connections for self-connection detection.
    nonces: HandshakeNonces,

    /// The connections currently claiming one of this node's high-bandwidth
    /// compact block announcement slots.
    hb_slots: crate::peer_set::SlotCounter,

    /// The directory of content-addressed synchronization artifacts served
    /// to `get-object` requests, when it exists.
    artifact_dir: Option<Arc<std::path::PathBuf>>,
}

impl<S, C> Handshake<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    /// Creates a new version 2 handshake service.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        user_agent: String,
        our_services: PeerServices,
        relay: bool,
        inbound_service: S,
        address_book_updater: crate::peer_book::ChangeSender,
        misbehavior_tx: tokio::sync::mpsc::Sender<(PeerSocketAddr, u32)>,
        inv_collector: tokio::sync::broadcast::Sender<InventoryChange>,
        minimum_peer_version: MinimumPeerVersion<C>,
        local_listener: std::net::SocketAddr,
    ) -> Self {
        let artifact_dir = config
            .cache_dir
            .artifact_dir_path(&config.network)
            .map(Arc::new);

        Handshake {
            config,
            user_agent,
            our_services,
            relay,
            inbound_service,
            address_book_updater,
            misbehavior_tx,
            inv_collector,
            minimum_peer_version,
            artifact_dir,
            local_listener,
            nonces: HandshakeNonces::default(),
            hb_slots: Default::default(),
        }
    }

    /// Builds the local `init` record for a new connection.
    fn local_init(&mut self, announce: bool) -> InitRecord {
        let start_height = self
            .minimum_peer_version
            .chain_tip()
            .best_tip_height()
            .unwrap_or(zebra_chain::block::Height(0));

        InitRecord {
            version: constants::CURRENT_NETWORK_PROTOCOL_VERSION,
            services: self.our_services,
            nonce: Nonce::default(),
            user_agent: self.user_agent.clone(),
            start_height,
            relay: self.relay,
            announce,
            // Short transaction IDs are matched against the local mempool,
            // so the larger full-ID compact blocks are never requested.
            full_ids: false,
        }
    }
}

impl<S, C> Service<HandshakeRequest> for Handshake<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    type Response = Client;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: HandshakeRequest) -> Self::Future {
        let HandshakeRequest {
            connection,
            connected_addr,
            mut connection_tracker,
        } = request;

        let negotiator_span = tracing::debug_span!("v2_handshake", peer = ?connected_addr);

        // Request high-bandwidth compact block announcements on up to
        // `MAX_HIGH_BANDWIDTH_PEERS` self-initiated connections at a time.
        // The preference is fixed for the life of a connection, so the slot
        // is held until the connection is dropped.
        let hb_slot = if connected_addr.is_inbound() {
            None
        } else {
            self.hb_slots.try_claim(MAX_HIGH_BANDWIDTH_PEERS)
        };
        let local_init = self.local_init(hb_slot.is_some());
        let params = handshake::HandshakeParams {
            local_init: local_init.clone(),
            min_remote_version: std::cmp::max(
                MIN_V2_PROTOCOL_VERSION,
                self.minimum_peer_version.current(),
            ),
            nonces: self.nonces.clone(),
            nonce_limit: self.config.peerset_total_connection_limit(),
        };

        let inbound_service = self.inbound_service.clone();
        let artifact_dir = self.artifact_dir.clone();
        let address_book_updater = self.address_book_updater.clone();
        let misbehavior_tx = self.misbehavior_tx.clone();
        let inv_collector = self.inv_collector.clone();
        let local_listener = self.local_listener;
        let network = self.config.network.clone();

        let fut = async move {
            let handshake_result = if connected_addr.is_inbound() {
                handshake::respond(&connection, &params).await
            } else {
                handshake::initiate(&connection, &params).await
            };
            let handshake = handshake_result?;

            let remote_init = Arc::new(handshake.remote_init);
            let connection_info = Arc::new(ConnectionInfo {
                connected_addr,
                remote: RemoteHandshake::Init(remote_init.clone()),
                negotiated_version: handshake.negotiated_version,
            });

            // The handshake succeeded: update the address book and the
            // active connection counter.
            report_handshake_success(
                &mut connection_tracker,
                &connected_addr,
                remote_init.services,
                remote_init.user_agent.clone(),
                handshake.negotiated_version,
                &address_book_updater,
            )
            .await;

            let (server_tx, server_rx) = futures::channel::mpsc::channel(0);
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let (announcement_tx, announcement_rx) =
                tokio::sync::mpsc::channel(connection::ANNOUNCEMENT_QUEUE_LIMIT);
            let error_slot = crate::peer::ErrorSlot::default();

            let shared = Arc::new(connection::SharedConnection {
                quic: connection.clone(),
                error_slot: error_slot.clone(),
                remote_init,
                local_relay: local_init.relay,
                hb_slot,
                transient_addr: connected_addr.get_transient_addr(),
                is_inbound: connected_addr.is_inbound(),
                inv_collector: inv_collector.clone(),
                misbehavior_tx,
                announcement_tx,
                pushed_transactions: Default::default(),
                sent_blocks: Default::default(),
                reconstructed_blocks: Default::default(),
                mempool_updates: tokio::sync::broadcast::channel(
                    connection::MEMPOOL_UPDATES_CHANNEL_CAPACITY,
                )
                .0,
                mempool_subscribed: Default::default(),
                remote_mempool: Default::default(),
                pending_tx_announcements: Default::default(),
                cached_addrs: Default::default(),
                addr_tokens: Default::default(),
                open_inbound_announcements: Default::default(),
                sent_get_addr: Default::default(),
                consecutive_timeouts: Default::default(),
                active_bulk_streams: Default::default(),
                artifact_dir,
            });

            let announcer_shared = shared.clone();
            let subscription_shared = shared.clone();
            let subscription_service = inbound_service.clone();
            let trickle_shared = shared.clone();

            let server = connection::Connection {
                shared,
                inbound_service,
                client_rx: server_rx.into(),
                handshake_recv: handshake.handshake_recv,
                handshake_send: handshake.handshake_send,
                announcement_rx,
                connection_tracker,
            };

            let connection_task = tokio::spawn(server.run().in_current_span().boxed());

            // Announce this node's own listener address on the address
            // announcement stream: peers learn dialable v2 addresses only
            // through address announcements and `get-addr`. The task ends
            // when the connection closes.
            tokio::spawn(
                announce_local_listener(announcer_shared, local_listener, network)
                    .in_current_span(),
            );

            // Trickle queued transaction announcements and mempool
            // subscription updates to the peer. The task ends when the
            // connection closes.
            tokio::spawn(
                connection::trickle_transaction_announcements(trickle_shared).in_current_span(),
            );

            // Subscribe to the peer's mempool, unless this node declined
            // transaction relay. The subscription mirrors the peer's
            // mempool into the connection's cache, and ends when the peer
            // refuses it or the connection closes.
            if local_init.relay {
                tokio::spawn(
                    connection::maintain_mempool_subscription(
                        subscription_shared,
                        subscription_service,
                    )
                    .in_current_span(),
                );
            }

            let heartbeat_task = tokio::spawn(
                send_periodic_heartbeats_with_shutdown_handle(
                    connected_addr,
                    shutdown_rx,
                    server_tx.clone(),
                    address_book_updater.clone(),
                )
                .instrument(tracing::debug_span!("v2_heartbeat"))
                .boxed(),
            );

            Ok::<Client, BoxError>(Client {
                connection_info,
                shutdown_tx: Some(shutdown_tx),
                server_tx,
                inv_collector,
                error_slot,
                connection_task,
                heartbeat_task,
            })
        };

        // Defence-in-depth against handshake hangs.
        let fut = timeout(constants::HANDSHAKE_TIMEOUT, fut);

        async move {
            match fut.await {
                Ok(result) => result,
                Err(_elapsed) => Err(crate::peer::HandshakeError::Timeout.into()),
            }
        }
        .instrument(negotiator_span)
        .boxed()
    }
}

#[cfg(test)]
mod tests;

/// Announces this node's own listener address on `shared`'s address
/// announcement stream, once immediately and then per
/// [`SELF_ADDR_ANNOUNCEMENT_INTERVAL`], until the connection closes.
///
/// Addresses that must not be gossiped (for example, an unspecified
/// listener IP) are never announced: `sanitize` rejects them, and truncates
/// times so announcements cannot fingerprint this node.
async fn announce_local_listener(
    shared: Arc<connection::SharedConnection>,
    local_listener: std::net::SocketAddr,
    network: zebra_chain::parameters::Network,
) {
    use zebra_chain::serialization::{DateTime32, ZcashSerialize};

    loop {
        let listener_entry = MetaAddr::new_local_listener_change(local_listener)
            .local_listener_into_new_meta_addr(DateTime32::now());

        if let Some(sanitized) = listener_entry.sanitize(&network) {
            let mut payload = Vec::new();
            if AddrV2::from(sanitized)
                .zcash_serialize(&mut payload)
                .is_ok()
            {
                shared.enqueue_announcement(StreamType::AddressAnnouncements, payload);
            }
        }

        tokio::select! {
            _ = shared.quic.closed() => break,
            _ = tokio::time::sleep(SELF_ADDR_ANNOUNCEMENT_INTERVAL) => {}
        }
    }
}
