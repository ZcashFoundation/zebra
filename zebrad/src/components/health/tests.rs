use super::*;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use zebra_chain::{chain_sync_status::mock::MockSyncStatus, parameters::Network};
use zebra_network::{address_book_peers::MockAddressBookPeers, PeerSocketAddr};

// Build a config tailored for tests: enable the listener and disable the
// built-in rate limiter so assertions don't race the cooldown unless a test
// overrides it.
fn config_for(addr: SocketAddr) -> Config {
    Config {
        listen_addr: Some(addr),
        enforce_on_test_networks: true,
        ..Default::default()
    }
}

// Populate the mock address book with `count` live peers so we can exercise the
// peer threshold logic without spinning up real network state.
fn peers_with_count(count: usize) -> MockAddressBookPeers {
    let mut peers = MockAddressBookPeers::default();
    for _ in 0..count {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let peer = PeerSocketAddr::from(socket);
        peers.add_peer(peer);
    }
    peers
}

// Minimal HTTP client used by the tests; we intentionally avoid abstractions to
// verify the exact wire responses emitted by the health server.
async fn http_get(addr: SocketAddr, path: &str) -> Option<(u16, String)> {
    let mut stream = timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr))
        .await
        .expect("connect timeout")
        .expect("connect ok");
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        path
    );
    timeout(Duration::from_secs(2), stream.write_all(request.as_bytes()))
        .await
        .expect("write timeout")
        .expect("write ok");

    let mut buf = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut buf))
        .await
        .expect("read timeout")
        .ok()?;

    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    Some((status, text))
}

#[tokio::test]
async fn healthy_and_ready_ok() {
    // Happy-path coverage: peers, sync status, lag, and tip age are all within
    // thresholds, so both endpoints return 200.
    let cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);

    let (chain_tip_metrics_sender, chain_tip_metrics_receiver) = ChainTipMetrics::channel();
    let _ = chain_tip_metrics_sender.send(ChainTipMetrics::new(Instant::now(), Some(0)));

    let (task, addr_opt) = init(
        cfg,
        Network::Mainnet,
        chain_tip_metrics_receiver,
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr = addr_opt.expect("server bound addr");

    let (status_h, body_h) = http_get(addr, "/healthy").await.unwrap();
    assert_eq!(status_h, 200, "healthy response: {}", body_h);

    let (status_r, body_r) = http_get(addr, "/ready").await.unwrap();
    assert_eq!(status_r, 200, "ready response: {}", body_r);

    task.abort();
}

#[tokio::test]
async fn not_ready_when_syncing_or_lagging() {
    let cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));

    // Syncing -> not ready
    let sync_status = MockSyncStatus::default();
    let (chain_tip_metrics_sender, chain_tip_metrics_receiver) = ChainTipMetrics::channel();
    let _ = chain_tip_metrics_sender.send(ChainTipMetrics::new(Instant::now(), Some(0)));
    let (task_syncing, addr_syncing) = init(
        cfg.clone(),
        Network::Mainnet,
        chain_tip_metrics_receiver.clone(),
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr_syncing = addr_syncing.expect("addr");
    let (status_syncing, body_syncing) = http_get(addr_syncing, "/ready").await.unwrap();
    assert_eq!(status_syncing, 503, "body: {}", body_syncing);
    assert!(body_syncing.contains("syncing"));
    task_syncing.abort();

    // Lagging beyond threshold -> not ready
    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);
    let _ = chain_tip_metrics_sender.send(ChainTipMetrics::new(Instant::now(), Some(5)));
    let (task_lagging, addr_lagging) = init(
        cfg,
        Network::Mainnet,
        chain_tip_metrics_receiver,
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr_lagging = addr_lagging.expect("addr");
    let (status_lagging, body_lagging) = http_get(addr_lagging, "/ready").await.unwrap();
    assert_eq!(status_lagging, 503, "body: {}", body_lagging);
    assert!(body_lagging.contains("lag=5"));
    task_lagging.abort();
}

#[tokio::test]
async fn not_ready_when_tip_is_too_old() {
    // A stale block time must cause readiness to fail even if everything else
    // looks healthy, preventing long-lived false positives.
    let mut cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
    cfg.ready_max_tip_age = Duration::from_secs(1);

    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);

    let (chain_tip_metrics_sender, chain_tip_metrics_receiver) = ChainTipMetrics::channel();
    let _ = chain_tip_metrics_sender.send(ChainTipMetrics::new(
        Instant::now()
            .checked_sub(Duration::from_secs(10))
            .expect("should not overflow"),
        Some(0),
    ));

    let (task, addr_opt) = init(
        cfg,
        Network::Mainnet,
        chain_tip_metrics_receiver,
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr = addr_opt.expect("addr");

    let (status, body) = http_get(addr, "/ready").await.unwrap();
    assert_eq!(status, 503, "body: {}", body);
    assert!(body.contains("tip_age"));

    task.abort();
}

#[tokio::test]
async fn rate_limiting_drops_bursts() {
    // With a sleep shorter than the configured interval we should only be able
    // to observe one successful request before the limiter responds with 429.
    let cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));

    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);

    let (chain_tip_metrics_sender, chain_tip_metrics_receiver) = ChainTipMetrics::channel();
    let _ = chain_tip_metrics_sender.send(ChainTipMetrics::new(Instant::now(), Some(0)));

    let (task, addr_opt) = init(
        cfg,
        Network::Mainnet,
        chain_tip_metrics_receiver,
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr = addr_opt.expect("addr");

    let (first_status, first_body) = http_get(addr, "/healthy").await.unwrap();
    assert_eq!(first_status, 200, "first response: {}", first_body);

    let mut was_request_dropped = false;
    for i in 0..(MAX_RECENT_REQUESTS + 10) {
        if http_get(addr, "/healthy").await.is_none() {
            was_request_dropped = true;
            println!("got expected status after some reqs: {i}");
            break;
        }
    }

    assert!(
        was_request_dropped,
        "requests should be dropped past threshold"
    );

    tokio::time::sleep(RECENT_REQUEST_INTERVAL + Duration::from_millis(100)).await;

    let (third_status, third_body) = http_get(addr, "/healthy").await.unwrap();
    assert_eq!(third_status, 200, "last response: {}", third_body);

    task.abort();
}
