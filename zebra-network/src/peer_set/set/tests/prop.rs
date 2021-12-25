use std::net::SocketAddr;

use futures::FutureExt;
use proptest::prelude::*;
use tower::{discover::Discover, BoxError, ServiceExt};

use zebra_chain::{
    block,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
};

use super::{BlockHeightPairAcrossNetworkUpgrades, PeerSetBuilder, PeerVersions};
use crate::{
    peer::{ClientTestHarness, LoadTrackedClient, MinimumPeerVersion, ReceiveRequestAttempt},
    peer_set::PeerSet,
    protocol::external::types::Version,
    Request,
};

proptest! {
    /// Check if discovered outdated peers are immediately dropped by the [`PeerSet`].
    #[test]
    fn only_non_outdated_peers_are_accepted(
        network in any::<Network>(),
        block_height in any::<block::Height>(),
        peer_versions in any::<PeerVersions>(),
    ) {
        let runtime = zebra_test::init_async();

        let (mut minimum_peer_version, best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(network);

        best_tip_height
            .send(Some(block_height))
            .expect("receiving endpoint lives as long as `minimum_peer_version`");

        let current_minimum_version = minimum_peer_version.current();

        runtime.block_on(async move {
            let (discovered_peers, mut harnesses) = peer_versions.mock_peer_discovery();
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version)
                .build();

            check_if_only_up_to_date_peers_are_live(
                &mut peer_set,
                &mut harnesses,
                current_minimum_version,
            )?;

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Check if peers that become outdated after a network upgrade are dropped by the [`PeerSet`].
    #[test]
    fn outdated_peers_are_dropped_on_network_upgrade(
        block_heights in any::<BlockHeightPairAcrossNetworkUpgrades>(),
        peer_versions in any::<PeerVersions>(),
    ) {
        let runtime = zebra_test::init_async();

        let (mut minimum_peer_version, best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(block_heights.network);

        best_tip_height
            .send(Some(block_heights.before_upgrade))
            .expect("receiving endpoint lives as long as `minimum_peer_version`");

        runtime.block_on(async move {
            let (discovered_peers, mut harnesses) = peer_versions.mock_peer_discovery();
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version.clone())
                .build();

            check_if_only_up_to_date_peers_are_live(
                &mut peer_set,
                &mut harnesses,
                minimum_peer_version.current(),
            )?;

            best_tip_height
                .send(Some(block_heights.after_upgrade))
                .expect("receiving endpoint lives as long as `minimum_peer_version`");

            check_if_only_up_to_date_peers_are_live(
                &mut peer_set,
                &mut harnesses,
                minimum_peer_version.current(),
            )?;

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test if requests are broadcasted to the right number of peers.
    #[test]
    fn broadcast_to_peers(
        peer_versions in any::<PeerVersions>(),
    ) {
        // Get a dummy block hash to help us construct a valid request to be broadcasted
        let block: block::Block = zebra_test::vectors::BLOCK_MAINNET_10_BYTES
            .zcash_deserialize_into()
            .unwrap();
        let block_hash = block::Hash::from(&block);

        // Get the current valid peer version
        let current_mainnet_version = Version::min_specified_for_upgrade(
            Network::Mainnet,
            NetworkUpgrade::Canopy
        );

        // Start the runtime
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        // Get peers and handles
        let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
        let (minimum_peer_version, _best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(Network::Mainnet);

        // Build a peerset
        runtime.block_on(async move {
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version.clone())
                .build();

            // Get the total number of active peers
            let total_number_of_active_peers = check_if_only_up_to_date_peers_are_live(
                &mut peer_set,
                &mut handles,
                current_mainnet_version,
            )?;

            // Exit the test early if the total number of active peers is zero
            if total_number_of_active_peers == 0 {
                return Ok::<_, TestCaseError>(());
            }

            // Get the total number of peers to broadcast
            let number_of_peers_to_broadcast = peer_set.number_of_peers_to_broadcast();

            // Send a request to all peers
            let _ = peer_set.route_broadcast(Request::AdvertiseBlock(block_hash));

            // Check how many peers received the request
            let mut received = 0;
            for mut h in handles {
                if let ReceiveRequestAttempt::Request(client_request) = h.try_to_receive_outbound_client_request() {
                    assert_eq!(client_request.request, Request::AdvertiseBlock(block_hash));
                    received += 1;
                };
            }

            // Make sure the message wass broadcasted to the right number of peers
            assert_eq!(received, number_of_peers_to_broadcast);

            Ok::<_, TestCaseError>(())
        })?;
    }
}

/// Check if only peers with up-to-date protocol versions are live.
///
/// This will poll the `peer_set` to allow it to drop outdated peers, and then check the peer
/// `harnesses` to assert that only up-to-date peers are kept by the `peer_set`.
/// Returns the number of up-to-date peers on success.
fn check_if_only_up_to_date_peers_are_live<D, C>(
    peer_set: &mut PeerSet<D, C>,
    harnesses: &mut Vec<ClientTestHarness>,
    minimum_version: Version,
) -> Result<usize, TestCaseError>
where
    D: Discover<Key = SocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    // Force `poll_discover` to be called to process all discovered peers.
    let poll_result = peer_set.ready().now_or_never();
    let all_peers_are_outdated = harnesses
        .iter()
        .all(|harness| harness.version() < minimum_version);

    if all_peers_are_outdated {
        prop_assert!(matches!(poll_result, None));
    } else {
        prop_assert!(matches!(poll_result, Some(Ok(_))));
    }

    let mut number_of_connected_peers = 0;
    for harness in harnesses {
        let is_outdated = harness.version() < minimum_version;
        let is_connected = harness.wants_connection_heartbeats();

        prop_assert!(
            is_connected != is_outdated,
            "is_connected: {}, is_outdated: {}",
            is_connected,
            is_outdated,
        );

        if is_connected {
            number_of_connected_peers += 1;
        }
    }

    Ok(number_of_connected_peers)
}
