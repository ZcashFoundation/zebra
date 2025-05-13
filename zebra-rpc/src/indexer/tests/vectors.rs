//! Fixed test vectors for indexer RPCs

use std::time::Duration;

use futures::StreamExt;
use tokio::task::JoinHandle;
use tower::BoxError;
use zebra_chain::{
    block::Height,
    chain_tip::mock::{MockChainTip, MockChainTipSender},
};
use zebra_test::{
    mock_service::MockService,
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::indexer::{self, indexer_client::IndexerClient, Empty};

#[tokio::test]
async fn rpc_server_spawn() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, client, mock_chain_tip_sender) = start_server_and_get_client().await?;

    test_chain_tip_change(client.clone(), mock_chain_tip_sender).await?;

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

async fn start_server_and_get_client() -> Result<(
    JoinHandle<Result<(), BoxError>>,
    IndexerClient<tonic::transport::Channel>,
    MockChainTipSender,
)> {
    let listen_addr: std::net::SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and u16 port should parse successfully");

    let mock_read_service = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();

    let (mock_chain_tip_change, mock_chain_tip_change_sender) = MockChainTip::new();
    let (_tx, rx) = tokio::sync::broadcast::channel(1);

    let (server_task, listen_addr) =
        indexer::server::init(listen_addr, mock_read_service, mock_chain_tip_change, rx)
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

    Ok((server_task, client, mock_chain_tip_change_sender))
}
