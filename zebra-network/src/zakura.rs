//! Zakura P2P dependency, identity, handshake, and protocol-handler scaffolding.
//!
//! This module reserves the iroh dependency, privacy-preserving endpoint posture,
//! persistent identity storage surface, and bounded Zakura handshake wire types.

use std::time::Duration;

use iroh::{endpoint, Endpoint, NodeAddr, NodeId, RelayMode, SecretKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::watch;

use crate::{
    meta_addr::{MetaAddr, MetaAddrChange},
    PeerSocketAddr,
};

mod block_sync;
mod discovery;
mod exchange;
mod handler;
mod handshake;
mod header_sync;
mod legacy_gossip;
#[cfg(any(test, feature = "zakura-testkit"))]
pub mod testkit;
mod trace;
pub mod transport;

pub use block_sync::*;
pub use discovery::*;
pub use exchange::*;
pub use handler::*;
pub use handshake::*;
pub use header_sync::*;
pub use legacy_gossip::*;
pub use trace::{
    commit_state_trace, peer_label as zakura_trace_peer_label,
    reject_reason_label as zakura_trace_reject_reason_label, ZakuraTrace, ZakuraTraceEvent,
    BLOCK_SYNC_TABLE, COMMIT_STATE_TABLE, CONN_TABLE, HANDSHAKE_TABLE, HEADER_SYNC_TABLE,
    LEGACY_REQUEST_TABLE, RATELIMIT_TABLE, STREAM_TABLE,
};
pub use transport::*;

#[cfg(any(test, feature = "zakura-testkit"))]
pub(crate) use handler::run_native_initiator_handshake_without_trace as run_native_initiator_handshake;

#[cfg(test)]
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

/// The pinned iroh version the Zakura P2P plan was verified against.
pub const IROH_VERSION: &str = "0.92.0";

/// Capability bit for the legacy gossip compatibility service.
pub const ZAKURA_CAP_LEGACY_GOSSIP: u64 = 1 << 0;

/// Capability bit for the native header-sync service.
pub const ZAKURA_CAP_HEADER_SYNC: u64 = 1 << 1;

/// Capability bit for the native discovery service.
pub const ZAKURA_CAP_DISCOVERY: u64 = 1 << 2;

/// Production default for per-service peer caps.
pub const DEFAULT_SERVICE_MAX_PEERS: usize = 256;
/// Production default for per-service bounded inbound work queues.
pub const DEFAULT_SERVICE_INBOUND_QUEUE_DEPTH: usize = 128;
/// Production default for per-service bounded outbound work queues.
pub const DEFAULT_SERVICE_OUTBOUND_QUEUE_DEPTH: usize = 128;
/// Production default for demand-gated service escalations.
pub const DEFAULT_SERVICE_MAX_PENDING_ESCALATIONS: usize = 32;

/// Per-service peer and queue limits owned by a Zakura service reactor.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServicePeerLimits {
    /// Maximum inbound peers this service admits.
    pub max_inbound_peers: usize,
    /// Maximum outbound peers this service admits.
    pub max_outbound_peers: usize,
    /// Inbound queue depth reserved for this service.
    ///
    /// Reserved for future transport queue wiring; not enforced in this phase.
    pub inbound_queue_depth: usize,
    /// Outbound queue depth reserved for this service.
    ///
    /// Reserved for future transport queue wiring; not enforced in this phase.
    pub outbound_queue_depth: usize,
    /// Maximum service escalations that may be pending admission.
    ///
    /// Reserved for future lazy service escalation; not enforced in this phase.
    pub max_pending_escalations: usize,
}

impl Default for ServicePeerLimits {
    fn default() -> Self {
        Self {
            max_inbound_peers: DEFAULT_SERVICE_MAX_PEERS,
            max_outbound_peers: DEFAULT_SERVICE_MAX_PEERS,
            inbound_queue_depth: DEFAULT_SERVICE_INBOUND_QUEUE_DEPTH,
            outbound_queue_depth: DEFAULT_SERVICE_OUTBOUND_QUEUE_DEPTH,
            max_pending_escalations: DEFAULT_SERVICE_MAX_PENDING_ESCALATIONS,
        }
    }
}

/// Local service admission result.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ServiceAdmissionDecision {
    /// The service accepted the typed peer session.
    Admit,
    /// The local service-specific peer cap is full.
    RejectFull,
    /// The peer is not useful for this service right now.
    RejectNotUseful,
    /// The peer is still in a local retry backoff window.
    RejectBackoff,
    /// The peer does not support this service.
    RejectUnsupported,
}

