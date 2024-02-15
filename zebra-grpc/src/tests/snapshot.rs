//! Snapshot tests for Zebra Scan gRPC responses.
//!
//! Currently we snapshot the `get_info` and `get_results` responses for both mainnet and testnet with an empty
//! scanner database and with a mocked one. Calls that return `Empty` responses are not snapshoted in this suite.
//!
//! To update these snapshots, run:
//! ```sh
//! cargo insta test --review
//! ```
use std::{thread::sleep, time::Duration};
use tower::ServiceBuilder;

use zebra_chain::{block::Height, parameters::Network};
use zebra_scan::{
    service::ScanService,
    storage::db::tests::{fake_sapling_results, new_test_storage},
    tests::ZECPAGES_SAPLING_VIEWING_KEY,
    Config,
};

use crate::{
    scanner::{
        scanner_client::ScannerClient, Empty, GetResultsRequest, GetResultsResponse, InfoReply,
    },
    server::init,
};

#[tokio::test(flavor = "multi_thread")]
async fn test_grpc_response_data() {
    let _init_guard = zebra_test::init();

    tokio::join!(
        test_grpc_response_data_for_network(Network::Mainnet, zebra_test::net::random_known_port()),
        test_grpc_response_data_for_network(Network::Testnet, zebra_test::net::random_known_port()),
        test_mocked_rpc_response_data_for_network(
            Network::Mainnet,
            zebra_test::net::random_known_port()
        ),
        test_mocked_rpc_response_data_for_network(
            Network::Testnet,
            zebra_test::net::random_known_port()
        ),
    );
}

async fn test_grpc_response_data_for_network(network: Network, random_port: u16) {
    // get a state and chain tip.
    let (state, _read_state, _latest_chain_tip, chain_tip_change) =
        zebra_state::init_test_services(network);

    // create a scan service
    let scan_service = ServiceBuilder::new()
        .buffer(10)
        .service(ScanService::new(&Config::default(), network, state, chain_tip_change).await);

    // start the gRPC server
    let listen_addr: std::net::SocketAddr = format!("127.0.0.1:{random_port}").parse().unwrap();

    tokio::spawn(async move {
        init(listen_addr, scan_service).await.unwrap();
    });
    sleep(Duration::from_secs(1)); // wait for the server to start

    // connect to the gRPC server
    let mut client = ScannerClient::connect(format!("http://127.0.0.1:{random_port}"))
        .await
        .unwrap();

    // insta settings
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(format!("{}_empty", network_string(network)));

    // snapshot the get_info grpc call
    let get_info_request = tonic::Request::new(Empty {});
    let get_info_response = client.get_info(get_info_request).await.unwrap();
    snapshot_rpc_getinfo(get_info_response.into_inner(), &settings);

    // snapshot the get_results grpc call
    let get_results_request = tonic::Request::new(GetResultsRequest {
        keys: vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string()],
    });
    let get_results_response = client.get_results(get_results_request).await.unwrap();
    snapshot_rpc_getresults(get_results_response.into_inner(), &settings);
}

async fn test_mocked_rpc_response_data_for_network(network: Network, random_port: u16) {
    // introduce fake results to the database
    let mut db = new_test_storage(network);
    let zec_pages_sapling_efvk = ZECPAGES_SAPLING_VIEWING_KEY.to_string();
    for fake_result_height in [Height::MIN, Height(1), Height::MAX] {
        db.insert_sapling_results(
            &zec_pages_sapling_efvk,
            fake_result_height,
            fake_sapling_results([
                zebra_state::TransactionIndex::MIN,
                zebra_state::TransactionIndex::from_index(40),
                zebra_state::TransactionIndex::MAX,
            ]),
        );
    }

    // get a mocked scan service
    let (scan_service, _cmd_receiver) = ScanService::new_with_mock_scanner(db.clone());
    let scan_service = ServiceBuilder::new().buffer(10).service(scan_service);

    // start the gRPC server
    let listen_addr: std::net::SocketAddr = format!("127.0.0.1:{random_port}").parse().unwrap();
    tokio::spawn(async move {
        init(listen_addr, scan_service).await.unwrap();
    });
    sleep(Duration::from_secs(1)); // wait for the server to start

    // connect to the gRPC server
    let mut client = ScannerClient::connect(format!("http://127.0.0.1:{random_port}"))
        .await
        .unwrap();

    // insta settings
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(format!("{}_mocked", network_string(network)));

    // snapshot the get_info grpc call
    let get_info_request = tonic::Request::new(Empty {});
    let get_info_response = client.get_info(get_info_request).await.unwrap();
    snapshot_rpc_getinfo(get_info_response.into_inner(), &settings);

    // snapshot the get_results grpc call
    let get_results_request = tonic::Request::new(GetResultsRequest {
        keys: vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string()],
    });
    let get_results_response = client.get_results(get_results_request).await.unwrap();
    snapshot_rpc_getresults(get_results_response.into_inner(), &settings);
}

/// Snapshot `getinfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getinfo(info: InfoReply, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_info", info));
}

/// Snapshot `getresults` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getresults(results: GetResultsResponse, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_results", results));
}

/// Utility function to convert a `Network` to a lowercase string.
// TODO: move this to a common location.
fn network_string(network: Network) -> String {
    let mut net_suffix = network.to_string();
    net_suffix.make_ascii_lowercase();
    net_suffix
}
