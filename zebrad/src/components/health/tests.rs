use super::*;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use chrono::{Duration as ChronoDuration, Utc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use zebra_chain::{
    block, chain_sync_status::mock::MockSyncStatus, chain_tip::mock::MockChainTip,
    parameters::Network,
};
use zebra_network::{address_book_peers::MockAddressBookPeers, PeerSocketAddr};

// Build a config tailored for tests: enable the listener and disable the
// built-in rate limiter so assertions don't race the cooldown unless a test
// overrides it.
fn config_for(addr: SocketAddr) -> Config {
    let mut config = Config {
        listen_addr: Some(addr),
        enforce_on_test_networks: true,
        ..Default::default()
    };
    config.min_request_interval = Duration::from_secs(0);
    config
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

// Produce a `MockChainTip` snapshot with controllable lag and tip age so each
// test can focus on a single failure condition.
fn configured_chain_tip(distance: block::HeightDiff, age: ChronoDuration) -> MockChainTip {
    let (chain_tip, sender) = MockChainTip::new();
    sender.send_best_tip_height(Some(block::Height(100))); // arbitrary non-zero height
    sender.send_best_tip_block_time(Some(Utc::now() - age));
    sender.send_estimated_distance_to_network_chain_tip(Some(distance));
    chain_tip
}

// Minimal HTTP client used by the tests; we intentionally avoid abstractions to
// verify the exact wire responses emitted by the health server.
async fn http_get(addr: SocketAddr, path: &str) -> (u16, String) {
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
        .expect("read ok");

    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    (status, text)
}

/// Happy-path coverage: peers, sync status, lag, and tip age are all within
/// thresholds, so both endpoints return 200.
#[tokio::test]
async fn healthy_and_ready_ok() {
    let cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);

    let chain_tip = configured_chain_tip(0, ChronoDuration::zero());

    let (task, addr_opt) = init(
        cfg,
        Network::Mainnet,
        chain_tip,
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr = addr_opt.expect("server bound addr");

    let (status_h, body_h) = http_get(addr, "/healthy").await;
    assert_eq!(status_h, 200, "healthy response: {}", body_h);

    let (status_r, body_r) = http_get(addr, "/ready").await;
    assert_eq!(status_r, 200, "ready response: {}", body_r);

    task.abort();
}

/// Losing sync progress or falling too far behind should stop readiness even
/// if Zebra still has peers.
#[tokio::test]
async fn not_ready_when_syncing_or_lagging() {
    let cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));

    // Syncing -> not ready
    let sync_status = MockSyncStatus::default();
    let chain_tip = configured_chain_tip(0, ChronoDuration::zero());
    let (task_syncing, addr_syncing) = init(
        cfg.clone(),
        Network::Mainnet,
        chain_tip,
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr_syncing = addr_syncing.expect("addr");
    let (status_syncing, body_syncing) = http_get(addr_syncing, "/ready").await;
    assert_eq!(status_syncing, 503, "body: {}", body_syncing);
    assert!(body_syncing.contains("syncing"));
    task_syncing.abort();

    // Lagging beyond threshold -> not ready
    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);
    let chain_tip = configured_chain_tip(5, ChronoDuration::zero());
    let (task_lagging, addr_lagging) = init(
        cfg,
        Network::Mainnet,
        chain_tip,
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr_lagging = addr_lagging.expect("addr");
    let (status_lagging, body_lagging) = http_get(addr_lagging, "/ready").await;
    assert_eq!(status_lagging, 503, "body: {}", body_lagging);
    assert!(body_lagging.contains("lag=5"));
    task_lagging.abort();
}

/// A stale block time must cause readiness to fail even if everything else
/// looks healthy, preventing long-lived false positives.
#[tokio::test]
async fn not_ready_when_tip_is_too_old() {
    let mut cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
    cfg.ready_max_tip_age = Duration::from_secs(1);

    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);

    let chain_tip = configured_chain_tip(0, ChronoDuration::seconds(10));

    let (task, addr_opt) = init(
        cfg,
        Network::Mainnet,
        chain_tip,
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr = addr_opt.expect("addr");

    let (status, body) = http_get(addr, "/ready").await;
    assert_eq!(status, 503, "body: {}", body);
    assert!(body.contains("tip_age"));

    task.abort();
}

/// With a sleep shorter than the configured interval we should only be able
/// to observe one successful request before the limiter responds with 429.
#[tokio::test]
async fn rate_limiting_drops_bursts() {
    let mut cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
    cfg.min_request_interval = Duration::from_millis(200);

    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);

    let chain_tip = configured_chain_tip(0, ChronoDuration::zero());

    let (task, addr_opt) = init(
        cfg,
        Network::Mainnet,
        chain_tip,
        sync_status,
        peers_with_count(1),
    )
    .await;
    let addr = addr_opt.expect("addr");

    let (first_status, first_body) = http_get(addr, "/healthy").await;
    assert_eq!(first_status, 200, "first response: {}", first_body);

    let (second_status, second_body) = http_get(addr, "/healthy").await;
    assert_eq!(second_status, 429, "second response: {}", second_body);

    tokio::time::sleep(Duration::from_millis(220)).await;

    let (third_status, third_body) = http_get(addr, "/healthy").await;
    assert_eq!(third_status, 200, "third response: {}", third_body);

    task.abort();
}

