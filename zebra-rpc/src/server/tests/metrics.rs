//! End-to-end tests for JSON-RPC HTTP connection, payload-size, and batch metrics.

use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};

use jsonrpsee::{
    server::{middleware::rpc::RpcServiceBuilder, Server, ServerHandle},
    RpcModule,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{watch, Notify},
};
use tower::buffer::Buffer;
use zebra_chain::{
    chain_sync_status::MockSyncStatus, chain_tip::NoChainTip, parameters::Network::Mainnet,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_test::mock_service::MockService;

use crate::{
    config::rpc::Config,
    methods::RpcImpl,
    metrics::tests::TestMetricsRecorder,
    server::{
        http_metrics::HttpMetricsLayer, http_request_compatibility::HttpRequestMiddlewareLayer,
        rpc_metrics::RpcMetricsMiddleware, RpcServer,
    },
};

async fn send_raw_http_post(addr: SocketAddr, body: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("should connect to test RPC server");
    let request = format!(
        "POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("should write HTTP request");

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("should read HTTP response");

    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response should contain HTTP header separator");
    let header = std::str::from_utf8(&raw[..header_end]).expect("HTTP headers should be UTF-8");
    let status_line = header.lines().next().expect("status line should exist");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .expect("HTTP status code should exist")
        .parse()
        .expect("HTTP status code should be numeric");
    let resp_body = raw[header_end + 4..].to_vec();
    (status, resp_body)
}

async fn start_test_rpc_server(
    max_connections: u32,
) -> (SocketAddr, ServerHandle, Arc<Notify>, Arc<Notify>) {
    let parked_started = Arc::new(Notify::new());
    let parked_release = Arc::new(Notify::new());

    let mut module = RpcModule::new(());
    {
        let parked_started = parked_started.clone();
        let parked_release = parked_release.clone();
        module
            .register_async_method("park", move |_, _, _| {
                let parked_started = parked_started.clone();
                let parked_release = parked_release.clone();
                async move {
                    parked_started.notify_one();
                    parked_release.notified().await;
                    "parked-ok"
                }
            })
            .expect("registering park succeeds");
    }
    module
        .register_method("ping", |_, _, _| "pong")
        .expect("registering ping succeeds");

    let registered_methods: Arc<HashSet<&'static str>> = Arc::new(module.method_names().collect());
    let http_middleware = tower::ServiceBuilder::new()
        .layer(HttpRequestMiddlewareLayer::new(None, 1024 * 1024))
        .layer(HttpMetricsLayer::new(max_connections));
    let rpc_middleware = RpcServiceBuilder::new()
        .layer_fn(move |service| RpcMetricsMiddleware::new(service, registered_methods.clone()));

    let server = Server::builder()
        .http_only()
        .max_connections(max_connections)
        .set_http_middleware(http_middleware)
        .set_rpc_middleware(rpc_middleware)
        .build(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("test RPC server should bind");
    let addr = server.local_addr().expect("bound server has local_addr");
    let server_handle = server.start(module);

    (addr, server_handle, parked_started, parked_release)
}

#[tokio::test]
async fn json_rpc_server_tracks_active_connections_and_429_rejections() {
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let (addr, server_handle, parked_started, parked_release) = start_test_rpc_server(1).await;

    let park_req = r#"{"jsonrpc":"2.0","method":"park","params":[],"id":1}"#;
    let first = tokio::spawn(send_raw_http_post(addr, park_req));
    parked_started.notified().await;

    assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(1.0));
    assert_eq!(recorder.gauge("rpc.active_requests", &[]), Some(1.0));

    // Second concurrent request exceeds `max_connections = 1` and gets HTTP 429.
    let ping_req = r#"{"jsonrpc":"2.0","method":"ping","params":[],"id":2}"#;
    let (status_429, _body_429) = send_raw_http_post(addr, ping_req).await;
    assert_eq!(status_429, 429);
    assert_eq!(
        recorder.counter("rpc.connections.rejected.total", &[]),
        Some(1)
    );
    assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(1.0));
    assert_eq!(recorder.gauge_max("rpc.connections.active", &[]), Some(1.0));
    assert!(recorder.histogram("rpc.request.size_bytes", &[]).is_empty());
    assert!(recorder
        .histogram("rpc.response.size_bytes", &[])
        .is_empty());
    assert!(recorder.histogram("rpc.batch.size", &[]).is_empty());

    // Release parked request and verify cleanup + single-request payload sizes.
    parked_release.notify_one();
    let (status_ok, body_ok) = first.await.unwrap();
    assert_eq!(status_ok, 200);
    assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(0.0));
    assert_eq!(recorder.gauge("rpc.active_requests", &[]), Some(0.0));
    assert_eq!(
        recorder.histogram("rpc.request.size_bytes", &[]),
        vec![park_req.len() as f64]
    );
    assert_eq!(
        recorder.histogram("rpc.response.size_bytes", &[]),
        vec![body_ok.len() as f64]
    );
    assert!(recorder.histogram("rpc.batch.size", &[]).is_empty());

    let _ = server_handle.stop();
}

#[tokio::test]
async fn json_rpc_server_records_batch_and_payload_size_histograms() {
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let (addr, server_handle, _parked_started, _parked_release) = start_test_rpc_server(2).await;

    let ping_req = r#"{"jsonrpc":"2.0","method":"ping","params":[],"id":2}"#;
    let batch_req = format!("[{ping_req},{ping_req}]");
    let (batch_status, batch_resp) = send_raw_http_post(addr, &batch_req).await;

    assert_eq!(batch_status, 200);
    assert_eq!(recorder.histogram("rpc.batch.size", &[]), vec![2.0]);
    assert_eq!(
        recorder.histogram("rpc.request.size_bytes", &[]),
        vec![batch_req.len() as f64]
    );
    assert_eq!(
        recorder.histogram("rpc.response.size_bytes", &[]),
        vec![batch_resp.len() as f64]
    );
    assert_eq!(
        recorder.counter(
            "rpc.requests.total",
            &[("method", "ping"), ("status", "success")],
        ),
        Some(2)
    );

    let _ = server_handle.stop();
}

#[tokio::test]
async fn rpc_server_start_initializes_connection_and_request_metrics_at_zero() {
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let conf = Config {
        listen_addr: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()),
        enable_cookie_auth: false,
        ..Default::default()
    };
    let mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let read_state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let block_verifier_router: MockService<_, _, _, BoxError> =
        MockService::build().for_unit_tests();

    let (_tx, rx) = watch::channel(None);
    let (rpc_impl, _) = RpcImpl::new(
        Mainnet,
        Default::default(),
        false,
        "RPC test",
        "RPC test",
        Buffer::new(mempool, 1),
        Buffer::new(state, 1),
        Buffer::new(read_state, 1),
        Buffer::new(block_verifier_router, 1),
        MockSyncStatus::default(),
        NoChainTip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    let task = RpcServer::start(rpc_impl, conf)
        .await
        .expect("RPC server should start");
    assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(0.0));
    assert_eq!(recorder.gauge("rpc.active_requests", &[]), Some(0.0));
    assert_eq!(
        recorder.counter("rpc.connections.rejected.total", &[]),
        Some(0)
    );

    task.abort();
}
