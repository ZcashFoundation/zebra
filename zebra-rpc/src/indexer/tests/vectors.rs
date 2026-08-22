//! Fixed test vectors for indexer RPCs

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures::StreamExt;
use tokio::{sync::broadcast, task::JoinHandle};
use tower::BoxError;
use zebra_chain::{
    block::{Block, Height},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    serialization::ZcashDeserializeInto,
    transaction::{self, UnminedTxId},
};
use zebra_node_services::mempool::{MempoolChange, MempoolTxSubscriber};
use zebra_state::{
    BlockField, BlockQuery, ChainSelector, HashOrHeight, NonFinalizedBlocksListener, QueriedBlock,
    ReadRequest, ReadResponse,
};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::indexer::{
    self, indexer_client::IndexerClient, BlockRequest, Empty, NonFinalizedStateChangeRequest,
};

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

    test_chain_tip_change(client.clone(), mock_chain_tip_sender).await?;
    test_mempool_change(client.clone(), mempool_transaction_sender).await?;
    test_get_block(client.clone(), mock_read_service.clone()).await?;
    test_non_finalized_state_change(client.clone(), mock_read_service).await?;

    Ok(())
}

/// Tests that `NonFinalizedStateChange` streams every buffered block when the listener channel is
/// already full as the subscription starts.
///
/// A full listener buffer is ordinary backpressure: the state side fills it while sending the
/// initial dump of the non-finalized state, faster than the consumer can serialize blocks onto the
/// wire. Treating it as a dead consumer drops the subscription on connect, and the client then
/// re-subscribes into the same condition.
async fn test_non_finalized_state_change(
    client: IndexerClient<tonic::transport::Channel>,
    mut mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>,
) -> Result<()> {
    let blocks: Vec<Arc<Block>> = [
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_3_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_4_BYTES[..],
    ]
    .into_iter()
    .map(|bytes| bytes.zcash_deserialize_into())
    .collect::<std::result::Result<_, _>>()?;

    // Fill the listener channel to capacity, like the state side does when the non-finalized state
    // has more blocks than the buffer has slots.
    let (sender, receiver) = tokio::sync::mpsc::channel(blocks.len());
    for block in &blocks {
        sender
            .try_send((block.hash(), block.clone()))
            .expect("channel has one slot per block");
    }
    assert_eq!(
        receiver.capacity(),
        0,
        "the listener buffer should be full before the subscription starts"
    );

    let mut request_client = client.clone();
    let request_task = tokio::spawn(async move {
        request_client
            .non_finalized_state_change(tonic::Request::new(NonFinalizedStateChangeRequest {
                chain_tip_hashes: Vec::new(),
            }))
            .await
    });

    mock_read_service
        .expect_request(ReadRequest::NonFinalizedBlocksListener {
            known_chain_tips: HashSet::new(),
        })
        .await
        .respond(ReadResponse::NonFinalizedBlocksListener(
            NonFinalizedBlocksListener(Arc::new(receiver)),
        ));

    let mut response = request_task
        .await?
        .expect("non_finalized_state_change should succeed")
        .into_inner();

    for block in &blocks {
        let message = tokio::time::timeout(Duration::from_secs(3), response.next())
            .await
            .expect("should receive a non-finalized block before timeout")
            .expect("response stream should not end while blocks are buffered")
            .expect("non-finalized state change response should not be an error message");

        let (_decoded_block, decoded_hash) = message.decode().expect("response should decode");
        assert_eq!(decoded_hash, block.hash());
    }

    // Keep the state side of the channel open until every block has been received, so the stream
    // can only end early because the subscription was dropped.
    drop(sender);

    Ok(())
}

/// Tests that the `GetBlock` method returns the requested block and rejects an empty request.
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
        .expect_request(ReadRequest::BlockQuery(BlockQuery {
            hash_or_height: HashOrHeight::Height(height),
            chain: ChainSelector::Best,
            fields: [BlockField::Block].into_iter().collect(),
        }))
        .await
        .respond(ReadResponse::BlockQuery(Some(QueriedBlock {
            block: Some(block.clone()),
            ..QueriedBlock::default()
        })));

    let response = request_task
        .await?
        .expect("get_block should succeed")
        .into_inner();
    let (decoded_block, decoded_hash) = response.decode().expect("response should decode");
    assert_eq!(decoded_hash, expected_hash);
    assert_eq!(decoded_block.hash(), expected_hash);

    Ok(())
}

async fn test_chain_tip_change(
    mut client: IndexerClient<tonic::transport::Channel>,
    mock_chain_tip_sender: MockChainTipSender,
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
    mempool_transaction_sender: tokio::sync::broadcast::Sender<MempoolChange>,
) -> Result<()> {
    let request = tonic::Request::new(Empty {});
    let mut response = client.mempool_change(request).await?.into_inner();

    let change_tx_ids = [UnminedTxId::Legacy(transaction::Hash::from([0; 32]))]
        .into_iter()
        .collect();

    mempool_transaction_sender
        .send(MempoolChange::added(change_tx_ids))
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
    broadcast::Sender<MempoolChange>,
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