/// Direction of the underlying Zakura connection.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum ServicePeerDirection {
    /// The remote peer dialed this node.
    Inbound,
    /// This node dialed the remote peer.
    Outbound,
}

impl ServicePeerDirection {
    pub(crate) fn trace_label(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

/// Current admitted peer counts and slot availability for one service.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ServicePeerSnapshot {
    /// Currently admitted inbound peers.
    pub inbound_peers: usize,
    /// Currently admitted outbound peers.
    pub outbound_peers: usize,
    /// Free inbound slots under the configured cap.
    pub inbound_slots_free: usize,
    /// Free outbound slots under the configured cap.
    pub outbound_slots_free: usize,
}

impl ServicePeerSnapshot {
    /// Build a snapshot from current admitted counts.
    pub fn new(inbound_peers: usize, outbound_peers: usize, limits: ServicePeerLimits) -> Self {
        Self {
            inbound_peers,
            outbound_peers,
            inbound_slots_free: limits.max_inbound_peers.saturating_sub(inbound_peers),
            outbound_slots_free: limits.max_outbound_peers.saturating_sub(outbound_peers),
        }
    }
}

impl Default for ServicePeerSnapshot {
    fn default() -> Self {
        Self::new(0, 0, ServicePeerLimits::default())
    }
}

/// How long the legacy->Zakura liveness keeper waits for the upgraded QUIC
/// connection to register with the supervisor before giving up.
///
/// The native dial spawned by the upgrade is asynchronous, so the peer only
/// appears in the supervisor's registered set a little later. If it never
/// registers (the dial failed), the keeper exits without marking the peer live,
/// so the outbound crawler reconnects to it normally.
const ZAKURA_LIVENESS_APPEAR_TIMEOUT: Duration = Duration::from_secs(15);

/// How often the keeper refreshes an upgraded peer's legacy address-book entry.
///
/// Must stay below [`constants::MIN_PEER_RECONNECTION_DELAY`](crate::constants::MIN_PEER_RECONNECTION_DELAY)
/// so the `Responded` liveness never ages into a reconnection candidate while
/// the Zakura connection is alive.
const ZAKURA_LIVENESS_REFRESH_INTERVAL: Duration = Duration::from_secs(45);

/// Returns an iroh endpoint builder with relays and external address lookup disabled.
///
/// Callers must add direct bind addresses before binding if they do not want the
/// endpoint to listen on iroh's default unspecified sockets.
pub fn direct_endpoint_builder(secret_key: SecretKey) -> endpoint::Builder {
    Endpoint::builder()
        .relay_mode(RelayMode::Disabled)
        .clear_discovery()
        .secret_key(secret_key)
}

/// The result of routing a mutually P2P-v2-capable legacy handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ZakuraUpgradeOutcome {
    /// The peer was authenticated and registered with the Zakura supervisor.
    Upgraded {
        /// The authenticated Zakura/Iroh peer identity.
        peer_id: ZakuraPeerId,
    },

    /// The peer was authenticated, but a better duplicate connection already exists.
    Duplicate {
        /// The authenticated Zakura/Iroh peer identity.
        peer_id: ZakuraPeerId,
    },

    /// The upgrade was rejected neutrally.
    Rejected {
        /// The neutral rejection reason.
        reason: ZakuraRejectReason,
    },
}

/// An error from the Zakura handshake upgrade hook.
#[derive(Error, Debug)]
pub enum ZakuraUpgradeError {
    /// Local config and the remote service bit selected Zakura, but no supervisor exists yet.
    #[error("Zakura P2P v2 upgrade selected but no Zakura handshake connector is available")]
    Unavailable,
}

/// Handle used by the legacy handshake to enter the Zakura P2P upgrade path.
///
/// After a mutually P2P-v2-capable legacy `version`/`verack` exchange, the legacy
/// handshake swaps a bounded upgrade prelude over the TCP stream to learn the
/// peer's Zakura node address, then uses this connector to dial it over QUIC.
#[derive(Clone, Debug, Default)]
pub struct ZakuraHandshakeConnector {
    /// The live Zakura endpoint, used to read our own dial hints and to dial a
    /// peer over QUIC once the legacy upgrade prelude exchange reveals its
    /// Zakura node address.
    endpoint: Option<ZakuraEndpoint>,
    #[cfg(test)]
    test_outcome: Option<(Arc<AtomicUsize>, ZakuraUpgradeOutcome)>,
}

impl ZakuraHandshakeConnector {
    /// Create a placeholder connector for a node that can advertise P2P v2, but
    /// has no live Zakura endpoint, so it cannot complete a handoff.
    pub fn unavailable() -> Self {
        Self {
            endpoint: None,
            #[cfg(test)]
            test_outcome: None,
        }
    }

