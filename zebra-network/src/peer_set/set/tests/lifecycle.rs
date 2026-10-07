//! Connection-lifetime regressions for find-response stall tracking.

use std::{net::SocketAddr, time::Duration};

use futures::{channel::mpsc, FutureExt};
use tokio::time::timeout;
use tower::{discover::Change, Service, ServiceExt};

use zebra_chain::{
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::Network,
};

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ClientTestHarness, MinimumPeerVersion, PeerError, TrackedClient},
    peer_set::{stall_tracker::FIND_RESPONSE_STALL_THRESHOLD, PeerSet},
    BoxError, PeerSocketAddr, Request, Response,
};

use super::{super::ResponseFuture, PeerSetBuilder, PeerSetGuard};

/// A replacement connection does not inherit stalls from an errored ready service.
#[tokio::test]
async fn reconnect_after_ready_error_starts_without_stalls() {
    let _test_guard = zebra_test::init();
    let mut harness = Harness::new();
    let mut original = harness.connect();

    for _ in 0..FIND_RESPONSE_STALL_THRESHOLD - 1 {
        harness.stall(&mut original).await;
    }

    harness.ready().await;
    original.set_error(PeerError::ConnectionClosed);
    harness.poll();
    assert!(
        !original.wants_connection_heartbeats(),
        "the failed connection was removed"
    );

    let mut replacement = harness.connect();
    harness.stall(&mut replacement).await;
    harness.poll();
    assert!(
        replacement.wants_connection_heartbeats(),
        "one stall must not disconnect the replacement connection"
    );
}

/// A response future from a removed connection cannot stall its replacement.
#[tokio::test]
async fn delayed_old_response_cannot_stall_replacement() {
    let _test_guard = zebra_test::init();
    let mut harness = Harness::new();
    let mut original = harness.connect();
    let old_response = harness.request().await;
    original
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap()
        .tx
        .send(Ok(Response::BlockHashes {
            hashes: vec![],
            feedback: None,
        }))
        .unwrap();

    harness.ready().await;
    original.set_error(PeerError::ConnectionClosed);
    harness.poll();
    assert!(!original.wants_connection_heartbeats());

    let mut replacement = harness.connect();
    for _ in 0..FIND_RESPONSE_STALL_THRESHOLD - 1 {
        harness.stall(&mut replacement).await;
    }
    harness.ready().await;

    timeout(Duration::from_secs(5), old_response)
        .await
        .unwrap()
        .unwrap();
    harness.poll();
    assert!(
        replacement.wants_connection_heartbeats(),
        "a delayed old response must not count as a replacement stall"
    );
}

/// A late guard drop does not erase stalls accumulated by a replacement connection.
#[tokio::test]
async fn delayed_old_closure_preserves_replacement_stalls() {
    let _test_guard = zebra_test::init();
    let mut harness = Harness::new();
    let _original = harness.connect();
    harness.ready().await;

    // Hold the old service outside the ready map to delay its guard's drop.
    let old_service = harness
        .peer_set
        .take_ready_service(&harness.address)
        .unwrap();
    let mut replacement = harness.connect();
    for _ in 0..FIND_RESPONSE_STALL_THRESHOLD - 1 {
        harness.stall(&mut replacement).await;
    }
    harness.ready().await;

    drop(old_service);
    harness.poll();
    harness.stall(&mut replacement).await;
    harness.poll();
    assert!(
        !replacement.wants_connection_heartbeats(),
        "old cleanup must not erase the replacement's accumulated stalls"
    );
}

/// Rejecting a duplicate connection preserves the admitted connection's stall count.
#[tokio::test]
async fn rejected_duplicate_preserves_current_stalls() {
    let _test_guard = zebra_test::init();
    let mut harness = Harness::new();
    let mut original = harness.connect();
    for _ in 0..FIND_RESPONSE_STALL_THRESHOLD - 1 {
        harness.stall(&mut original).await;
    }
    harness.ready().await;

    let mut duplicate = harness.connect();
    harness.poll();
    harness.poll();
    assert!(
        !duplicate.wants_connection_heartbeats(),
        "the duplicate was rejected"
    );
    assert!(
        original.wants_connection_heartbeats(),
        "the admitted connection remains"
    );

    harness.stall(&mut original).await;
    harness.poll();
    assert!(
        !original.wants_connection_heartbeats(),
        "duplicate rejection must not reset the admitted connection's stalls"
    );
}

/// A discovery notification for an individually controlled mock connection.
type DiscoveryEvent = Result<Change<PeerSocketAddr, TrackedClient>, BoxError>;

/// A peer set with channel-driven discovery and an unknown chain tip.
type TestPeerSet = PeerSet<mpsc::UnboundedReceiver<DiscoveryEvent>, MockChainTip>;

/// Owns mock discovery and peer-set dependencies without owning the test's tracing guard.
struct Harness {
    peer_set: TestPeerSet,
    discovery_sender: mpsc::UnboundedSender<DiscoveryEvent>,
    address: PeerSocketAddr,
    _chain_tip: MockChainTipSender,
    _peer_set_guard: PeerSetGuard,
}

impl Harness {
    /// Creates a peer set with stall tracking enabled by its unknown chain tip.
    fn new() -> Self {
        let address = "127.0.0.1:8233".parse::<SocketAddr>().unwrap().into();
        let (discovery_sender, discovery) = mpsc::unbounded();
        let (minimum_peer_version, chain_tip) =
            MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (peer_set, peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovery)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        Self {
            peer_set,
            discovery_sender,
            address,
            _chain_tip: chain_tip,
            _peer_set_guard: peer_set_guard,
        }
    }

    /// Announces a new connection at the test address without polling admission.
    fn connect(&mut self) -> ClientTestHarness {
        let (client, handle) = ClientTestHarness::build()
            .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
            .finish();
        self.discovery_sender
            .unbounded_send(Ok(Change::Insert(self.address, client.into())))
            .unwrap();

        handle
    }

    /// Waits for a ready connection, bounding unexpected mock setup failures.
    async fn ready(&mut self) {
        timeout(Duration::from_secs(5), self.peer_set.ready())
            .await
            .unwrap()
            .unwrap();
    }

    /// Polls event processing and connection maintenance without waiting for readiness.
    fn poll(&mut self) {
        let _ = self.peer_set.ready().now_or_never();
    }

    /// Routes a find request while leaving its response future under test control.
    async fn request(&mut self) -> ResponseFuture {
        self.ready().await;
        self.peer_set.call(Request::FindBlocks {
            known_blocks: vec![],
            stop: None,
        })
    }

    /// Completes one find request with an empty response, leaving event processing pending.
    async fn stall(&mut self, peer: &mut ClientTestHarness) {
        let response = self.request().await;
        peer.try_to_receive_outbound_client_request()
            .request()
            .unwrap()
            .tx
            .send(Ok(Response::BlockHashes {
                hashes: vec![],
                feedback: None,
            }))
            .unwrap();
        timeout(Duration::from_secs(5), response)
            .await
            .unwrap()
            .unwrap();
    }
}
