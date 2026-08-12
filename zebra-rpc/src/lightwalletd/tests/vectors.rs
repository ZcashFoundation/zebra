//! Fixed test vectors for the lightwalletd gRPC server.

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures::StreamExt;
use tokio::{sync::broadcast, task::JoinHandle};
use tower::{buffer::Buffer, BoxError};

use zebra_chain::{
    block::{self, Block},
    chain_sync_status::MockSyncStatus,
    chain_tip::{
        mock::{MockChainTip, MockChainTipSender},
        NoChainTip,
    },
    parameters::Network::Mainnet,
    serialization::ZcashDeserializeInto,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::mempool::{self, MempoolChange, MempoolTxSubscriber};
use zebra_state::{ReadRequest, ReadResponse};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::{
    lightwalletd::{
        self, compact_tx_streamer_client::CompactTxStreamerClient, BlockId, BlockRange, ChainSpec,
        CompactBlock, Empty,
    },
    methods::RpcImpl,
};

/// The mocked read state service the server is built on.
type MockReadState = MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>;

/// The mocked mempool service the server is built on.
type MockMempool = MockService<mempool::Request, mempool::Response, PanicAssertion, BoxError>;

/// `GetLatestBlock` must return the current best tip without reading state.
#[tokio::test]
async fn get_latest_block_returns_the_mocked_tip() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;

    let tip_height = block::Height(42);
    let tip_hash = block::Hash([0xab; 32]);
    chain_tip_sender.send_best_tip_height(tip_height);
    chain_tip_sender.send_best_tip_hash(tip_hash);

    let response = client
        .get_latest_block(tonic::Request::new(ChainSpec {}))
        .await?
        .into_inner();

    assert_eq!(response.height, u64::from(tip_height.0));
    assert_eq!(response.hash, tip_hash.0.to_vec());
    read_state.expect_no_requests().await;

    Ok(())
}

/// An empty `BlockId` must be rejected instead of being read as genesis.
#[tokio::test]
async fn get_block_rejects_an_unspecified_id_without_reading_state() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;

    let status = client
        .get_block(tonic::Request::new(BlockId {
            height: 0,
            hash: Vec::new(),
        }))
        .await
        .expect_err("an unspecified block ID must be rejected");

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert_eq!(status.message(), "block id must specify a hash or a height");
    read_state.expect_no_requests().await;

    Ok(())
}

/// `GetBlockRange` must include both bounds and stream them in ascending order.
#[tokio::test]
async fn get_block_range_is_inclusive_and_ascending() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;
    let blocks = mainnet_blocks_1_to_3()?;

    let mut stream = client
        .get_block_range(tonic::Request::new(block_range(1, 3)))
        .await?
        .into_inner();

    for (block, expected_height) in blocks.into_iter().zip(1..=3) {
        respond_with_block(&mut read_state, block).await;

        let response = next_compact_block_message(&mut stream)
            .await??
            .ok_or_else(|| eyre!("block range ended before height {expected_height}"))?;
        assert_eq!(response.height, expected_height);
    }

    assert!(next_compact_block_message(&mut stream).await??.is_none());

    Ok(())
}

/// A descending range must preserve the caller's requested order.
#[tokio::test]
async fn get_block_range_is_inclusive_and_descending() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;
    let blocks = mainnet_blocks_1_to_3()?;

    let mut stream = client
        .get_block_range(tonic::Request::new(block_range(3, 1)))
        .await?
        .into_inner();

    for (block, expected_height) in blocks.into_iter().rev().zip((1..=3).rev()) {
        respond_with_block(&mut read_state, block).await;

        let response = next_compact_block_message(&mut stream)
            .await??
            .ok_or_else(|| eyre!("block range ended before height {expected_height}"))?;
        assert_eq!(response.height, expected_height);
    }

    assert!(next_compact_block_message(&mut stream).await??.is_none());

    Ok(())
}