    /// Create a connector backed by the live Zakura endpoint, so the legacy
    /// handshake can exchange dial hints and connect to the peer over QUIC.
    pub(crate) fn new_with_endpoint(endpoint: ZakuraEndpoint) -> Self {
        Self {
            endpoint: Some(endpoint),
            #[cfg(test)]
            test_outcome: None,
        }
    }

    /// Returns our local Zakura dial hints (iroh node id, and direct addresses
    /// each encoded as a `SocketAddr` string) for the legacy upgrade prelude, or
    /// `None` if this node has no live Zakura endpoint.
    pub(crate) async fn local_iroh_hints(&self) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
        let endpoint = self.endpoint.as_ref()?;
        Some(endpoint.local_upgrade_hints().await)
    }

    /// Dial a peer over Zakura QUIC using the node id and direct-address hints it
    /// advertised in the legacy upgrade prelude, then wait until the connection
    /// registers with the local supervisor.
    ///
    /// The legacy side drops its TCP connection once the upgrade is selected, so
    /// the Zakura request adapter must have a usable outbound handle before the
    /// handoff reports success.
    pub(crate) async fn spawn_zakura_dial_to_hints_and_wait(
        &self,
        peer_id: &ZakuraPeerId,
        node_id: &[u8],
        direct_addresses: &[Vec<u8>],
    ) -> bool {
        let Some(endpoint) = self.endpoint.as_ref() else {
            return false;
        };
        let Some(node_addr) = node_addr_from_hints(node_id, direct_addresses) else {
            return false;
        };
        let mut registered = endpoint.supervisor().subscribe();
        if !endpoint.ensure_upgrade_native_dial(node_addr) {
            return false;
        }
        if wait_for_zakura_peer(&mut registered, peer_id, ZAKURA_LIVENESS_APPEAR_TIMEOUT).await {
            return true;
        }

        // The hand-off did not complete within the wait window. The dial spawned
        // by `ensure_upgrade_native_dial` uses `RedialPolicy::maintain`, so it
        // would keep redialing this peer-supplied address forever and retain its
        // `upgrade_dials` entry. Unless the peer registered in the meantime
        // (keep its maintained dial as the recovery path), cancel the dial and
        // drop the entry so a malicious legacy responder cannot leak unbounded
        // maintained dials and outbound QUIC traffic by repeating failed
        // upgrades with distinct node ids.
        if !registered.borrow().iter().any(|id| id == peer_id) {
            endpoint.cancel_upgrade_native_dial(peer_id);
        }
        false
    }

    /// Wait until the upgraded peer's inbound native QUIC connection registers
    /// with the local supervisor.
    ///
    /// Used by the inbound legacy responder, which does not dial: after sending
    /// `Accept` the remote peer is expected to dial our advertised Zakura
    /// endpoint, and our iroh router registers that connection separately. The
    /// outer handshake drops the legacy TCP connection once the upgrade is
    /// reported, so the responder must confirm a usable Zakura replacement
    /// exists first. Returns `false` (keep legacy) if the peer never registers
    /// within [`ZAKURA_LIVENESS_APPEAR_TIMEOUT`] or this node has no live
    /// endpoint, so a peer that sends a valid `Init` and then never completes
    /// the native dial cannot make us silently drop a working legacy peer.
    pub(crate) async fn wait_for_zakura_registration(&self, peer_id: &ZakuraPeerId) -> bool {
        let Some(endpoint) = self.endpoint.as_ref() else {
            return false;
        };
        let mut registered = endpoint.supervisor().subscribe();
        wait_for_zakura_peer(&mut registered, peer_id, ZAKURA_LIVENESS_APPEAR_TIMEOUT).await
    }

    /// Keep an upgraded peer's legacy address-book entry live for the lifetime
    /// of its Zakura connection, so the outbound crawler does not re-dial it.
    ///
    /// After a legacy->Zakura upgrade the legacy TCP connection is dropped, so
    /// nothing else refreshes the peer's `Responded` liveness. Without this, the
    /// crawler re-dials the peer once its entry ages past
    /// [`constants::MIN_PEER_RECONNECTION_DELAY`](crate::constants::MIN_PEER_RECONNECTION_DELAY),
    /// re-running the upgrade and churning the QUIC connection. While the peer
    /// is registered with the supervisor the keeper marks it `Responded`; once
    /// it deregisters the keeper stops, so a genuinely gone peer becomes a
    /// reconnection candidate again.
    ///
    /// Only meaningful for outbound connections, where `book_addr` is the
    /// dialable remote address the crawler would otherwise reconnect to. Does
    /// nothing without a live endpoint.
    pub(crate) fn spawn_legacy_liveness_keeper(
        &self,
        peer_id: ZakuraPeerId,
        book_addr: PeerSocketAddr,
        address_book_updater: tokio::sync::mpsc::Sender<MetaAddrChange>,
    ) {
        let Some(endpoint) = self.endpoint.as_ref() else {
            return;
        };
        let registered = endpoint.supervisor().subscribe();
        tokio::spawn(run_legacy_liveness_keeper(
            registered,
            peer_id,
            book_addr,
            address_book_updater,
            ZAKURA_LIVENESS_APPEAR_TIMEOUT,
            ZAKURA_LIVENESS_REFRESH_INTERVAL,
        ));
    }

    /// Returns the deterministic upgrade outcome injected for handshake routing
    /// tests (incrementing the call counter), if one was configured.
    #[cfg(test)]
    pub(crate) fn consume_test_outcome(&self) -> Option<ZakuraUpgradeOutcome> {
        let (calls, outcome) = self.test_outcome.as_ref()?;
        calls.fetch_add(1, Ordering::SeqCst);
        Some(outcome.clone())
    }

    /// Create a deterministic connector for handshake routing tests.
    #[cfg(test)]
    pub(crate) fn for_test(calls: Arc<AtomicUsize>, outcome: ZakuraUpgradeOutcome) -> Self {
        Self {
            endpoint: None,
            test_outcome: Some((calls, outcome)),
        }
    }
}

