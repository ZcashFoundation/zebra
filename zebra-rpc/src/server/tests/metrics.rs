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

fn parse_http_head(raw: &[u8]) -> Option<(usize, u16, Option<usize>)> {
    let header_end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let header = std::str::from_utf8(&raw[..header_end]).expect("HTTP headers should be UTF-8");
    let mut lines = header.lines();
    let status_line = lines.next().expect("status line should exist");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .expect("HTTP status code should exist")
        .parse()
        .expect("HTTP status code should be numeric");
    let content_length = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("content-length").then(|| {
            value
                .trim()
                .parse::<usize>()
                .expect("content-length header should be numeric")
        })
    });
    Some((header_end, status, content_length))
}

async fn write_http_post(stream: &mut TcpStream, addr: SocketAddr, connection: &str, body: &str) {
    let request = format!(
        "POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nConnection: {connection}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("should write HTTP request");
}

async fn send_raw_http_post(addr: SocketAddr, body: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("should connect to test RPC server");
    write_http_post(&mut stream, addr, "close", body).await;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("should read HTTP response");

    let (header_end, status, _) =
        parse_http_head(&raw).expect("response should contain HTTP header separator");
    let resp_body = raw[header_end + 4..].to_vec();
    (status, resp_body)
}

async fn send_keep_alive_http_post(
    stream: &mut TcpStream,
    addr: SocketAddr,
    body: &str,
) -> (u16, Vec<u8>) {
    write_http_post(stream, addr, "keep-alive", body).await;

    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if let Some((header_end, status, content_length)) = parse_http_head(&raw) {
            let body_start = header_end + 4;
            let len = content_length.expect("keep-alive response has content-length");
            if raw.len() >= body_start + len {
                return (status, raw[body_start..body_start + len].to_vec());
            }
        }

        let read = stream
            .read(&mut buf)
            .await
            .expect("should read HTTP keep-alive response");
        assert!(read > 0, "unexpected EOF while reading keep-alive response");
        raw.extend_from_slice(&buf[..read]);
    }
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

fn assert_connection_metrics(
    recorder: &TestMetricsRecorder,
    active: f64,
    max_active: f64,
    rejected: Option<u64>,
) {
    assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(active));
    assert_eq!(
        recorder.gauge_max("rpc.connections.active", &[]),
        Some(max_active)
    );
    assert_eq!(
        recorder.counter("rpc.connections.rejected.total", &[]),
        rejected
    );
}

fn assert_payload_histograms(
    recorder: &TestMetricsRecorder,
    req_bytes: &[f64],
    resp_bytes: &[f64],
    batch_sizes: &[f64],
) {
    assert_eq!(recorder.histogram("rpc.request.size_bytes", &[]), req_bytes);
    assert_eq!(
        recorder.histogram("rpc.response.size_bytes", &[]),
        resp_bytes
    );
    assert_eq!(recorder.histogram("rpc.batch.size", &[]), batch_sizes);
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

    assert_connection_metrics(&recorder, 1.0, 1.0, None);
    assert_eq!(recorder.gauge("rpc.active_requests", &[]), Some(1.0));

    // Second concurrent request exceeds `max_connections = 1` and gets HTTP 429.
    let ping_req = r#"{"jsonrpc":"2.0","method":"ping","params":[],"id":2}"#;
    let (status_429, _body_429) = send_raw_http_post(addr, ping_req).await;
    assert_eq!(status_429, 429);
    assert_connection_metrics(&recorder, 1.0, 1.0, Some(1));
    assert_payload_histograms(&recorder, &[], &[], &[]);

    // Release parked request and verify cleanup + single-request payload sizes.
    parked_release.notify_one();
    let (status_ok, body_ok) = first.await.unwrap();
    assert_eq!(status_ok, 200);
    assert_connection_metrics(&recorder, 0.0, 1.0, Some(1));
    assert_eq!(recorder.gauge("rpc.active_requests", &[]), Some(0.0));
    assert_payload_histograms(
        &recorder,
        &[park_req.len() as f64],
        &[body_ok.len() as f64],
        &[],
    );

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
    assert_payload_histograms(
        &recorder,
        &[batch_req.len() as f64],
        &[batch_resp.len() as f64],
        &[2.0],
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
async fn json_rpc_server_tracks_persistent_keep_alive_and_concurrent_connections() {
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let (addr, server_handle, parked_started, parked_release) = start_test_rpc_server(1).await;

    let mut keep_alive = TcpStream::connect(addr)
        .await
        .expect("should connect persistent HTTP stream");
    let ping_req = r#"{"jsonrpc":"2.0","method":"ping","params":[],"id":1}"#;

    for _ in 0..2 {
        let (status, _) = send_keep_alive_http_post(&mut keep_alive, addr, ping_req).await;
        assert_eq!(status, 200);
        assert_connection_metrics(&recorder, 0.0, 1.0, None);
    }

    let park_req = r#"{"jsonrpc":"2.0","method":"park","params":[],"id":2}"#;
    let first = tokio::spawn(async move {
        let (status, body) = send_keep_alive_http_post(&mut keep_alive, addr, park_req).await;
        (keep_alive, status, body)
    });
    parked_started.notified().await;
    assert_connection_metrics(&recorder, 1.0, 1.0, None);

    let mut second = TcpStream::connect(addr)
        .await
        .expect("should connect second persistent HTTP stream");
    let (status_429, _) = send_keep_alive_http_post(&mut second, addr, ping_req).await;
    assert_eq!(status_429, 429);
    assert_connection_metrics(&recorder, 1.0, 1.0, Some(1));

    parked_release.notify_one();
    let (mut keep_alive, status_ok, _) = first.await.unwrap();
    assert_eq!(status_ok, 200);
    assert_connection_metrics(&recorder, 0.0, 1.0, Some(1));

    let (reuse_first, _) = send_keep_alive_http_post(&mut keep_alive, addr, ping_req).await;
    assert_eq!(reuse_first, 200);
    let (reuse_second, _) = send_keep_alive_http_post(&mut second, addr, ping_req).await;
    assert_eq!(reuse_second, 200);
    assert_connection_metrics(&recorder, 0.0, 1.0, Some(1));

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
