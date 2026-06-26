//! Fixed test vectors for indexer RPCs

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

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
use zebra_node_services::mempool::{
    self, replica_digest, MempoolBatch, MempoolChange, MempoolTxSubscriber, QueuedStage,
};
use zebra_state::{
    HashOrHeight, NonFinalizedBlocksListener, NonFinalizedState, ReadRequest, ReadResponse,
    WatchReceiver,
};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::indexer::{
    self, chain_state_change_message::Change, indexer_client::IndexerClient, mempool_event,
    mempool_removed, BlockRequest, ChainStateChangeRequest, Empty, StateInfoProvider,
};

/// A mock mempool tower service used to drive the `SyncMempool` handler in tests.
type MockMempool = MockService<mempool::Request, mempool::Response, PanicAssertion, BoxError>;

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
        mock_mempool,
        mempool_transaction_sender,
    ) = start_server_and_get_client().await?;

    test_chain_state_change(
        client.clone(),
        mock_read_service.clone(),
        &mock_chain_tip_sender,
    )
    .await?;
    test_chain_tip_change(client.clone(), &mock_chain_tip_sender).await?;
    test_sync_mempool_bootstrap(client.clone(), mock_mempool.clone()).await?;
    test_sync_mempool_live_cycle(
        client.clone(),
        mock_mempool.clone(),
        mempool_transaction_sender.clone(),
    )
    .await?;
    test_sync_mempool_reorg(
        client.clone(),
        mock_mempool.clone(),
        mempool_transaction_sender.clone(),
    )
    .await?;
    test_sync_mempool_lagged_connection(
        client.clone(),
        mock_mempool.clone(),
        mempool_transaction_sender,
    )
    .await?;
    test_get_block(client.clone(), mock_read_service).await?;

    Ok(())
}

/// A test [`UnminedTxId`] derived from a single byte.
fn txid(byte: u8) -> UnminedTxId {
    UnminedTxId::Legacy(transaction::Hash::from([byte; 32]))
}

/// An empty bootstrap-state response, for tests that start from an empty mempool.
fn empty_bootstrap_state() -> mempool::Response {
    mempool::Response::MempoolBootstrapState {
        queued: HashMap::new(),
        verified: Vec::new(),
        rejected: Vec::new(),
    }
}

/// Tests that `SyncMempool` replays the current state as a bootstrap burst terminated by a batch
/// with `initial_sync_complete=true` and the projection checksum.
async fn test_sync_mempool_bootstrap(
    mut client: IndexerClient<tonic::transport::Channel>,
    mut mock_mempool: MockMempool,
) -> Result<()> {
    let mut response = client.sync_mempool(Empty {}).await?.into_inner();

    // The server reads a consistent point-in-time bootstrap snapshot. Respond with a single queued
    // transaction so the burst is non-empty.
    let mut queued = HashMap::new();
    queued.insert(txid(1), QueuedStage::AwaitingDownload);
    let expected_checksum = replica_digest(&HashSet::new(), &queued);

    mock_mempool
        .expect_request(mempool::Request::MempoolBootstrapState)
        .await
        .respond(mempool::Response::MempoolBootstrapState {
            queued,
            verified: Vec::new(),
            rejected: Vec::new(),
        });

    let batch = tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive a bootstrap batch before timeout")
        .expect("response stream should not be empty")
        .expect("bootstrap batch should not be an error message");

    assert!(
        batch.initial_sync_complete,
        "the terminal bootstrap batch sets initial_sync_complete"
    );
    assert_eq!(
        batch.checksum,
        Some(expected_checksum.to_vec()),
        "the terminal bootstrap batch carries the projection checksum"
    );
    assert!(
        batch
            .events
            .iter()
            .any(|event| matches!(&event.event, Some(mempool_event::Event::Queued(_)))),
        "the bootstrap burst replays the queued set as a Queued event"
    );

    Ok(())
}

/// Tests that after bootstrap, each live change cycle is forwarded as one batch carrying its
/// post-cycle checksum.
async fn test_sync_mempool_live_cycle(
    mut client: IndexerClient<tonic::transport::Channel>,
    mut mock_mempool: MockMempool,
    mempool_transaction_sender: broadcast::Sender<MempoolBatch>,
) -> Result<()> {
    let mut response = client.sync_mempool(Empty {}).await?.into_inner();
    mock_mempool
        .expect_request(mempool::Request::MempoolBootstrapState)
        .await
        .respond(empty_bootstrap_state());

    // Consume the terminal bootstrap batch.
    let bootstrap = tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive a bootstrap batch before timeout")
        .expect("response stream should not be empty")
        .expect("bootstrap batch should not be an error message");
    assert!(bootstrap.initial_sync_complete);

    // A live change cycle: a queued transaction, with the source's post-cycle checksum.
    let mut queued = HashMap::new();
    queued.insert(txid(2), QueuedStage::AwaitingVerification);
    let checksum = replica_digest(&HashSet::new(), &queued);
    mempool_transaction_sender
        .send(MempoolBatch::new(
            vec![MempoolChange::queued_awaiting_verification(
                [txid(2)].into_iter().collect(),
            )],
            Some(checksum),
        ))
        .expect("rpc server should have a receiver");

    let batch = tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive a live batch before timeout")
        .expect("response stream should not be empty")
        .expect("live batch should not be an error message");

    assert!(
        !batch.initial_sync_complete,
        "live batches are not bootstrap batches"
    );
    assert_eq!(
        batch.checksum,
        Some(checksum.to_vec()),
        "each live batch carries its source-computed post-cycle checksum"
    );

    Ok(())
}

