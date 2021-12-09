use std::net::SocketAddr;

use futures::FutureExt;
use proptest::prelude::*;
use tower::{discover::Discover, BoxError, ServiceExt};

use zebra_chain::{block, chain_tip::ChainTip, parameters::Network};

use super::{
    BlockHeightPairAcrossNetworkUpgrades, MockedClientHandle, PeerSetBuilder, PeerVersions,
};
use crate::{
    peer::{LoadTrackedClient, MinimumPeerVersion},
    peer_set::PeerSet,
    protocol::external::types::Version,
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

        let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
        let (mut minimum_peer_version, best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(network);

        best_tip_height
            .send(Some(block_height))
            .expect("receiving endpoint lives as long as `minimum_peer_version`");

        let current_minimum_version = minimum_peer_version.current();

        runtime.block_on(async move {
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version)
                .build();

            check_if_only_up_to_date_peers_are_live(
                &mut peer_set,
                &mut handles,
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

        let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
        let (mut minimum_peer_version, best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(block_heights.network);

        best_tip_height
            .send(Some(block_heights.before_upgrade))
            .expect("receiving endpoint lives as long as `minimum_peer_version`");

        runtime.block_on(async move {
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version.clone())
                .build();

            check_if_only_up_to_date_peers_are_live(
                &mut peer_set,
                &mut handles,
                minimum_peer_version.current(),
            )?;

            best_tip_height
                .send(Some(block_heights.after_upgrade))
                .expect("receiving endpoint lives as long as `minimum_peer_version`");

            check_if_only_up_to_date_peers_are_live(
                &mut peer_set,
                &mut handles,
                minimum_peer_version.current(),
            )?;

            Ok::<_, TestCaseError>(())
        })?;
    }
}

/// Check if only peers with up-to-date protocol versions are live.
///
/// This will poll the `peer_set` to allow it to drop outdated peers, and then check the peer
/// `handles` to assert that only up-to-date peers are kept by the `peer_set`.
fn check_if_only_up_to_date_peers_are_live<D, C>(
    peer_set: &mut PeerSet<D, C>,
    handles: &mut Vec<MockedClientHandle>,
    minimum_version: Version,
) -> Result<(), TestCaseError>
where
    D: Discover<Key = SocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    // Force `poll_discover` to be called to process all discovered peers.
    let poll_result = peer_set.ready().now_or_never();
    let all_peers_are_outdated = handles
        .iter()
        .all(|handle| handle.version() < minimum_version);

    if all_peers_are_outdated {
        prop_assert!(matches!(poll_result, None));
    } else {
        prop_assert!(matches!(poll_result, Some(Ok(_))));
    }

    for handle in handles {
        let is_outdated = handle.version() < minimum_version;
        let is_connected = handle.is_connected();

        prop_assert!(
            is_connected != is_outdated,
            "is_connected: {}, is_outdated: {}",
            is_connected,
            is_outdated,
        );
    }

    Ok(())
}
