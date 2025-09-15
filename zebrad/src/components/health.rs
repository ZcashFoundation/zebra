//! HTTP health and readiness endpoints for `zebrad`.
//!
//! Overview
//!
//! - This module exposes two small HTTP/1.1 endpoints for basic liveness/readiness checks,
//!   suitable for Kubernetes probes and load balancers.
//! - Endpoints are opt-in, disabled by default. Enable by setting a `listen_addr` in the
//!   `health` config section.
//! - Plain-text responses, constant-time logic, and small responses keep the checks fast and safe.
//!
//! Endpoints
//!
//! - `GET /healthy` — returns `200 OK` if the process is up and the node has at least
//!   `min_connected_peers` recently live peers (default: 1). Otherwise `503 Service Unavailable`.
//! - `GET /ready` — returns `200 OK` if both conditions hold:
//!   - the syncer is near tip (`ChainSyncStatus::is_close_to_tip()`), and
//!   - the estimated block lag is less than or equal to `ready_max_blocks_behind` (default: 2).
//!   Negative lag due to clock skew is treated as 0. If either check fails, returns `503`.
//!   On regtest/testnet, readiness returns `200` unless `enforce_on_test_networks` is set.
//!
//! Security
//!
//! - Endpoints are unauthenticated by design. Bind to internal interfaces, and restrict exposure
//!   using network policy, firewall rules, and service configuration.
//! - The server does not access or return private data. It only summarises coarse node state.
//!
//! Configuration and examples
//!
//! - See the Zebra Book for configuration details and Kubernetes probe examples:
//!   https://zebra.zfnd.org/user/health.html

use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use bytes::Bytes;
use chrono::Utc;
use http_body_util::Full;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::Method;
use hyper::{
    body::Incoming, http::response::Builder as ResponseBuilder, Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tracing::*;

use zebra_chain::{chain_sync_status::ChainSyncStatus, chain_tip::ChainTip, parameters::Network};
use zebra_network::AddressBookPeers;

/// Health server configuration.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Address to bind the health server to.
    ///
    /// The server is disabled when this is `None`.
    pub listen_addr: Option<SocketAddr>,
    /// Minimum number of recently live peers to consider the node healthy.
    ///
    /// Used by `/healthy`.
    pub min_connected_peers: usize,
    /// Maximum allowed estimated blocks behind the network tip for readiness.
    ///
    /// Used by `/ready`. Negative estimates are treated as 0.
    pub ready_max_blocks_behind: i64,
    /// Enforce readiness checks on test networks.
    ///
    /// If `false`, `/ready` always returns 200 on regtest and testnets.
    pub enforce_on_test_networks: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: None,
            min_connected_peers: 1,
            ready_max_blocks_behind: 2,
            enforce_on_test_networks: false,
        }
    }
}

/// Starts the health server if `listen_addr` is configured.
///
/// Returns a task handle and the bound socket address. When disabled, returns a
/// pending task and `None` for the address.
///
/// The server accepts HTTP/1.1 requests on a dedicated TCP listener and serves
/// two endpoints: `/healthy` and `/ready`.
///
/// Panics
///
/// - If the configured `listen_addr` cannot be bound.
pub async fn init<A, Tip, SyncStatus>(
    config: Config,
    network: Network,
    latest_chain_tip: Tip,
    sync_status: SyncStatus,
    address_book: A,
) -> (JoinHandle<()>, Option<SocketAddr>)
where
    A: AddressBookPeers + Clone + Send + Sync + 'static,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    if let Some(listen_addr) = config.listen_addr {
        info!("Trying to open health endpoint at {}...", listen_addr);

        // Bind before spawning to know the actual bound address.
        let listener = tokio::net::TcpListener::bind(listen_addr)
            .await
            .unwrap_or_else(|e| panic!("Opening health endpoint listener {listen_addr:?} failed: {e:?}. Hint: Check if another zebrad is running, or change the health listen_addr in the config."));

        let local = listener.local_addr().unwrap_or(listen_addr);
        info!("Opened health endpoint at {}", local);

        let shared = Arc::new(HealthCtx {
            config,
            network,
            latest_chain_tip,
            sync_status,
            address_book,
        });

        let task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _peer)) => {
                        let io = TokioIo::new(stream);
                        let svc_ctx = shared.clone();
                        let service = hyper::service::service_fn(move |req| {
                            handle_request(req, svc_ctx.clone())
                        });

                        tokio::spawn(async move {
                            if let Err(err) =
                                http1::Builder::new().serve_connection(io, service).await
                            {
                                debug!(?err, "health server connection closed with error");
                            }
                        });
                    }
                    Err(err) => {
                        warn!(?err, "health server accept failed");
                    }
                }
            }
        });

        (task, Some(local))
    } else {
        // Disabled: return a task that never completes.
        (tokio::spawn(std::future::pending()), None)
    }
}

