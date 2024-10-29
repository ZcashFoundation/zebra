//! Fixed test vectors for the RPC server.

// These tests call functions which can take unit arguments if some features aren't enabled.
#![allow(clippy::unit_arg)]

use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use futures::FutureExt;
use tower::buffer::Buffer;

use zebra_chain::{
    chain_sync_status::MockSyncStatus, chain_tip::NoChainTip, parameters::Network::*,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;

use zebra_test::mock_service::MockService;

use super::super::*;

/// Test that the JSON-RPC server spawns when configured with a single thread.
#[test]
fn rpc_server_spawn_single_thread() {
    rpc_server_spawn(false)
}

/// Test that the JSON-RPC server spawns when configured with multiple threads.
#[test]
#[cfg(not(target_os = "windows"))]
fn rpc_server_spawn_parallel_threads() {
    rpc_server_spawn(true)
}

/// Test if the RPC server will spawn on a randomly generated port.
///
/// Set `parallel_cpu_threads` to true to auto-configure based on the number of CPU cores.
#[tracing::instrument]
fn rpc_server_spawn(parallel_cpu_threads: bool) {
    let _init_guard = zebra_test::init();

    let config = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()),
        indexer_listen_addr: None,
        parallel_cpu_threads: if parallel_cpu_threads { 2 } else { 1 },
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
    };

    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut block_verifier_router: MockService<_, _, _, BoxError> =
            MockService::build().for_unit_tests();

        info!("spawning RPC server...");

        let (rpc_server_task_handle, rpc_tx_queue_task_handle, _rpc_server) = RpcServer::spawn(
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
        );

        info!("spawned RPC server, checking services...");

        mempool.expect_no_requests().await;
        state.expect_no_requests().await;
        block_verifier_router.expect_no_requests().await;

        // The server and queue tasks should continue without errors or panics
        let rpc_server_task_result = rpc_server_task_handle.now_or_never();
        assert!(rpc_server_task_result.is_none());

        let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
        assert!(rpc_tx_queue_task_result.is_none());
    });

    info!("waiting for RPC server to shut down...");
    rt.shutdown_timeout(Duration::from_secs(1));
}

/// Test that the JSON-RPC server spawns when configured with a single thread,
/// on an OS-assigned unallocated port.
#[test]
fn rpc_server_spawn_unallocated_port_single_thread() {
    rpc_server_spawn_unallocated_port(false, false)
}

/// Test that the JSON-RPC server spawns and shuts down when configured with a single thread,
/// on an OS-assigned unallocated port.
#[test]
fn rpc_server_spawn_unallocated_port_single_thread_shutdown() {
    rpc_server_spawn_unallocated_port(false, true)
}

/// Test that the JSON-RPC server spawns when configured with multiple threads,
/// on an OS-assigned unallocated port.
#[test]
fn rpc_sever_spawn_unallocated_port_parallel_threads() {
    rpc_server_spawn_unallocated_port(true, false)
}

/// Test that the JSON-RPC server spawns and shuts down when configured with multiple threads,
/// on an OS-assigned unallocated port.
#[test]
fn rpc_sever_spawn_unallocated_port_parallel_threads_shutdown() {
    rpc_server_spawn_unallocated_port(true, true)
}

/// Test if the RPC server will spawn on an OS-assigned unallocated port.
///
/// Set `parallel_cpu_threads` to true to auto-configure based on the number of CPU cores,
/// and `do_shutdown` to true to close the server using the close handle.
#[tracing::instrument]
fn rpc_server_spawn_unallocated_port(parallel_cpu_threads: bool, do_shutdown: bool) {
    let _init_guard = zebra_test::init();

    let port = zebra_test::net::random_unallocated_port();
    #[allow(unknown_lints)]
    #[allow(clippy::bool_to_int_with_if)]
    let config = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()),
        indexer_listen_addr: None,
        parallel_cpu_threads: if parallel_cpu_threads { 0 } else { 1 },
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
    };

    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut block_verifier_router: MockService<_, _, _, BoxError> =
            MockService::build().for_unit_tests();

        info!("spawning RPC server...");

        let (rpc_server_task_handle, rpc_tx_queue_task_handle, rpc_server) = RpcServer::spawn(
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
        );

        info!("spawned RPC server, checking services...");

        mempool.expect_no_requests().await;
        state.expect_no_requests().await;
        block_verifier_router.expect_no_requests().await;

        if do_shutdown {
            rpc_server
                .expect("unexpected missing RpcServer for configured RPC port")
                .shutdown()
                .await
                .expect("unexpected panic during RpcServer shutdown");

            // The server and queue tasks should shut down without errors or panics
            let rpc_server_task_result = rpc_server_task_handle.await;
            assert!(
                matches!(rpc_server_task_result, Ok(())),
                "unexpected server task panic during shutdown: {rpc_server_task_result:?}"
            );

            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.await;
            assert!(
                matches!(rpc_tx_queue_task_result, Ok(())),
                "unexpected queue task panic during shutdown: {rpc_tx_queue_task_result:?}"
            );
        } else {
            // The server and queue tasks should continue without errors or panics
            let rpc_server_task_result = rpc_server_task_handle.now_or_never();
            assert!(rpc_server_task_result.is_none());

            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            assert!(rpc_tx_queue_task_result.is_none());
        }
    });

    info!("waiting for RPC server to shut down...");
    rt.shutdown_timeout(Duration::from_secs(1));
}