/// Builds an iroh dial address from the node id and direct-address hints a peer
/// advertised in a legacy upgrade prelude.
///
/// Direct addresses are carried as `SocketAddr` strings (the same encoding used
/// by configured bootstrap peers), so each entry is parsed back into a
/// `SocketAddr`. Returns `None` if the node id is malformed or no direct address
/// parses, since a peer with no reachable address cannot be dialed.
fn node_addr_from_hints(node_id: &[u8], direct_addresses: &[Vec<u8>]) -> Option<NodeAddr> {
    let node_id_bytes: [u8; 32] = node_id.try_into().ok()?;
    let node_id = NodeId::from_bytes(&node_id_bytes).ok()?;

    let direct: Vec<std::net::SocketAddr> = direct_addresses
        .iter()
        .filter_map(|address| std::str::from_utf8(address).ok()?.parse().ok())
        .collect();

    if direct.is_empty() {
        return None;
    }

    Some(NodeAddr::new(node_id).with_direct_addresses(direct))
}

/// Refresh an upgraded peer's legacy `Responded` liveness while it stays
/// registered with the Zakura supervisor.
///
/// See [`ZakuraHandshakeConnector::spawn_legacy_liveness_keeper`]. Exits when
/// the peer never registers within `appear_timeout`, when it deregisters, or
/// when the address book updater closes (node shutdown).
async fn run_legacy_liveness_keeper(
    mut registered: watch::Receiver<Vec<ZakuraPeerId>>,
    peer_id: ZakuraPeerId,
    book_addr: PeerSocketAddr,
    address_book_updater: tokio::sync::mpsc::Sender<MetaAddrChange>,
    appear_timeout: Duration,
    refresh_interval: Duration,
) {
    if !wait_for_zakura_peer(&mut registered, &peer_id, appear_timeout).await {
        return;
    }

    loop {
        // Refresh the peer's `Responded` liveness so the crawler treats it as a
        // live (Zakura) peer instead of re-dialing it over legacy TCP.
        if address_book_updater
            .send(MetaAddr::new_responded(book_addr, None))
            .await
            .is_err()
        {
            // The address book updater is gone: the node is shutting down.
            break;
        }

        // Wait for the next refresh, but wake early if the peer set changes so
        // we react to deregistration promptly.
        tokio::select! {
            changed = registered.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            _ = tokio::time::sleep(refresh_interval) => {}
        }

        if !registered.borrow().iter().any(|id| id == &peer_id) {
            // The Zakura connection deregistered: stop refreshing so the entry
            // ages out and the peer can be reconnected over legacy.
            break;
        }
    }
}

