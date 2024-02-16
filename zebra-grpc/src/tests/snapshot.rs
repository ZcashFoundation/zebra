//! Snapshot tests for Zebra Scan gRPC responses.
//!
//! Currently we snapshot the `get_info` and `get_results` responses for both mainnet and testnet with an empty
//! scanner database and with a mocked one. Calls that return `Empty` responses are not snapshoted in this suite.
//!
//! To update these snapshots, run:
//! ```sh
//! cargo insta test --review
//! ```
use std::{collections::BTreeMap, thread::sleep, time::Duration};

use zebra_chain::{block::Height, parameters::Network, transaction};
use zebra_scan::tests::ZECPAGES_SAPLING_VIEWING_KEY;
use zebra_test::mock_service::MockService;

use zebra_node_services::scan_service::{
    request::Request as ScanRequest, response::Response as ScanResponse,
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

async fn test_mocked_rpc_response_data_for_network(network: Network, random_port: u16) {
    // get a mocked scan service
    let mock_scan_service = MockService::build().for_unit_tests();

    // start the gRPC server
    let listen_addr: std::net::SocketAddr = format!("127.0.0.1:{random_port}")
        .parse()
        .expect("hard-coded IP and u16 port should parse successfully");

    {
        let mock_scan_service = mock_scan_service.clone();
        tokio::spawn(async move {
            init(listen_addr, mock_scan_service)
                .await
                .expect("Possible port conflict");
        });
    }

    sleep(Duration::from_secs(1)); // wait for the server to start

    // connect to the gRPC server
    let client = ScannerClient::connect(format!("http://127.0.0.1:{random_port}"))
        .await
        .expect("server should receive connection");

    // insta settings
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(format!("{}_mocked", network_string(network)));

    // snapshot the get_info grpc call
    let get_info_response_fut = {
        let mut client = client.clone();
        let get_info_request = tonic::Request::new(Empty {});
        tokio::spawn(async move { client.get_info(get_info_request).await })
    };

    {
        let mut mock_scan_service = mock_scan_service.clone();
        tokio::spawn(async move {
            mock_scan_service
                .expect_request_that(|req| matches!(req, ScanRequest::Info))
                .await
                .respond(ScanResponse::Info {
                    min_sapling_birthday_height: network.sapling_activation_height(),
                })
        });
    }

    let get_info_response = get_info_response_fut
        .await
        .expect("tokio task should join successfully")
        .expect("get_info request should succeed");

    snapshot_rpc_getinfo(get_info_response.into_inner(), &settings);

    // snapshot the get_results grpc call

    let get_results_response_fut = {
        let mut client = client.clone();
        let get_results_request = tonic::Request::new(GetResultsRequest {
            keys: vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string()],
        });
        tokio::spawn(async move { client.get_results(get_results_request).await })
    };

    {
        let mut mock_scan_service = mock_scan_service.clone();
        tokio::spawn(async move {
            let zec_pages_sapling_efvk = ZECPAGES_SAPLING_VIEWING_KEY.to_string();
            let mut fake_results = BTreeMap::new();
            for fake_result_height in [Height::MIN, Height(1), Height::MAX] {
                fake_results.insert(
                    fake_result_height,
                    [transaction::Hash::from([0; 32])].repeat(3),
                );
            }

            let mut fake_results_response = BTreeMap::new();
            fake_results_response.insert(zec_pages_sapling_efvk, fake_results);

            mock_scan_service
                .expect_request_that(|req| matches!(req, ScanRequest::Results(_)))
                .await
                .respond(ScanResponse::Results(fake_results_response))
        });
    }

    let get_results_response = get_results_response_fut
        .await
        .expect("tokio task should join successfully")
        .expect("get_results request should succeed");

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
