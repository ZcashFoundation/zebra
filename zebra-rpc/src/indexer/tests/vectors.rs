//! Fixed test vectors for indexer RPCs

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures::StreamExt;
use tokio::{sync::broadcast, task::JoinHandle};
use tower::BoxError;
use zebra_chain::{
    block::{Block, Height},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{self, UnminedTxId},
};
use zebra_node_services::mempool::{MempoolBatch, MempoolChange, MempoolTxSubscriber};
use zebra_state::{
    HashOrHeight, NonFinalizedBlocksListener, NonFinalizedState, ReadRequest, ReadResponse,
    WatchReceiver,
};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::indexer::{
    self, chain_state_change_message::Change, indexer_client::IndexerClient, BlockRequest,
    ChainStateChangeRequest, Empty, StateInfoProvider,
};

// The generic indexer server requires its read-state service to implement `StateInfoProvider`
// (used by `GetStateInfo`). The mock read service stands in for `ReadStateService` here, so it
// returns placeholder metadata. A local trait on an external type is allowed in this crate.
impl StateInfoProvider for MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError> {
    fn state_info(&self) -> zebra_state::StateInfo {
        zebra_state::StateInfo {
            db_path: std::path::PathBuf::new(),
            db_format_version: semver::Version::new(0, 0, 0),
            network: Network::Mainnet,
        }
    }
}

#[tokio::test]
async fn rpc_server_spawn() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (
        _server_task,
        client,
        mock_read_service,
        mock_chain_tip_sender,
        mempool_transaction_sender,
    ) = start_server_and_get_client().await?;

    test_chain_state_change(
        client.clone(),
        mock_read_service.clone(),
        &mock_chain_tip_sender,
    )
    .await?;
    test_chain_tip_change(client.clone(), &mock_chain_tip_sender).await?;
    test_mempool_change(client.clone(), mempool_transaction_sender).await?;
    test_get_block(client.clone(), mock_read_service).await?;

    Ok(())
}

/// Tests that the unified `ChainStateChange` method forwards finalized-tip-change signals.
async fn test_chain_state_change(
    mut client: IndexerClient<tonic::transport::Channel>,
    mut mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
    mock_chain_tip_sender: &MockChainTipSender,
) -> Result<()> {
    let mut response = client
        .chain_state_change(tonic::Request::new(ChainStateChangeRequest {
            chain_tip_hashes: Vec::new(),
        }))
        .await?
        .into_inner();

    // The server's task first subscribes to the non-finalized blocks listener. Respond with a real,
    // empty, open listener (it emits no blocks); keep its sender alive so it stays open.
    let (block_sender, block_receiver) =
        tokio::sync::watch::channel(NonFinalizedState::new(&Network::Mainnet));
    let listener =
        NonFinalizedBlocksListener::spawn(WatchReceiver::new(block_receiver), HashSet::new());
    mock_read_service
        .expect_request_that(|req| matches!(req, ReadRequest::NonFinalizedBlocksListener { .. }))
        .await
        .respond(ReadResponse::NonFinalizedBlocksListener(listener));

    // A change to the primary's best tip should produce a finalized-tip-change message.
    mock_chain_tip_sender.send_best_tip_height(Height::MIN);
    mock_chain_tip_sender.send_best_tip_hash(zebra_chain::block::Hash([0; 32]));

    let message = tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive a chain state change before timeout")
        .expect("response stream should not be empty")
        .expect("chain state change response should not be an error message");

    assert!(
        matches!(message.change, Some(Change::FinalizedTipChange(_))),
        "expected a finalized_tip_change message, got {:?}",
        message.change
    );

    drop(block_sender);

    Ok(())
}

