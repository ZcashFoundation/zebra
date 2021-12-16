use futures::FutureExt;
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

        // Wait until the peer set is ready
        let peer_set1 = peer_set.ready().now_or_never();

        //
        match peer_set1 {
            Some(ready) => {
                // make a call that returns a future
                let fut = ready
                    .expect("peer set service is always ready")
                    .call(Request::Peers);
                // drop the future
                std::mem::drop(fut);

                // Peer set will still be ready
                let peer_set2 = peer_set.ready().now_or_never();
                match peer_set2 {
                    Some(ready2) => {
                        // make a new call
                        let _response = ready2
                            .expect("peer set service is always ready")
                            .call(Request::Peers)
                            .now_or_never();
                    }
                    None => panic!("peer set service is always ready"),
                }
            }
            None => panic!("peer set service is always ready"),
        }
    });
}
