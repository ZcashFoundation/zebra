//! Fixed test vectors for request routing by connection capability.

use std::sync::Arc;

use zebra_chain::{block::Height, state_snapshot::ObjectHash};

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ClientTestHarness, LoadTrackedClient, RemoteHandshake},
    protocol::{external::types::PeerServices, v2::handshake::InitRecord},
    Request,
};

/// A client for a peer that advertised `services`, over a version 2
/// connection if `v2`, and over a legacy one otherwise, with the harness that
/// keeps its mocked tasks alive.
fn test_client(services: PeerServices, v2: bool) -> (LoadTrackedClient, ClientTestHarness) {
    let (mut client, harness) = ClientTestHarness::build().with_services(services).finish();
    if v2 {
        let mut connection_info = (*client.connection_info).clone();
        connection_info.remote = RemoteHandshake::Init(InitRecord {
            version: CURRENT_NETWORK_PROTOCOL_VERSION,
            services,
            nonce: Default::default(),
            user_agent: "/v2 routing test/".to_string(),
            start_height: Height(0),
            relay: true,
            announce: false,
            full_ids: false,
        });
        client.connection_info = Arc::new(connection_info);
    }
    (LoadTrackedClient::from(client), harness)
}

/// Object requests are only routed to version 2 peers that advertise
/// `NODE_SYNC_ARTIFACTS`, whatever else they advertise: a legacy connection
/// never supports them, whatever service bits the peer set, because the legacy
/// protocol has no `get-object` and bit 4 meant something else there. The
/// mempool snapshot is still a legacy-only request, and other requests go to
/// either protocol.
#[tokio::test]
async fn object_requests_route_only_to_v2_artifact_servers() {
    let _init_guard = zebra_test::init();

    let object = Request::Object {
        hash: ObjectHash([0; 32]),
        offset: 0,
        length: 1,
    };
    for (services, v2, supported) in [
        (PeerServices::NODE_SYNC_ARTIFACTS, true, true),
        (
            PeerServices::NODE_NETWORK | PeerServices::NODE_SYNC_ARTIFACTS,
            true,
            true,
        ),
        (PeerServices::NODE_NETWORK, true, false),
        (PeerServices::empty(), true, false),
        (
            PeerServices::NODE_NETWORK | PeerServices::NODE_SYNC_ARTIFACTS,
            false,
            false,
        ),
        (PeerServices::all(), false, false),
    ] {
        let (client, _harness) = test_client(services, v2);
        assert_eq!(
            client.supports(&object),
            supported,
            "{services:?} over {}",
            if v2 { "version 2" } else { "legacy" }
        );
        assert_eq!(client.supports(&Request::MempoolTransactionIds), !v2);
        // Version 2 address records go to the version 2 address segment instead.
        assert_eq!(client.supports(&Request::Peers), !v2);
    }
}