#[derive(Clone)]
struct HealthCtx<A, Tip, SyncStatus>
where
    A: AddressBookPeers + Clone + Send + Sync + 'static,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    config: Config,
    network: Network,
    latest_chain_tip: Tip,
    sync_status: SyncStatus,
    address_book: A,
}

async fn handle_request<A, Tip, SyncStatus>(
    req: Request<Incoming>,
    ctx: Arc<HealthCtx<A, Tip, SyncStatus>>,
) -> Result<Response<Full<Bytes>>, Infallible>
where
    A: AddressBookPeers + Clone + Send + Sync + 'static,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    // Only allow GET; everything else is 405 Method Not Allowed
    if method != Method::GET {
        return Ok(simple_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
        ));
    }

    let res = match path.as_str() {
        "/healthy" => healthy(&ctx).await,
        "/ready" => ready(&ctx).await,
        _ => simple_response(StatusCode::NOT_FOUND, "not found"),
    };

    Ok(res)
}

/// `/healthy` endpoint handler.
///
/// Returns `200 OK` when the number of recently live peers is greater than or equal to
/// `min_connected_peers`. Otherwise returns `503 Service Unavailable`.
async fn healthy<A, Tip, SyncStatus>(ctx: &HealthCtx<A, Tip, SyncStatus>) -> Response<Full<Bytes>>
where
    A: AddressBookPeers + Clone + Send + Sync + 'static,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    let live = ctx.address_book.recently_live_peers(Utc::now()).len();
    if live >= ctx.config.min_connected_peers {
        simple_response(StatusCode::OK, "ok")
    } else {
        simple_response(StatusCode::SERVICE_UNAVAILABLE, "no peers")
    }
}

/// `/ready` endpoint handler.
///
/// Returns `200 OK` when the node is near the tip and the estimated lag is less than or equal
/// to `ready_max_blocks_behind`. Negative lags are treated as 0. When `enforce_on_test_networks`
/// is `false`, returns `200` on regtest/testnets regardless of state.
async fn ready<A, Tip, SyncStatus>(ctx: &HealthCtx<A, Tip, SyncStatus>) -> Response<Full<Bytes>>
where
    A: AddressBookPeers + Clone + Send + Sync + 'static,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    // Allow everything on test networks unless enforced
    if !ctx.config.enforce_on_test_networks && ctx.network.is_a_test_network() {
        return simple_response(StatusCode::OK, "ok");
    }

    if !ctx.sync_status.is_close_to_tip() {
        return simple_response(StatusCode::SERVICE_UNAVAILABLE, "syncing");
    }

    match ctx
        .latest_chain_tip
        .estimate_distance_to_network_chain_tip(&ctx.network)
    {
        Some((distance, _local_tip)) => {
            // Treat negative distance as 0 (ahead due to clock skew)
            let behind: i64 = std::cmp::max(0, distance);
            if behind <= ctx.config.ready_max_blocks_behind {
                simple_response(StatusCode::OK, "ok")
            } else {
                simple_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &format!("lag={} blocks", behind),
                )
            }
        }
        None => simple_response(StatusCode::SERVICE_UNAVAILABLE, "no tip"),
    }
}

/// Creates a minimal plain-text HTTP response with the provided status.
fn simple_response(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    let bytes = Bytes::from(body.to_string());
    let len = bytes.len();
    ResponseBuilder::new()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(CONTENT_LENGTH, len.to_string())
        .body(Full::new(bytes))
        .expect("valid response")
}