/// Tests that the `GetBlock` method returns the requested block and rejects an empty request.
#[allow(deprecated)]
async fn test_get_block(
    mut client: IndexerClient<tonic::transport::Channel>,
    mut mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
) -> Result<()> {
    // A request whose bytes are neither a 32-byte hash nor a 4-byte height is rejected without
    // touching the state.
    let status = client
        .get_block(tonic::Request::new(BlockRequest {
            hash_or_height: Vec::new(),
        }))
        .await
        .expect_err("a block request without a valid hash or height should be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // A height above the maximum valid block height is rejected without touching the state.
    let status = client
        .get_block(tonic::Request::new(BlockRequest {
            hash_or_height: u32::MAX.to_be_bytes().to_vec(),
        }))
        .await
        .expect_err("an out-of-range block height should be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // A block requested by height is returned along with its hash.
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let expected_hash = block.hash();
    let height = block
        .coinbase_height()
        .expect("test block has a coinbase height");

    let mut request_client = client.clone();
    let request_task = tokio::spawn(async move {
        request_client
            .get_block(tonic::Request::new(BlockRequest {
                hash_or_height: height.0.to_be_bytes().to_vec(),
            }))
            .await
    });

    mock_read_service
        .expect_request(ReadRequest::Block(HashOrHeight::Height(height)))
        .await
        .respond(ReadResponse::Block(Some(block.clone())));

    let response = request_task
        .await?
        .expect("get_block should succeed")
        .into_inner();
    let (decoded_block, decoded_hash) = response.decode().expect("response should decode");
    assert_eq!(decoded_hash, expected_hash);
    assert_eq!(decoded_block.hash(), expected_hash);

    Ok(())
}

#[allow(deprecated)]
async fn test_chain_tip_change(
    mut client: IndexerClient<tonic::transport::Channel>,
    mock_chain_tip_sender: &MockChainTipSender,
) -> Result<()> {
    let request = tonic::Request::new(Empty {});
    let mut response = client.chain_tip_change(request).await?.into_inner();
    mock_chain_tip_sender.send_best_tip_height(Height::MIN);
    mock_chain_tip_sender.send_best_tip_hash(zebra_chain::block::Hash([0; 32]));

    // Wait for RPC server to send a message
    tokio::time::sleep(Duration::from_millis(500)).await;

    tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive chain tip change notification before timeout")
        .expect("response stream should not be empty")
        .expect("chain tip change response should not be an error message");

    Ok(())
}

async fn test_mempool_change(
    mut client: IndexerClient<tonic::transport::Channel>,
    mempool_transaction_sender: tokio::sync::broadcast::Sender<MempoolBatch>,
) -> Result<()> {
    let request = tonic::Request::new(Empty {});
    let mut response = client.mempool_change(request).await?.into_inner();

    let change_tx_ids = [UnminedTxId::Legacy(transaction::Hash::from([0; 32]))]
        .into_iter()
        .collect();

    mempool_transaction_sender
        .send(MempoolBatch::event(MempoolChange::added(change_tx_ids)))
        .expect("rpc server should have a receiver");

    tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive chain tip change notification before timeout")
        .expect("response stream should not be empty")
        .expect("chain tip change response should not be an error message");

    Ok(())
}

async fn start_server_and_get_client() -> Result<(
    JoinHandle<Result<(), BoxError>>,
    IndexerClient<tonic::transport::Channel>,
    MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
    MockChainTipSender,
    broadcast::Sender<MempoolBatch>,
)> {
    let listen_addr: std::net::SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and u16 port should parse successfully");

    let mock_read_service = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();

    let (mock_chain_tip_change, mock_chain_tip_change_sender) = MockChainTip::new();
    let (mempool_transaction_sender, _) = tokio::sync::broadcast::channel(1);
    let mempool_tx_subscriber = MempoolTxSubscriber::new(mempool_transaction_sender.clone());
    let (server_task, listen_addr) = indexer::server::init(
        listen_addr,
        mock_read_service.clone(),
        mock_chain_tip_change,
        mempool_tx_subscriber.clone(),
    )
    .await
    .map_err(|err| eyre!(err))?;

    // wait for the server to start
    tokio::time::sleep(Duration::from_secs(1)).await;

    let endpoint = tonic::transport::channel::Endpoint::new(format!("http://{listen_addr}"))
        .unwrap()
        .timeout(Duration::from_secs(2));

    // connect to the gRPC server
    let client = IndexerClient::connect(endpoint)
        .await
        .expect("server should receive connection");

    Ok((
        server_task,
        client,
        mock_read_service,
        mock_chain_tip_change_sender,
        mempool_transaction_sender,
    ))
}
