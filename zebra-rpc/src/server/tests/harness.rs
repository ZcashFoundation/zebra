//! A harness that starts the full RPC server stack and posts literal request bodies to it.
//!
//! Unlike calls on [`RpcImpl`] or on its `RpcModule`, requests posted here go through the HTTP
//! compatibility layer (JSON-RPC 1.0 and versionless envelopes) and the RPC middleware
//! (`FixRpcResponseMiddleware` rewrites `-32602` to `-1`), so tests see exactly what a client sees.

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};

use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use tokio::sync::watch;
use tower::buffer::Buffer;

use zebra_chain::{
    chain_sync_status::{ChainSyncStatus, MockSyncStatus},
    chain_tip::{ChainTip, NoChainTip},
    parameters::Network::Mainnet,
};
use zebra_consensus::router::service_trait::BlockVerifierService;
use zebra_network::{address_book_peers::MockAddressBookPeers, AddressBookPeers};
use zebra_node_services::{mempool::MempoolService, BoxError};
use zebra_state::{ReadState as ReadStateService, State as StateService};
use zebra_test::mock_service::MockService;

use crate::{
    config::rpc::Config,
    methods::RpcImpl,
    server::{RpcServer, ServerTask},
};

/// How long [`post`] waits for the server's response.
const POST_TIMEOUT: Duration = Duration::from_secs(30);

/// Starts `rpc_impl` behind the full RPC server stack on an OS-assigned loopback port.
///
/// Returns the address the server listens on and the server task. Cookie authentication is
/// disabled, so requests need no credentials.
pub(crate) async fn spawn<
    Mempool,
    State,
    ReadState,
    Tip,
    BlockVerifierRouter,
    SyncStatus,
    AddressBook,
>(
    rpc_impl: RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>,
) -> (SocketAddr, ServerTask)
where
    Mempool: MempoolService,
    State: StateService,
    ReadState: ReadStateService,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
    BlockVerifierRouter: BlockVerifierService,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    // The `rpc_server_spawn` config, with the production response size limit: a limit of 0
    // would replace every response with an oversized-response error.
    let conf = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()),
        indexer_listen_addr: None,
        lightwalletd_listen_addr: None,
        parallel_cpu_threads: 0,
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
        max_response_body_size: Config::default().max_response_body_size,
    };

    let (server_task, listen_addr) = RpcServer::start_with_addr(rpc_impl, conf)
        .await
        .expect("port 0 on the loopback interface lets the OS assign a free port");

    (listen_addr, server_task)
}

/// Posts `body` verbatim to the RPC server at `addr`, and returns the response body verbatim.
///
/// Sends `Content-Type: application/json`, as lightwalletd's RPC client does. Panics if the
/// server does not answer within [`POST_TIMEOUT`].
pub(crate) async fn post(addr: SocketAddr, body: &str) -> String {
    // A client per request: a client's connection pool is tied to the runtime that created it,
    // and every test has its own runtime.
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(POST_TIMEOUT)
        .build()
        .expect(
            "the client sets only a timeout and no proxy, and Zebra's reqwest features need no \
             system TLS or DNS configuration",
        );

    client
        .post(format!("http://{addr}"))
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_owned())
        .send()
        .await
        .expect("the test server is listening on the loopback interface")
        .text()
        .await
        .expect("the test server sends a complete response body")
}

/// The harness serves legacy envelopes and rewrites `-32602` to `-1`, so it runs the full server
/// stack rather than the bare `RpcModule`.
#[tokio::test]
async fn harness_runs_the_full_server_stack() {
    let _init_guard = zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut read_state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut block_verifier_router: MockService<_, _, _, BoxError> =
        MockService::build().for_unit_tests();

    let (_tx, rx) = watch::channel(None);
    let (rpc_impl, rpc_tx_queue) = RpcImpl::new(
        Mainnet,
        Default::default(),
        false,
        "RPC test",
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        Buffer::new(block_verifier_router.clone(), 1),
        MockSyncStatus::default(),
        NoChainTip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    let (addr, server_task) = spawn(rpc_impl).await;

    // A string verbosity fails parameter parsing before the handler reads the state.
    let response = post(
        addr,
        r#"{"jsonrpc":"1.0","method":"getblock","params":["1","1"],"id":7}"#,
    )
    .await;
    let response: Value = serde_json::from_str(&response).expect("the server responds with JSON");

    assert_eq!(response["jsonrpc"], "1.0", "the HTTP layer restores 1.0");
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"], Value::Null);
    assert_eq!(
        response["error"]["code"], -1,
        "FixRpcResponseMiddleware rewrites -32602 to -1"
    );
    assert_eq!(response["error"]["message"], "Invalid params");

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
    read_state.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    server_task.abort();
    rpc_tx_queue.abort();
}
