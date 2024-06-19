//! Snapshot tests for Zebra Scan gRPC responses.
//!
//! Currently we snapshot the `get_info` and `get_results` responses for both mainnet and testnet with a
//! mocked scanner database. Calls that return `Empty` responses are not snapshotted in this suite.
//!
//! To update these snapshots, run:
//! ```sh
//! cargo insta test --review --delete-unreferenced-snapshots
//! ```
use std::{collections::BTreeMap, thread::sleep, time::Duration};

use zebra_chain::{block::Height, parameters::Network, transaction};
use zebra_test::mock_service::MockService;

use zebra_node_services::scan_service::{
    request::Request as ScanRequest,
    response::{Response as ScanResponse, ScanResult},
};

use crate::{
    scanner::{
        self, scanner_client::ScannerClient, Empty, GetResultsRequest, GetResultsResponse,
        InfoReply, KeyWithHeight,
    },
    server::init,
};

/// The extended Sapling viewing key of [ZECpages](https://zecpages.com/boardinfo)
pub const ZECPAGES_SAPLING_VIEWING_KEY: &str = "zxviews1q0duytgcqqqqpqre26wkl45gvwwwd706xw608hucmvfalr759ejwf7qshjf5r9aa7323zulvz6plhttp5mltqcgs9t039cx2d09mgq05ts63n8u35hyv6h9nc9ctqqtue2u7cer2mqegunuulq2luhq3ywjcz35yyljewa4mgkgjzyfwh6fr6jd0dzd44ghk0nxdv2hnv4j5nxfwv24rwdmgllhe0p8568sgqt9ckt02v2kxf5ahtql6s0ltjpkckw8gtymxtxuu9gcr0swvz";

#[tokio::test(flavor = "multi_thread")]
async fn test_grpc_response_data() {
    let _init_guard = zebra_test::init();

    tokio::join!(
        test_mocked_rpc_response_data_for_network(Network::Mainnet,),
        test_mocked_rpc_response_data_for_network(Network::new_default_testnet(),),
    );
}

async fn test_mocked_rpc_response_data_for_network(network: Network) {
    // get a mocked scan service
    let mock_scan_service = MockService::build().for_unit_tests();

    // start the gRPC server
    let listen_addr: std::net::SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and u16 port should parse successfully");

    let (_server_task, listen_addr) = {
        let mock_scan_service = mock_scan_service.clone();
        tokio::spawn(init(listen_addr, mock_scan_service))
            .await
            .expect("task should join successfully")
            .expect("should spawn tonic server")
    };

    // wait for the server to start
    sleep(Duration::from_secs(1));

    // connect to the gRPC server
    let client = ScannerClient::connect(format!("http://{listen_addr}"))
        .await
        .expect("server should receive connection");

    // insta settings
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(network.lowercase_name());

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

    // snapshot the get_info grpc call

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

    // snapshot the scan grpc method

    let scan_response_fut = {
        let mut client = client.clone();
        let get_results_request = tonic::Request::new(scanner::ScanRequest {
            keys: vec![KeyWithHeight {
                key: ZECPAGES_SAPLING_VIEWING_KEY.to_string(),
                height: None,
            }],
        });
        tokio::spawn(async move { client.scan(get_results_request).await })
    };

    let (fake_results_sender, fake_results_receiver) = tokio::sync::mpsc::channel(1);

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
            fake_results_response.insert(zec_pages_sapling_efvk.clone(), fake_results);

            mock_scan_service
                .expect_request_that(|req| matches!(req, ScanRequest::RegisterKeys(_)))
                .await
                .respond(ScanResponse::RegisteredKeys(vec![]));

            mock_scan_service
                .expect_request_that(|req| matches!(req, ScanRequest::SubscribeResults(_)))
                .await
                .respond(ScanResponse::SubscribeResults(fake_results_receiver));

            mock_scan_service
                .expect_request_that(|req| matches!(req, ScanRequest::Results(_)))
                .await
                .respond(ScanResponse::Results(fake_results_response));
        });
    }

    let scan_response = scan_response_fut
        .await
        .expect("tokio task should join successfully")
        .expect("get_results request should succeed");

    let mut scan_response_stream = scan_response.into_inner();

    let scan_response_message = scan_response_stream
        .message()
        .await
        .expect("scan response message should be ok")
        .expect("scan response message should be some");

    snapshot_rpc_scan("cached", scan_response_message, &settings);

    fake_results_sender
        .send(ScanResult {
            key: ZECPAGES_SAPLING_VIEWING_KEY.to_string(),
            height: Height::MIN,
            tx_id: transaction::Hash::from([0; 32]),
        })
        .await
        .expect("should send fake result successfully");

    let scan_response_message = scan_response_stream
        .message()
        .await
        .expect("scan response message should be ok")
        .expect("scan response message should be some");

    snapshot_rpc_scan("subscribed", scan_response_message, &settings);
}

/// Snapshot `getinfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getinfo(info: InfoReply, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_info", info));
}

/// Snapshot `getresults` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getresults(results: GetResultsResponse, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_results", results));
}

/// Snapshot `scan` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_scan(
    variant: &'static str,
    scan_response: scanner::ScanResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!(format!("scan_{variant}"), scan_response));
}