/// Tests that a reorg (tip reset) is forwarded as a `Removed{Reorged}` marker event (design §5a).
async fn test_sync_mempool_reorg(
    mut client: IndexerClient<tonic::transport::Channel>,
    mut mock_mempool: MockMempool,
    mempool_transaction_sender: broadcast::Sender<MempoolBatch>,
) -> Result<()> {
    let mut response = client.sync_mempool(Empty {}).await?.into_inner();
    mock_mempool
        .expect_request(mempool::Request::MempoolBootstrapState)
        .await
        .respond(empty_bootstrap_state());

    let bootstrap = tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive a bootstrap batch before timeout")
        .expect("response stream should not be empty")
        .expect("bootstrap batch should not be an error message");
    assert!(bootstrap.initial_sync_complete);

    // A reorg re-queues verified txs for re-verification, signalled by a Reorged removal.
    mempool_transaction_sender
        .send(MempoolBatch::event(MempoolChange::removed_reorged(
            [txid(3)].into_iter().collect(),
        )))
        .expect("rpc server should have a receiver");

    let batch = tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive a reorg batch before timeout")
        .expect("response stream should not be empty")
        .expect("reorg batch should not be an error message");

    let removed = batch
        .events
        .iter()
        .find_map(|event| match &event.event {
            Some(mempool_event::Event::Removed(removed)) => Some(removed),
            _ => None,
        })
        .expect("the reorg batch carries a Removed event");
    assert!(
        matches!(removed.reason, Some(mempool_removed::Reason::Reorged(_))),
        "the reorg marker is a Reorged removal, got {:?}",
        removed.reason
    );

    Ok(())
}

/// Tests that when the server's own broadcast subscription lags (it missed events), the connection
/// is dropped so the follower re-bootstraps (design §5 "drop-and-resync").
async fn test_sync_mempool_lagged_connection(
    mut client: IndexerClient<tonic::transport::Channel>,
    mut mock_mempool: MockMempool,
    mempool_transaction_sender: broadcast::Sender<MempoolBatch>,
) -> Result<()> {
    let mut response = client.sync_mempool(Empty {}).await?.into_inner();

    // The handler subscribes to the broadcast before it reads the bootstrap snapshot, so its
    // receiver exists now. Overflow the capacity-1 channel before responding to the bootstrap read,
    // so the server's first live `recv` returns `Lagged`.
    for byte in 0..4u8 {
        let _ = mempool_transaction_sender.send(MempoolBatch::event(
            MempoolChange::removed_expired([txid(byte)].into_iter().collect()),
        ));
    }

    mock_mempool
        .expect_request(mempool::Request::MempoolBootstrapState)
        .await
        .respond(empty_bootstrap_state());

    // The terminal bootstrap batch still arrives.
    let bootstrap = tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("should receive a bootstrap batch before timeout")
        .expect("response stream should not be empty")
        .expect("bootstrap batch should not be an error message");
    assert!(bootstrap.initial_sync_complete);

    // Then the server detects its lagged subscription and drops the connection, ending the stream.
    let next = tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .expect("the stream should end promptly after the server drops the connection");
    assert!(
        next.is_none(),
        "the server drops the connection on Lagged, ending the stream, got {next:?}"
    );

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

async fn start_server_and_get_client() -> Result<(
    JoinHandle<Result<(), BoxError>>,
    IndexerClient<tonic::transport::Channel>,
    MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
    MockChainTipSender,
    MockMempool,
    broadcast::Sender<MempoolBatch>,
)> {
    let listen_addr: std::net::SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and u16 port should parse successfully");

    let mock_read_service = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();

    let mock_mempool: MockMempool = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();

    let (mock_chain_tip_change, mock_chain_tip_change_sender) = MockChainTip::new();
    let (mempool_transaction_sender, _) = tokio::sync::broadcast::channel(1);
    let mempool_tx_subscriber = MempoolTxSubscriber::new(mempool_transaction_sender.clone());
    let (server_task, listen_addr) = indexer::server::init(
        listen_addr,
        mock_read_service.clone(),
        mock_chain_tip_change,
        mock_mempool.clone(),
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
        mock_mempool,
        mempool_transaction_sender,
    ))
}
