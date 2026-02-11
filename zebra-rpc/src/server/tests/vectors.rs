//! Fixed test vectors for the RPC server.

// These tests call functions which can take unit arguments if some features aren't enabled.
#![allow(clippy::unit_arg)]

use std::net::{Ipv4Addr, SocketAddrV4};

use tokio::sync::watch;
use tower::buffer::Buffer;

use zebra_chain::{
    chain_sync_status::MockSyncStatus, chain_tip::NoChainTip, parameters::Network::*,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_test::mock_service::MockService;

use super::super::*;

use crate::methods::types::submit_block::MinedBlocksCounter;
use config::rpc::Config;

/// Test that the JSON-RPC server spawns.
#[tokio::test]
async fn rpc_server_spawn_test() {
    rpc_server_spawn().await
}

/// Test if the RPC server will spawn on a randomly generated port.
#[tracing::instrument]
async fn rpc_server_spawn() {
    let _init_guard = zebra_test::init();

    let conf = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()),
        indexer_listen_addr: None,
        parallel_cpu_threads: 0,
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
        max_response_body_size: Default::default(),
    };

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut read_state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut block_verifier_router: MockService<_, _, _, BoxError> =
        MockService::build().for_unit_tests();

    info!("spawning RPC server...");

    let (_tx, rx) = watch::channel(None);
    let (rpc_impl, _) = RpcImpl::new(
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
        MinedBlocksCounter::new().0,
    );

    RpcServer::start(rpc_impl, conf)
        .await
        .expect("RPC server should start");

    info!("spawned RPC server, checking services...");

    mempool.expect_no_requests().await;
    read_state.expect_no_requests().await;
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
    let conf = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()),
        indexer_listen_addr: None,
        parallel_cpu_threads: 0,
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
        max_response_body_size: Default::default(),
    };

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut read_state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut block_verifier_router: MockService<_, _, _, BoxError> =
        MockService::build().for_unit_tests();

    info!("spawning RPC server...");

    let (_tx, rx) = watch::channel(None);
    let (rpc_impl, _) = RpcImpl::new(
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
        MinedBlocksCounter::new().0,
    );

    let rpc = RpcServer::start(rpc_impl, conf)
        .await
        .expect("server should start");

    info!("spawned RPC server, checking services...");

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
    read_state.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    if do_shutdown {
        rpc.abort();
    }
}

/// Test if the RPC server will panic correctly when there is a port conflict.
#[tokio::test]
async fn rpc_server_spawn_port_conflict() {
    use std::time::Duration;
    let _init_guard = zebra_test::init();

    let port = zebra_test::net::random_known_port();
    let conf = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()),
        indexer_listen_addr: None,
        debug_force_finished_sync: false,
        parallel_cpu_threads: 0,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
        max_response_body_size: Default::default(),
    };

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut read_state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut block_verifier_router: MockService<_, _, _, BoxError> =
        MockService::build().for_unit_tests();

    let (_tx, rx) = watch::channel(None);
    let (rpc_impl, _) = RpcImpl::new(
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
        rx.clone(),
        None,
        MinedBlocksCounter::new().0,
    );

    RpcServer::start(rpc_impl.clone(), conf.clone())
        .await
        .expect("RPC server should start");

    tokio::time::sleep(Duration::from_secs(3)).await;

    RpcServer::start(rpc_impl, conf)
        .await
        .expect_err("RPC server should not start");

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
    read_state.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;
}
