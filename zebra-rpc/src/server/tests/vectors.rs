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
        lightwalletd_listen_addr: None,
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
        lightwalletd_listen_addr: None,
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

/// Test that the RPC server rejects an address held by another RPC server.
#[tokio::test]
async fn rpc_server_spawn_port_conflict() {
    let _init_guard = zebra_test::init();

    let mut conf = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()),
        indexer_listen_addr: None,
        lightwalletd_listen_addr: None,
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
    );

    let server = Server::builder()
        .build(conf.listen_addr.expect("the test config enables RPC"))
        .await
        .expect("RPC server should bind");
    conf.listen_addr = Some(
        server
            .local_addr()
            .expect("the RPC server has bound its listener"),
    );
    let _server_handle = server.start(rpc_impl.clone().into_rpc());

    RpcServer::start(rpc_impl, conf)
        .await
        .expect_err("RPC server should not start");

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
    read_state.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;
}

/// Sends `body` to the RPC server at `addr` over a plain HTTP/1.1 connection.
///
/// Returns the HTTP status code and the raw response body.
async fn post_raw(addr: std::net::SocketAddr, body: &str) -> (u16, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("RPC server should accept connections");
    let request = format!(
        "POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request should be sent");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("response should be read");

    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response should have a header block");
    let head = std::str::from_utf8(&response[..split]).expect("headers should be UTF-8");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .expect("response should have a status code");
    // The parser below only handles fixed-length bodies.
    assert!(
        !head.to_ascii_lowercase().contains("transfer-encoding"),
        "unexpected chunked response: {head}"
    );

    (status, response[split + 4..].to_vec())
}

/// Test that each client's JSON-RPC dialect round-trips through the real server and its
/// HTTP compatibility middleware.
#[tokio::test]
async fn rpc_server_http_compatibility_end_to_end() {
    use serde_json::{json, Value};

    /// A JSON-RPC 2.0 success response; derived deserialization rejects duplicate fields.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct StrictSuccess {
        jsonrpc: String,
        result: Value,
        id: Value,
    }

    let _init_guard = zebra_test::init();

    // `RpcServer::start` doesn't return its bound address, so reserve a free port first.
    let listen_addr = std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .expect("a free local port should exist");
    let conf = Config {
        listen_addr: Some(listen_addr),
        indexer_listen_addr: None,
        lightwalletd_listen_addr: None,
        parallel_cpu_threads: 0,
        debug_force_finished_sync: false,
        cookie_dir: Default::default(),
        enable_cookie_auth: false,
        // A zero limit rejects every response, so use the real default.
        max_response_body_size: Config::default().max_response_body_size,
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
        rx,
        None,
    );

    let rpc = RpcServer::start(rpc_impl, conf)
        .await
        .expect("RPC server should start");

    // `getstandardfee` needs no service calls and returns a deterministic object.
    let parse = |body: &[u8]| -> Value {
        serde_json::from_slice(body).expect("response body should be JSON")
    };

    // 1. A canonical 2.0 request is forwarded unchanged, and so is its response.
    let (status, canonical_raw) = post_raw(
        listen_addr,
        r#"{"jsonrpc":"2.0","method":"getstandardfee","params":[],"id":1}"#,
    )
    .await;
    assert_eq!(status, 200);
    let strict: StrictSuccess =
        serde_json::from_slice(&canonical_raw).expect("response should have no duplicate fields");
    assert_eq!(strict.jsonrpc, "2.0");
    assert_eq!(strict.id, json!(1));
    assert!(
        strict.result.is_object(),
        "unexpected result: {}",
        strict.result
    );
    let canonical = parse(&canonical_raw);
    let result = canonical["result"].clone();

    // 2. A spaced 2.0 request is re-encoded, and gets the same response.
    let (status, body) = post_raw(
        listen_addr,
        r#"{"jsonrpc": "2.0", "method": "getstandardfee", "params": [], "id": 1}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(parse(&body), canonical);

    // 3. A lightwalletd 1.0 request gets a 1.0 response with a null error.
    let (status, body) = post_raw(
        listen_addr,
        r#"{"jsonrpc":"1.0","method":"getstandardfee","params":[],"id":"lwd"}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        parse(&body),
        json!({"jsonrpc": "1.0", "result": result, "error": null, "id": "lwd"})
    );

    // 4. A zcash-cli request without a version gets a response without a version.
    let (status, body) = post_raw(
        listen_addr,
        r#"{"method":"getstandardfee","params":[],"id":"cli"}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        parse(&body),
        json!({"result": result, "error": null, "id": "cli"})
    );

    // 5. A notification gets jsonrpsee's empty HTTP 200 reply, not a middleware error.
    let (status, body) = post_raw(
        listen_addr,
        r#"{"jsonrpc":"2.0","method":"getstandardfee","params":[]}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        body.is_empty(),
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );

    // 6. A batch of two canonical requests gets two responses.
    let (status, body) = post_raw(
        listen_addr,
        r#"[{"jsonrpc":"2.0","method":"getstandardfee","params":[],"id":1},{"jsonrpc":"2.0","method":"getstandardfee","params":[],"id":2}]"#,
    )
    .await;
    assert_eq!(status, 200);
    let mut batch = parse(&body)
        .as_array()
        .cloned()
        .expect("batch response should be an array");
    batch.sort_by_key(|response| response["id"].as_u64());
    assert_eq!(
        batch,
        vec![
            json!({"jsonrpc": "2.0", "result": result, "id": 1}),
            json!({"jsonrpc": "2.0", "result": result, "id": 2}),
        ]
    );

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
    read_state.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    rpc.abort();
}