/// A range crossing the available state must return prior blocks, then stop with `NotFound`.
#[tokio::test]
async fn get_block_range_stops_when_a_block_is_missing() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;
    let [_block_1, block_2, block_3] = mainnet_blocks_1_to_3()?;

    let mut stream = client
        .get_block_range(tonic::Request::new(block_range(2, 4)))
        .await?
        .into_inner();

    for (block, expected_height) in [(block_2, 2), (block_3, 3)] {
        respond_with_block(&mut read_state, block).await;

        let response = next_compact_block_message(&mut stream)
            .await??
            .ok_or_else(|| eyre!("block range ended before height {expected_height}"))?;
        assert_eq!(response.height, expected_height);
    }

    read_state
        .expect_request(ReadRequest::Block(block::Height(4).into()))
        .await
        .respond(ReadResponse::Block(None));

    let status = next_compact_block_message(&mut stream)
        .await?
        .expect_err("a missing block must terminate the range with an error");
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(status.message(), "block not found");
    read_state.expect_no_requests().await;

    Ok(())
}

/// A missing note commitment tree must not be served as an empty tree.
///
/// The block read can succeed while a tree read fails, for example when a reorg
/// lands between them. Reporting a tree size of zero would tell a light client
/// that no notes exist as of that block, corrupting every note position it
/// derives from it, so the request must fail instead.
#[tokio::test]
async fn get_block_errors_when_a_commitment_tree_is_missing() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let height = block
        .coinbase_height()
        .expect("test block has a coinbase height");

    let request_task = tokio::spawn(async move {
        client
            .get_block(tonic::Request::new(BlockId {
                height: height.0.into(),
                hash: Vec::new(),
            }))
            .await
    });

    read_state
        .expect_request_that(|request| matches!(request, ReadRequest::Block(_)))
        .await
        .respond(ReadResponse::Block(Some(block)));

    // The Sapling and Orchard trees are read concurrently, so answer whichever
    // arrives first. Both are missing here.
    for _ in 0..2 {
        let handler = read_state
            .expect_request_that(|request| {
                matches!(
                    request,
                    ReadRequest::SaplingTree(_) | ReadRequest::OrchardTree(_)
                )
            })
            .await;

        let response = match handler.request() {
            ReadRequest::SaplingTree(_) => ReadResponse::SaplingTree(None),
            _ => ReadResponse::OrchardTree(None),
        };

        handler.respond(response);
    }

    let status = request_task
        .await
        .expect("request task should not panic")
        .expect_err("a missing commitment tree must not be served as a zero tree size");

    assert_ne!(status.code(), tonic::Code::Ok);

    Ok(())
}

/// A lagged mempool change subscription must not silently drop transactions.
///
/// The change channel is bounded, so a stream that falls behind loses changes and the
/// transactions they carried. Skipping them would leave the client short with no way to
/// notice, so the mempool is re-read instead.
#[tokio::test]
async fn mempool_stream_re_reads_the_mempool_after_lagging() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, _read_state, mut mempool, _chain_tip_sender, mempool_change) =
        start_server_and_get_client().await?;

    // Hold the stream open for the whole test, so the server keeps serving it.
    let _stream_task = tokio::spawn(async move {
        let response = client
            .get_mempool_stream(tonic::Request::new(Empty {}))
            .await
            .expect("the mempool stream should open");

        let mut stream = response.into_inner();
        while stream.next().await.is_some() {}
    });

    // Hold the initial snapshot open, so the change subscription falls behind while the
    // server is busy with it.
    let initial_read = mempool
        .expect_request_that(|request| matches!(request, mempool::Request::FullTransactions))
        .await;

    // The change channel holds one message, so this leaves the subscription lagging.
    for _ in 0..3 {
        let _ = mempool_change.send(MempoolChange::added(HashSet::new()));
    }

    initial_read.respond(empty_mempool_response());

    // Lagging must trigger a second read of the mempool rather than being skipped.
    mempool
        .expect_request_that(|request| matches!(request, mempool::Request::FullTransactions))
        .await
        .respond(empty_mempool_response());

    Ok(())
}

/// Returns mainnet blocks at heights 1 through 3.
fn mainnet_blocks_1_to_3() -> Result<[Arc<Block>; 3]> {
    Ok([
        zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?,
        zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?,
        zebra_test::vectors::BLOCK_MAINNET_3_BYTES.zcash_deserialize_into()?,
    ])
}

