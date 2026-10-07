//! Connection-lifecycle tests and their shared peer-set scenario.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{channel::mpsc, FutureExt as _};
use indexmap::IndexMap;
use proptest::prop_assert;
use proptest::test_runner::TestCaseError;
use proptest_derive::Arbitrary;
use tokio::{sync::watch, time::timeout};
use tower::{discover::Change, BoxError, Service as _, ServiceExt as _};

use zebra_chain::{
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::Network,
};
use zebra_test::prelude::eyre::{self, eyre};

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ClientTestHarness, MinimumPeerVersion, TrackedClient},
    peer_set::{stall_tracker::FIND_RESPONSE_STALL_THRESHOLD, PeerSet},
    PeerSocketAddr, Request, Response,
};

use super::super::ResponseFuture;
use super::{PeerSetBuilder, PeerSetGuard};

mod prop;

/// A discovery notification for an individually controlled mock connection.
type DiscoveryEvent = Result<Change<PeerSocketAddr, TrackedClient>, BoxError>;

/// A peer set with channel-driven discovery and an unknown chain tip.
type TestPeerSet = PeerSet<mpsc::UnboundedReceiver<DiscoveryEvent>, MockChainTip>;

/// Owns mock discovery and peer-set dependencies without owning the test's tracing guard.
struct TestScenario {
    request_kind: FindRequestKind,
    peer_set: TestPeerSet,
    discovery_sender: mpsc::UnboundedSender<DiscoveryEvent>,
    bans_sender: watch::Sender<Arc<IndexMap<IpAddr, Instant>>>,
    address: PeerSocketAddr,
    _chain_tip: MockChainTipSender,
    _peer_set_guard: PeerSetGuard,
}

impl TestScenario {
    /// Creates a peer set with stall tracking enabled by its unknown chain tip.
    fn new(request_kind: FindRequestKind) -> Self {
        let address = SocketAddr::from(([127, 0, 0, 1], 8233)).into();
        let (discovery_sender, discovery) = mpsc::unbounded();
        let (bans_sender, bans_receiver) = watch::channel(Arc::new(IndexMap::new()));
        let (minimum_peer_version, chain_tip) =
            MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (peer_set, peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovery)
            .with_minimum_peer_version(minimum_peer_version)
            .with_bans_receiver(bans_receiver)
            .build();

        Self {
            request_kind,
            peer_set,
            discovery_sender,
            bans_sender,
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
            .expect("the scenario retains the discovery receiver");

        handle
    }

    /// Bans the test address without polling connection maintenance.
    fn ban(&mut self) {
        self.bans_sender.send_modify(|bans| {
            Arc::make_mut(bans).insert(self.address.ip(), Instant::now());
        });
    }

    /// Waits for a ready connection, bounding unexpected mock setup failures.
    async fn ready(&mut self) -> eyre::Result<()> {
        timeout(Duration::from_secs(5), self.peer_set.ready())
            .await?
            .map_err(|error| eyre!(error))?;

        Ok(())
    }

    /// Polls event processing and connection maintenance without waiting for readiness.
    fn poll(&mut self) {
        let _ = self.peer_set.ready().now_or_never();
    }

    /// Routes a find request while leaving its response future under test control.
    async fn request(&mut self) -> eyre::Result<ResponseFuture> {
        self.ready().await?;

        Ok(self.peer_set.call(self.request_kind.request()))
    }

    /// Checks that a full fresh allowance is needed to reach the stall threshold.
    ///
    /// Records stalls directly, exhausting the allowance without routing requests
    /// or disconnecting the peer.
    fn assert_fresh_stall_allowance(&mut self) -> Result<(), TestCaseError> {
        for _ in 0..FIND_RESPONSE_STALL_THRESHOLD - 1 {
            prop_assert!(
                !self
                    .peer_set
                    .find_response_stalls
                    .record_stall(self.address),
                "cleanup must restore the full stall allowance",
            );
        }

        prop_assert!(
            self.peer_set
                .find_response_stalls
                .record_stall(self.address),
            "fresh stalls must reach the threshold after cleanup",
        );

        Ok(())
    }

    /// Processes `stall_count` empty responses, checking that each leaves `peer` connected.
    async fn stall_without_disconnecting(
        &mut self,
        peer: &mut ClientTestHarness,
        stall_count: usize,
    ) -> Result<(), TestCaseError> {
        for stall_index in 0..stall_count {
            self.stall(peer)
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            self.poll();

            prop_assert!(
                peer.wants_connection_heartbeats(),
                "the connection must remain connected after stall {} of {stall_count}",
                stall_index + 1,
            );
        }

        Ok(())
    }

    /// Completes one find request with an empty response, leaving event processing pending.
    async fn stall(&mut self, peer: &mut ClientTestHarness) -> eyre::Result<()> {
        let response = self.request().await?;

        peer.try_to_receive_outbound_client_request()
            .request()
            .ok_or_else(|| eyre!("expected an outbound find request"))?
            .tx
            .send(Ok(self.request_kind.empty_response()))
            .map_err(|_| eyre!("the find response receiver was dropped"))?;

        timeout(Duration::from_secs(5), response)
            .await?
            .map_err(|error| eyre!(error))?;

        Ok(())
    }
}

/// The find request and response pair exercised by a lifecycle scenario.
#[derive(Arbitrary, Clone, Copy, Debug)]
enum FindRequestKind {
    Blocks,
    Headers,
}

impl FindRequestKind {
    /// Creates a request with an empty locator and no stop hash.
    fn request(self) -> Request {
        match self {
            Self::Blocks => Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            },
            Self::Headers => Request::FindHeaders {
                known_blocks: vec![],
                stop: None,
            },
        }
    }

    /// Creates an empty response matching this request kind.
    fn empty_response(self) -> Response {
        match self {
            Self::Blocks => Response::BlockHashes {
                hashes: vec![],
                feedback: None,
            },
            Self::Headers => Response::BlockHeaders(vec![]),
        }
    }
}
