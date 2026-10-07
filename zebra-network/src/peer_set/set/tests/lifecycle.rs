//! Connection-lifetime regressions for find-response stall tracking.

use std::{net::SocketAddr, time::Duration};

use futures::{channel::mpsc, FutureExt};
use tokio::time::timeout;
use tower::{discover::Change, Service, ServiceExt};

use zebra_chain::parameters::Network;

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ClientTestHarness, MinimumPeerVersion, PeerError},
    peer_set::stall_tracker::FIND_RESPONSE_STALL_THRESHOLD,
    BoxError, PeerSocketAddr, Request, Response,
};

use super::PeerSetBuilder;

/// A replacement connection does not inherit stalls from an errored ready service.
#[tokio::test]
async fn reconnect_after_ready_error_starts_without_stalls() {
    let _test_guard = zebra_test::init();
    let address: PeerSocketAddr = "127.0.0.1:8233".parse::<SocketAddr>().unwrap().into();
    let (discovery_sender, discovery) = mpsc::unbounded::<Result<_, BoxError>>();
    let (minimum_peer_version, _chain_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
        .with_discover(discovery)
        .with_minimum_peer_version(minimum_peer_version)
        .build();
    let (client, mut original) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .finish();

    discovery_sender
        .unbounded_send(Ok(Change::Insert(address, client.into())))
        .unwrap();

    for _ in 0..FIND_RESPONSE_STALL_THRESHOLD - 1 {
        let response = timeout(Duration::from_secs(5), peer_set.ready())
            .await
            .unwrap()
            .unwrap()
            .call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });
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
        timeout(Duration::from_secs(5), response)
            .await
            .unwrap()
            .unwrap();
    }

    timeout(Duration::from_secs(5), peer_set.ready())
        .await
        .unwrap()
        .unwrap();
    original.set_error(PeerError::ConnectionClosed);
    let _ = peer_set.ready().now_or_never();
    assert!(
        !original.wants_connection_heartbeats(),
        "the failed connection was removed"
    );

    let (client, mut replacement) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .finish();
    discovery_sender
        .unbounded_send(Ok(Change::Insert(address, client.into())))
        .unwrap();

    let response = timeout(Duration::from_secs(5), peer_set.ready())
        .await
        .unwrap()
        .unwrap()
        .call(Request::FindBlocks {
            known_blocks: vec![],
            stop: None,
        });
    replacement
        .try_to_receive_outbound_client_request()
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

    let _ = peer_set.ready().now_or_never();
    assert!(
        replacement.wants_connection_heartbeats(),
        "one stall must not disconnect the replacement connection"
    );
}