/// Returns an inclusive block range specified by height.
fn block_range(start: u64, end: u64) -> BlockRange {
    let block_id = |height| BlockId {
        height,
        hash: Vec::new(),
    };

    BlockRange {
        start: Some(block_id(start)),
        end: Some(block_id(end)),
    }
}

/// Responds to the state requests needed to build one compact block.
async fn respond_with_block(read_state: &mut MockReadState, block: Arc<Block>) {
    let height = block
        .coinbase_height()
        .expect("fixed mainnet block vectors contain a coinbase transaction");

    read_state
        .expect_request(ReadRequest::Block(height.into()))
        .await
        .respond(ReadResponse::Block(Some(block)));

    // The Sapling and Orchard trees are read concurrently, so answer whichever
    // arrives first.
    for _ in 0..2 {
        let handler = read_state
            .expect_request_that(|request| {
                matches!(
                    request,
                    ReadRequest::SaplingTree(_) | ReadRequest::OrchardTree(_)
                )
            })
            .await;

        let response = match handler.request() {
            ReadRequest::SaplingTree(_) => ReadResponse::SaplingTree(Some(Default::default())),
            _ => ReadResponse::OrchardTree(Some(Default::default())),
        };

        handler.respond(response);
    }
}

/// Returns the next compact block stream message within the test timeout.
async fn next_compact_block_message(
    stream: &mut tonic::Streaming<CompactBlock>,
) -> Result<Result<Option<CompactBlock>, tonic::Status>> {
    tokio::time::timeout(Duration::from_secs(2), stream.message())
        .await
        .map_err(|_| eyre!("timed out waiting for the next compact block stream message"))
}

/// Returns a mempool response with no transactions in it.
fn empty_mempool_response() -> mempool::Response {
    mempool::Response::FullTransactions {
        transactions: Vec::new(),
        transaction_dependencies: Default::default(),
        last_seen_tip_hash: zebra_chain::block::Hash([0; 32]),
    }
}

/// Starts the lightwalletd gRPC server against mocked services, and connects a client to it.
async fn start_server_and_get_client() -> Result<(
    JoinHandle<Result<(), BoxError>>,
    CompactTxStreamerClient<tonic::transport::Channel>,
    MockReadState,
    MockMempool,
    MockChainTipSender,
    broadcast::Sender<MempoolChange>,
)> {
    let listen_addr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and port should parse");

    let read_state: MockReadState = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let mempool: MockMempool = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    let (chain_tip, chain_tip_sender) = MockChainTip::new();
    let (mempool_change_sender, _) = broadcast::channel(1);

    let (_misbehavior_tx, misbehavior_rx) = tokio::sync::watch::channel(None);
    let (rpc, _rpc_tx_queue) = RpcImpl::new(
        Mainnet,
        Default::default(),
        Default::default(),
        "0.0.1",
        "lightwalletd test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state, 1),
        Buffer::new(read_state.clone(), 1),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        NoChainTip,
        MockAddressBookPeers::default(),
        misbehavior_rx,
        None,
    );

    let (server_task, listen_addr) = lightwalletd::server::init(
        listen_addr,
        rpc,
        read_state.clone(),
        Buffer::new(mempool.clone(), 1),
        chain_tip,
        MempoolTxSubscriber::new(mempool_change_sender.clone()),
        Mainnet,
    )
    .await
    .map_err(|error| eyre!(error))?;

    // Retry the connection instead of sleeping for a fixed time, so the test does
    // not depend on how long the server takes to start accepting.
    let endpoint = tonic::transport::channel::Endpoint::new(format!("http://{listen_addr}"))
        .expect("endpoint built from a bound address should parse")
        .timeout(Duration::from_secs(2));

    let mut client = None;
    for _ in 0..50 {
        if let Ok(connected) = CompactTxStreamerClient::connect(endpoint.clone()).await {
            client = Some(connected);
            break;
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let client = client.ok_or_else(|| eyre!("could not connect to the lightwalletd server"))?;

    Ok((
        server_task,
        client,
        read_state,
        mempool,
        chain_tip_sender,
        mempool_change_sender,
    ))
}
