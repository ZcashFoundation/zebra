//! Unit tests for gRPC methods
use std::{collections::BTreeMap, thread::sleep, time::Duration};

use tonic::transport::Channel;

use zebra_chain::{block::Height, parameters::Network, transaction};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    net::random_known_port,
};

use crate::{
    scanner::{
        scanner_client::ScannerClient, ClearResultsRequest, DeleteKeysRequest, Empty,
        GetResultsRequest, GetResultsResponse, InfoReply, KeyWithHeight, RegisterKeysRequest,
        RegisterKeysResponse,
    },
    server::{init, MAX_KEYS_PER_REQUEST},
};
use zebra_node_services::scan_service::{
    request::Request as ScanRequest, response::Response as ScanResponse,
};

/// The extended Sapling viewing key of [ZECpages](https://zecpages.com/boardinfo)
pub const ZECPAGES_SAPLING_VIEWING_KEY: &str = "zxviews1q0duytgcqqqqpqre26wkl45gvwwwd706xw608hucmvfalr759ejwf7qshjf5r9aa7323zulvz6plhttp5mltqcgs9t039cx2d09mgq05ts63n8u35hyv6h9nc9ctqqtue2u7cer2mqegunuulq2luhq3ywjcz35yyljewa4mgkgjzyfwh6fr6jd0dzd44ghk0nxdv2hnv4j5nxfwv24rwdmgllhe0p8568sgqt9ckt02v2kxf5ahtql6s0ltjpkckw8gtymxtxuu9gcr0swvz";

/// Test the gRPC methods with mocked responses
#[tokio::test(flavor = "multi_thread")]
async fn test_grpc_methods_mocked() {
    let _init_guard = zebra_test::init();

    let (client, mock_scan_service) = start_server_and_get_client(random_known_port()).await;

    test_get_results_errors(client.clone()).await;
    test_register_keys_errors(client.clone()).await;
    test_clear_results_errors(client.clone()).await;
    test_delete_keys_errors(client.clone()).await;

    for network in Network::iter() {
        test_mocked_getinfo_for_network(&client, &mock_scan_service, network).await;
        test_mocked_getresults_for_network(&client, &mock_scan_service, network).await;
        test_mocked_register_keys_for_network(&client, &mock_scan_service, network).await;
        test_mocked_clear_results_for_network(&client, &mock_scan_service, network).await;
        test_mocked_delete_keys_for_network(&client, &mock_scan_service, network).await;
    }
}

/// Test the `get_info` gRPC method with a mocked service response.
async fn test_mocked_getinfo_for_network(
    client: &ScannerClient<Channel>,
    mock_scan_service: &MockService<ScanRequest, ScanResponse, PanicAssertion>,
    network: Network,
) {
    // create request, fake results and get response
    let get_info_response = call_get_info(client.clone(), mock_scan_service.clone(), network).await;

    // test the response
    assert_eq!(
        get_info_response.into_inner().min_sapling_birthday_height,
        network.sapling_activation_height().0,
        "get_info response min sapling height should match network sapling activation height"
    );
}

/// Test the `get_results` gRPC method with populated and empty results
async fn test_mocked_getresults_for_network(
    client: &ScannerClient<Channel>,
    mock_scan_service: &MockService<ScanRequest, ScanResponse, PanicAssertion>,
    network: Network,
) {
    // create request, fake populated results and get response
    let get_results_response =
        call_get_results(client.clone(), mock_scan_service.clone(), network, false).await;

    // test the response
    let transaction_heights = get_results_response
        .into_inner()
        .results
        .first_key_value()
        .expect("should have at least 1 value")
        .1
        .by_height
        .len();
    match network {
        Network::Mainnet => {
            assert_eq!(
                transaction_heights, 3,
                "there should be 3 transaction heights"
            );
        }
        Network::Testnet => {
            assert_eq!(
                transaction_heights, 1,
                "there should be 1 transaction height"
            );
        }
    }

    // create request, fake empty results and get response
    let get_results_response =
        call_get_results(client.clone(), mock_scan_service.clone(), network, true).await;

    // test the response
    let is_results_empty = get_results_response
        .into_inner()
        .results
        .first_key_value()
        .expect("should have at least 1 value")
        .1
        .by_height
        .is_empty();

    assert!(is_results_empty, "results should be empty");
}

/// Test the `register_keys` gRPC method
async fn test_mocked_register_keys_for_network(
    client: &ScannerClient<Channel>,
    mock_scan_service: &MockService<ScanRequest, ScanResponse, PanicAssertion>,
    network: Network,
) {
    // create request, fake return value and get response
    let register_keys_response =
        call_register_keys(client.clone(), mock_scan_service.clone(), network).await;

    // test the response
    assert_eq!(
        register_keys_response.into_inner().keys,
        vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string()],
        "keys should match mocked response"
    );
}

