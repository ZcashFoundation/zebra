//! Fixed test vectors for the lightwalletd gRPC server.

use std::{sync::Arc, time::Duration};

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
use zebra_node_services::mempool::{MempoolChange, MempoolTxSubscriber};
use zebra_state::{ReadRequest, ReadResponse};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    prelude::color_eyre::{eyre::eyre, Result},
};

use crate::{
    lightwalletd::{self, compact_tx_streamer_client::CompactTxStreamerClient, BlockId},
    methods::RpcImpl,
};

/// The mocked read state service the server is built on.
type MockReadState = MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError>;

/// A missing note commitment tree must not be served as an empty tree.
///
/// The block read can succeed while a tree read fails, for example when a reorg
/// lands between them. Reporting a tree size of zero would tell a light client
/// that no notes exist as of that block, corrupting every note position it
/// derives from it, so the request must fail instead.
#[tokio::test]
async fn get_block_errors_when_a_commitment_tree_is_missing() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_server_task, mut client, mut read_state, _chain_tip_sender, _mempool_change_sender) =
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

/// Starts the lightwalletd gRPC server against mocked services, and connects a client to it.
async fn start_server_and_get_client() -> Result<(
    JoinHandle<Result<(), BoxError>>,
    CompactTxStreamerClient<tonic::transport::Channel>,
    MockReadState,
    MockChainTipSender,
    broadcast::Sender<MempoolChange>,
)> {
    let listen_addr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and port should parse");

    let read_state: MockReadState = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
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
        Buffer::new(mempool, 1),
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
        chain_tip_sender,
        mempool_change_sender,
    ))
}
