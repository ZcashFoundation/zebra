//! Regression tests for indexer gRPC decoding, concurrent stream, and subscription limits.

use std::{
    collections::HashSet,
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::future::try_join_all;
use prost::Message;
use tokio::{sync::Barrier, task::JoinHandle};
use tonic::Code;
use tower::{BoxError, Service};
use zebra_chain::{
    block::{self, Block},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    serialization::{BytesInDisplayOrder, ZcashDeserializeInto},
};
use zebra_node_services::mempool::MempoolTxSubscriber;
use zebra_state::{
    NonFinalizedBlocksListener, ReadRequest, ReadResponse, ReadState,
    MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS,
};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::indexer::{
    self,
    indexer_client::IndexerClient,
    server::{MAX_ACTIVE_SUBSCRIPTIONS, MAX_CONCURRENT_STREAMS, MAX_DECODING_MESSAGE_SIZE},
    BlockRequest, Empty, NonFinalizedStateChangeRequest,
};

type MockReadState = MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>;

struct TestServer {
    _server_task: JoinHandle<Result<(), BoxError>>,
    client: IndexerClient<tonic::transport::Channel>,
    mock_read_service: MockReadState,
    _chain_tip_sender: MockChainTipSender,
}

/// A `ReadState` service that holds concurrent `GetBlock` calls at a barrier so the test proves
/// all calls are in-flight simultaneously over one HTTP/2 connection.
#[derive(Clone)]
struct BarrierReadState {
    barrier: Arc<Barrier>,
    block: Arc<Block>,
}

impl Service<ReadRequest> for BarrierReadState {
    type Response = ReadResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ReadRequest) -> Self::Future {
        let barrier = self.barrier.clone();
        let block = self.block.clone();
        Box::pin(async move {
            match request {
                ReadRequest::Block(_) => {
                    barrier.wait().await;
                    Ok(ReadResponse::Block(Some(block)))
                }
                other => panic!("unexpected ReadRequest in BarrierReadState: {other:?}"),
            }
        })
    }
}

async fn start_test_server_with_read_state<S>(
    read_state: S,
) -> (
    JoinHandle<Result<(), BoxError>>,
    IndexerClient<tonic::transport::Channel>,
    MockChainTipSender,
)
where
    S: ReadState,
{
    let (chain_tip, chain_tip_sender) = MockChainTip::new();
    let (mempool_tx_sender, _) = tokio::sync::broadcast::channel(1);
    let mempool_subscriber = MempoolTxSubscriber::new(mempool_tx_sender);

    let (server_task, listen_addr) = indexer::server::init(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        read_state,
        chain_tip,
        mempool_subscriber,
    )
    .await
    .expect("indexer gRPC test server binds to ephemeral loopback port");

    let endpoint = tonic::transport::Endpoint::new(format!("http://{listen_addr}"))
        .expect("loopback endpoint URI is valid")
        .timeout(Duration::from_secs(5));
    let client = IndexerClient::connect(endpoint)
        .await
        .expect("client connects to bound indexer gRPC test server");

    (server_task, client, chain_tip_sender)
}

async fn start_test_server() -> TestServer {
    let mock_read_service = MockService::build()
        .with_max_request_delay(Duration::from_secs(5))
        .for_unit_tests();
    let (server_task, client, chain_tip_sender) =
        start_test_server_with_read_state(mock_read_service.clone()).await;

    TestServer {
        _server_task: server_task,
        client,
        mock_read_service,
        _chain_tip_sender: chain_tip_sender,
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

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("mainnet genesis test vector is a valid block");
    let expected_hash = block.hash().bytes_in_display_order().to_vec();
    let read_state = BarrierReadState {
        barrier: Arc::new(Barrier::new(STREAM_COUNT)),
        block,
    };
    let (_server_task, client, _tip_sender) = start_test_server_with_read_state(read_state).await;

    // 25 simultaneous unary `GetBlock` calls on one HTTP/2 channel deadlock at the barrier
    // under the old 20-stream cap because only 20 can be in-flight at once.
    let requests = (0..STREAM_COUNT).map(|_| {
        let mut client = client.clone();
        async move {
            client
                .get_block(tonic::Request::new(BlockRequest {
                    hash_or_height: 0u32.to_be_bytes().to_vec(),
                }))
                .await
                .map(tonic::Response::into_inner)
        }
    });

    let responses = tokio::time::timeout(Duration::from_secs(5), try_join_all(requests))
        .await
        .expect("25 concurrent GetBlock calls complete together past the old 20-stream cap")
        .expect("all 25 concurrent GetBlock calls succeed");

    assert_eq!(responses.len(), STREAM_COUNT);
    assert!(responses.iter().all(|block| block.hash == expected_hash));
}

#[tokio::test]
async fn active_streaming_subscriptions_are_bounded_and_release_on_drop() {
    let mut server = start_test_server().await;

    let open_chain_tips = (0..MAX_ACTIVE_SUBSCRIPTIONS - 1).map(|_| {
        let mut client = server.client.clone();
        async move {
            client
                .chain_tip_change(tonic::Request::new(Empty {}))
                .await
                .map(tonic::Response::into_inner)
        }
    });
    let mut chain_tip_streams = try_join_all(open_chain_tips)
        .await
        .expect("subscriptions within MAX_ACTIVE_SUBSCRIPTIONS succeed");
    let _mempool_stream = server
        .client
        .mempool_change(tonic::Request::new(Empty {}))
        .await
        .expect("final subscription slot succeeds")
        .into_inner();

    let tip_err = server
        .client
        .chain_tip_change(tonic::Request::new(Empty {}))
        .await
        .expect_err("21st chain_tip_change must be rejected");
    assert_eq!(tip_err.code(), Code::ResourceExhausted);

    let mempool_err = server
        .client
        .mempool_change(tonic::Request::new(Empty {}))
        .await
        .expect_err("21st mempool_change must be rejected");
    assert_eq!(mempool_err.code(), Code::ResourceExhausted);

    let nf_err = server
        .client
        .non_finalized_state_change(tonic::Request::new(NonFinalizedStateChangeRequest {
            chain_tip_hashes: Vec::new(),
        }))
        .await
        .expect_err("21st non_finalized_state_change must be rejected");
    assert_eq!(nf_err.code(), Code::ResourceExhausted);

    let malformed_err = server
        .client
        .non_finalized_state_change(tonic::Request::new(NonFinalizedStateChangeRequest {
            chain_tip_hashes: vec![vec![0u8; 1]],
        }))
        .await
        .expect_err("malformed request still returns InvalidArgument at capacity");
    assert_eq!(malformed_err.code(), Code::InvalidArgument);

    drop(chain_tip_streams.pop());

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if server
                .client
                .chain_tip_change(tonic::Request::new(Empty {}))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropping an active subscription frees a slot promptly");
}