/// Test the `clear_results` gRPC method
async fn test_mocked_clear_results_for_network(
    client: &ScannerClient<Channel>,
    mock_scan_service: &MockService<ScanRequest, ScanResponse, PanicAssertion>,
    network: Network,
) {
    // create request, fake results and get response
    let get_results_response =
        call_get_results(client.clone(), mock_scan_service.clone(), network, false).await;

    // test the response
    let transaction_heights = get_results_response
        .into_inner()
        .results
        .first_key_value()
        .unwrap()
        .1
        .by_height
        .len();
    match network {
        Network::Mainnet => {
            assert_eq!(transaction_heights, 3);
        }
        Network::Testnet => {
            assert_eq!(transaction_heights, 1);
        }
    }

    // create request, fake results and get response
    let clear_results_response =
        call_clear_results(client.clone(), mock_scan_service.clone(), network).await;

    // test the response
    assert_eq!(clear_results_response.into_inner(), Empty {});

    // create request, fake results and get response
    let get_results_response =
        call_get_results(client.clone(), mock_scan_service.clone(), network, true).await;

    // test the response
    let is_results_empty = get_results_response
        .into_inner()
        .results
        .first_key_value()
        .unwrap()
        .1
        .by_height
        .is_empty();

    assert!(is_results_empty, "results should be empty");
}

/// Test the `delete_keys` gRPC method
async fn test_mocked_delete_keys_for_network(
    client: &ScannerClient<Channel>,
    mock_scan_service: &MockService<ScanRequest, ScanResponse, PanicAssertion>,
    network: Network,
) {
    // create request, fake results and get response
    let register_keys_response =
        call_register_keys(client.clone(), mock_scan_service.clone(), network).await;

    // test the response
    assert_eq!(
        register_keys_response.into_inner().keys,
        vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string()]
    );

    // create request, fake results and get response
    let get_results_response =
        call_get_results(client.clone(), mock_scan_service.clone(), network, false).await;
    let transaction_heights = get_results_response
        .into_inner()
        .results
        .first_key_value()
        .unwrap()
        .1
        .by_height
        .len();
    match network {
        Network::Mainnet => {
            assert_eq!(transaction_heights, 3);
        }
        Network::Testnet => {
            assert_eq!(transaction_heights, 1);
        }
    }

    let delete_keys_response =
        call_delete_keys(client.clone(), mock_scan_service.clone(), network).await;
    // test the response
    assert_eq!(delete_keys_response.into_inner(), Empty {});

    let get_results_response =
        call_get_results(client.clone(), mock_scan_service.clone(), network, true).await;
    let is_results_empty = get_results_response
        .into_inner()
        .results
        .first_key_value()
        .unwrap()
        .1
        .by_height
        .is_empty();

    assert!(is_results_empty, "results should be empty");
}

/// Start the gRPC server, get a client and a mock service
async fn start_server_and_get_client(
    random_port: u16,
) -> (
    ScannerClient<tonic::transport::Channel>,
    MockService<ScanRequest, ScanResponse, PanicAssertion>,
) {
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

    // wait for the server to start
    sleep(Duration::from_secs(1));

    // connect to the gRPC server
    let client = ScannerClient::connect(format!("http://127.0.0.1:{random_port}"))
        .await
        .expect("server should receive connection");

    (client, mock_scan_service)
}

