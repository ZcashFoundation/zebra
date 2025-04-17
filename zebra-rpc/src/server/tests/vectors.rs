//! Fixed test vectors for the RPC server.

// These tests call functions which can take unit arguments if some features aren't enabled.
#![allow(clippy::unit_arg)]

use std::net::{Ipv4Addr, SocketAddrV4};

use tower::buffer::Buffer;

use zebra_chain::{
    chain_sync_status::MockSyncStatus, chain_tip::NoChainTip, parameters::Network::*,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;

use zebra_test::mock_service::MockService;

use super::super::*;

/// Test that the JSON-RPC server spawns.
#[tokio::test]
async fn rpc_server_spawn_test() {
    rpc_server_spawn().await
}

/// Test if the RPC server will spawn on a randomly generated port.
#[tracing::instrument]
async fn rpc_server_spawn() {
    let _init_guard = zebra_test::init();

    let config = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()),
        indexer_listen_addr: None,
        parallel_cpu_threads: 0,
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
    };

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut block_verifier_router: MockService<_, _, _, BoxError> =
        MockService::build().for_unit_tests();

    info!("spawning RPC server...");

    let (_tx, rx) = watch::channel(None);
    let _rpc_server_task_handle = RpcServer::spawn(
        config,
        Default::default(),
        "RPC server test",
        "RPC server test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        Buffer::new(block_verifier_router.clone(), 1),
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
        NoChainTip,
        Mainnet,
        None,
        rx,
    );

    info!("spawned RPC server, checking services...");

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;
}

/// Test that the JSON-RPC server spawns on an OS-assigned unallocated port.
#[tokio::test]
async fn rpc_server_spawn_unallocated_port() {
    rpc_spawn_unallocated_port(false).await
}

/// Test that the JSON-RPC server spawns and shuts down on an OS-assigned unallocated port.
#[tokio::test]
async fn rpc_server_spawn_unallocated_port_shutdown() {
    rpc_spawn_unallocated_port(true).await
}

/// Test if the RPC server will spawn on an OS-assigned unallocated port.
///
/// Set `do_shutdown` to true to close the server using the close handle.
#[tracing::instrument]
async fn rpc_spawn_unallocated_port(do_shutdown: bool) {
    let _init_guard = zebra_test::init();

    let port = zebra_test::net::random_unallocated_port();
    #[allow(unknown_lints)]
    #[allow(clippy::bool_to_int_with_if)]
    let config = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()),
        indexer_listen_addr: None,
        parallel_cpu_threads: 0,
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
    };

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut block_verifier_router: MockService<_, _, _, BoxError> =
        MockService::build().for_unit_tests();

    info!("spawning RPC server...");

    let (_tx, rx) = watch::channel(None);
    let rpc_server_task_handle = RpcServer::spawn(
        config,
        Default::default(),
        "RPC server test",
        "RPC server test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        Buffer::new(block_verifier_router.clone(), 1),
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
        NoChainTip,
        Mainnet,
        None,
        rx,
    )
    .await
    .expect("");

    info!("spawned RPC server, checking services...");

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    if do_shutdown {
        rpc_server_task_handle.0.abort();
    }
}

/// Test if the RPC server will panic correctly when there is a port conflict.
///
/// This test is sometimes unreliable on Windows, and hangs on macOS.
/// We believe this is a CI infrastructure issue, not a platform-specific issue.
#[tokio::test]
#[should_panic(expected = "Unable to start RPC server")]
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
async fn rpc_server_spawn_port_conflict() {
    use std::time::Duration;
    let _init_guard = zebra_test::init();

    let port = zebra_test::net::random_known_port();
    let config = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()),
        indexer_listen_addr: None,
        debug_force_finished_sync: false,
        parallel_cpu_threads: 0,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
    };

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut block_verifier_router: MockService<_, _, _, BoxError> =
        MockService::build().for_unit_tests();

    info!("spawning RPC server 1...");

    let (_tx, rx) = watch::channel(None);
    let _rpc_server_1_task_handle = RpcServer::spawn(
        config.clone(),
        Default::default(),
        "RPC server 1 test",
        "RPC server 1 test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        Buffer::new(block_verifier_router.clone(), 1),
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
        NoChainTip,
        Mainnet,
        None,
        rx.clone(),
    )
    .await;

    tokio::time::sleep(Duration::from_secs(3)).await;

    info!("spawning conflicted RPC server 2...");

    let _rpc_server_2_task_handle = RpcServer::spawn(
        config,
        Default::default(),
        "RPC server 2 conflict test",
        "RPC server 2 conflict test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        Buffer::new(block_verifier_router.clone(), 1),
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
        NoChainTip,
        Mainnet,
        None,
        rx,
    )
    .await;

    info!("spawned RPC servers, checking services...");

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;
}
