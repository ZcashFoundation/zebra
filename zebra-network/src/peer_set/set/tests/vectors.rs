use tower::{Service, ServiceExt};

use zebra_chain::parameters::{Network, NetworkUpgrade};

use super::{PeerSetBuilder, PeerVersions};
use crate::{
    peer::{MinimumPeerVersion, ReceiveRequestAttempt},
    protocol::external::types::Version,
    Request,
};

#[test]
fn peer_set_drop() {
    // We are going to use just one peer version in this test
    let peer_versions = PeerVersions {
        peer_versions: vec![Version::min_specified_for_upgrade(
            Network::Mainnet,
            NetworkUpgrade::Canopy,
        )],
    };

    // Start the runtime
    let runtime = zebra_test::init_async();
    let _guard = runtime.enter();

    // Get peers and client handles of them
    let (discovered_peers, handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(Network::Mainnet);

    // We will just use the first peer handle
    let mut client_handle = handles
        .into_iter()
        .next()
        .expect("we always have at least one client");

    // Client did not received anything yet
    assert_eq!(
        std::mem::discriminant(&client_handle.try_to_receive_outbound_client_request()),
        std::mem::discriminant(&ReceiveRequestAttempt::Empty)
    );

    // Build a peerset
    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .build();

        // Get a ready future
        let peer_ready_future = peer_set.ready();

        // Drop the future
        std::mem::drop(peer_ready_future);

        // Peer set will remain ready for requests
        let peer_ready1 = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Make sure the client did not received anything yet
        assert_eq!(
            std::mem::discriminant(&client_handle.try_to_receive_outbound_client_request()),
            std::mem::discriminant(&ReceiveRequestAttempt::Empty)
        );

        // Make a call to the peer set that returns a future
        let fut = peer_ready1.call(Request::Peers);

        // Client received the request
        match client_handle.try_to_receive_outbound_client_request() {
            ReceiveRequestAttempt::Request(client_request) => {
                assert_eq!(client_request.request, Request::Peers)
            }
            _ => unreachable!(),
        };

        // Drop the future
        std::mem::drop(fut);

        // Peer set will remain ready for requests
        let peer_ready2 = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Get a new future calling a different request than before
        let _fut = peer_ready2.call(Request::MempoolTransactionIds);

        // Client received the request
        match client_handle.try_to_receive_outbound_client_request() {
            ReceiveRequestAttempt::Request(client_request) => {
                assert_eq!(client_request.request, Request::MempoolTransactionIds)
            }
            _ => unreachable!(),
        };
    });
}