/// Add fake populated results to the mock scan service
async fn add_fake_populated_results(
    mock_scan_service: MockService<ScanRequest, ScanResponse, PanicAssertion>,
    network: Network,
) {
    let mut mock_scan_service = mock_scan_service.clone();
    tokio::spawn(async move {
        let zec_pages_sapling_efvk = ZECPAGES_SAPLING_VIEWING_KEY.to_string();
        let mut fake_results = BTreeMap::new();
        let heights = match network {
            Network::Mainnet => vec![Height::MIN, Height(1), Height::MAX],
            Network::Testnet => vec![Height::MIN],
        };
        for fake_result_height in heights {
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

/// Add fake empty results to the mock scan service
async fn add_fake_empty_results(
    mock_scan_service: MockService<ScanRequest, ScanResponse, PanicAssertion>,
    _network: Network,
) {
    let mut mock_scan_service = mock_scan_service.clone();
    tokio::spawn(async move {
        let zec_pages_sapling_efvk = ZECPAGES_SAPLING_VIEWING_KEY.to_string();
        let fake_results = BTreeMap::new();
        let mut fake_results_response = BTreeMap::new();
        fake_results_response.insert(zec_pages_sapling_efvk, fake_results);

        mock_scan_service
            .expect_request_that(|req| matches!(req, ScanRequest::Results(_)))
            .await
            .respond(ScanResponse::Results(fake_results_response))
    });
}

/// Call the `get_results` gRPC method, mock and return the response
async fn call_get_results(
    client: ScannerClient<Channel>,
    mock_scan_service: MockService<ScanRequest, ScanResponse, PanicAssertion>,
    network: Network,
    empty_results: bool,
) -> tonic::Response<GetResultsResponse> {
    let get_results_response_fut = {
        let mut client = client.clone();
        let get_results_request = tonic::Request::new(GetResultsRequest {
            keys: vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string()],
        });
        tokio::spawn(async move { client.get_results(get_results_request).await })
    };

    if empty_results {
        add_fake_empty_results(mock_scan_service, network).await;
    } else {
        add_fake_populated_results(mock_scan_service, network).await;
    }

    get_results_response_fut
        .await
        .expect("tokio task should join successfully")
        .expect("get_results request should succeed")
}

/// Calls the `get_results` gRPC method with bad request data and asserts that it returns errors
async fn test_get_results_errors(mut client: ScannerClient<Channel>) {
    let request = tonic::Request::new(GetResultsRequest { keys: vec![] });
    let response = client.get_results(request).await;

    let response_error_code = response
        .expect_err("calling get_results with no keys should return an error")
        .code();

    assert_eq!(
        response_error_code,
        tonic::Code::InvalidArgument,
        "error code should be an invalid argument error"
    );

    let request = tonic::Request::new(GetResultsRequest {
        keys: vec![
            ZECPAGES_SAPLING_VIEWING_KEY.to_string();
            MAX_KEYS_PER_REQUEST
                .checked_add(1)
                .expect("should fit in usize")
        ],
    });
    let response = client.get_results(request).await;
    let response_error_code = response
        .expect_err("calling get_results too with many keys should return an error")
        .code();

    assert_eq!(
        response_error_code,
        tonic::Code::InvalidArgument,
        "error code should be an invalid argument error"
    );
}

/// Call the `get_info` gRPC method, mock and return the response
async fn call_get_info(
    client: ScannerClient<Channel>,
    mock_scan_service: MockService<ScanRequest, ScanResponse, PanicAssertion>,
    network: Network,
) -> tonic::Response<InfoReply> {
    let get_info_response_fut = {
        let mut client = client.clone();
        let get_info_request = tonic::Request::new(Empty {});
        tokio::spawn(async move { client.get_info(get_info_request).await })
    };

    let mut mock_scan_service = mock_scan_service.clone();
    tokio::spawn(async move {
        mock_scan_service
            .expect_request_that(|req| matches!(req, ScanRequest::Info))
            .await
            .respond(ScanResponse::Info {
                min_sapling_birthday_height: network.sapling_activation_height(),
            })
    });

    get_info_response_fut
        .await
        .expect("tokio task should join successfully")
        .expect("get_info request should succeed")
}

/// Call the `register_keys` gRPC method, mock and return the response
async fn call_register_keys(
    client: ScannerClient<Channel>,
    mock_scan_service: MockService<ScanRequest, ScanResponse, PanicAssertion>,
    _network: Network,
) -> tonic::Response<RegisterKeysResponse> {
    let key_with_height = KeyWithHeight {
        key: ZECPAGES_SAPLING_VIEWING_KEY.to_string(),
        height: None,
    };

    let register_keys_response_fut = {
        let mut client = client.clone();
        let register_keys_request = tonic::Request::new(RegisterKeysRequest {
            keys: vec![key_with_height],
        });
        tokio::spawn(async move { client.register_keys(register_keys_request).await })
    };

    let mut mock_scan_service = mock_scan_service.clone();
    tokio::spawn(async move {
        mock_scan_service
            .expect_request_that(|req| matches!(req, ScanRequest::RegisterKeys(_)))
            .await
            .respond(ScanResponse::RegisteredKeys(vec![
                ZECPAGES_SAPLING_VIEWING_KEY.to_string(),
            ]))
    });

    register_keys_response_fut
        .await
        .expect("tokio task should join successfully")
        .expect("register_keys request should succeed")
}

/// Calls the `register_keys` gRPC method with bad request data and asserts that it returns errors
async fn test_register_keys_errors(client: ScannerClient<Channel>) {
    let key_with_height = KeyWithHeight {
        key: ZECPAGES_SAPLING_VIEWING_KEY.to_string(),
        height: None,
    };

    let fut = {
        let mut client = client.clone();
        let request = tonic::Request::new(RegisterKeysRequest { keys: vec![] });
        tokio::spawn(async move { client.register_keys(request).await })
    };

    let response = fut.await.expect("tokio task should join successfully");
    assert!(response.is_err());
    assert_eq!(response.err().unwrap().code(), tonic::Code::InvalidArgument);

    let fut = {
        let mut client = client.clone();
        let request = tonic::Request::new(RegisterKeysRequest {
            keys: vec![key_with_height; 11],
        });
        tokio::spawn(async move { client.register_keys(request).await })
    };

    let response = fut.await.expect("tokio task should join successfully");
    assert!(response.is_err());
    assert_eq!(response.err().unwrap().code(), tonic::Code::InvalidArgument);
}

async fn call_clear_results(
    client: ScannerClient<Channel>,
    mock_scan_service: MockService<ScanRequest, ScanResponse, PanicAssertion>,
    _network: Network,
) -> tonic::Response<Empty> {
    let clear_results_response_fut = {
        let mut client = client.clone();
        let clear_results_request = tonic::Request::new(ClearResultsRequest {
            keys: vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string()],
        });
        tokio::spawn(async move { client.clear_results(clear_results_request).await })
    };

    let mut mock_scan_service = mock_scan_service.clone();
    tokio::spawn(async move {
        mock_scan_service
            .expect_request_that(|req| matches!(req, ScanRequest::ClearResults(_)))
            .await
            .respond(ScanResponse::ClearedResults)
    });

    clear_results_response_fut
        .await
        .expect("tokio task should join successfully")
        .expect("register_keys request should succeed")
}

/// Calls the `clear_results` gRPC method with bad request data and asserts that it returns errors
async fn test_clear_results_errors(client: ScannerClient<Channel>) {
    let fut = {
        let mut client = client.clone();
        let request = tonic::Request::new(ClearResultsRequest { keys: vec![] });
        tokio::spawn(async move { client.clear_results(request).await })
    };

    let response = fut.await.expect("tokio task should join successfully");
    assert!(response.is_err());
    assert_eq!(response.err().unwrap().code(), tonic::Code::InvalidArgument);

    let fut = {
        let mut client = client.clone();
        let request = tonic::Request::new(ClearResultsRequest {
            keys: vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string(); 11],
        });
        tokio::spawn(async move { client.clear_results(request).await })
    };

    let response = fut.await.expect("tokio task should join successfully");
    assert!(response.is_err());
    assert_eq!(response.err().unwrap().code(), tonic::Code::InvalidArgument);
}

/// Call the `delete_keys` gRPC method, mock and return the response
async fn call_delete_keys(
    client: ScannerClient<Channel>,
    mock_scan_service: MockService<ScanRequest, ScanResponse, PanicAssertion>,
    _network: Network,
) -> tonic::Response<Empty> {
    let delete_keys_response_fut = {
        let mut client = client.clone();
        let delete_keys_request = tonic::Request::new(DeleteKeysRequest {
            keys: vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string()],
        });
        tokio::spawn(async move { client.delete_keys(delete_keys_request).await })
    };

    let mut mock_scan_service = mock_scan_service.clone();
    tokio::spawn(async move {
        mock_scan_service
            .expect_request_that(|req| matches!(req, ScanRequest::DeleteKeys(_)))
            .await
            .respond(ScanResponse::DeletedKeys)
    });
    delete_keys_response_fut
        .await
        .expect("tokio task should join successfully")
        .expect("delete_keys request should succeed")
}

/// Calls the `delete_keys` gRPC method with bad request data and asserts that it returns errors
async fn test_delete_keys_errors(client: ScannerClient<Channel>) {
    let fut = {
        let mut client = client.clone();
        let request = tonic::Request::new(DeleteKeysRequest { keys: vec![] });
        tokio::spawn(async move { client.delete_keys(request).await })
    };

    let response = fut.await.expect("tokio task should join successfully");
    assert!(response.is_err());
    assert_eq!(response.err().unwrap().code(), tonic::Code::InvalidArgument);

    let fut = {
        let mut client = client.clone();
        let request = tonic::Request::new(DeleteKeysRequest {
            keys: vec![ZECPAGES_SAPLING_VIEWING_KEY.to_string(); 11],
        });
        tokio::spawn(async move { client.delete_keys(request).await })
    };

    let response = fut.await.expect("tokio task should join successfully");
    assert!(response.is_err());
    assert_eq!(response.err().unwrap().code(), tonic::Code::InvalidArgument);
}
