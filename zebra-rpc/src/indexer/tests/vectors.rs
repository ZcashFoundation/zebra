//! Fixed test vectors for indexer RPCs

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use tokio::{sync::broadcast, task::JoinHandle};
use tower::BoxError;
use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    serialization::ZcashDeserializeInto,
    transaction::{self, UnminedTxId},
};
use zebra_node_services::mempool::{MempoolChange, MempoolTxSubscriber};
use zebra_state::{NonFinalizedBlocksListener, ReadRequest, ReadResponse};
use zebra_test::{
    mock_service::MockService,
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::indexer::{self, indexer_client::IndexerClient, Empty};

#[tokio::test]
async fn rpc_server_spawn() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, client, mock_chain_tip_sender, mempool_transaction_sender) =
        start_server_and_get_client().await?;

    test_chain_tip_change(client.clone(), mock_chain_tip_sender).await?;
    test_mempool_change(client.clone(), mempool_transaction_sender).await?;

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

/// Verify that `non_finalized_state_change` can deliver more than 64 blocks without
/// dropping the stream (regression test for #10728).
#[tokio::test]
async fn non_finalized_state_change_delivers_burst() -> Result<()> {
    let _init_guard = zebra_test::init();

    const BLOCK_COUNT: usize = 100;

    let listen_addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut mock_read_service = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let (mock_chain_tip, _mock_chain_tip_sender) = MockChainTip::new();
    let (mempool_tx_sender, _) = tokio::sync::broadcast::channel(1);
    let mempool_tx_subscriber = MempoolTxSubscriber::new(mempool_tx_sender);

    let (server_task, listen_addr) = indexer::server::init(
        listen_addr,
        mock_read_service.clone(),
        mock_chain_tip,
        mempool_tx_subscriber,
    )
    .await
    .map_err(|err| eyre!(err))?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let endpoint = tonic::transport::channel::Endpoint::new(format!("http://{listen_addr}"))
        .unwrap()
        .timeout(Duration::from_secs(10));
    let mut client = IndexerClient::connect(endpoint).await.unwrap();

    // Pre-load a channel with BLOCK_COUNT fake blocks.
    let (sender, receiver) = tokio::sync::mpsc::channel(BLOCK_COUNT);
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();

    for i in 0..BLOCK_COUNT {
        let hash = block::Hash([i as u8; 32]);
        sender.send((hash, block.clone())).await.unwrap();
    }
    drop(sender);

    let listener = NonFinalizedBlocksListener(Arc::new(receiver));

    // Spawn request so MockService can respond.
    let stream_handle = tokio::spawn(async move {
        let request = tonic::Request::new(Empty {});
        client.non_finalized_state_change(request).await
    });

    // Respond with our pre-loaded listener.
    mock_read_service
        .expect_request(ReadRequest::NonFinalizedBlocksListener)
        .await
        .respond(ReadResponse::NonFinalizedBlocksListener(listener));

    let response = stream_handle.await.unwrap().unwrap();
    let mut stream = response.into_inner();
    let mut received = 0;
    while let Some(Ok(_)) = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap_or(None)
    {
        received += 1;
    }

    assert_eq!(
        received, BLOCK_COUNT,
        "stream should deliver all {BLOCK_COUNT} blocks without dropping"
    );

    drop(server_task);
    Ok(())
}

async fn start_server_and_get_client() -> Result<(
    JoinHandle<Result<(), BoxError>>,
    IndexerClient<tonic::transport::Channel>,
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
        mock_read_service,
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
        mock_chain_tip_change_sender,
        mempool_transaction_sender,
    ))
}