#[cfg(all(test, feature = "proptest-impl"))]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::timeout;
    use zebra_chain::{block, parameters::Network};
    use zebra_network::address_book_peers::MockAddressBookPeers;

    #[derive(Clone)]
    struct TestSyncStatus {
        close: bool,
    }

    impl ChainSyncStatus for TestSyncStatus {
        fn is_close_to_tip(&self) -> bool {
            self.close
        }
    }

    #[derive(Clone)]
    struct FixedDistanceTip {
        distance: block::HeightDiff,
    }

    impl ChainTip for FixedDistanceTip {
        fn best_tip_height(&self) -> Option<block::Height> {
            Some(block::Height(0))
        }
        fn best_tip_hash(&self) -> Option<block::Hash> {
            None
        }
        fn best_tip_height_and_hash(&self) -> Option<(block::Height, block::Hash)> {
            None
        }
        fn best_tip_block_time(&self) -> Option<chrono::DateTime<chrono::Utc>> {
            Some(chrono::Utc::now())
        }
        fn best_tip_height_and_block_time(
            &self,
        ) -> Option<(block::Height, chrono::DateTime<chrono::Utc>)> {
            Some((block::Height(0), chrono::Utc::now()))
        }
        fn best_tip_mined_transaction_ids(&self) -> Arc<[zebra_chain::transaction::Hash]> {
            Arc::new([])
        }
        async fn best_tip_changed(&mut self) -> Result<(), zebra_chain::BoxError> {
            Ok(())
        }
        fn mark_best_tip_seen(&mut self) {}

        fn estimate_distance_to_network_chain_tip(
            &self,
            _network: &Network,
        ) -> Option<(block::HeightDiff, block::Height)> {
            Some((self.distance, block::Height(0)))
        }
    }

    fn peers_with_count(count: usize) -> MockAddressBookPeers {
        use zebra_network::PeerSocketAddr;
        let mut mock = MockAddressBookPeers::default();
        for _ in 0..count {
            let sock = std::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
            let peer = PeerSocketAddr::from(sock);
            mock.add_peer(peer);
        }
        mock
    }

    async fn http_get(addr: SocketAddr, path: &str) -> (u16, String) {
        let mut stream = timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr))
            .await
            .expect("connect timeout")
            .expect("connect ok");
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            path
        );
        timeout(Duration::from_secs(2), stream.write_all(req.as_bytes()))
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

    #[tokio::test]
    async fn healthy_and_ready_ok() {
        let cfg = Config {
            listen_addr: Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)),
            min_connected_peers: 1,
            ready_max_blocks_behind: 2,
            enforce_on_test_networks: true,
        };
        let (task, addr_opt) = init(
            cfg,
            Network::Mainnet,
            FixedDistanceTip { distance: 0 },
            TestSyncStatus { close: true },
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

    #[tokio::test]
    async fn not_ready_when_syncing_or_lagging() {
        // Syncing -> not ready
        let cfg = Config {
            listen_addr: Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)),
            min_connected_peers: 1,
            ready_max_blocks_behind: 2,
            enforce_on_test_networks: true,
        };
        let (task1, addr1) = init(
            cfg.clone(),
            Network::Mainnet,
            FixedDistanceTip { distance: 0 },
            TestSyncStatus { close: false },
            peers_with_count(1),
        )
        .await;
        let addr1 = addr1.expect("addr");
        let (status_r1, _) = http_get(addr1, "/ready").await;
        assert_eq!(status_r1, 503);
        task1.abort();

        // Lagging beyond threshold -> not ready
        let (task2, addr2) = init(
            cfg,
            Network::Mainnet,
            FixedDistanceTip { distance: 5 },
            TestSyncStatus { close: true },
            peers_with_count(1),
        )
        .await;
        let addr2 = addr2.expect("addr");
        let (status_r2, body_r2) = http_get(addr2, "/ready").await;
        assert_eq!(status_r2, 503, "body: {}", body_r2);
        assert!(body_r2.contains("lag=5"));
        task2.abort();
    }

    #[tokio::test]
    async fn unhealthy_with_no_peers() {
        let cfg = Config {
            listen_addr: Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)),
            min_connected_peers: 1,
            ready_max_blocks_behind: 2,
            enforce_on_test_networks: true,
        };
        let (task, addr) = init(
            cfg,
            Network::Mainnet,
            FixedDistanceTip { distance: 0 },
            TestSyncStatus { close: true },
            peers_with_count(0),
        )
        .await;
        let addr = addr.expect("addr");
        let (status_h, body_h) = http_get(addr, "/healthy").await;
        assert_eq!(status_h, 503, "body: {}", body_h);
        assert!(body_h.contains("no peers"));
        task.abort();
    }
}
