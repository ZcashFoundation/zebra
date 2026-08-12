//! Fixed test vectors for the lightwalletd gRPC server.

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures::StreamExt;
use tokio::{sync::broadcast, task::JoinHandle};
use tower::{buffer::Buffer, BoxError};

use zebra_chain::{
    block::Block,
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
    lightwalletd::{self, compact_tx_streamer_client::CompactTxStreamerClient, BlockId, Empty},
    methods::RpcImpl,
};

/// The mocked read state service the server is built on.
type MockReadState = MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>;

/// The mocked mempool service the server is built on.
type MockMempool = MockService<mempool::Request, mempool::Response, PanicAssertion, BoxError>;

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

/// A `nullifiers_only` response must not read the note commitment trees.
///
/// lightwalletd's `pruneCompactBlockToNullifiers` zeroes the tree sizes, so reading
/// them costs two state reads per block on the bulk-scan path for a value the client
/// discards, and lets a reorg between the reads abort the whole stream.
#[tokio::test]
async fn get_block_nullifiers_does_not_read_the_commitment_trees() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let height = block
        .coinbase_height()
        .expect("test block has a coinbase height");

    let request_task = tokio::spawn(async move {
        client
            .get_block_nullifiers(tonic::Request::new(BlockId {
                height: height.0.into(),
                hash: Vec::new(),
            }))
            .await
    });

    read_state
        .expect_request_that(|request| matches!(request, ReadRequest::Block(_)))
        .await
        .respond(ReadResponse::Block(Some(block)));

    // Only the block was answered: if the trees were still read, this would hang.
    let compact_block = request_task
        .await
        .expect("request task should not panic")
        .expect("the block should be served without reading the trees")
        .into_inner();

    let chain_metadata = compact_block
        .chain_metadata
        .expect("compact blocks always carry chain metadata");

    assert_eq!(chain_metadata.sapling_commitment_tree_size, 0);
    assert_eq!(chain_metadata.orchard_commitment_tree_size, 0);

    Ok(())
}

/// A raw transaction larger than a block must be rejected before it is hex-encoded.
///
/// Hex-encoding doubles a client-controlled buffer, and a transaction that big can
/// never be mined.
#[tokio::test]
async fn send_transaction_rejects_oversized_raw_transactions() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, _read_state, _mempool, _chain_tip_sender, _mempool_change) =
        start_server_and_get_client().await?;

    let oversized = vec![0; zebra_chain::block::MAX_BLOCK_BYTES as usize + 1];

    let status = client
        .send_transaction(tonic::Request::new(lightwalletd::RawTransaction {
            data: oversized,
            height: 0,
        }))
        .await
        .expect_err("an oversized transaction must be rejected");

    assert_eq!(status.code(), tonic::Code::InvalidArgument);

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