/// Test if the RPC server will panic correctly when there is a port conflict.
///
/// This test is sometimes unreliable on Windows, and hangs on macOS.
/// We believe this is a CI infrastructure issue, not a platform-specific issue.
#[test]
#[should_panic(expected = "Unable to start RPC server")]
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn rpc_server_spawn_port_conflict() {
    let _init_guard = zebra_test::init();

    let port = zebra_test::net::random_known_port();
    let config = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()),
        indexer_listen_addr: None,
        parallel_cpu_threads: 1,
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
    };

    let rt = tokio::runtime::Runtime::new().unwrap();

    let test_task_handle = rt.spawn(async {
        let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut block_verifier_router: MockService<_, _, _, BoxError> =
            MockService::build().for_unit_tests();

        info!("spawning RPC server 1...");

        let (_rpc_server_1_task_handle, _rpc_tx_queue_1_task_handle, _rpc_server) =
            RpcServer::spawn(
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
            );

        tokio::time::sleep(Duration::from_secs(3)).await;

        info!("spawning conflicted RPC server 2...");

        let (rpc_server_2_task_handle, _rpc_tx_queue_2_task_handle, _rpc_server) = RpcServer::spawn(
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
        );

        info!("spawned RPC servers, checking services...");

        mempool.expect_no_requests().await;
        state.expect_no_requests().await;
        block_verifier_router.expect_no_requests().await;

        // Because there is a panic inside a multi-threaded executor,
        // we can't depend on the exact behaviour of the other tasks,
        // particularly across different machines and OSes.

        // The second server should panic, so its task handle should return the panic
        let rpc_server_2_task_result = rpc_server_2_task_handle.await;
        match rpc_server_2_task_result {
            Ok(()) => panic!(
                "RPC server with conflicting port should exit with an error: \
                 unexpected Ok result"
            ),
            Err(join_error) => match join_error.try_into_panic() {
                Ok(panic_object) => panic::resume_unwind(panic_object),
                Err(cancelled_error) => panic!(
                    "RPC server with conflicting port should exit with an error: \
                     unexpected JoinError: {cancelled_error:?}"
                ),
            },
        }

        // Ignore the queue task result
    });

    // Wait until the spawned task finishes
    std::thread::sleep(Duration::from_secs(10));

    info!("waiting for RPC server to shut down...");
    rt.shutdown_timeout(Duration::from_secs(3));

    match test_task_handle.now_or_never() {
        Some(Ok(_never)) => unreachable!("test task always panics"),
        None => panic!("unexpected test task hang"),
        Some(Err(join_error)) => match join_error.try_into_panic() {
            Ok(panic_object) => panic::resume_unwind(panic_object),
            Err(cancelled_error) => panic!(
                "test task should exit with a RPC server panic: \
                 unexpected non-panic JoinError: {cancelled_error:?}"
            ),
        },
    }
}

/// Check if the RPC server detects a port conflict when running parallel threads.
///
/// If this test fails, that's great!
/// We can make parallel the default, and remove the warnings in the config docs.
///
/// This test is sometimes unreliable on Windows, and hangs on macOS.
/// We believe this is a CI infrastructure issue, not a platform-specific issue.
#[test]
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn rpc_server_spawn_port_conflict_parallel_auto() {
    let _init_guard = zebra_test::init();

    let port = zebra_test::net::random_known_port();
    let config = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()),
        indexer_listen_addr: None,
        parallel_cpu_threads: 2,
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
    };

    let rt = tokio::runtime::Runtime::new().unwrap();

    let test_task_handle = rt.spawn(async {
        let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut block_verifier_router: MockService<_, _, _, BoxError> =
            MockService::build().for_unit_tests();

        info!("spawning parallel RPC server 1...");

        let (_rpc_server_1_task_handle, _rpc_tx_queue_1_task_handle, _rpc_server) =
            RpcServer::spawn(
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
            );

        tokio::time::sleep(Duration::from_secs(3)).await;

        info!("spawning parallel conflicted RPC server 2...");

        let (rpc_server_2_task_handle, _rpc_tx_queue_2_task_handle, _rpc_server) = RpcServer::spawn(
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
        );

        info!("spawned RPC servers, checking services...");

        mempool.expect_no_requests().await;
        state.expect_no_requests().await;
        block_verifier_router.expect_no_requests().await;

        // Because there might be a panic inside a multi-threaded executor,
        // we can't depend on the exact behaviour of the other tasks,
        // particularly across different machines and OSes.

        // The second server doesn't panic, but we'd like it to.
        // (See the function docs for details.)
        let rpc_server_2_task_result = rpc_server_2_task_handle.await;
        match rpc_server_2_task_result {
            Ok(()) => info!(
                "Parallel RPC server with conflicting port should exit with an error: \
                 but we're ok with it ignoring the conflict for now"
            ),
            Err(join_error) => match join_error.try_into_panic() {
                Ok(panic_object) => panic::resume_unwind(panic_object),
                Err(cancelled_error) => info!(
                    "Parallel RPC server with conflicting port should exit with an error: \
                     but we're ok with it ignoring the conflict for now: \
                     unexpected JoinError: {cancelled_error:?}"
                ),
            },
        }

        // Ignore the queue task result
    });

    // Wait until the spawned task finishes
    std::thread::sleep(Duration::from_secs(10));

    info!("waiting for parallel RPC server to shut down...");
    rt.shutdown_timeout(Duration::from_secs(3));

    match test_task_handle.now_or_never() {
        Some(Ok(())) => {
            info!("parallel RPC server task successfully exited");
        }
        None => panic!("unexpected test task hang"),
        Some(Err(join_error)) => match join_error.try_into_panic() {
            Ok(panic_object) => panic::resume_unwind(panic_object),
            Err(cancelled_error) => info!(
                "Parallel RPC server with conflicting port should exit with an error: \
                 but we're ok with it ignoring the conflict for now: \
                 unexpected JoinError: {cancelled_error:?}"
            ),
        },
    }
}
