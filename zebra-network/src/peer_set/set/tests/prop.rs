//! Randomised property tests for the peer set.

use std::net::SocketAddr;

use futures::FutureExt;
use proptest::prelude::*;
use tower::{discover::Discover, BoxError, ServiceExt};

use zebra_chain::{
    block, chain_tip::ChainTip, parameters::Network, serialization::ZcashDeserializeInto,
};

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ClientTestHarness, LoadTrackedClient, MinimumPeerVersion, ReceiveRequestAttempt},
    peer_set::PeerSet,
    protocol::external::types::Version,
    PeerSocketAddr, Request,
};

use super::{BlockHeightPairAcrossNetworkUpgrades, PeerSetBuilder, PeerVersions};

proptest! {
    /// Check if discovered outdated peers are immediately dropped by the [`PeerSet`].
    #[test]
    fn only_non_outdated_peers_are_accepted(
        network in any::<Network>(),
        block_height in any::<block::Height>(),
        peer_versions in any::<PeerVersions>(),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();

        let (mut minimum_peer_version, best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(&network);

        best_tip_height.send_best_tip_height(block_height);

        let current_minimum_version = minimum_peer_version.current();

        runtime.block_on(async move {
            let (discovered_peers, mut harnesses) = peer_versions.mock_peer_discovery();
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version)
                .max_conns_per_ip(usize::MAX)
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
        let (runtime, _init_guard) = zebra_test::init_async();

        let (mut minimum_peer_version, best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(&block_heights.network);

        best_tip_height.send_best_tip_height(block_heights.before_upgrade);

        runtime.block_on(async move {
            let (discovered_peers, mut harnesses) = peer_versions.mock_peer_discovery();
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version.clone())
                .max_conns_per_ip(usize::MAX)
                .build();

            check_if_only_up_to_date_peers_are_live(
                &mut peer_set,
                &mut harnesses,
                minimum_peer_version.current(),
            )?;

            best_tip_height.send_best_tip_height(block_heights.after_upgrade);

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
        total_number_of_peers in (1..100usize)
    ) {
        // Get a dummy block hash to help us construct a valid request to be broadcasted
        let block: block::Block = zebra_test::vectors::BLOCK_MAINNET_10_BYTES
            .zcash_deserialize_into()
            .unwrap();
        let block_hash = block::Hash::from(&block);

        // Start the runtime
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();

        let peer_versions = vec![CURRENT_NETWORK_PROTOCOL_VERSION; total_number_of_peers];
        let peer_versions = PeerVersions {
            peer_versions,
        };

        // Get peers and handles
        let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
        let (minimum_peer_version, _best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

        // Build a peerset
        runtime.block_on(async move {
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version.clone())
                .max_conns_per_ip(usize::MAX)
                .build();

            // Get the total number of active peers
            let total_number_of_active_peers = check_if_only_up_to_date_peers_are_live(
                &mut peer_set,
                &mut handles,
                CURRENT_NETWORK_PROTOCOL_VERSION,
            )?;

            // Since peer addresses are unique, and versions are valid, every peer should be active
            prop_assert_eq!(total_number_of_peers, total_number_of_active_peers);

            // Get the number of peers to broadcast
            let number_of_peers_to_broadcast = peer_set.number_of_peers_to_broadcast();

            // The number of peers to broadcast should be at least 1,
            // and if possible, it should be less than the number of ready peers.
            // (Since there are no requests, all active peers should be ready.)
            prop_assert!(number_of_peers_to_broadcast >= 1);
            if total_number_of_active_peers > 1 {
                prop_assert!(number_of_peers_to_broadcast < total_number_of_active_peers);
            }

            // Send a request to all peers
            let response_future = peer_set.route_broadcast(Request::AdvertiseBlock(block_hash));
            std::mem::drop(response_future);

            // Check how many peers received the request
            let mut received = 0;
            for mut h in handles {
                if let ReceiveRequestAttempt::Request(client_request) = h.try_to_receive_outbound_client_request() {
                    prop_assert_eq!(client_request.request, Request::AdvertiseBlock(block_hash));
                    received += 1;
                };
            }

            // Make sure the message was broadcasted to the right number of peers
            prop_assert_eq!(received, number_of_peers_to_broadcast);

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test the peerset will always broadcast iff there is at least one
    /// peer in the set.
    #[test]
    fn peerset_always_broadcasts(
        total_number_of_peers in (2..10usize)
    ) {
        // Get a dummy block hash to help us construct a valid request to be broadcasted
        let block: block::Block = zebra_test::vectors::BLOCK_MAINNET_10_BYTES
            .zcash_deserialize_into()
            .unwrap();
        let block_hash = block::Hash::from(&block);

        // Start the runtime
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();

        // All peers will have the current version
        let peer_versions = vec![CURRENT_NETWORK_PROTOCOL_VERSION; total_number_of_peers];
        let peer_versions = PeerVersions {
            peer_versions,
        };

        // Get peers and handles
        let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
        let (minimum_peer_version, _best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

        runtime.block_on(async move {
            // Build a peerset
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version.clone())
                .max_conns_per_ip(usize::MAX)
                .build();

            // Remove peers, test broadcast until there is only 1 peer left in the peerset
            for port in 1u16..total_number_of_peers as u16 {
                peer_set.remove(&SocketAddr::new([127, 0, 0, 1].into(), port).into());
                handles.remove(0);

                // poll the peers
                check_if_only_up_to_date_peers_are_live(
                    &mut peer_set,
                    &mut handles,
                    CURRENT_NETWORK_PROTOCOL_VERSION,
                )?;

                // Get the new number of active peers after removal
                let number_of_peers_to_broadcast = peer_set.number_of_peers_to_broadcast();

                // Send a request to all peers we have now
                let response_future = peer_set.route_broadcast(Request::AdvertiseBlock(block_hash));
                std::mem::drop(response_future);

                // Check how many peers received the request
                let mut received = 0;
                for h in &mut handles {
                    if let ReceiveRequestAttempt::Request(client_request) = h.try_to_receive_outbound_client_request() {
                        prop_assert_eq!(client_request.request, Request::AdvertiseBlock(block_hash));
                        received += 1;
                    };
                }

                // Make sure the message is always broadcasted to the right number of peers
                prop_assert_eq!(received, number_of_peers_to_broadcast);
            }

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test the peerset panics if a request is sent and no more peers are available.
    #[test]
    #[should_panic(expected = "requests must be routed to at least one peer")]
    fn panics_when_broadcasting_to_no_peers(
        total_number_of_peers in (2..10usize)
    ) {
        // Get a dummy block hash to help us construct a valid request to be broadcasted
        let block: block::Block = zebra_test::vectors::BLOCK_MAINNET_10_BYTES
            .zcash_deserialize_into()
            .unwrap();
        let block_hash = block::Hash::from(&block);

        // Start the runtime
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();

        // All peers will have the current version
        let peer_versions = vec![CURRENT_NETWORK_PROTOCOL_VERSION; total_number_of_peers];
        let peer_versions = PeerVersions {
            peer_versions,
        };

        // Get peers and handles
        let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
        let (minimum_peer_version, _best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

        runtime.block_on(async move {
            // Build a peerset
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version.clone())
                .max_conns_per_ip(usize::MAX)
                .build();

            // Remove peers
            for port in 1u16..=total_number_of_peers as u16 {
                peer_set.remove(&SocketAddr::new([127, 0, 0, 1].into(), port).into());
                handles.remove(0);
            }

            // this will panic as expected
            let response_future = peer_set.route_broadcast(Request::AdvertiseBlock(block_hash));
            std::mem::drop(response_future);

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
    D: Discover<Key = PeerSocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    // Force `poll_discover` to be called to process all discovered peers.
    let poll_result = peer_set.ready().now_or_never();
    let all_peers_are_outdated = harnesses
        .iter()
        .all(|harness| harness.remote_version() < minimum_version);

    if all_peers_are_outdated {
        prop_assert!(poll_result.is_none());
    } else {
        prop_assert!(matches!(poll_result, Some(Ok(_))));
    }

    let mut number_of_connected_peers = 0;
    for harness in harnesses {
        let is_outdated = harness.remote_version() < minimum_version;
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