/// Regtest should always return ready regardless of peer count or sync status.
#[tokio::test]
async fn ready_always_returns_ok_on_regtest() {
    let cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
    let sync_status = MockSyncStatus::default(); // not close to tip
    let chain_tip = configured_chain_tip(100, ChronoDuration::seconds(3600)); // very old and lagging

    let (task, addr_opt) = init(
        cfg,
        Network::new_regtest(Default::default()),
        chain_tip,
        sync_status,
        peers_with_count(0), // no peers
    )
    .await;
    let addr = addr_opt.expect("addr");

    let (status, body) = http_get(addr, "/ready").await;
    assert_eq!(status, 200, "body: {}", body);
    assert!(body.contains("ok (regtest)"));

    task.abort();
}

/// Verify that both healthy and ready endpoints include peer count in response.
#[tokio::test]
async fn endpoints_include_peer_count() {
    let cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);
    let chain_tip = configured_chain_tip(0, ChronoDuration::zero());

    let (task, addr_opt) = init(
        cfg,
        Network::Mainnet,
        chain_tip,
        sync_status,
        peers_with_count(5), // 5 peers
    )
    .await;
    let addr = addr_opt.expect("addr");

    let (status_h, body_h) = http_get(addr, "/healthy").await;
    assert_eq!(status_h, 200, "healthy response: {}", body_h);
    assert!(
        body_h.contains("peers=5"),
        "healthy body should include peer count"
    );

    let (status_r, body_r) = http_get(addr, "/ready").await;
    assert_eq!(status_r, 200, "ready response: {}", body_r);
    assert!(
        body_r.contains("peers=5"),
        "ready body should include peer count"
    );

    task.abort();
}

/// Verify error messages include details about peer requirements.
#[tokio::test]
async fn insufficient_peers_shows_details() {
    let mut cfg = config_for(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
    cfg.min_connected_peers = 3; // require 3 peers

    let mut sync_status = MockSyncStatus::default();
    sync_status.set_is_close_to_tip(true);
    let chain_tip = configured_chain_tip(0, ChronoDuration::zero());

    let (task, addr_opt) = init(
        cfg,
        Network::Mainnet,
        chain_tip,
        sync_status,
        peers_with_count(1), // only 1 peer
    )
    .await;
    let addr = addr_opt.expect("addr");

    let (status_h, body_h) = http_get(addr, "/healthy").await;
    assert_eq!(status_h, 503, "healthy response: {}", body_h);
    assert!(body_h.contains("insufficient peers: 1 < 3"));

    let (status_r, body_r) = http_get(addr, "/ready").await;
    assert_eq!(status_r, 503, "ready response: {}", body_r);
    assert!(body_r.contains("insufficient peers: 1 < 3"));

    task.abort();
}