/// Wait until `peer_id` appears in the supervisor's registered set, or
/// `appear_timeout` elapses. Returns whether the peer is registered.
async fn wait_for_zakura_peer(
    registered: &mut watch::Receiver<Vec<ZakuraPeerId>>,
    peer_id: &ZakuraPeerId,
    appear_timeout: Duration,
) -> bool {
    tokio::time::timeout(appear_timeout, async {
        loop {
            if registered.borrow().iter().any(|id| id == peer_id) {
                return true;
            }
            if registered.changed().await.is_err() {
                return false;
            }
        }
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        net::{Ipv4Addr, SocketAddrV4},
        path::PathBuf,
    };

    use iroh::{
        endpoint::Connection,
        protocol::{AcceptError, ProtocolHandler, Router},
        SecretKey, Watcher as _,
    };
    use zebra_chain::parameters::Network;

    use super::*;
    use crate::CacheDir;

    #[derive(Debug, Clone)]
    struct SmokeProtocolHandler;

    impl ProtocolHandler for SmokeProtocolHandler {
        async fn accept(&self, _connection: Connection) -> Result<(), AcceptError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn iroh_endpoint_starts_without_relay_or_discovery() -> Result<(), Box<dyn Error>> {
        let secret_key = SecretKey::from_bytes(&[7; 32]);

        let endpoint = direct_endpoint_builder(secret_key)
            .bind_addr_v4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .bind()
            .await?;

        let router = Router::builder(endpoint)
            .accept(b"/zakura/smoke/0", SmokeProtocolHandler)
            .spawn();

        let addr = router.endpoint().node_addr().initialized().await;

        assert_eq!(addr.node_id, router.endpoint().node_id());
        assert!(addr.direct_addresses().next().is_some());
        assert!(addr.relay_url().is_none());
        assert!(router.endpoint().discovery().is_none());

        router.shutdown().await?;

        Ok(())
    }

    #[test]
    fn zakura_secret_key_path_uses_network_cache_dir() {
        let cache_dir = CacheDir::custom_path("/tmp/zebra-cache");

        assert_eq!(
            cache_dir
                .zakura_node_secret_key_file_path(&Network::Mainnet)
                .expect("custom cache path should be enabled"),
            PathBuf::from("/tmp/zebra-cache/network/mainnet.zakura-iroh-secret-key")
        );
    }

    fn test_peer_id() -> ZakuraPeerId {
        ZakuraPeerId::new(vec![7u8; 32]).expect("32-byte node id is within bounds")
    }

    fn test_book_addr() -> PeerSocketAddr {
        "127.0.0.1:18233"
            .parse::<std::net::SocketAddr>()
            .expect("valid socket addr")
            .into()
    }

    /// While the peer stays registered, the keeper repeatedly refreshes its
    /// `Responded` liveness; once it deregisters, the keeper stops.
    #[tokio::test]
    async fn legacy_liveness_keeper_refreshes_until_deregistered() {
        let peer_id = test_peer_id();
        let (registered_tx, registered_rx) = watch::channel(vec![peer_id.clone()]);
        let (updater_tx, mut updater_rx) = tokio::sync::mpsc::channel(16);

        let keeper = tokio::spawn(run_legacy_liveness_keeper(
            registered_rx,
            peer_id.clone(),
            test_book_addr(),
            updater_tx,
            Duration::from_secs(5),
            Duration::from_millis(20),
        ));

        // The keeper should keep marking the peer `Responded` while it is registered.
        for _ in 0..2 {
            let change = tokio::time::timeout(Duration::from_secs(1), updater_rx.recv())
                .await
                .expect("keeper refreshes liveness on a registered peer")
                .expect("the keeper holds the sender open");
            assert!(
                matches!(change, MetaAddrChange::UpdateResponded { addr, .. } if addr == test_book_addr()),
                "keeper should refresh the upgraded peer's responded liveness, got {change:?}",
            );
        }

        // Deregister the peer: the keeper must observe the change and exit.
        registered_tx.send(Vec::new()).expect("receiver is alive");
        tokio::time::timeout(Duration::from_secs(2), keeper)
            .await
            .expect("keeper exits after the peer deregisters")
            .expect("keeper task does not panic");
    }

    /// If the upgraded connection never registers, the keeper gives up without
    /// marking the peer live, so the crawler can reconnect normally.
    #[tokio::test]
    async fn legacy_liveness_keeper_exits_when_peer_never_registers() {
        let peer_id = test_peer_id();
        let (_registered_tx, registered_rx) = watch::channel(Vec::new());
        let (updater_tx, mut updater_rx) = tokio::sync::mpsc::channel(16);

        let keeper = tokio::spawn(run_legacy_liveness_keeper(
            registered_rx,
            peer_id,
            test_book_addr(),
            updater_tx,
            Duration::from_millis(50),
            Duration::from_millis(20),
        ));

        tokio::time::timeout(Duration::from_secs(2), keeper)
            .await
            .expect("keeper exits after the appear timeout")
            .expect("keeper task does not panic");

        assert!(
            updater_rx.try_recv().is_err(),
            "keeper must not refresh liveness for a peer that never registered",
        );
    }
}
