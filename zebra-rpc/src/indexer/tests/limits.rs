//! Regression tests for indexer gRPC decoding and concurrent stream limits.

use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use futures::{future::try_join_all, StreamExt};
use prost::Message;
use tokio::task::JoinHandle;
use tonic::Code;
use tower::BoxError;
use zebra_chain::{
    block::{self, Height},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    serialization::BytesInDisplayOrder,
};
use zebra_node_services::mempool::MempoolTxSubscriber;
use zebra_state::{
    NonFinalizedBlocksListener, ReadRequest, ReadResponse,
    MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS,
};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::indexer::{
    self,
    indexer_client::IndexerClient,
    server::{MAX_CONCURRENT_STREAMS, MAX_DECODING_MESSAGE_SIZE},
    BlockRequest, Empty, NonFinalizedStateChangeRequest,
};

type MockReadState = MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>;

struct TestServer {
    _server_task: JoinHandle<Result<(), BoxError>>,
    client: IndexerClient<tonic::transport::Channel>,
    mock_read_service: MockReadState,
    chain_tip_sender: MockChainTipSender,
}

async fn start_test_server() -> TestServer {
    let mock_read_service = MockService::build()
        .with_max_request_delay(Duration::from_secs(5))
        .for_unit_tests();
    let (chain_tip, chain_tip_sender) = MockChainTip::new();
    let (mempool_tx_sender, _) = tokio::sync::broadcast::channel(1);
    let mempool_subscriber = MempoolTxSubscriber::new(mempool_tx_sender);

    let (server_task, listen_addr) = indexer::server::init(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        mock_read_service.clone(),
        chain_tip,
        mempool_subscriber,
    )
    .await
    .expect("indexer gRPC test server should bind to ephemeral port");

    let endpoint = tonic::transport::Endpoint::new(format!("http://{listen_addr}"))
        .expect("loopback endpoint URI is valid")
        .timeout(Duration::from_secs(5));
    let client = IndexerClient::connect(endpoint)
        .await
        .expect("client should connect to indexer gRPC test server");

    TestServer {
        _server_task: server_task,
        client,
        mock_read_service,
        chain_tip_sender,
    }
}

#[tokio::test]
async fn oversized_requests_are_rejected_before_decoding() {
    let mut server = start_test_server().await;

    // Each empty repeated `bytes` element encodes as 2 wire bytes (tag + 0 length).
    let oversized_tips = NonFinalizedStateChangeRequest {
        chain_tip_hashes: vec![Vec::new(); MAX_DECODING_MESSAGE_SIZE / 2 + 1],
    };
    assert!(oversized_tips.encoded_len() > MAX_DECODING_MESSAGE_SIZE);

    let status = server
        .client
        .non_finalized_state_change(tonic::Request::new(oversized_tips))
        .await
        .expect_err("oversized NonFinalizedStateChangeRequest must be rejected");
    assert_eq!(status.code(), Code::OutOfRange);

    let oversized_block = BlockRequest {
        hash_or_height: vec![0u8; MAX_DECODING_MESSAGE_SIZE],
    };
    assert!(oversized_block.encoded_len() > MAX_DECODING_MESSAGE_SIZE);

    let status = server
        .client
        .get_block(tonic::Request::new(oversized_block))
        .await
        .expect_err("oversized BlockRequest must be rejected");
    assert_eq!(status.code(), Code::OutOfRange);
}

#[tokio::test]
async fn largest_valid_non_finalized_state_change_request_is_accepted() {
    let mut server = start_test_server().await;

    let expected_tips: HashSet<block::Hash> = (0..MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS)
        .map(|i| {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&u64::try_from(i).expect("index fits in u64").to_be_bytes());
            block::Hash::from_bytes_in_display_order(&bytes)
        })
        .collect();
    let request = NonFinalizedStateChangeRequest {
        chain_tip_hashes: expected_tips
            .iter()
            .map(|hash| hash.bytes_in_display_order().to_vec())
            .collect(),
    };
    assert!(request.encoded_len() <= MAX_DECODING_MESSAGE_SIZE);

    let mut client = server.client.clone();
    let request_task =
        tokio::spawn(async move { client.non_finalized_state_change(request).await });

    let (_tx, rx) = tokio::sync::mpsc::channel(1);
    server
        .mock_read_service
        .expect_request(ReadRequest::NonFinalizedBlocksListener {
            known_chain_tips: expected_tips,
        })
        .await
        .respond(ReadResponse::NonFinalizedBlocksListener(
            NonFinalizedBlocksListener(Arc::new(rx)),
        ));

    request_task
        .await
        .expect("request task should not panic")
        .expect("largest valid NonFinalizedStateChangeRequest should succeed");
}

#[tokio::test]
async fn more_than_twenty_concurrent_streams_succeed_on_single_connection() {
    const STREAM_COUNT: usize = 25;
    assert!(u32::try_from(STREAM_COUNT).expect("fits in u32") <= MAX_CONCURRENT_STREAMS);

    let server = start_test_server().await;

    // Opening 25 concurrent server streams on a single HTTP/2 channel blocks on the 21st
    // stream under the old 20-stream cap.
    let open_streams = (0..STREAM_COUNT).map(|_| {
        let mut client = server.client.clone();
        async move {
            client
                .chain_tip_change(tonic::Request::new(Empty {}))
                .await
                .map(tonic::Response::into_inner)
        }
    });

    let mut streams = tokio::time::timeout(Duration::from_secs(5), try_join_all(open_streams))
        .await
        .expect("opening 25 concurrent streams should not stall on the old 20-stream cap")
        .expect("all 25 concurrent streams should open successfully");

    server.chain_tip_sender.send_best_tip_height(Height(1));
    server
        .chain_tip_sender
        .send_best_tip_hash(block::Hash([1; 32]));

    for stream in &mut streams {
        let item = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .expect("each open stream should receive the tip update before timeout")
            .expect("stream should remain open")
            .expect("tip update should not be an error");
        assert_eq!(item.height, 1);
    }
}
