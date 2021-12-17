use tower::{Service, ServiceExt};

use zebra_chain::parameters::{Network, NetworkUpgrade};

use super::{PeerSetBuilder, PeerVersions};
use crate::{peer::MinimumPeerVersion, protocol::external::types::Version, Request};

#[test]
fn peer_set_drop() {
    //
    let peer_versions = PeerVersions {
        peer_versions: vec![Version::min_specified_for_upgrade(
            Network::Mainnet,
            NetworkUpgrade::Canopy,
        )],
    };

    //
    let runtime = zebra_test::init_async();

    //
    let (discovered_peers, _handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(Network::Mainnet);

    //
    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .build();

        // Get a ready future
        let peer_ready_future = peer_set.ready();
        // Drop the future
        std::mem::drop(peer_ready_future);

        // Wait until the peer set is ready awaiting in the ready() future
        let peer_ready1 = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Make a call to the peer set that returns a future
        let fut = peer_ready1.call(Request::Peers);
        // Drop the future
        std::mem::drop(fut);

        // Peer set will still be ready
        let peer_ready2 = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Get a new future with a call to the peer set
        let _fut = peer_ready2.call(Request::Peers);
    });
}
